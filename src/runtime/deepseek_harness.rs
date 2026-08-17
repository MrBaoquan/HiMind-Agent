use reqwest::blocking::Client;
use serde_json::{json, Value};
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

#[derive(Debug, Clone)]
pub(crate) struct InteractiveLaunch {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
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
        .map(|value| first_line(&value))
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

pub(crate) fn install(
    options: &Options,
    client_instance_id: &str,
) -> Result<DeepSeekHarnessRuntimeStatus, String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(INSTALL_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("创建 Dashboard 客户端失败: {error}"))?;
    let update = resolve_runtime_component(
        &client,
        &options.api_base,
        RUNTIME_PRODUCT_ID,
        "0.0.0",
        RUNTIME_CHANNEL,
        RUNTIME_PLATFORM,
        RUNTIME_ARCHITECTURE,
        client_instance_id,
    )
    .map_err(|error| format!("解析 DeepSeek Harness Runtime 发布失败: {error}"))?
    .ok_or_else(|| "Dashboard 没有可用的 DeepSeek Harness Runtime Release。".to_string())?;
    validate_update(&options.api_base, &update)?;
    let archive = download_runtime_archive(&client, &update)?;
    let result = install_runtime_archive(&archive, &update);
    if result.is_ok() {
        let _ = fs::remove_file(&archive);
    }
    result.map(|_| status())
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
    let invocation = Invocation {
        executable: executable.clone(),
        args: Vec::new(),
        workspace: std::env::current_dir().map_err(|error| error.to_string())?,
        home: home.clone(),
        api_key: credential.api_key.clone(),
        base_url: credential.access.base_url.clone(),
        model: credential.access.model.clone(),
        permission_mode: "ask",
        run_id: "interactive".to_string(),
    };
    ensure_home_config(&invocation).map_err(|error| error.to_string())?;
    Ok(InteractiveLaunch {
        executable: PathBuf::from(executable),
        home,
        api_key: credential.api_key,
        base_url: credential.access.base_url,
        model: credential.access.model,
    })
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
) -> Result<(), String> {
    let versions = runtime_root().join("versions");
    fs::create_dir_all(&versions).map_err(|error| format!("创建 Runtime 安装目录失败: {error}"))?;
    let suffix = &update.sha256[..12.min(update.sha256.len())];
    let target = versions.join(format!("{}-{}", safe_segment(&update.version)?, suffix));
    let temporary = target.with_extension("installing");
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    extract_runtime_archive(archive_path, &temporary)?;
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
    if target.exists() {
        let _ = fs::remove_dir_all(&target);
    }
    fs::rename(&temporary, &target).map_err(|error| format!("提交 Runtime 安装失败: {error}"))?;
    write_runtime_state(&InstalledRuntimeState {
        schema_version: 2,
        product_id: RUNTIME_PRODUCT_ID.to_string(),
        provider: RUNTIME_CONTRACT.to_string(),
        version: manifest.version,
        executable_path: target.join(executable).to_string_lossy().to_string(),
    })?;
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
    ensure_home_config(&invocation)?;
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
    let home = dsh_home(&runtime_version)?;
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

fn ensure_home_config(invocation: &Invocation) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&invocation.home)?;
    ensure_himind_profile(&invocation.home)?;
    let settings = render_settings(&invocation.model, &invocation.base_url);
    let path = invocation.home.join("settings.yaml");
    fs::write(path, settings)?;
    Ok(())
}

fn ensure_himind_profile(home: &Path) -> Result<(), Box<dyn Error>> {
    let profile = home.join("profiles").join("himind");
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
        (
            "cordis.patch.yml",
            include_str!("../../runtime-profiles/himind/cordis.patch.yml"),
        ),
    ];
    for (name, content) in files {
        fs::write(profile.join(name), content)?;
    }
    Ok(())
}

fn render_settings(model: &str, base_url: &str) -> String {
    format!(
        "agent-default-model:\n  provider: himind-proxy\n  model: {}\nllm-pi-ai:\n  providers:\n    himind-proxy:\n      displayName: HiMind AI\n      apiKeyEnv: DEEPSEEK_API_KEY\n      api: openai-completions\n      baseURL: {}\n      models:\n        - id: {}\n",
        yaml_scalar(model),
        yaml_scalar(base_url),
        yaml_scalar(model)
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
        first_line, parse_runtime_version, safe_relative_path, safe_segment, versioned_home,
        InteractiveEventProjector,
    };
    use serde_json::json;

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
    fn yaml_scalar_quotes_model_and_url() {
        assert_eq!(super::yaml_scalar("model:1"), "\"model:1\"");
    }

    #[test]
    fn settings_route_uses_only_the_claim_scoped_himind_proxy() {
        let settings = super::render_settings(
            "deepseek-model",
            "https://dashboard.example/api/agent/runs/run-1/ai/v1",
        );
        assert!(settings.contains("provider: himind-proxy"));
        assert!(settings.contains("apiKeyEnv: DEEPSEEK_API_KEY"));
        assert!(settings.contains("api: openai-completions"));
        assert!(settings.contains("model: \"deepseek-model\""));
        assert!(
            settings.contains("baseURL: \"https://dashboard.example/api/agent/runs/run-1/ai/v1\"")
        );
        assert!(!settings.contains("apiKey:"));
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
