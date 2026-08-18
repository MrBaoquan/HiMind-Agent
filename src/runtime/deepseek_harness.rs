use reqwest::blocking::Client;
use serde_json::{json, Value};
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::client::update_agent_run_status;
use crate::api::distribution::{resolve_runtime_component, RuntimeComponentUpdate};
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::app::system::{validate_update_download_url, verify_runtime_component_signature};
use crate::runtime::builtin::BuiltinAIRuntimeEvent;
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope, PROVIDER_BUILTIN};
use crate::skill::store::SkillStore;
use crate::skill::types::SkillRecord;
use crate::Options;

const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const OUTPUT_CAPTURE_LIMIT: usize = 256 * 1024;
const DSH_HOME_ENV: &str = "DSH_HOME";
const DSH_HOME_ROOT_ENV: &str = "HIMIND_DSH_HOME_ROOT";
const RUNTIME_PRODUCT_ID: &str = "com.himind.runtime.deepseek-harness";
const RUNTIME_CONTRACT: &str = PROVIDER_BUILTIN;
const RUNTIME_ENGINE_ID: &str = "deepseek-harness";
const RUNTIME_CHANNEL: &str = "stable";
const RUNTIME_PLATFORM: &str = "windows";
const RUNTIME_ARCHITECTURE: &str = "x64";
const RUNTIME_MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const RUNTIME_MAX_UNCOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const INSTALL_TIMEOUT_SECONDS: u64 = 30 * 60;
const UPDATE_CHECK_TIMEOUT_SECONDS: u64 = 30;
const INTERACTIVE_PERMISSION_MODE: &str = "workspace-write";
const HIMIND_PROFILE: &str = "himind";
const HIMIND_MCP_SERVER_NAME: &str = "himind";
const HIMIND_MCP_CLIENT_ID: &str = "himind-ai";
const HIMIND_SKILL_ADAPTER_DIR: &str = "himind-skills";
const DSH_SETTINGS_MIGRATION_MARKER: &str = ".himind-dsh-settings-v2";

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct InstalledRuntimeState {
    schema_version: u32,
    product_id: String,
    provider: String,
    version: String,
    executable_path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DeepSeekHarnessRuntimeStatus {
    pub provider: String,
    pub status: String,
    pub version: String,
    pub cli_compatible: bool,
    pub executable_path: String,
    pub install_command: String,
    pub message: String,
    pub candidate: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DeepSeekHarnessRuntimeUpdateStatus {
    pub update_available: bool,
    pub current_version: String,
    pub available_version: String,
    pub release_notes: String,
    pub mandatory: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InteractiveLaunch {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub api_key: String,
    pub base_url: String,
    pub permission_mode: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct InteractiveToolContextSummary {
    pub skills: usize,
    pub mcp_services: usize,
}

#[derive(Debug, Default)]
struct InteractiveSessionProjection {
    current_turn: String,
    next_sequence: i64,
    tool_labels: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub(crate) struct InteractiveEventProjector {
    sessions: HashMap<String, InteractiveSessionProjection>,
}

impl InteractiveEventProjector {
    pub(crate) fn project(&mut self, message: &Value) -> Option<BuiltinAIRuntimeEvent> {
        let frame = message.get("payload").unwrap_or(message);
        let frame_type = frame.get("type")?.as_str()?;
        let session_id = bounded_text(frame.get("sessionId")?.as_str()?, 240);
        if session_id.is_empty() {
            return None;
        }
        let state = self.sessions.entry(session_id.clone()).or_default();
        if frame_type == "session/event" {
            return project_session_event(&session_id, state, frame.get("event")?);
        }

        let rpc_id = message
            .get("rpcId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (event_type, identity, label, outcome) = match frame_type {
            "approval/requested" => (
                "approval.requested",
                frame.get("approvalId")?.as_str()?,
                bounded_text(
                    frame
                        .get("toolName")
                        .and_then(Value::as_str)
                        .unwrap_or("需要确认的操作"),
                    240,
                ),
                String::new(),
            ),
            "approval/resolved" => (
                "approval.resolved",
                frame.get("approvalId")?.as_str()?,
                String::new(),
                bounded_text(frame.get("outcome")?.as_str()?, 80),
            ),
            "question/requested" => (
                "question.requested",
                rpc_id,
                "需要你的回复".to_string(),
                String::new(),
            ),
            "question/resolved" => (
                "question.resolved",
                frame
                    .get("questionRpcId")
                    .and_then(Value::as_str)
                    .unwrap_or(rpc_id),
                String::new(),
                bounded_text(frame.get("outcome")?.as_str()?, 80),
            ),
            _ => return None,
        };
        if identity.trim().is_empty() {
            return None;
        }
        let sequence = next_projection_sequence(state, None);
        Some(BuiltinAIRuntimeEvent {
            schema_version: 1,
            session_id: session_id.clone(),
            event_id: stable_event_id(&session_id, frame_type, identity),
            sequence,
            turn_id: state.current_turn.clone(),
            event_type: event_type.to_string(),
            content: String::new(),
            label,
            outcome,
            occurred_at_ms: unix_time_millis(),
        })
    }
}

fn project_session_event(
    session_id: &str,
    state: &mut InteractiveSessionProjection,
    event: &Value,
) -> Option<BuiltinAIRuntimeEvent> {
    let source_type = event.get("type")?.as_str()?;
    let source_sequence = event.get("seq")?.as_i64()?;
    let occurred_at_ms = event
        .get("time")
        .and_then(Value::as_i64)
        .unwrap_or_else(unix_time_millis);
    let data = event.get("data")?;
    let mut content = String::new();
    let mut label = String::new();
    let mut outcome = String::new();
    let mut turn_id = data
        .get("turn")
        .and_then(Value::as_i64)
        .map(|turn| format!("turn-{turn}"))
        .unwrap_or_else(|| state.current_turn.clone());
    let event_type = match source_type {
        "turn/start" => {
            if turn_id.is_empty() {
                return None;
            }
            state.current_turn = turn_id.clone();
            "turn.started"
        }
        "user/message" => {
            if data
                .get("source")
                .and_then(|source| source.get("kind"))
                .and_then(Value::as_str)
                != Some("user")
            {
                return None;
            }
            content = message_text(data);
            if content.is_empty() {
                return None;
            }
            turn_id = state.current_turn.clone();
            "message.user"
        }
        "assistant/message" => {
            content = message_text(data.get("message")?);
            if content.is_empty() {
                return None;
            }
            "message.assistant"
        }
        "tool/call" => {
            let call_id = data.get("callId")?.as_str()?;
            label = bounded_text(data.get("name")?.as_str()?, 240);
            state.tool_labels.insert(call_id.to_string(), label.clone());
            "tool.started"
        }
        "tool/result" => {
            let block = data
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first());
            let call_id = block
                .and_then(|block| block.get("toolCallId"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            label = state.tool_labels.remove(call_id).unwrap_or_default();
            let failed = data.get("error").is_some_and(|value| !value.is_null())
                || block
                    .and_then(|value| value.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            outcome = if failed { "failed" } else { "succeeded" }.to_string();
            "tool.completed"
        }
        "turn/end" => {
            let kind = data
                .get("reason")
                .and_then(|reason| reason.get("kind"))
                .and_then(Value::as_str)?;
            outcome = match kind {
                "completed" => "succeeded",
                "aborted" => "canceled",
                "blocked" | "error" | "max-tokens" | "interrupted" => "failed",
                _ => return None,
            }
            .to_string();
            state.current_turn.clear();
            state.tool_labels.clear();
            match outcome.as_str() {
                "succeeded" => "turn.completed",
                "canceled" => "turn.canceled",
                _ => "turn.failed",
            }
        }
        _ => return None,
    };
    let sequence = next_projection_sequence(state, Some(source_sequence));
    Some(BuiltinAIRuntimeEvent {
        schema_version: 1,
        session_id: session_id.to_string(),
        event_id: stable_event_id(session_id, source_type, &source_sequence.to_string()),
        sequence,
        turn_id,
        event_type: event_type.to_string(),
        content,
        label,
        outcome,
        occurred_at_ms,
    })
}

fn next_projection_sequence(
    state: &mut InteractiveSessionProjection,
    source_sequence: Option<i64>,
) -> i64 {
    let candidate = source_sequence
        .unwrap_or(state.next_sequence)
        .saturating_add(1);
    let sequence = candidate.max(state.next_sequence.saturating_add(1));
    state.next_sequence = sequence;
    sequence
}

fn message_text(message: &Value) -> String {
    let text = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    bounded_text(text.trim(), 64 * 1024)
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn stable_event_id(session_id: &str, event_type: &str, identity: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(session_id.as_bytes());
    digest.update([0]);
    digest.update(event_type.as_bytes());
    digest.update([0]);
    digest.update(identity.as_bytes());
    format!("{:x}", digest.finalize())
}

fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[derive(Debug, serde::Deserialize)]
struct RuntimePackageManifest {
    schema_version: u32,
    product_id: String,
    runtime_contract: String,
    engine_id: String,
    version: String,
    executable: String,
}

#[derive(Debug)]
struct Invocation {
    executable: OsString,
    args: Vec<OsString>,
    workspace: PathBuf,
    home: PathBuf,
    api_key: String,
    base_url: String,
    model: String,
    models: Vec<String>,
    permission_mode: &'static str,
    run_id: String,
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    let resolved = resolve_executable();
    let executable = resolved.as_ref().ok();
    let version = executable
        .as_ref()
        .and_then(|path| process::verify_command(path, &["--version"]).ok())
        .map(|value| first_line(&value))
        .unwrap_or_default();
    let cli_compatible = executable.as_ref().is_some_and(|path| {
        process::verify_command(path, &["--profile", "headless", "--help"]).is_ok()
    });
    RuntimeInstallationReport {
        provider: PROVIDER_BUILTIN.to_string(),
        version: if version.is_empty() {
            resolved
                .as_ref()
                .ok()
                .and_then(|_| load_runtime_state().ok().flatten())
                .map(|state| state.version)
                .unwrap_or_default()
        } else {
            version
        },
        status: if cli_compatible {
            "ready"
        } else {
            "unavailable"
        }
        .to_string(),
        capabilities: json!({
            "managed_execution": true,
            "billing_owner": "himind",
            "ai_proxy": true,
            "runtime_contract": 1,
            "engine_id": RUNTIME_ENGINE_ID,
            "cli_compatible": cli_compatible,
            "sandbox": "windows-restricted-token"
        }),
    }
}

pub(crate) fn status() -> DeepSeekHarnessRuntimeStatus {
    let executable = resolve_executable().ok();
    let version = executable
        .as_ref()
        .and_then(|path| process::verify_command(path, &["--version"]).ok())
        .and_then(|value| parse_runtime_version(&value))
        .unwrap_or_default();
    let cli_compatible = executable.as_ref().is_some_and(|path| {
        process::verify_command(path, &["--profile", "headless", "--help"]).is_ok()
    });
    let installed_version = load_runtime_state()
        .ok()
        .flatten()
        .map(|state| state.version)
        .unwrap_or_default();
    DeepSeekHarnessRuntimeStatus {
        provider: PROVIDER_BUILTIN.to_string(),
        status: if cli_compatible {
            "ready"
        } else {
            "unavailable"
        }
        .to_string(),
        version: if version.is_empty() {
            installed_version
        } else {
            version
        },
        cli_compatible,
        executable_path: executable
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        install_command: "Dashboard Runtime Resolve + SHA-256/RSA-PSS 校验 + 隔离安装".to_string(),
        message: if cli_compatible {
            "Runtime engine is ready.".to_string()
        } else {
            "Runtime engine is unavailable.".to_string()
        },
        candidate: false,
    }
}

pub(crate) fn check_update(
    options: &Options,
    client_instance_id: &str,
) -> Result<DeepSeekHarnessRuntimeUpdateStatus, String> {
    let runtime = status();
    if !runtime.cli_compatible || runtime.version.trim().is_empty() {
        return Err("HiMind AI 运行时尚未安装".to_string());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(UPDATE_CHECK_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("创建 Dashboard 客户端失败: {error}"))?;
    let update = resolve_runtime_component(
        &client,
        &options.api_base,
        RUNTIME_PRODUCT_ID,
        &runtime.version,
        RUNTIME_CHANNEL,
        RUNTIME_PLATFORM,
        RUNTIME_ARCHITECTURE,
        client_instance_id,
    )
    .map_err(|error| format!("检查 HiMind AI 运行时更新失败: {error}"))?;
    if let Some(update) = update {
        validate_update(&options.api_base, &update)?;
        return Ok(DeepSeekHarnessRuntimeUpdateStatus {
            update_available: true,
            current_version: runtime.version,
            available_version: update.version,
            release_notes: update.release_notes,
            mandatory: update.mandatory,
        });
    }
    Ok(DeepSeekHarnessRuntimeUpdateStatus {
        update_available: false,
        current_version: runtime.version,
        available_version: String::new(),
        release_notes: String::new(),
        mandatory: false,
    })
}

pub(crate) fn install(
    options: &Options,
    client_instance_id: &str,
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    let mut ignore_progress = |_: &str, _: u8, _: &str| {};
    install_with_progress(options, client_instance_id, &mut ignore_progress)
}

pub(crate) fn install_with_progress(
    options: &Options,
    client_instance_id: &str,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    install_resolved_with_progress(options, client_instance_id, "0.0.0", report_progress)
}

pub(crate) fn update_with_progress(
    options: &Options,
    client_instance_id: &str,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    let runtime = status();
    if !runtime.cli_compatible || runtime.version.trim().is_empty() {
        return Err("HiMind AI 运行时尚未安装".to_string());
    }
    install_resolved_with_progress(
        options,
        client_instance_id,
        &runtime.version,
        report_progress,
    )
}

fn install_resolved_with_progress(
    options: &Options,
    client_instance_id: &str,
    current_version: &str,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    report_progress("resolving", 5, "正在检查可用的 HiMind AI 运行时");
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(INSTALL_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("创建 Dashboard 客户端失败: {error}"))?;
    let update = resolve_runtime_component(
        &client,
        &options.api_base,
        RUNTIME_PRODUCT_ID,
        current_version,
        RUNTIME_CHANNEL,
        RUNTIME_PLATFORM,
        RUNTIME_ARCHITECTURE,
        client_instance_id,
    )
    .map_err(|error| format!("解析 HiMind AI 运行时发布失败: {error}"))?;
    let Some(update) = update else {
        if current_version == "0.0.0" {
            return Err("当前没有可用的 HiMind AI 运行时安装包".to_string());
        }
        report_progress("ready", 100, "HiMind AI 运行时已是最新版本");
        return Ok(status());
    };
    validate_update(&options.api_base, &update)?;
    report_progress("downloading", 12, "正在下载 HiMind AI 运行时");
    let archive = download_runtime_archive(&client, &update, report_progress)?;
    report_progress("verifying", 78, "正在校验下载的运行时");
    let result = install_runtime_archive(&archive, &update, report_progress);
    if result.is_ok() {
        let _ = fs::remove_file(&archive);
    }
    result.map(|_| {
        report_progress("ready", 100, "HiMind AI 运行时已准备就绪");
        status()
    })
}

pub(crate) fn uninstall_with_progress(
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    if load_runtime_state().ok().flatten().is_none()
        && env::var_os("HIMIND_DSH_EXECUTABLE").is_some_and(|value| !value.is_empty())
    {
        return Err("当前运行时由外部环境提供，不能由 Agent 卸载".to_string());
    }
    report_progress("uninstalling", 20, "正在停止并移除 HiMind AI 运行时");
    remove_managed_runtime(&runtime_root())?;
    report_progress("uninstalling", 100, "HiMind AI 运行时已卸载");
    Ok(status())
}

pub(crate) fn prepare_interactive_launch(options: &Options) -> Result<InteractiveLaunch, String> {
    let executable = resolve_executable().map_err(|error| error.to_string())?;
    let version = resolve_runtime_version(&executable).map_err(|error| error.to_string())?;
    let delegated =
        crate::api::oauth::platform_access_token(options, crate::api::oauth::AI_CONVERSATION_SCOPE)
            .map_err(|error| error.to_string())?;
    let credential =
        crate::api::ai::fetch_client_credential(options, &delegated.user_id, "himind-agent")
            .map_err(|error| error.to_string())?;
    let home = dsh_home(&version)?;
    let models = managed_model_catalog(&credential.access)?;
    let model = credential.access.model.trim().to_string();
    let invocation = Invocation {
        executable: executable.clone(),
        args: Vec::new(),
        workspace: std::env::current_dir().map_err(|error| error.to_string())?,
        home: home.clone(),
        api_key: credential.api_key.clone(),
        base_url: credential.access.base_url.clone(),
        model,
        models,
        permission_mode: INTERACTIVE_PERMISSION_MODE,
        run_id: "interactive".to_string(),
    };
    ensure_home_config(&invocation, options).map_err(|error| error.to_string())?;
    Ok(InteractiveLaunch {
        executable: PathBuf::from(executable),
        home,
        api_key: credential.api_key,
        base_url: credential.access.base_url,
        permission_mode: INTERACTIVE_PERMISSION_MODE,
    })
}

pub(crate) fn interactive_tool_context_summary(
    options: &Options,
) -> Result<InteractiveToolContextSummary, String> {
    let personal_mcp = crate::app::mcp_settings::load(&options.state_path)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|server| server.enabled)
        .count();
    Ok(InteractiveToolContextSummary {
        skills: himind_skill_records()
            .map_err(|error| error.to_string())?
            .len(),
        // The managed profile injects the Agent's local MCP bridge for every
        // interactive session. Individual capabilities remain governed by Agent.
        mcp_services: 1 + personal_mcp,
    })
}

fn managed_model_catalog(
    credential: &crate::api::ai::AIUserCredential,
) -> Result<Vec<String>, String> {
    let default_model = credential.model.trim();
    if default_model.is_empty() {
        return Err("当前账号没有可用模型".to_string());
    }
    let mut models = vec![default_model.to_string()];
    models.extend(
        credential
            .models
            .iter()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .map(str::to_string),
    );
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    if models.is_empty() {
        return Err("当前账号没有可用模型".to_string());
    }
    Ok(models)
}

fn validate_update(api_base: &str, update: &RuntimeComponentUpdate) -> Result<(), String> {
    if update.product_id != RUNTIME_PRODUCT_ID
        || update.channel != RUNTIME_CHANNEL
        || update.package_type != "directory-zip"
        || !update.file_name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err(
            "Dashboard Runtime manifest 不符合 DeepSeek Harness directory-zip 契约。".to_string(),
        );
    }
    validate_update_download_url(api_base, &update.artifact_url)
        .map_err(|error| format!("Runtime 下载地址校验失败: {error}"))?;
    if update.size == 0 || update.size > RUNTIME_MAX_PACKAGE_BYTES {
        return Err("Runtime 包大小超出 Agent 安全限制。".to_string());
    }
    Ok(())
}

fn download_runtime_archive(
    client: &Client,
    update: &RuntimeComponentUpdate,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<PathBuf, String> {
    let directory = runtime_root().join("downloads");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建 Runtime 下载目录失败: {error}"))?;
    let target = directory.join(format!("{}.zip", update.sha256.to_ascii_lowercase()));
    let partial = target.with_extension("zip.part");
    let mut response = client
        .get(&update.artifact_url)
        .send()
        .map_err(|error| format!("下载 Runtime 失败: {error}"))?;
    response
        .error_for_status_ref()
        .map_err(|error| format!("Runtime 下载响应失败: {error}"))?;
    let mut file =
        File::create(&partial).map_err(|error| format!("创建 Runtime 临时文件失败: {error}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    let mut last_reported_percent = 12_u8;
    loop {
        let count = response
            .read(&mut buffer)
            .map_err(|error| format!("读取 Runtime 下载失败: {error}"))?;
        if count == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(count as u64);
        if downloaded > update.size || downloaded > RUNTIME_MAX_PACKAGE_BYTES {
            let _ = fs::remove_file(&partial);
            return Err("Runtime 下载大小超出 manifest。".to_string());
        }
        file.write_all(&buffer[..count])
            .map_err(|error| format!("写入 Runtime 下载失败: {error}"))?;
        hasher.update(&buffer[..count]);
        let percent = 12_u8
            .saturating_add(((downloaded.saturating_mul(64) / update.size.max(1)).min(64)) as u8);
        if percent >= last_reported_percent.saturating_add(2) {
            report_progress("downloading", percent, "正在下载 HiMind AI 运行时");
            last_reported_percent = percent;
        }
    }
    file.flush()
        .map_err(|error| format!("刷新 Runtime 下载失败: {error}"))?;
    if downloaded != update.size
        || !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&update.sha256)
    {
        let _ = fs::remove_file(&partial);
        return Err("Runtime SHA-256 或文件大小校验失败。".to_string());
    }
    verify_runtime_component_signature(
        &partial,
        &update.signature,
        &update.signature_key_id,
        &update.signature_algorithm,
    )
    .map_err(|error| format!("Runtime RSA-PSS 签名校验失败: {error}"))?;
    if target.exists() {
        let _ = fs::remove_file(&target);
    }
    fs::rename(&partial, &target).map_err(|error| format!("保存 Runtime 下载包失败: {error}"))?;
    Ok(target)
}

fn install_runtime_archive(
    archive_path: &Path,
    update: &RuntimeComponentUpdate,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<(), String> {
    report_progress("installing", 84, "正在安装 HiMind AI 运行时");
    let versions = runtime_root().join("versions");
    fs::create_dir_all(&versions).map_err(|error| format!("创建 Runtime 安装目录失败: {error}"))?;
    let suffix = &update.sha256[..12.min(update.sha256.len())];
    let target = versions.join(format!("{}-{}", safe_segment(&update.version)?, suffix));
    let temporary = target.with_extension("installing");
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    extract_runtime_archive(archive_path, &temporary)?;
    report_progress("verifying", 92, "正在验证 HiMind AI 运行时");
    let manifest: RuntimePackageManifest = serde_json::from_slice(
        &fs::read(temporary.join("runtime.json"))
            .map_err(|error| format!("读取 runtime.json 失败: {error}"))?,
    )
    .map_err(|error| format!("runtime.json 无效: {error}"))?;
    let executable = safe_relative_path(&manifest.executable)?;
    if manifest.schema_version != 2
        || manifest.product_id != RUNTIME_PRODUCT_ID
        || manifest.runtime_contract != RUNTIME_CONTRACT
        || manifest.engine_id != RUNTIME_ENGINE_ID
        || manifest.version != update.version
        || !temporary.join(&executable).is_file()
    {
        let _ = fs::remove_dir_all(&temporary);
        return Err("Runtime manifest 与 Dashboard 发布信息不一致。".to_string());
    }
    if let Err(error) =
        verify_staged_runtime(&temporary.join(&executable), &temporary, &manifest.version)
    {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    let previous = target.with_extension("previous");
    if previous.exists() {
        let _ = fs::remove_dir_all(&previous);
    }
    if target.exists() {
        fs::rename(&target, &previous)
            .map_err(|error| format!("备份当前 HiMind AI 运行时失败: {error}"))?;
    }
    report_progress("installing", 97, "正在完成 HiMind AI 运行时安装");
    if let Err(error) = fs::rename(&temporary, &target) {
        if previous.exists() {
            let _ = fs::rename(&previous, &target);
        }
        return Err(format!("提交 HiMind AI 运行时安装失败: {error}"));
    }
    let state = InstalledRuntimeState {
        schema_version: 2,
        product_id: RUNTIME_PRODUCT_ID.to_string(),
        provider: RUNTIME_CONTRACT.to_string(),
        version: manifest.version,
        executable_path: target.join(executable).to_string_lossy().to_string(),
    };
    if let Err(error) = write_runtime_state(&state) {
        let _ = fs::remove_dir_all(&target);
        if previous.exists() {
            let _ = fs::rename(&previous, &target);
        }
        return Err(error);
    }
    if previous.exists() {
        let _ = fs::remove_dir_all(previous);
    }
    Ok(())
}

fn verify_staged_runtime(
    executable: &Path,
    workspace: &Path,
    expected_version: &str,
) -> Result<(), String> {
    let preflight_home = workspace.join(".himind-preflight");
    fs::create_dir_all(&preflight_home)
        .map_err(|error| format!("创建 Runtime 预检目录失败: {error}"))?;
    let result = (|| -> Result<(), String> {
        let run = |arguments: &[&str]| -> Result<String, String> {
            let mut command = Command::new(executable);
            process::remove_himind_secret_environment(&mut command);
            command
                .args(arguments)
                .current_dir(workspace)
                .env(DSH_HOME_ENV, &preflight_home)
                .env("DSH_TELEMETRY_MODE", "DISABLED")
                .env("NO_COLOR", "1")
                .env_remove("DEEPSEEK_API_KEY")
                .env_remove("OPENAI_API_KEY")
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("GOOGLE_API_KEY")
                .env_remove("GEMINI_API_KEY")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            process::configure_hidden_process(&mut command);
            let output = command
                .output()
                .map_err(|error| format!("启动 Runtime 预检失败: {error}"))?;
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .trim()
            .to_string();
            if !output.status.success() {
                return Err(format!(
                    "Runtime CLI 预检失败 (exit={}): {}",
                    output.status.code().unwrap_or(-1),
                    process::summarize_output(&combined, 2_000)
                ));
            }
            Ok(combined)
        };
        let version = run(&["--version"])?;
        if !version
            .lines()
            .any(|line| line.trim().contains(expected_version))
        {
            return Err(format!(
                "Runtime CLI 版本与 manifest 不一致: expected={expected_version}, actual={}",
                first_line(&version)
            ));
        }
        run(&["--profile", "headless", "--help"])?;
        run(&["--profile", "headless", "--dump-default-config"])?;
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&preflight_home)
        .map_err(|error| format!("清理 Runtime 预检目录失败: {error}"));
    result.and(cleanup)
}

fn extract_runtime_archive(archive_path: &Path, target: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(
        File::open(archive_path).map_err(|error| format!("打开 Runtime ZIP 失败: {error}"))?,
    )
    .map_err(|error| format!("Runtime ZIP 无效: {error}"))?;
    if archive.len() == 0 || archive.len() > 50_000 {
        return Err("Runtime ZIP 文件数量不合法。".to_string());
    }
    fs::create_dir_all(target).map_err(|error| format!("创建 Runtime 临时目录失败: {error}"))?;
    let mut total = 0_u64;
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取 Runtime ZIP 条目失败: {error}"))?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| "Runtime ZIP 包含不安全路径。".to_string())?
            .to_path_buf();
        let name = entry.name().to_string();
        if !seen.insert(name.clone())
            || name.contains('\\')
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Runtime ZIP 包含反斜杠或符号链接。".to_string());
        }
        total = total.saturating_add(entry.size());
        if total > RUNTIME_MAX_UNCOMPRESSED_BYTES {
            return Err("Runtime ZIP 解压内容过大。".to_string());
        }
        let destination = target.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("创建 Runtime 目录失败: {error}"))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建 Runtime 目录失败: {error}"))?;
        }
        let mut output =
            File::create(destination).map_err(|error| format!("创建 Runtime 文件失败: {error}"))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("解压 Runtime 文件失败: {error}"))?;
    }
    if !target.join("runtime.json").is_file() {
        let _ = fs::remove_dir_all(target);
        return Err("Runtime ZIP 缺少 runtime.json。".to_string());
    }
    Ok(())
}

fn safe_segment(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err("Runtime 版本号不适合作为目录名。".to_string());
    }
    Ok(value.to_string())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value.trim());
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("Runtime executable 路径不安全。".to_string());
    }
    Ok(path.to_path_buf())
}

fn write_runtime_state(state: &InstalledRuntimeState) -> Result<(), String> {
    let path = runtime_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 Runtime 状态目录失败: {error}"))?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(state)
            .map_err(|error| format!("序列化 Runtime 状态失败: {error}"))?,
    )
    .map_err(|error| format!("写入 Runtime 状态失败: {error}"))?;
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    fs::rename(temporary, path).map_err(|error| format!("提交 Runtime 状态失败: {error}"))
}

fn remove_managed_runtime(root: &Path) -> Result<(), String> {
    for directory in [root.join("versions"), root.join("downloads")] {
        if directory.exists() {
            fs::remove_dir_all(&directory)
                .map_err(|error| format!("移除 HiMind AI 运行时文件失败: {error}"))?;
        }
    }
    for file in [root.join("state.json"), root.join("state.json.tmp")] {
        if file.exists() {
            fs::remove_file(&file)
                .map_err(|error| format!("移除 HiMind AI 运行时状态失败: {error}"))?;
        }
    }
    if root.is_dir()
        && fs::read_dir(root)
            .map_err(|error| format!("检查 HiMind AI 运行时目录失败: {error}"))?
            .next()
            .is_none()
    {
        fs::remove_dir(root).map_err(|error| format!("移除 HiMind AI 运行时目录失败: {error}"))?;
    }
    Ok(())
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    execute_managed(
        client,
        options,
        agent_id,
        task,
        envelope,
        PROVIDER_BUILTIN,
        |claim| execute_claimed(client, options, agent_id, task, claim),
    )
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
) -> Result<Value, Box<dyn Error>> {
    let invocation = build_invocation(options, claim)?;
    process::verify_command(&invocation.executable, &["--version"])
        .map_err(|error| format!("DeepSeek Harness CLI is unavailable: {error}"))?;
    ensure_home_config(&invocation, options)?;
    update_agent_run_status(
        client,
        &options.api_base,
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = process::start_run_lease_renewal(client, options, agent_id, claim);
    let mut child = spawn(&invocation)?;
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child(
        client,
        options,
        agent_id,
        &task.id,
        &mut child,
        "HIMIND_DSH_TIMEOUT_SECONDS",
        DEFAULT_TIMEOUT_SECONDS,
        "DeepSeek Harness",
    )?;
    let stdout = process::join_output(stdout);
    let stderr = process::join_output(stderr);
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!(
            "DeepSeek Harness execution failed (exit={}): {}",
            status.code().unwrap_or(-1),
            process::redact_error(&detail, claim, &options.agent_credential())
        )
        .into());
    }
    let final_message = stdout.trim().to_string();
    if final_message.is_empty() {
        return Err("DeepSeek Harness returned an empty final response".into());
    }
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": PROVIDER_BUILTIN,
        "completed": true,
        "final_message": process::summarize_output(&final_message, OUTPUT_CAPTURE_LIMIT),
        "billing_owner": "himind"
    }))
}

