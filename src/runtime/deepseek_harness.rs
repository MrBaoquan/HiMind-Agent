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
const HIMIND_HEADLESS_PROFILE: &str = "himind-headless";
// Reserved for the Agent-owned bridge. Keep this namespace distinct from
// user-managed DSH MCP servers and make the ownership visible in diagnostics.
const HIMIND_MCP_SERVER_NAME: &str = "himind-agent";
const HIMIND_MCP_ROW_ID: &str = "himind-agent-mcp";
const HIMIND_SKILL_ROW_ID: &str = "himind-agent-skill-filesystem";
const HIMIND_MCP_CLIENT_ID: &str = "himind-ai";
const HIMIND_SKILL_ADAPTER_DIR: &str = "himind-skills";
const HIMIND_AGENT_OVERLAY_DIR: &str = ".himind";
const HIMIND_AGENT_OVERLAY_FILE: &str = "agent.patch.yml";
const HIMIND_AGENT_PATCH_MARKER: &str =
    "# Agent-owned context. This layer is regenerated for each new HiMind AI session.";
const DSH_SETTINGS_MIGRATION_MARKER: &str = ".himind-dsh-settings-v2";
const INTERACTIVE_HOME_DIRECTORY: &str = "interactive";
const INTERACTIVE_HOME_MIGRATION_MARKER: &str = ".himind-interactive-home-v1.json";
const INTERACTIVE_HOME_MIGRATION_MAX_FILES: u64 = 200_000;
const INTERACTIVE_HOME_MIGRATION_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct InstalledRuntimeState {
    schema_version: u32,
    product_id: String,
    provider: String,
    version: String,
    executable_path: String,
}

#[derive(Default)]
struct HomeMigrationBudget {
    files: u64,
    bytes: u64,
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
    pub workspace: PathBuf,
    pub user_id: String,
    pub api_key: String,
    pub api_key_env: Option<String>,
    pub base_url: String,
    pub agent_patch: PathBuf,
    pub default_model: String,
    pub models: Vec<String>,
    pub credential_fingerprint: String,
    pub catalog_fingerprint: String,
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
        let (event_type, identity, label, outcome, request_rpc_id, approval_id, question_payload) =
            match frame_type {
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
                    rpc_id.to_string(),
                    frame.get("approvalId")?.as_str()?.to_string(),
                    None,
                ),
                "approval/resolved" => (
                    "approval.resolved",
                    frame.get("approvalId")?.as_str()?,
                    String::new(),
                    bounded_text(frame.get("outcome")?.as_str()?, 80),
                    String::new(),
                    frame.get("approvalId")?.as_str()?.to_string(),
                    None,
                ),
                "question/requested" => (
                    "question.requested",
                    rpc_id,
                    "需要你的回复".to_string(),
                    String::new(),
                    rpc_id.to_string(),
                    String::new(),
                    sanitize_question_payload(frame),
                ),
                "question/resolved" => (
                    "question.resolved",
                    frame
                        .get("questionRpcId")
                        .and_then(Value::as_str)
                        .unwrap_or(rpc_id),
                    String::new(),
                    bounded_text(frame.get("outcome")?.as_str()?, 80),
                    frame
                        .get("questionRpcId")
                        .and_then(Value::as_str)
                        .unwrap_or(rpc_id)
                        .to_string(),
                    String::new(),
                    None,
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
            request_rpc_id,
            approval_id,
            question_payload,
            occurred_at_ms: unix_time_millis(),
        })
    }
}