fn build_invocation(
    options: &Options,
    claim: &AgentRunClaim,
) -> Result<Invocation, Box<dyn Error>> {
    if claim.claim_token.trim().is_empty() {
        return Err("Dashboard did not return an Agent Run AI proxy credential".into());
    }
    if claim.ai_model.trim().is_empty() {
        return Err("Dashboard did not configure a model for DeepSeek Harness".into());
    }
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let mut prompt = claim.run.instruction.trim().to_string();
    if prompt.is_empty() {
        return Err("Agent Run instruction is empty".into());
    }
    if !claim.run.input.is_null()
        && claim
            .run
            .input
            .as_object()
            .is_none_or(|value| !value.is_empty())
    {
        prompt.push_str("\n\nStructured input (JSON):\n");
        prompt.push_str(&serde_json::to_string(&claim.run.input)?);
    }
    prompt.push_str("\n\nReturn only a JSON object with keys summary, changes, verification, and remaining_risks.");
    let executable = resolve_executable()?;
    let runtime_version = resolve_runtime_version(&executable)?;
    let home = dsh_run_home(&runtime_version, &claim.run.id)?;
    let permission_mode = match effective_access_mode(claim)? {
        crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED => "workspace-write",
        crate::app::remote_execution::ACCESS_MODE_FULL_ACCESS => "danger-full-access",
        _ => unreachable!(),
    };
    Ok(Invocation {
        executable,
        args: vec![
            OsString::from("--profile"),
            OsString::from("headless"),
            OsString::from(prompt),
        ],
        workspace,
        home,
        api_key: claim.claim_token.clone(),
        base_url: format!(
            "{}/api/agent/runs/{}/ai/v1",
            options.api_base.trim_end_matches('/'),
            claim.run.id
        ),
        model: claim.ai_model.trim().to_string(),
        models: vec![claim.ai_model.trim().to_string()],
        permission_mode,
        run_id: claim.run.id.clone(),
    })
}

fn effective_access_mode(claim: &AgentRunClaim) -> Result<&str, Box<dyn Error>> {
    let value = if claim.run.access_mode.trim().is_empty() {
        claim.access_mode.trim()
    } else {
        claim.run.access_mode.trim()
    };
    match value {
        crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED
        | crate::app::remote_execution::ACCESS_MODE_FULL_ACCESS => Ok(value),
        _ => Err("Dashboard returned an unsupported Agent Run access mode".into()),
    }
}

fn spawn(invocation: &Invocation) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(&invocation.executable);
    process::remove_himind_secret_environment(&mut command);
    command
        .args(&invocation.args)
        .current_dir(&invocation.workspace)
        .env(DSH_HOME_ENV, &invocation.home)
        .env("DSH_TELEMETRY_MODE", "DISABLED")
        .env("DEEPSEEK_API_KEY", &invocation.api_key)
        .env("DEEPSEEK_BASE_URL", &invocation.base_url)
        .env("DSH_PERMISSION_MODE", invocation.permission_mode)
        .env("HIMIND_AGENT_RUN_ID", &invocation.run_id)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_remove("OPENAI_API_KEY");
    command.env_remove("ANTHROPIC_API_KEY");
    command.env_remove("GOOGLE_API_KEY");
    command.env_remove("GEMINI_API_KEY");
    process::configure_hidden_process(&mut command);
    Ok(command.spawn()?)
}