fn sanitize_question_payload(frame: &Value) -> Option<Value> {
    let questions = frame.get("questions")?.as_array()?;
    if questions.is_empty() {
        return None;
    }
    let mut safe_questions = Vec::with_capacity(questions.len());
    for question in questions {
        let object = question.as_object()?;
        let id = object.get("id")?.as_str()?;
        let text = object.get("question")?.as_str()?;
        let mut safe = json!({"id": id, "question": text});
        for key in ["detail", "header"] {
            if let Some(value) = object.get(key).and_then(Value::as_str) {
                if !value.trim().is_empty() {
                    safe[key] = json!(bounded_text(value, 2000));
                }
            }
        }
        if let Some(multi_select) = object.get("multiSelect").and_then(Value::as_bool) {
            safe["multiSelect"] = json!(multi_select);
        }
        if let Some(options) = object.get("options").and_then(Value::as_array) {
            let mut safe_options = Vec::with_capacity(options.len());
            for option in options {
                let Some(option_object) = option.as_object() else {
                    continue;
                };
                let Some(label) = option_object.get("label").and_then(Value::as_str) else {
                    continue;
                };
                let mut safe_option = json!({"label": bounded_text(label, 500)});
                if let Some(description) = option_object.get("description").and_then(Value::as_str)
                {
                    if !description.trim().is_empty() {
                        safe_option["description"] = json!(bounded_text(description, 1000));
                    }
                }
                safe_options.push(safe_option);
            }
            safe["options"] = json!(safe_options);
        }
        safe_questions.push(safe);
    }
    Some(json!({"questions": safe_questions}))
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
        request_rpc_id: String::new(),
        approval_id: String::new(),
        question_payload: None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserModelSelection {
    provider: String,
    model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NativeDshProviderConfig {
    provider: String,
    model: String,
    api_key_env: Option<String>,
    base_url: String,
    models: Vec<String>,
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

pub(crate) fn prepare_interactive_launch(
    options: &Options,
    workspace: Option<&Path>,
) -> Result<InteractiveLaunch, String> {
    let executable = resolve_executable().map_err(|error| error.to_string())?;
    let version = resolve_runtime_version(&executable).map_err(|error| error.to_string())?;
    let workspace = interactive_workspace(workspace)?;
    if !options.mode().dashboard_enabled() {
        return prepare_independent_interactive_launch(
            options,
            executable.to_string_lossy().to_string(),
            version,
            workspace,
        );
    }
    let delegated =
        crate::api::oauth::platform_access_token(options, crate::api::oauth::AI_CONVERSATION_SCOPE)
            .map_err(|error| error.to_string())?;
    let credential =
        crate::api::ai::fetch_client_credential(options, &delegated.user_id, "himind-agent")
            .map_err(|error| error.to_string())?;
    let home = dsh_home(&version)?;
    let models = managed_model_catalog(&credential.access)?;
    let model = credential.access.model.trim().to_string();
    let sync_snapshot =
        crate::app::builtin_ai_model_sync::snapshot(&delegated.user_id, &credential);
    let invocation = Invocation {
        executable: executable.clone(),
        args: Vec::new(),
        workspace: workspace.clone(),
        home: home.clone(),
        api_key: credential.api_key.clone(),
        base_url: credential.access.base_url.clone(),
        model: model.clone(),
        models: models.clone(),
        permission_mode: INTERACTIVE_PERMISSION_MODE,
        run_id: "interactive".to_string(),
    };
    ensure_home_config(&invocation, options).map_err(|error| error.to_string())?;
    let agent_patch = agent_overlay_path(&home);
    Ok(InteractiveLaunch {
        executable: PathBuf::from(executable),
        home,
        workspace,
        user_id: delegated.user_id,
        api_key: credential.api_key,
        api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
        base_url: credential.access.base_url,
        agent_patch,
        default_model: model.clone(),
        models: models.clone(),
        credential_fingerprint: sync_snapshot.credential_fingerprint,
        catalog_fingerprint: sync_snapshot.catalog_fingerprint,
        permission_mode: INTERACTIVE_PERMISSION_MODE,
    })
}

fn prepare_independent_interactive_launch(
    options: &Options,
    executable: String,
    version: String,
    workspace: PathBuf,
) -> Result<InteractiveLaunch, String> {
    let home = native_dsh_home(&version)?;
    let provider_config = native_dsh_provider_config(&home);
    let api_key = match provider_config.api_key_env.as_deref() {
        Some(api_key_env) => env::var(api_key_env).unwrap_or_default(),
        None => String::new(),
    };
    let invocation = Invocation {
        executable: executable.clone().into(),
        args: Vec::new(),
        workspace: workspace.clone(),
        home: home.clone(),
        api_key: api_key.clone(),
        base_url: provider_config.base_url.clone(),
        model: provider_config.model.clone(),
        models: provider_config.models.clone(),
        permission_mode: INTERACTIVE_PERMISSION_MODE,
        run_id: "interactive".to_string(),
    };
    ensure_home_config(&invocation, options).map_err(|error| error.to_string())?;
    let agent_patch = agent_overlay_path(&home);
    Ok(InteractiveLaunch {
        executable: PathBuf::from(executable),
        home,
        workspace,
        user_id: "local".to_string(),
        api_key,
        api_key_env: provider_config.api_key_env,
        base_url: provider_config.base_url,
        agent_patch,
        default_model: provider_config.model,
        models: provider_config.models,
        credential_fingerprint: String::new(),
        catalog_fingerprint: String::new(),
        permission_mode: INTERACTIVE_PERMISSION_MODE,
    })
}

fn native_dsh_provider_config(home: &Path) -> NativeDshProviderConfig {
    let settings_path = home.join("settings.yaml");
    fs::read_to_string(settings_path)
        .ok()
        .and_then(|source| parse_native_dsh_provider_config(&source).ok())
        .unwrap_or_default()
}

fn interactive_workspace(workspace: Option<&Path>) -> Result<PathBuf, String> {
    let workspace = match workspace {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("HiMind AI 工作目录不可用: {error}"))?;
    if !workspace.is_dir() {
        return Err("HiMind AI 工作目录必须是本机目录".to_string());
    }
    Ok(workspace)
}

fn parse_native_dsh_provider_config(source: &str) -> Result<NativeDshProviderConfig, String> {
    let document = serde_yaml::from_str::<YamlValue>(source)
        .map_err(|error| format!("读取 DSH 原生 Provider 配置失败: {error}"))?;
    let root = document
        .as_mapping()
        .ok_or("DSH 原生 settings.yaml 必须是对象")?;
    let selection = root
        .get(YamlValue::String("agent-default-model".to_string()))
        .and_then(YamlValue::as_mapping);
    let provider = selection
        .and_then(|value| value.get(YamlValue::String("provider".to_string())))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("Independent Mode 需要在 DSH settings.yaml 配置 agent-default-model.provider")?;
    let selected_model = selection
        .and_then(|value| value.get(YamlValue::String("model".to_string())))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("Independent Mode 需要在 DSH settings.yaml 配置 agent-default-model.model")?;
    let providers = root
        .get(YamlValue::String("llm-pi-ai".to_string()))
        .and_then(YamlValue::as_mapping)
        .and_then(|value| value.get(YamlValue::String("providers".to_string())))
        .and_then(YamlValue::as_mapping)
        .ok_or("DSH settings.yaml 缺少 llm-pi-ai.providers")?;
    let provider_config = providers
        .get(YamlValue::String(provider.to_string()))
        .and_then(YamlValue::as_mapping)
        .ok_or_else(|| format!("DSH settings.yaml 未找到 Provider: {provider}"))?;
    let api_key_env = provider_config
        .get(YamlValue::String("apiKeyEnv".to_string()))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let base_url = provider_config
        .get(YamlValue::String("baseURL".to_string()))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let mut models = vec![selected_model.to_string()];
    if let Some(values) = provider_config
        .get(YamlValue::String("models".to_string()))
        .and_then(YamlValue::as_sequence)
    {
        models.extend(values.iter().filter_map(|value| {
            value
                .as_mapping()
                .and_then(|item| item.get(YamlValue::String("id".to_string())))
                .and_then(YamlValue::as_str)
                .or_else(|| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }));
    }
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    Ok(NativeDshProviderConfig {
        provider: provider.to_string(),
        model: selected_model.to_string(),
        api_key_env,
        base_url: base_url.to_string(),
        models,
    })
}

pub(crate) fn interactive_tool_context_summary(
    options: &Options,
) -> Result<InteractiveToolContextSummary, String> {
    let personal_mcp = crate::app::mcp_registry::list(&options.state_path)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|server| server.enabled)
        .count();
    Ok(InteractiveToolContextSummary {
        skills: himind_skill_records(options)
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
            OsString::from(HIMIND_HEADLESS_PROFILE),
            OsString::from("--patch"),
            agent_overlay_path(&home).into_os_string(),
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
    ensure_himind_skill_adapter(&invocation.home, options)?;
    migrate_legacy_managed_settings(&invocation.home)?;
    ensure_himind_profile(&invocation.home, options, invocation)?;
    ensure_himind_headless_profile(&invocation.home, options, invocation)?;
    ensure_agent_overlay(&invocation.home, options, invocation)?;
    Ok(())
}

fn ensure_himind_profile(
    home: &Path,
    options: &Options,
    invocation: &Invocation,
) -> Result<(), Box<dyn Error>> {
    ensure_profile_files(
        home,
        options,
        invocation,
        HIMIND_PROFILE,
        include_str!("../../runtime-profiles/himind/package.json"),
        include_str!("../../runtime-profiles/himind/pnpm-workspace.yaml"),
        include_str!("../../runtime-profiles/himind/cordis.yml"),
        include_str!("../../runtime-profiles/himind/cordis.patch.yml"),
    )
}

fn ensure_himind_headless_profile(
    home: &Path,
    options: &Options,
    invocation: &Invocation,
) -> Result<(), Box<dyn Error>> {
    ensure_profile_files(
        home,
        options,
        invocation,
        HIMIND_HEADLESS_PROFILE,
        include_str!("../../runtime-profiles/himind-headless/package.json"),
        include_str!("../../runtime-profiles/himind-headless/pnpm-workspace.yaml"),
        include_str!("../../runtime-profiles/himind-headless/cordis.yml"),
        include_str!("../../runtime-profiles/himind-headless/cordis.patch.yml"),
    )
}

fn ensure_profile_files(
    home: &Path,
    _options: &Options,
    _invocation: &Invocation,
    profile_name: &str,
    package_json: &str,
    pnpm_workspace: &str,
    cordis: &str,
    base_patch: &str,
) -> Result<(), Box<dyn Error>> {
    let profile = home.join("profiles").join(profile_name);
    fs::create_dir_all(&profile)?;
    merge_profile_package(&profile.join("package.json"), package_json)?;
    for (name, content) in [
        ("pnpm-workspace.yaml", pnpm_workspace),
        ("cordis.yml", cordis),
    ] {
        let path = profile.join(name);
        if !path.is_file() {
            fs::write(path, content)?;
        }
    }
    ensure_profile_patch(&profile.join("cordis.patch.yml"), base_patch)?;
    Ok(())
}

fn ensure_profile_patch(path: &Path, base_patch: &str) -> Result<(), Box<dyn Error>> {
    if !path.is_file() {
        fs::write(path, base_patch)?;
        return Ok(());
    }
    migrate_managed_profile_patch(path)
}

fn migrate_managed_profile_patch(path: &Path) -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    if !source.contains(HIMIND_AGENT_PATCH_MARKER) && !source.contains("serverName: \"himind\"") {
        return Ok(());
    }
    let Ok(document) = serde_yaml::from_str::<YamlValue>(&source) else {
        // Do not destroy a user-authored file that is currently invalid; DSH
        // will report its native parse error and the user can repair it.
        return Ok(());
    };
    let Some(rows) = document.as_sequence() else {
        return Ok(());
    };
    let filtered: Vec<YamlValue> = rows.iter().filter_map(strip_managed_patch_row).collect();
    if filtered.as_slice() == rows.as_slice() {
        return Ok(());
    }
    fs::write(path, serde_yaml::to_string(&filtered)?)?;
    Ok(())
}

fn strip_managed_patch_row(row: &YamlValue) -> Option<YamlValue> {
    let Some(mapping) = row.as_mapping() else {
        return Some(row.clone());
    };
    let id = mapping
        .get(YamlValue::String("id".to_string()))
        .and_then(YamlValue::as_str)
        .unwrap_or_default();
    if matches!(
        id,
        "agent-default-model" | "llm-pi-ai" | "himind-mcp" | "himind-skill-filesystem"
    ) || id == HIMIND_MCP_ROW_ID
        || id == HIMIND_SKILL_ROW_ID
        || id.starts_with("personal-mcp-")
        || id.starts_with("himind-agent-personal-mcp-")
    {
        return None;
    }
    let Some(inserted) = mapping
        .get(YamlValue::String("insert".to_string()))
        .and_then(YamlValue::as_sequence)
    else {
        return Some(row.clone());
    };
    let kept: Vec<YamlValue> = inserted
        .iter()
        .filter_map(strip_managed_patch_row)
        .collect();
    if kept.as_slice() == inserted.as_slice() {
        return Some(row.clone());
    }
    let mut updated = mapping.clone();
    updated.insert(
        YamlValue::String("insert".to_string()),
        YamlValue::Sequence(kept),
    );
    Some(YamlValue::Mapping(updated))
}

fn agent_overlay_path(home: &Path) -> PathBuf {
    home.join(HIMIND_AGENT_OVERLAY_DIR)
        .join(HIMIND_AGENT_OVERLAY_FILE)
}

fn ensure_agent_overlay(
    home: &Path,
    options: &Options,
    invocation: &Invocation,
) -> Result<(), Box<dyn Error>> {
    validate_agent_mcp_namespace(home)?;
    let path = agent_overlay_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let overlay = render_himind_agent_overlay(
        home,
        options,
        &invocation.model,
        &invocation.base_url,
        &invocation.models,
        &invocation.workspace,
    )?;
    fs::write(path, overlay)?;
    Ok(())
}

fn validate_agent_mcp_namespace(home: &Path) -> Result<(), Box<dyn Error>> {
    for path in [
        home.join("cordis.patch.yml"),
        home.join("profiles")
            .join(HIMIND_PROFILE)
            .join("cordis.patch.yml"),
        home.join("profiles")
            .join(HIMIND_HEADLESS_PROFILE)
            .join("cordis.patch.yml"),
    ] {
        if !path.is_file() {
            continue;
        }
        let Ok(document) = serde_yaml::from_str::<YamlValue>(&fs::read_to_string(&path)?) else {
            continue;
        };
        if yaml_contains_server_name(&document, HIMIND_MCP_SERVER_NAME) {
            return Err(format!(
                "DSH MCP serverName 已被用户配置占用: {HIMIND_MCP_SERVER_NAME} ({})，请改用其他名称",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn yaml_contains_server_name(value: &YamlValue, target: &str) -> bool {
    match value {
        YamlValue::Sequence(values) => values
            .iter()
            .any(|value| yaml_contains_server_name(value, target)),
        YamlValue::Mapping(values) => values.iter().any(|(key, value)| {
            key.as_str() == Some("serverName") && value.as_str().is_some_and(|name| name == target)
                || yaml_contains_server_name(key, target)
                || yaml_contains_server_name(value, target)
        }),
        _ => false,
    }
}

fn merge_profile_package(path: &Path, defaults: &str) -> Result<(), Box<dyn Error>> {
    let mut document = if path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(path)?)?
    } else {
        serde_json::from_str::<Value>(defaults)?
    };
    let defaults = serde_json::from_str::<Value>(defaults)?;
    let object = document
        .as_object_mut()
        .ok_or("DSH Profile package.json 必须是 JSON 对象")?;
    let default_object = defaults
        .as_object()
        .ok_or("DSH 内置 Profile package.json 无效")?;
    for key in ["name", "private"] {
        if let Some(value) = default_object.get(key) {
            object
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
    let default_dsh = default_object
        .get("dsh")
        .and_then(Value::as_object)
        .ok_or("DSH 内置 Profile 缺少 dsh 配置")?;
    let dsh = object
        .entry("dsh".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("DSH Profile dsh 配置无效")?;
    let default_profile = default_dsh
        .get("profile")
        .and_then(Value::as_object)
        .ok_or("DSH 内置 Profile 缺少 dsh.profile 配置")?;
    let profile = dsh
        .entry("profile".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("DSH Profile dsh.profile 配置无效")?;
    let mut bundles = default_profile
        .get("bundles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = profile.get("bundles").and_then(Value::as_array) {
        for bundle in existing {
            if !bundles.contains(bundle) {
                bundles.push(bundle.clone());
            }
        }
    }
    profile.insert("bundles".to_string(), Value::Array(bundles));
    fs::write(path, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

fn render_himind_profile_patch(
    home: &Path,
    options: &Options,
    default_model: &str,
    base_url: &str,
    models: &[String],
) -> Result<String, Box<dyn Error>> {
    render_himind_profile_patch_from_base(
        home,
        options,
        default_model,
        base_url,
        models,
        include_str!("../../runtime-profiles/himind/cordis.patch.yml"),
        None,
    )
}

fn render_himind_agent_overlay(
    home: &Path,
    options: &Options,
    default_model: &str,
    base_url: &str,
    models: &[String],
    workspace: &Path,
) -> Result<String, Box<dyn Error>> {
    render_himind_profile_patch_from_base(
        home,
        options,
        default_model,
        base_url,
        models,
        "",
        Some(workspace),
    )
}

fn render_himind_profile_patch_from_base(
    home: &Path,
    options: &Options,
    default_model: &str,
    base_url: &str,
    models: &[String],
    base_patch: &str,
    workspace: Option<&Path>,
) -> Result<String, Box<dyn Error>> {
    let executable = himind_mcp_executable()?;
    let args = himind_mcp_arguments(options);
    let mut patch = base_patch.trim_end().to_string();
    if options.mode().dashboard_enabled() {
        append_managed_model_profile(&mut patch, home, default_model, base_url, models)?;
    } else {
        patch.push_str(
            "\n\n# Independent Mode: provider and model selection remain owned by DSH settings.yaml.\n",
        );
    }
    patch.push_str("\n\n# Agent-owned context. This layer is regenerated for each new HiMind AI session.\n- insert:\n");
    patch.push_str(&format!(
        "    - id: {HIMIND_MCP_ROW_ID}\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        transport: stdio\n"
    ));
    patch.push_str(&format!(
        "        serverName: {}\n        command: {}\n        args:\n",
        yaml_scalar(HIMIND_MCP_SERVER_NAME),
        yaml_scalar(&executable.to_string_lossy()),
    ));
    for argument in args {
        patch.push_str(&format!("          - {}\n", yaml_scalar(&argument)));
    }
    patch.push_str(&format!(
        "        env:\n          HIMIND_AI_CLIENT_ID: {}\n          HIMIND_AGENT_PROFILE: {}\n",
        yaml_scalar(HIMIND_MCP_CLIENT_ID),
        yaml_scalar(&crate::store::paths::profile_name()),
    ));
    if let Some(workspace) = workspace {
        patch.push_str(&format!(
            "          HIMIND_AI_WORKSPACE: {}\n",
            yaml_scalar(&workspace.to_string_lossy()),
        ));
    }
    patch.push_str(
        "        failOnStartupError: false\n        reconnect:\n          enabled: true\n          initialDelayMs: 500\n          maxDelayMs: 30000\n          maxAttempts: 5\n",
    );
    patch.push_str(&format!(
        "\n    - id: {HIMIND_SKILL_ROW_ID}\n      name: '@deepseek-ai/dsh-skill-filesystem'\n      config:\n        providerName: himind-managed\n        includeDefaultRoots: true\n        customSkillDirs:\n          - {}\n        watch: false\n",
        yaml_scalar(&home.join(HIMIND_SKILL_ADAPTER_DIR).to_string_lossy()),
    ));
    Ok(patch)
}

fn append_managed_model_profile(
    patch: &mut String,
    home: &Path,
    default_model: &str,
    base_url: &str,
    models: &[String],
) -> Result<(), Box<dyn Error>> {
    let default_model = default_model.trim();
    let base_url = base_url.trim();
    if default_model.is_empty() || base_url.is_empty() {
        return Err("HiMind AI 模型配置不完整".into());
    }
    let user_selection = read_user_model_selection(home)?;
    let selected_provider = user_selection
        .as_ref()
        .map(|selection| selection.provider.as_str())
        .unwrap_or("himind-proxy");
    let selected_model = user_selection
        .as_ref()
        .map(|selection| selection.model.as_str())
        .unwrap_or(default_model);
    let mut catalog = vec![default_model.to_string()];
    if selected_provider == "himind-proxy" && selected_model != default_model {
        catalog.push(selected_model.to_string());
    }
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
        "- id: agent-default-model\n  config:\n    provider: {}\n    model: {}\n",
        yaml_scalar(selected_provider),
        yaml_scalar(selected_model),
    ));
    patch.push_str(
        "- id: llm-pi-ai\n  config:\n    providers:\n      himind-proxy:\n        displayName: HiMind AI\n        apiKeyEnv: DEEPSEEK_API_KEY\n        api: openai-completions\n",
    );
    patch.push_str(&format!("        baseURL: {}\n", yaml_scalar(base_url)));
    patch.push_str("        models:\n");
    for model in catalog {
        let model = yaml_scalar(&model);
        patch.push_str(&format!(
            "          - id: {model}\n            name: {model}\n"
        ));
    }
    Ok(())
}

fn read_user_model_selection(home: &Path) -> Result<Option<UserModelSelection>, Box<dyn Error>> {
    let path = home.join("settings.yaml");
    if !path.is_file() {
        return Ok(None);
    }
    let source = fs::read_to_string(&path)?;
    let document = serde_yaml::from_str::<YamlValue>(&source).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("读取 DSH 模型设置失败: {error}"),
        )
    })?;
    let namespace = document
        .as_mapping()
        .and_then(|root| root.get(YamlValue::String("agent-default-model".to_string())))
        .and_then(YamlValue::as_mapping);
    let Some(model) = namespace
        .and_then(|value| value.get(YamlValue::String("model".to_string())))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(None);
    };
    let provider = namespace
        .and_then(|value| value.get(YamlValue::String("provider".to_string())))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or("himind-proxy");
    Ok(Some(UserModelSelection {
        provider: provider.to_string(),
        model: model.to_string(),
    }))
}

fn migrate_legacy_managed_settings(home: &Path) -> Result<(), Box<dyn Error>> {
    let marker = home.join(DSH_SETTINGS_MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    // Settings are user-owned state. Runtime upgrades must not delete or
    // rewrite the selected model, provider overrides, or UI preferences.
    fs::write(marker, b"3\n")?;
    Ok(())
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

fn himind_mcp_executable() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let current = env::current_exe()?;
    let mut candidates = Vec::new();
    if let Some(parent) = current.parent() {
        candidates.push(crate::install_layout::companion_mcp_path(&current));
        // `cargo test` runs from target/{debug,release}/deps while the binary
        // companion is emitted one directory above the test harness.
        if let Some(target_profile) = parent.parent() {
            candidates.push(target_profile.join(crate::install_layout::MCP_FILE));
        }
        if let Ok(root) =
            crate::install_layout::installation_root_from_executable(&current).canonicalize()
        {
            if let Ok(path) = crate::install_layout::resolve_mcp_path(&root) {
                candidates.push(path);
            }
        }
    }
    candidates.dedup();
    if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
        return Ok(path);
    }
    // Unit tests do not need to spawn MCP and may run without a companion.
    // Production/debug application launches fail clearly instead of silently
    // selecting the GUI-subsystem Agent.
    if cfg!(debug_assertions) {
        return Ok(current);
    }
    Err("HiMind Agent MCP console companion is missing; rebuild or reinstall the same Agent version".into())
}

fn ensure_himind_skill_adapter(home: &Path, options: &Options) -> Result<(), Box<dyn Error>> {
    let records = himind_skill_records(options)?;
    let target = home.join(HIMIND_SKILL_ADAPTER_DIR);
    let staging = home.join(format!(
        ".{HIMIND_SKILL_ADAPTER_DIR}-staging-{}-{}",
        std::process::id(),
        unix_time_millis()
    ));
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let mut names = HashSet::new();
        for record in &records {
            let source = fs::read_to_string(record.version_root.join("SKILL.md"))?;
            let name = dsh_skill_name(&record.manifest.id, &source, &mut names);
            let destination = staging.join(&name);
            copy_skill_package(record, &destination)?;
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

fn himind_skill_records(options: &Options) -> Result<Vec<SkillRecord>, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    // Skill discovery is an enhancement to the DSH launch. A malformed or
    // temporarily unavailable capability registry must not prevent the AI
    // session itself from starting; readiness filtering will conservatively
    // omit skills that cannot be proven usable in that case.
    let capability_facts = crate::skill::capability_facts_from_gateway(
        options,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::store::types::LocalWorkerStatus::default(),
        )),
        &crate::capability::types::InvocationContext::new(
            crate::capability::types::InvocationSource::Mcp,
            "himind-ai",
        ),
    )
    .unwrap_or_default();
    Ok(store
        .list_records()?
        .into_iter()
        // HiMind AI consumes the same portable Agent Skills packages as the
        // external clients. Client-specific internal packages remain scoped.
        .filter(|record| {
            record.current
                && skill_manifest_ready_for_himind_ai(&record.manifest, &capability_facts)
        })
        .filter(|record| required_plugin_dependencies_ready(&record.manifest.plugin_dependencies))
        .collect())
}

fn skill_manifest_ready_for_himind_ai(
    manifest: &crate::skill::types::SkillManifest,
    capability_facts: &[crate::skill::resolver::CapabilityFact],
) -> bool {
    crate::skill::clients::manifest_supports_client(manifest, HIMIND_MCP_CLIENT_ID)
        && crate::skill::resolver::SkillReadiness::resolve(
            manifest,
            capability_facts,
            crate::VERSION,
            HIMIND_MCP_CLIENT_ID,
        )
        .state
            != "blocked"
}

fn required_plugin_dependencies_ready(
    dependencies: &[crate::skill::types::SkillPluginDependency],
) -> bool {
    dependencies
        .iter()
        .filter(|dependency| dependency.required)
        .all(|dependency| {
            let Ok(Some(plugin)) = crate::capability::plugin::find_plugin(&dependency.plugin_id)
            else {
                return false;
            };
            if !plugin.enabled || plugin.error.is_some() {
                return false;
            }
            dependency
                .min_version
                .as_deref()
                .map(|minimum| {
                    crate::skill::resolver::compare_versions(&plugin.version, minimum)
                        != std::cmp::Ordering::Less
                })
                .unwrap_or(true)
        })
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

fn dsh_skill_name(skill_id: &str, source: &str, used: &mut HashSet<String>) -> String {
    let candidate = frontmatter_skill_name(source)
        .and_then(|name| normalize_dsh_skill_name(&name))
        .unwrap_or_else(|| {
            let slug = skill_id.rsplit('.').next().unwrap_or(skill_id);
            normalize_dsh_skill_name(slug).unwrap_or_else(|| "skill".to_string())
        });
    let candidate = shorten_dsh_skill_name(&candidate);
    if used.insert(candidate.clone()) {
        return candidate;
    }

    // Names are normally unique. Keep collisions deterministic while adding
    // only a short suffix instead of exposing the full namespaced Skill ID.
    let mut digest = Sha256::new();
    digest.update(skill_id.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    let base = candidate.chars().take(32).collect::<String>();
    for length in 6..=12 {
        let name = format!("{}-{}", base.trim_end_matches('-'), &digest[..length]);
        if used.insert(name.clone()) {
            return name;
        }
    }
    format!("{}-{}", base.trim_end_matches('-'), &digest[..16])
}

fn frontmatter_skill_name(source: &str) -> Option<String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("name:") {
            let value = value.trim();
            let value = value.trim_matches(|character| character == '\'' || character == '"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn normalize_dsh_skill_name(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut previous_dash = false;
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() {
            result.push((byte as char).to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            result.push('-');
            previous_dash = true;
        }
    }
    let result = result.trim_matches('-').to_string();
    if result.is_empty() || result.len() > 48 || result.starts_with("himind-com-himind-") {
        return None;
    }
    Some(result)
}

fn shorten_dsh_skill_name(value: &str) -> String {
    match value {
        "develop-himind-plugins" => "plugin-dev".to_string(),
        "develop-himind-skills" => "skill-dev".to_string(),
        "software-distribution" => "software-dist".to_string(),
        "unihper-unity-development" => "unity-dev".to_string(),
        "git-svn-commit-summary" => "commit-summary".to_string(),
        "image-delivery-preflight" => "image-preflight".to_string(),
        other => other.to_string(),
    }
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
    ensure_interactive_home(&root, version)?;
    Ok(root.join(INTERACTIVE_HOME_DIRECTORY))
}

fn native_dsh_home(version: &str) -> Result<PathBuf, String> {
    if let Some(value) = env::var_os(DSH_HOME_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    dsh_home(version)
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

fn ensure_interactive_home(root: &Path, current_version: &str) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("创建 HiMind AI 用户数据目录失败: {error}"))?;
    let target = root.join(INTERACTIVE_HOME_DIRECTORY);
    let marker = target.join(INTERACTIVE_HOME_MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(());
    }

    let source = latest_legacy_home(root, current_version)?;
    if !target.exists() {
        let staging = root.join(format!(
            ".{INTERACTIVE_HOME_DIRECTORY}.installing-{}",
            std::process::id()
        ));
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| format!("清理用户数据迁移临时目录失败: {error}"))?;
        }
        fs::create_dir_all(&staging)
            .map_err(|error| format!("创建用户数据迁移临时目录失败: {error}"))?;
        let result = copy_home_tree(
            source.as_deref(),
            &staging,
            &mut HomeMigrationBudget::default(),
            false,
        );
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        write_home_migration_marker(&staging, source.as_deref(), current_version)?;
        fs::rename(&staging, &target)
            .map_err(|error| format!("提交 HiMind AI 用户数据迁移失败: {error}"))?;
        return Ok(());
    }

    copy_home_tree(
        source.as_deref(),
        &target,
        &mut HomeMigrationBudget::default(),
        true,
    )?;
    write_home_migration_marker(&target, source.as_deref(), current_version)
}

fn latest_legacy_home(root: &Path, current_version: &str) -> Result<Option<PathBuf>, String> {
    let current = semver::Version::parse(current_version).ok();
    let mut candidates = Vec::new();
    let entries =
        fs::read_dir(root).map_err(|error| format!("读取 HiMind AI 用户数据目录失败: {error}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir()
            || path.file_name().and_then(|name| name.to_str()) == Some(INTERACTIVE_HOME_DIRECTORY)
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(version) = semver::Version::parse(name) else {
            continue;
        };
        if current.as_ref().is_some_and(|current| &version >= current) {
            continue;
        }
        candidates.push((version, path));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(candidates.pop().map(|(_, path)| path))
}

fn copy_home_tree(
    source: Option<&Path>,
    target: &Path,
    budget: &mut HomeMigrationBudget,
    merge: bool,
) -> Result<(), String> {
    let Some(source) = source else {
        return Ok(());
    };
    if !source.is_dir() {
        return Ok(());
    }
    copy_home_directory(source, target, budget, merge)
}

fn copy_home_directory(
    source: &Path,
    target: &Path,
    budget: &mut HomeMigrationBudget,
    merge: bool,
) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|error| format!("创建用户数据目录失败: {error}"))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("读取用户数据失败: {error}"))?
        .flatten()
    {
        let source_path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == HIMIND_SKILL_ADAPTER_DIR || name == ".himind-preflight" || name == "runs" {
            continue;
        }
        let target_path = target.join(name.as_ref());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| format!("读取用户数据元信息失败: {error}"))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            copy_home_directory(&source_path, &target_path, budget, merge)?;
            continue;
        }
        if !metadata.is_file() || (merge && target_path.exists()) {
            continue;
        }
        budget.files = budget.files.saturating_add(1);
        budget.bytes = budget.bytes.saturating_add(metadata.len());
        if budget.files > INTERACTIVE_HOME_MIGRATION_MAX_FILES {
            return Err("HiMind AI 用户数据迁移文件数量超出安全限制".to_string());
        }
        if budget.bytes > INTERACTIVE_HOME_MIGRATION_MAX_BYTES {
            return Err("HiMind AI 用户数据迁移大小超出安全限制".to_string());
        }
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建用户数据目录失败: {error}"))?;
        }
        fs::copy(&source_path, &target_path)
            .map_err(|error| format!("迁移 HiMind AI 用户数据失败: {error}"))?;
    }
    Ok(())
}

fn write_home_migration_marker(
    target: &Path,
    source: Option<&Path>,
    current_version: &str,
) -> Result<(), String> {
    let marker = target.join(INTERACTIVE_HOME_MIGRATION_MARKER);
    let source_name = source
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("none");
    let payload = json!({
        "schema_version": 1,
        "source_home": source_name,
        "target_home": INTERACTIVE_HOME_DIRECTORY,
        "runtime_version": current_version,
        "created_at": unix_time_millis(),
    });
    fs::write(
        marker,
        serde_json::to_vec_pretty(&payload).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("写入用户数据迁移记录失败: {error}"))
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
        dsh_run_home, dsh_skill_name, ensure_interactive_home, ensure_profile_patch, first_line,
        managed_model_catalog, merge_profile_package, migrate_legacy_managed_settings,
        native_dsh_provider_config, parse_native_dsh_provider_config, parse_runtime_version,
        remove_managed_runtime, render_himind_agent_overlay, render_himind_profile_patch,
        render_himind_profile_patch_from_base, safe_relative_path, safe_segment,
        skill_manifest_ready_for_himind_ai, strip_yaml_frontmatter, versioned_home,
        InteractiveEventProjector,
    };
    use crate::api::ai::AIUserCredential;
    use crate::app::mcp_settings::McpServerConfig;
    use serde_json::json;
    use std::collections::{BTreeMap, HashSet};

    #[test]
    fn extracts_first_non_empty_version_line() {
        assert_eq!(first_line("\n dsh 0.1.0-rc.6\n"), "dsh 0.1.0-rc.6");
        assert_eq!(
            parse_runtime_version("dsh 0.1.0-rc.6\n"),
            Some("0.1.0-rc.6".to_string())
        );
    }

    #[test]
    fn native_dsh_provider_config_reads_selected_provider_and_catalog() {
        let config = parse_native_dsh_provider_config(
            "agent-default-model:\n  provider: personal-deepseek\n  model: deepseek-chat\nllm-pi-ai:\n  providers:\n    personal-deepseek:\n      displayName: Personal DeepSeek\n      apiKeyEnv: PERSONAL_DEEPSEEK_API_KEY\n      api: openai-completions\n      baseURL: https://api.example.test/v1\n      models:\n        - id: deepseek-chat\n        - id: deepseek-reasoner\n        - local-compatible\n",
        )
        .unwrap();

        assert_eq!(config.provider, "personal-deepseek");
        assert_eq!(config.model, "deepseek-chat");
        assert_eq!(
            config.api_key_env.as_deref(),
            Some("PERSONAL_DEEPSEEK_API_KEY")
        );
        assert_eq!(config.base_url, "https://api.example.test/v1");
        assert_eq!(
            config.models,
            vec!["deepseek-chat", "deepseek-reasoner", "local-compatible"]
        );
    }

    #[test]
    fn independent_dsh_launch_allows_missing_native_settings() {
        let root = std::env::temp_dir().join(format!(
            "himind-independent-dsh-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(
            native_dsh_provider_config(&root),
            super::NativeDshProviderConfig::default()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn independent_dsh_launch_allows_incomplete_native_settings() {
        let root = std::env::temp_dir().join(format!(
            "himind-independent-dsh-incomplete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.yaml"), "theme:\n  mode: dark\n").unwrap();

        assert_eq!(
            native_dsh_provider_config(&root),
            super::NativeDshProviderConfig::default()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("settings.yaml")).unwrap(),
            "theme:\n  mode: dark\n"
        );
        let _ = std::fs::remove_dir_all(root);
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
    fn interactive_home_migrates_legacy_data_once_and_skips_generated_skill_adapter() {
        let root = std::env::temp_dir().join(format!(
            "himind-interactive-home-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let legacy = root.join("0.1.0-rc.6");
        std::fs::create_dir_all(legacy.join("sessions")).unwrap();
        std::fs::create_dir_all(legacy.join("himind-skills/old-generated")).unwrap();
        std::fs::write(legacy.join("sessions/session.jsonl"), b"conversation").unwrap();
        std::fs::write(
            legacy.join("settings.yaml"),
            b"agent-default-model:\n  model: deepseek-v4-flash\n",
        )
        .unwrap();
        std::fs::write(
            legacy.join("himind-skills/old-generated/SKILL.md"),
            b"stale",
        )
        .unwrap();

        ensure_interactive_home(&root, "0.1.0-rc.7").unwrap();
        let target = root.join("interactive");
        assert_eq!(
            std::fs::read_to_string(target.join("sessions/session.jsonl")).unwrap(),
            "conversation"
        );
        assert!(target.join("settings.yaml").is_file());
        assert!(!target.join("himind-skills").exists());
        assert!(target
            .join(super::INTERACTIVE_HOME_MIGRATION_MARKER)
            .is_file());

        std::fs::write(legacy.join("sessions/new.jsonl"), b"newer source").unwrap();
        ensure_interactive_home(&root, "0.1.0-rc.7").unwrap();
        assert!(!target.join("sessions/new.jsonl").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn profile_package_merge_preserves_user_plugins_and_required_bundles() {
        let root = std::env::temp_dir().join(format!(
            "himind-profile-package-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("package.json");
        std::fs::write(
            &path,
            r#"{"name":"himind-profile","private":true,"dependencies":{"@example/dsh-plugin":"1.2.3"},"dsh":{"profile":{"bundles":["@example/dsh-plugin"]}}}"#,
        )
        .unwrap();
        merge_profile_package(
            &path,
            include_str!("../../runtime-profiles/himind/package.json"),
        )
        .unwrap();
        let package: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(package["dependencies"]["@example/dsh-plugin"], "1.2.3");
        let bundles = package["dsh"]["profile"]["bundles"].as_array().unwrap();
        assert!(bundles.iter().any(|value| value == "@example/dsh-plugin"));
        assert!(bundles.iter().any(|value| value == "@deepseek-ai/dsh-base"));
        assert!(bundles
            .iter()
            .any(|value| value == "@deepseek-ai/dsh-web-app"));
        let _ = std::fs::remove_dir_all(root);
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
    fn managed_profile_adds_agent_mcp_and_himind_skills_without_disabling_dsh_roots() {
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
        assert!(patch.contains("- id: \"model-default\"\n            name: \"model-default\""));
        assert!(patch.contains("- id: \"model-fast\"\n            name: \"model-fast\""));
        assert!(patch.contains("id: himind-agent-mcp"));
        assert!(patch.contains("name: '@deepseek-ai/dsh-mcp-client'"));
        assert!(patch.contains("HIMIND_AI_CLIENT_ID: \"himind-ai\""));
        assert!(!patch.contains("HIMIND_AI_WORKSPACE:"));
        assert!(patch.contains("id: himind-agent-skill-filesystem"));
        assert!(patch.contains("providerName: himind-managed"));
        assert!(patch.contains("includeDefaultRoots: true"));
        assert!(
            patch.contains("C:/HiMind/runtime-home\\\\himind-skills")
                || patch.contains("C:/HiMind/runtime-home/himind-skills")
        );
    }

    #[test]
    fn project_overlay_passes_workspace_only_to_agent_mcp() {
        let mut options = crate::Options::from_env();
        options.state_path = std::path::PathBuf::from("C:/HiMind/state.json");
        let workspace = std::path::Path::new("C:/HiMind/extensions/demo-plugin");
        let patch = render_himind_agent_overlay(
            std::path::Path::new("C:/HiMind/runtime-home"),
            &options,
            "model-default",
            "https://gateway.example/v1",
            &["model-default".to_string()],
            workspace,
        )
        .unwrap();

        assert!(patch.contains("HIMIND_AI_WORKSPACE:"));
        assert!(patch.contains("C:/HiMind/extensions/demo-plugin"));
        assert_eq!(patch.matches("HIMIND_AI_WORKSPACE:").count(), 1);
    }

    #[test]
    fn existing_managed_profile_patch_is_migrated_without_losing_user_rows() {
        let root = std::env::temp_dir().join(format!(
            "himind-profile-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cordis.patch.yml");
        let mut old = render_himind_profile_patch(
            &root,
            &crate::Options::from_env(),
            "model-default",
            "https://gateway.example/v1",
            &["model-default".to_string()],
        )
        .unwrap();
        old.push_str("\n- id: user-owned-row\n  config:\n    enabled: true\n");
        std::fs::write(&path, old).unwrap();

        ensure_profile_patch(
            &path,
            include_str!("../../runtime-profiles/himind/cordis.patch.yml"),
        )
        .unwrap();

        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("id: user-owned-row"));
        assert!(!migrated.contains("id: agent-default-model"));
        assert!(!migrated.contains("serverName: \"himind\""));
        assert!(!migrated.contains("serverName: \"himind-agent\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn agent_mcp_namespace_conflicts_are_rejected_before_dsh_boot() {
        let root = std::env::temp_dir().join(format!(
            "himind-mcp-namespace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("profiles/himind")).unwrap();
        std::fs::write(
            root.join("profiles/himind/cordis.patch.yml"),
            "- id: user-mcp\n  config:\n    serverName: himind-agent\n",
        )
        .unwrap();
        let error = super::validate_agent_mcp_namespace(&root).unwrap_err();
        assert!(error.to_string().contains("himind-agent"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn independent_profile_does_not_inject_dashboard_provider() {
        let root = std::env::temp_dir().join(format!(
            "himind-independent-profile-{}-{}",
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
        crate::app::runtime_mode::save(
            &options.state_path,
            crate::app::runtime_mode::AgentMode::Independent,
        )
        .unwrap();
        options.effective_mode = crate::app::runtime_mode::AgentMode::Independent;
        let profile = render_himind_profile_patch(
            &root,
            &options,
            "local-model",
            "https://provider.example/v1",
            &["local-model".to_string()],
        )
        .unwrap();
        assert!(!profile.contains("provider: himind-proxy"));
        assert!(profile.contains("id: himind-agent-mcp"));
        assert!(profile.contains("Independent Mode"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_profile_preserves_user_selected_himind_model_after_runtime_update() {
        let root = std::env::temp_dir().join(format!(
            "himind-profile-selected-model-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.yaml"),
            "agent-default-model:\n  provider: himind-proxy\n  model: deepseek-v4-flash\n",
        )
        .unwrap();
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = root.join("agent-state.json");

        let profile = render_himind_profile_patch(
            &root,
            &options,
            "glm-5.2",
            "https://gateway.example/v1",
            &["glm-5.2".to_string(), "qwen-max".to_string()],
        )
        .unwrap();

        assert!(profile.contains(
            "- id: agent-default-model\n  config:\n    provider: \"himind-proxy\"\n    model: \"deepseek-v4-flash\""
        ));
        assert!(profile
            .contains("- id: \"deepseek-v4-flash\"\n            name: \"deepseek-v4-flash\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_profile_preserves_user_selected_custom_provider() {
        let root = std::env::temp_dir().join(format!(
            "himind-profile-custom-provider-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.yaml"),
            "agent-default-model:\n  provider: personal-deepseek\n  model: deepseek-chat\n",
        )
        .unwrap();
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = root.join("agent-state.json");

        let profile = render_himind_profile_patch(
            &root,
            &options,
            "glm-5.2",
            "https://gateway.example/v1",
            &["glm-5.2".to_string()],
        )
        .unwrap();

        assert!(profile.contains(
            "- id: agent-default-model\n  config:\n    provider: \"personal-deepseek\"\n    model: \"deepseek-chat\""
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_profile_routes_personal_mcp_through_agent_gateway() {
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
        assert!(patch.contains("id: himind-agent-mcp"));
        assert!(!patch.contains("id: himind-agent-personal-mcp-project-tools"));
        assert!(!patch.contains("serverName: \"project-tools\""));
        assert!(!patch.contains("\"API_KEY\": \"local-secret\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn himind_skill_adapter_uses_valid_stable_names_and_one_frontmatter_block() {
        let mut used = HashSet::new();
        let name = dsh_skill_name(
            "com.himind.skill.develop-himind-plugins",
            "---\nname: develop-himind-plugins\ndescription: test\n---\n# Test",
            &mut used,
        );
        assert_eq!(name, "plugin-dev");
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
        let mut used = HashSet::new();
        assert_eq!(
            name,
            dsh_skill_name(
                "com.himind.skill.develop-himind-plugins",
                "---\nname: develop-himind-plugins\ndescription: test\n---\n# Test",
                &mut used,
            )
        );
    }

    #[test]
    fn himind_skill_adapter_only_hashes_name_collisions() {
        let mut used = HashSet::new();
        let first = dsh_skill_name(
            "com.example.skill.one",
            "---\nname: shared\ndescription: one\n---\n# One",
            &mut used,
        );
        let second = dsh_skill_name(
            "com.other.skill.two",
            "---\nname: shared\ndescription: two\n---\n# Two",
            &mut used,
        );
        assert_eq!(first, "shared");
        assert!(second.starts_with("shared-"));
        assert!(second.len() <= 39);
    }

    #[test]
    fn himind_skill_adapter_accepts_portable_ready_skills() {
        let mut manifest = crate::skill::types::SkillManifest {
            id: "com.example.skill".to_string(),
            name: "Example".to_string(),
            author: String::new(),
            categories: Vec::new(),
            version: "1.0.0".to_string(),
            scope: crate::skill::types::SkillScope::User,
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: String::new(),
            supported_clients: vec!["codex".to_string()],
            capabilities: Vec::new(),
            plugin_dependencies: Vec::new(),
            risk_summary: String::new(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        assert!(skill_manifest_ready_for_himind_ai(&manifest, &[]));

        manifest.supported_clients.push("himind-ai".to_string());
        assert!(skill_manifest_ready_for_himind_ai(&manifest, &[]));

        manifest
            .capabilities
            .push(crate::skill::types::SkillCapabilityDependency {
                id: "missing.capability".to_string(),
                required: true,
                min_version: None,
                max_version: None,
                provider: None,
            });
        assert!(!skill_manifest_ready_for_himind_ai(&manifest, &[]));
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
            protocol: "openai-responses".to_string(),
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
        assert!(profile.contains("provider: \"himind-proxy\""));
        assert!(profile.contains("apiKeyEnv: DEEPSEEK_API_KEY"));
        assert!(profile.contains("api: openai-completions"));
        assert!(profile.contains("model: \"deepseek-model\""));
        assert!(
            profile.contains("baseURL: \"https://dashboard.example/api/agent/runs/run-1/ai/v1\"")
        );
        assert!(profile.contains("id: \"fast-model\""));
        assert!(profile.contains("- id: \"deepseek-model\"\n            name: \"deepseek-model\""));
        assert!(profile.contains("- id: \"fast-model\"\n            name: \"fast-model\""));
        assert!(!profile.contains("apiKey:"));
    }

    #[test]
    fn legacy_generated_settings_are_preserved_without_rewriting_user_preferences() {
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
        assert!(migrated.contains("agent-default-model"));
        assert!(migrated.contains("llm-pi-ai"));
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
    fn managed_web_profile_does_not_hide_dsh_configuration_surfaces() {
        let profile = include_str!("../../runtime-profiles/web/cordis.patch.yml");
        for id in [
            "ui-settings-general",
            "ui-settings-models",
            "ui-settings-plugin-inventory",
            "ui-settings-plugins",
            "ui-agent-preset",
            "ui-model-selection",
        ] {
            assert!(!profile.contains(&format!("- id: {id}\n  disabled: true")));
        }
        assert!(!profile.contains("disabled: true"));
    }

    #[test]
    fn himind_profile_keeps_dsh_extension_surfaces_open() {
        let profile = include_str!("../../runtime-profiles/himind/cordis.patch.yml");
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
        assert!(!profile.contains("- id: ui-agent-preset\n  disabled: true"));
    }

    #[test]
    fn managed_headless_profile_reuses_himind_services_without_web_surface() {
        let mut options = crate::Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        options.state_path = std::path::PathBuf::from("C:/HiMind/state.json");
        let profile = render_himind_profile_patch_from_base(
            std::path::Path::new("C:/HiMind/runtime-home"),
            &options,
            "model-default",
            "https://gateway.example/v1",
            &["model-default".to_string(), "model-fast".to_string()],
            include_str!("../../runtime-profiles/himind-headless/cordis.patch.yml"),
            None,
        )
        .unwrap();
        assert!(profile.contains("id: himind-agent-mcp"));
        assert!(profile.contains("id: himind-agent-skill-filesystem"));
        assert!(profile.contains("id: agent-default-model"));
        assert!(profile.contains("id: llm-pi-ai"));
        assert!(!profile.contains("id: web-runtime"));
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
        assert_eq!(approval.request_rpc_id, "rpc-1");
        assert_eq!(approval.approval_id, "approval-1");
        assert!(!serde_json::to_string(&approval)
            .unwrap()
            .contains("private arguments"));
    }
}