fn ensure_home_config(invocation: &Invocation, options: &Options) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&invocation.home)?;
    ensure_himind_skill_adapter(&invocation.home)?;
    migrate_legacy_managed_settings(&invocation.home)?;
    ensure_himind_profile(&invocation.home, options, invocation)?;
    Ok(())
}

fn ensure_himind_profile(
    home: &Path,
    options: &Options,
    invocation: &Invocation,
) -> Result<(), Box<dyn Error>> {
    let profile = home.join("profiles").join(HIMIND_PROFILE);
    fs::create_dir_all(&profile)?;
    let files = [
        (
            "package.json",
            include_str!("../../runtime-profiles/himind/package.json"),
        ),
        (
            "pnpm-workspace.yaml",
            include_str!("../../runtime-profiles/himind/pnpm-workspace.yaml"),
        ),
        (
            "cordis.yml",
            include_str!("../../runtime-profiles/himind/cordis.yml"),
        ),
    ];
    for (name, content) in files {
        fs::write(profile.join(name), content)?;
    }
    fs::write(
        profile.join("cordis.patch.yml"),
        render_himind_profile_patch(
            home,
            options,
            &invocation.model,
            &invocation.base_url,
            &invocation.models,
        )?,
    )?;
    Ok(())
}

fn render_himind_profile_patch(
    home: &Path,
    options: &Options,
    default_model: &str,
    base_url: &str,
    models: &[String],
) -> Result<String, Box<dyn Error>> {
    let executable = env::current_exe()?;
    let args = himind_mcp_arguments(options);
    let mut patch = include_str!("../../runtime-profiles/himind/cordis.patch.yml")
        .trim_end()
        .to_string();
    append_managed_model_profile(&mut patch, default_model, base_url, models)?;
    patch.push_str("\n\n# Agent-owned context. This layer is regenerated for each new HiMind AI session.\n- insert:\n");
    patch.push_str("    - id: himind-mcp\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        transport: stdio\n");
    patch.push_str(&format!(
        "        serverName: {}\n        command: {}\n        args:\n",
        yaml_scalar(HIMIND_MCP_SERVER_NAME),
        yaml_scalar(&executable.to_string_lossy()),
    ));
    for argument in args {
        patch.push_str(&format!("          - {}\n", yaml_scalar(&argument)));
    }
    patch.push_str(&format!(
        "        env:\n          HIMIND_AI_CLIENT_ID: {}\n        failOnStartupError: false\n        reconnect:\n          enabled: true\n          initialDelayMs: 500\n          maxDelayMs: 30000\n          maxAttempts: 5\n",
        yaml_scalar(HIMIND_MCP_CLIENT_ID),
    ));
    for server in crate::app::mcp_settings::load(&options.state_path)? {
        append_personal_mcp_row(&mut patch, &server);
    }
    patch.push_str(&format!(
        "\n    - id: himind-skill-filesystem\n      name: '@deepseek-ai/dsh-skill-filesystem'\n      config:\n        providerName: himind-managed\n        includeDefaultRoots: false\n        customSkillDirs:\n          - {}\n        watch: false\n",
        yaml_scalar(&home.join(HIMIND_SKILL_ADAPTER_DIR).to_string_lossy()),
    ));
    Ok(patch)
}

fn append_managed_model_profile(
    patch: &mut String,
    default_model: &str,
    base_url: &str,
    models: &[String],
) -> Result<(), Box<dyn Error>> {
    let default_model = default_model.trim();
    let base_url = base_url.trim();
    if default_model.is_empty() || base_url.is_empty() {
        return Err("HiMind AI 模型配置不完整".into());
    }
    let mut catalog = vec![default_model.to_string()];
    catalog.extend(
        models
            .iter()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .map(str::to_string),
    );
    let mut seen = HashSet::new();
    catalog.retain(|model| seen.insert(model.clone()));

    patch.push_str(
        "\n\n# HiMind provides the initial route. DSH settings remain the user-owned override.\n",
    );
    patch.push_str(&format!(
        "- id: agent-default-model\n  config:\n    provider: himind-proxy\n    model: {}\n",
        yaml_scalar(default_model),
    ));
    patch.push_str(
        "- id: llm-pi-ai\n  config:\n    providers:\n      himind-proxy:\n        displayName: HiMind AI\n        apiKeyEnv: DEEPSEEK_API_KEY\n        api: openai-completions\n",
    );
    patch.push_str(&format!("        baseURL: {}\n", yaml_scalar(base_url)));
    patch.push_str("        models:\n");
    for model in catalog {
        patch.push_str(&format!("          - id: {}\n", yaml_scalar(&model)));
    }
    Ok(())
}

fn migrate_legacy_managed_settings(home: &Path) -> Result<(), Box<dyn Error>> {
    let marker = home.join(DSH_SETTINGS_MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    let path = home.join("settings.yaml");
    if path.is_file() {
        let source = fs::read_to_string(&path)?;
        if let Ok(mut document) = serde_yaml::from_str::<YamlValue>(&source) {
            if remove_legacy_managed_sections(&mut document) {
                let empty = document
                    .as_mapping()
                    .is_some_and(|mapping| mapping.is_empty());
                if empty {
                    fs::remove_file(&path)?;
                } else {
                    fs::write(&path, serde_yaml::to_string(&document)?)?;
                }
            }
        }
    }
    fs::write(marker, b"2\n")?;
    Ok(())
}

fn remove_legacy_managed_sections(document: &mut YamlValue) -> bool {
    let Some(root) = document.as_mapping_mut() else {
        return false;
    };
    let default_key = YamlValue::String("agent-default-model".to_string());
    let llm_key = YamlValue::String("llm-pi-ai".to_string());
    let Some(default_model) = root
        .get(&default_key)
        .and_then(YamlValue::as_mapping)
        .filter(|mapping| mapping.len() == 2)
        .and_then(|mapping| {
            let provider = mapping
                .get(YamlValue::String("provider".to_string()))?
                .as_str()?;
            let model = mapping
                .get(YamlValue::String("model".to_string()))?
                .as_str()?;
            (provider == "himind-proxy" && !model.trim().is_empty()).then_some(model.to_string())
        })
    else {
        return false;
    };
    let legacy_provider = root
        .get(&llm_key)
        .and_then(YamlValue::as_mapping)
        .filter(|mapping| mapping.len() == 1)
        .and_then(|mapping| mapping.get(YamlValue::String("providers".to_string())))
        .and_then(YamlValue::as_mapping)
        .filter(|mapping| mapping.len() == 1)
        .and_then(|mapping| mapping.get(YamlValue::String("himind-proxy".to_string())))
        .and_then(YamlValue::as_mapping)
        .is_some_and(|provider| legacy_provider_matches(provider, &default_model));
    if !legacy_provider {
        return false;
    }
    root.remove(&default_key);
    root.remove(&llm_key);
    true
}

fn legacy_provider_matches(provider: &serde_yaml::Mapping, default_model: &str) -> bool {
    if provider.len() != 5 {
        return false;
    }
    let string_field = |key: &str| {
        provider
            .get(YamlValue::String(key.to_string()))
            .and_then(YamlValue::as_str)
    };
    if string_field("displayName") != Some("HiMind AI")
        || string_field("apiKeyEnv") != Some("DEEPSEEK_API_KEY")
        || string_field("api") != Some("openai-completions")
        || string_field("baseURL").is_none_or(|value| value.trim().is_empty())
    {
        return false;
    }
    provider
        .get(YamlValue::String("models".to_string()))
        .and_then(YamlValue::as_sequence)
        .filter(|models| models.len() == 1)
        .and_then(|models| models.first())
        .and_then(YamlValue::as_mapping)
        .filter(|model| model.len() == 1)
        .and_then(|model| model.get(YamlValue::String("id".to_string())))
        .and_then(YamlValue::as_str)
        == Some(default_model)
}

fn append_personal_mcp_row(patch: &mut String, server: &crate::app::mcp_settings::McpServerConfig) {
    patch.push_str(&format!(
        "\n    - id: personal-mcp-{}\n      name: '@deepseek-ai/dsh-mcp-client'\n",
        server.server_name
    ));
    if !server.enabled {
        patch.push_str("      disabled: true\n");
    }
    patch.push_str(&format!(
        "      config:\n        transport: {}\n        serverName: {}\n",
        yaml_scalar(&server.transport),
        yaml_scalar(&server.server_name),
    ));
    if server.transport == "stdio" {
        patch.push_str(&format!(
            "        command: {}\n",
            yaml_scalar(&server.command)
        ));
        if !server.args.is_empty() {
            patch.push_str("        args:\n");
            for argument in &server.args {
                patch.push_str(&format!("          - {}\n", yaml_scalar(argument)));
            }
        }
        if !server.env.is_empty() {
            patch.push_str("        env:\n");
            for (key, value) in &server.env {
                patch.push_str(&format!(
                    "          {}: {}\n",
                    yaml_scalar(key),
                    yaml_scalar(value)
                ));
            }
        }
        if !server.cwd.is_empty() {
            patch.push_str(&format!("        cwd: {}\n", yaml_scalar(&server.cwd)));
        }
    } else {
        patch.push_str(&format!("        url: {}\n", yaml_scalar(&server.url)));
        if !server.headers.is_empty() {
            patch.push_str("        headers:\n");
            for (key, value) in &server.headers {
                patch.push_str(&format!(
                    "          {}: {}\n",
                    yaml_scalar(key),
                    yaml_scalar(value)
                ));
            }
        }
    }
    patch.push_str(&format!(
        "        toolCallTimeoutMs: {}\n        failOnStartupError: {}\n        reconnect:\n          enabled: {}\n",
        server.tool_call_timeout_ms,
        server.fail_on_startup_error,
        server.reconnect,
    ));
}

fn himind_mcp_arguments(options: &Options) -> Vec<String> {
    vec![
        "--mcp".to_string(),
        "--api".to_string(),
        options.api_base.clone(),
        "--state".to_string(),
        options.state_path.to_string_lossy().to_string(),
    ]
}

fn ensure_himind_skill_adapter(home: &Path) -> Result<(), Box<dyn Error>> {
    let records = himind_skill_records()?;
    let target = home.join(HIMIND_SKILL_ADAPTER_DIR);
    let staging = home.join(format!(
        ".{HIMIND_SKILL_ADAPTER_DIR}-staging-{}-{}",
        std::process::id(),
        unix_time_millis()
    ));
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        for record in &records {
            let name = dsh_skill_name(&record.manifest.id);
            let destination = staging.join(&name);
            copy_skill_package(record, &destination)?;
            let source = fs::read_to_string(record.version_root.join("SKILL.md"))?;
            fs::write(
                destination.join("SKILL.md"),
                render_dsh_skill(record, &name, &source),
            )?;
        }
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&staging, &target)?;
        Ok(())
    })();
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn himind_skill_records() -> Result<Vec<SkillRecord>, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    store.list_records()
}

fn copy_skill_package(record: &SkillRecord, destination: &Path) -> Result<(), Box<dyn Error>> {
    for entry in walkdir::WalkDir::new(&record.version_root).follow_links(false) {
        let entry = entry?;
        if entry.path() == record.version_root {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(format!(
                "HiMind Skill 包不能包含符号链接: {}",
                entry.path().display()
            )
            .into());
        }
        let relative = entry.path().strip_prefix(&record.version_root)?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn render_dsh_skill(record: &SkillRecord, name: &str, source: &str) -> String {
    let description = if record.manifest.description.trim().is_empty() {
        record.manifest.name.trim()
    } else {
        record.manifest.description.trim()
    };
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        yaml_scalar(name),
        yaml_scalar(description),
        strip_yaml_frontmatter(source).trim(),
    )
}

fn strip_yaml_frontmatter(source: &str) -> &str {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(first_line_end) = source.find('\n') else {
        return source;
    };
    if source[..first_line_end].trim_end_matches('\r') != "---" {
        return source;
    }
    let remainder = &source[first_line_end + 1..];
    let mut offset = first_line_end + 1;
    for line in remainder.split_inclusive('\n') {
        if line.trim() == "---" {
            return &source[offset + line.len()..];
        }
        offset += line.len();
    }
    source
}

fn dsh_skill_name(skill_id: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for byte in skill_id.bytes() {
        if byte.is_ascii_alphanumeric() {
            slug.push((byte as char).to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let mut digest = Sha256::new();
    digest.update(skill_id.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    format!(
        "himind-{}-{}",
        if slug.is_empty() { "skill" } else { slug },
        &digest[..10]
    )
}

fn yaml_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "".to_string())
}

fn resolve_executable() -> Result<OsString, Box<dyn Error>> {
    env::var_os("HIMIND_DSH_EXECUTABLE")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            load_runtime_state()
                .ok()
                .flatten()
                .and_then(|state| {
                    let path = PathBuf::from(state.executable_path);
                    managed_executable_path(&path).then(|| path.into_os_string())
                })
        })
        .ok_or_else(|| "DeepSeek Harness Runtime is not installed; set HIMIND_DSH_EXECUTABLE or install the signed Dashboard Runtime".into())
}

fn resolve_runtime_version(executable: &OsString) -> Result<String, Box<dyn Error>> {
    let executable_path = PathBuf::from(executable);
    if let Some(state) = load_runtime_state()? {
        let state_path = PathBuf::from(&state.executable_path);
        if state_path.canonicalize().ok() == executable_path.canonicalize().ok() {
            return safe_segment(&state.version).map_err(Into::into);
        }
    }
    let output = process::verify_command(executable, &["--version"])?;
    parse_runtime_version(&output)
        .ok_or_else(|| "DeepSeek Harness CLI returned an invalid semantic version".into())
}

fn parse_runtime_version(value: &str) -> Option<String> {
    value.split_whitespace().find_map(|candidate| {
        let candidate = candidate.trim_start_matches('v');
        semver::Version::parse(candidate)
            .ok()
            .map(|version| version.to_string())
    })
}

fn runtime_root() -> PathBuf {
    crate::store::paths::agent_home()
        .join("runtimes")
        .join("deepseek-harness")
}

fn runtime_state_path() -> PathBuf {
    runtime_root().join("state.json")
}

fn load_runtime_state() -> Result<Option<InstalledRuntimeState>, Box<dyn Error>> {
    let path = runtime_state_path();
    if !path.is_file() {
        return Ok(None);
    }
    let state = serde_json::from_slice::<InstalledRuntimeState>(&fs::read(path)?)?;
    if state.schema_version != 2
        || state.product_id != RUNTIME_PRODUCT_ID
        || state.provider != RUNTIME_CONTRACT
        || state.executable_path.trim().is_empty()
    {
        return Err("DeepSeek Harness Runtime state is invalid".into());
    }
    Ok(Some(state))
}

fn managed_executable_path(path: &Path) -> bool {
    let Ok(executable) = path.canonicalize() else {
        return false;
    };
    let Ok(versions) = runtime_root().join("versions").canonicalize() else {
        return false;
    };
    executable.is_file() && executable.starts_with(versions)
}

fn dsh_home(version: &str) -> Result<PathBuf, String> {
    let root = env::var_os(DSH_HOME_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root().join("homes"));
    versioned_home(&root, version)
}

fn dsh_run_home(version: &str, run_id: &str) -> Result<PathBuf, String> {
    let root = env::var_os(DSH_HOME_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root().join("homes"));
    Ok(root
        .join("runs")
        .join(safe_segment(version)?)
        .join(safe_segment(run_id)?))
}

fn versioned_home(root: &Path, version: &str) -> Result<PathBuf, String> {
    Ok(root.join(safe_segment(version)?))
}

fn first_line(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        dsh_run_home, dsh_skill_name, first_line, managed_model_catalog,
        migrate_legacy_managed_settings, parse_runtime_version, remove_managed_runtime,
        render_himind_profile_patch, safe_relative_path, safe_segment, strip_yaml_frontmatter,
        versioned_home, InteractiveEventProjector,
    };
    use crate::api::ai::AIUserCredential;
    use crate::app::mcp_settings::McpServerConfig;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn extracts_first_non_empty_version_line() {
        assert_eq!(first_line("\n dsh 0.1.0-rc.6\n"), "dsh 0.1.0-rc.6");
        assert_eq!(
            parse_runtime_version("dsh 0.1.0-rc.6\n"),
            Some("0.1.0-rc.6".to_string())
        );
    }

    #[test]
    fn runtime_home_is_isolated_by_semantic_version() {
        assert_eq!(
            versioned_home(std::path::Path::new("runtime-homes"), "0.1.0-rc.6").unwrap(),
            std::path::PathBuf::from("runtime-homes").join("0.1.0-rc.6")
        );
        assert!(versioned_home(std::path::Path::new("runtime-homes"), "../shared").is_err());
    }

    #[test]
    fn managed_run_home_is_isolated_from_interactive_settings() {
        let home = dsh_run_home("0.1.0-rc.6", "run-123").unwrap();
        assert!(home.ends_with("runs/0.1.0-rc.6/run-123"));
        assert!(dsh_run_home("0.1.0-rc.6", "../interactive").is_err());
    }

    #[test]
    fn uninstall_removes_managed_files_and_preserves_user_home() {
        let root = std::env::temp_dir().join(format!(
            "himind-runtime-uninstall-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("versions/1.0.0")).unwrap();
        std::fs::create_dir_all(root.join("downloads")).unwrap();
        std::fs::create_dir_all(root.join("homes/1.0.0")).unwrap();
        std::fs::write(root.join("versions/1.0.0/runtime.exe"), b"runtime").unwrap();
        std::fs::write(root.join("downloads/runtime.zip"), b"archive").unwrap();
        std::fs::write(root.join("state.json"), b"{}").unwrap();
        std::fs::write(root.join("homes/1.0.0/preferences.json"), b"{}").unwrap();

        remove_managed_runtime(&root).unwrap();

        assert!(!root.join("versions").exists());
        assert!(!root.join("downloads").exists());
        assert!(!root.join("state.json").exists());
        assert!(root.join("homes/1.0.0/preferences.json").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn yaml_scalar_quotes_model_and_url() {
        assert_eq!(super::yaml_scalar("model:1"), "\"model:1\"");
    }

    #[test]
    fn managed_profile_adds_agent_mcp_and_himind_skills_without_default_roots() {
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = std::path::PathBuf::from("C:/HiMind/state.json");
        let patch = render_himind_profile_patch(
            std::path::Path::new("C:/HiMind/runtime-home"),
            &options,
            "model-default",
            "https://gateway.example/v1",
            &["model-default".to_string(), "model-fast".to_string()],
        )
        .unwrap();

        assert!(patch.contains("id: agent-default-model"));
        assert!(patch.contains("id: llm-pi-ai"));
        assert!(patch.contains("id: \"model-default\""));
        assert!(patch.contains("id: \"model-fast\""));
        assert!(patch.contains("id: himind-mcp"));
        assert!(patch.contains("name: '@deepseek-ai/dsh-mcp-client'"));
        assert!(patch.contains("HIMIND_AI_CLIENT_ID: \"himind-ai\""));
        assert!(patch.contains("id: himind-skill-filesystem"));
        assert!(patch.contains("providerName: himind-managed"));
        assert!(patch.contains("includeDefaultRoots: false"));
        assert!(
            patch.contains("C:/HiMind/runtime-home\\\\himind-skills")
                || patch.contains("C:/HiMind/runtime-home/himind-skills")
        );
    }

    #[test]
    fn managed_profile_adds_personal_mcp_connections() {
        let root = std::env::temp_dir().join(format!(
            "himind-profile-mcp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = root.join("agent-state.json");
        let server = McpServerConfig {
            server_name: "project-tools".to_string(),
            display_name: "Project tools".to_string(),
            transport: "stdio".to_string(),
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            env: BTreeMap::from([("API_KEY".to_string(), "local-secret".to_string())]),
            cwd: "C:/Projects/demo".to_string(),
            url: String::new(),
            headers: BTreeMap::new(),
            tool_call_timeout_ms: 45_000,
            fail_on_startup_error: false,
            reconnect: true,
            enabled: false,
        };
        crate::app::mcp_settings::upsert(&options.state_path, server).unwrap();

        let patch = render_himind_profile_patch(
            &root.join("runtime-home"),
            &options,
            "model-default",
            "https://gateway.example/v1",
            &["model-default".to_string()],
        )
        .unwrap();
        assert!(patch.contains("id: himind-mcp"));
        assert!(patch.contains("id: personal-mcp-project-tools"));
        assert!(patch.contains("serverName: \"project-tools\""));
        assert!(patch.contains("command: \"node\""));
        assert!(patch.contains("\"API_KEY\": \"local-secret\""));
        assert!(patch.contains("toolCallTimeoutMs: 45000"));
        assert!(patch.contains("disabled: true"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn himind_skill_adapter_uses_valid_stable_names_and_one_frontmatter_block() {
        let name = dsh_skill_name("com.himind.skill.example_tool");
        assert!(name.starts_with("himind-com-himind-skill-example-tool-"));
        assert!(name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'));
        assert_eq!(
            strip_yaml_frontmatter(
                "---\r\nname: original\r\ndescription: old\r\n---\r\n\r\n# Body"
            )
            .trim(),
            "# Body"
        );
        assert_eq!(name, dsh_skill_name("com.himind.skill.example_tool"));
    }

    #[test]
    fn interactive_catalog_starts_with_the_default_and_removes_duplicates() {
        let credential = AIUserCredential {
            active_entitlement_id: "entitlement".to_string(),
            active_personal_connection_id: String::new(),
            status: "active".to_string(),
            base_url: "https://example.test".to_string(),
            model: "fast".to_string(),
            models: vec!["fast".to_string(), "deep".to_string()],
        };
        assert_eq!(
            managed_model_catalog(&credential).unwrap(),
            vec!["fast", "deep"]
        );
    }

    #[test]
    fn profile_route_uses_only_the_claim_scoped_himind_proxy() {
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = std::path::PathBuf::from("C:/HiMind/state.json");
        let profile = render_himind_profile_patch(
            std::path::Path::new("C:/HiMind/runtime-home"),
            &options,
            "deepseek-model",
            "https://dashboard.example/api/agent/runs/run-1/ai/v1",
            &["deepseek-model".to_string(), "fast-model".to_string()],
        )
        .unwrap();
        assert!(profile.contains("provider: himind-proxy"));
        assert!(profile.contains("apiKeyEnv: DEEPSEEK_API_KEY"));
        assert!(profile.contains("api: openai-completions"));
        assert!(profile.contains("model: \"deepseek-model\""));
        assert!(
            profile.contains("baseURL: \"https://dashboard.example/api/agent/runs/run-1/ai/v1\"")
        );
        assert!(profile.contains("id: \"fast-model\""));
        assert!(!profile.contains("apiKey:"));
    }

    #[test]
    fn legacy_generated_settings_are_removed_without_touching_other_preferences() {
        let root = std::env::temp_dir().join(format!(
            "himind-settings-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.yaml"),
            "theme:\n  mode: dark\nagent-default-model:\n  provider: himind-proxy\n  model: glm-5.2\nllm-pi-ai:\n  providers:\n    himind-proxy:\n      displayName: HiMind AI\n      apiKeyEnv: DEEPSEEK_API_KEY\n      api: openai-completions\n      baseURL: https://gateway.example/v1\n      models:\n        - id: glm-5.2\n",
        )
        .unwrap();

        migrate_legacy_managed_settings(&root).unwrap();

        let migrated = std::fs::read_to_string(root.join("settings.yaml")).unwrap();
        assert!(migrated.contains("theme:"));
        assert!(!migrated.contains("agent-default-model"));
        assert!(!migrated.contains("llm-pi-ai"));
        assert!(root.join(super::DSH_SETTINGS_MIGRATION_MARKER).is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn customized_dsh_model_settings_are_preserved() {
        let root = std::env::temp_dir().join(format!(
            "himind-settings-custom-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let settings = "agent-default-model:\n  provider: custom\n  model: my-model\nllm-pi-ai:\n  providers:\n    custom:\n      displayName: My Provider\n      apiKeyEnv: CUSTOM_API_KEY\n      api: openai-completions\n      baseURL: https://example.test/v1\n      models:\n        - id: my-model\n";
        std::fs::write(root.join("settings.yaml"), settings).unwrap();

        migrate_legacy_managed_settings(&root).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("settings.yaml")).unwrap(),
            settings
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_web_profile_hides_runtime_configuration_without_removing_settings_service() {
        let profile = include_str!("../../runtime-profiles/web/cordis.patch.yml");
        for id in [
            "ui-settings-general",
            "ui-settings-models",
            "ui-settings-plugin-inventory",
            "ui-settings-plugins",
            "ui-agent-preset",
            "ui-model-selection",
        ] {
            assert!(profile.contains(&format!("- id: {id}\n  disabled: true")));
        }
        assert!(!profile.contains("- id: ui-settings\n  disabled: true"));
    }

    #[test]
    fn himind_profile_keeps_personal_plugin_settings_open() {
        let profile = include_str!("../../runtime-profiles/himind/cordis.patch.yml");
        assert!(profile.contains("- id: ui-agent-preset\n  disabled: true"));
        for id in [
            "ui-settings",
            "ui-settings-general",
            "ui-settings-models",
            "ui-settings-plugin-inventory",
            "ui-settings-plugins",
            "ui-model-selection",
        ] {
            assert!(!profile.contains(&format!("- id: {id}\n  disabled: true")));
        }
    }

    #[test]
    fn runtime_manifest_paths_are_relative_and_versions_are_path_safe() {
        assert_eq!(
            safe_relative_path("bin/dsh.cmd").unwrap(),
            std::path::PathBuf::from("bin/dsh.cmd")
        );
        assert!(safe_relative_path("../dsh.cmd").is_err());
        assert!(safe_relative_path("C:\\dsh.cmd").is_err());
        assert_eq!(safe_segment("0.1.0-rc.6").unwrap(), "0.1.0-rc.6");
        assert!(safe_segment("0.1.0/bad").is_err());
    }

    #[test]
    fn interactive_events_are_projected_without_engine_payloads() {
        let mut projector = InteractiveEventProjector::default();
        let started = projector
            .project(&json!({
                "type":"server-request",
                "rpcId":"rpc-start",
                "payload":{
                    "type":"session/event",
                    "sessionId":"session-1",
                    "event":{"type":"turn/start","seq":3,"time":1000,"data":{"turn":2}}
                }
            }))
            .unwrap();
        assert_eq!(started.event_type, "turn.started");
        assert_eq!(started.turn_id, "turn-2");

        let user = projector
            .project(&json!({
                "payload":{
                    "type":"session/event",
                    "sessionId":"session-1",
                    "event":{"type":"user/message","seq":4,"time":1001,"data":{
                        "id":"message-1","role":"user","source":{"kind":"user"},
                        "content":[{"type":"text","text":"请检查项目"},{"type":"image","attachment":{"id":"private"}}]
                    }}
                }
            }))
            .unwrap();
        assert_eq!(user.event_type, "message.user");
        assert_eq!(user.content, "请检查项目");
        assert!(!serde_json::to_string(&user).unwrap().contains("private"));

        let assistant = projector
            .project(&json!({
                "payload":{
                    "type":"session/event",
                    "sessionId":"session-1",
                    "event":{"type":"assistant/message","seq":5,"time":1002,"data":{
                        "turn":2,"step":1,"message":{"content":[
                            {"type":"reasoning","text":"internal reasoning"},
                            {"type":"text","text":"已经完成"},
                            {"type":"tool-call","name":"secret","arguments":"{\"token\":\"private\"}"}
                        ]}
                    }}
                }
            }))
            .unwrap();
        assert_eq!(assistant.content, "已经完成");
        let serialized = serde_json::to_string(&assistant).unwrap();
        assert!(!serialized.contains("reasoning"));
        assert!(!serialized.contains("private"));
    }

    #[test]
    fn interactive_tool_and_approval_events_only_expose_safe_labels() {
        let mut projector = InteractiveEventProjector::default();
        let _ = projector.project(&json!({"payload":{"type":"session/event","sessionId":"s","event":{"type":"turn/start","seq":0,"time":1,"data":{"turn":1}}}}));
        let tool = projector
            .project(&json!({"payload":{"type":"session/event","sessionId":"s","event":{
                "type":"tool/call","seq":1,"time":2,"data":{"turn":1,"step":1,"callId":"c1","name":"Read","arguments":"{\"path\":\"C:/secret\"}"}
            }}}))
            .unwrap();
        assert_eq!(tool.event_type, "tool.started");
        assert_eq!(tool.label, "Read");
        assert!(!serde_json::to_string(&tool).unwrap().contains("C:/secret"));

        let approval = projector
            .project(&json!({"type":"server-request","rpcId":"rpc-1","payload":{
                "type":"approval/requested","sessionId":"s","approvalId":"approval-1",
                "toolName":"Write","reason":"contains private arguments"
            }}))
            .unwrap();
        assert_eq!(approval.event_type, "approval.requested");
        assert_eq!(approval.label, "Write");
        assert!(!serde_json::to_string(&approval)
            .unwrap()
            .contains("private arguments"));
    }
}
