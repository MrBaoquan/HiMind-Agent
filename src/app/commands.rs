use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::api::distribution::ExtensionDesiredState;
use crate::api::types::AgentTaskHistoryItem;
use crate::app::extension_lock::ADHOC_SOURCE;
use crate::app::remote_clients;
use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    is_agent_auto_start_enabled, local_agent_executable_metadata, open_agent_install_directory,
    open_folder as open_system_folder, open_url, set_agent_auto_start,
};
use crate::approval::manager::ApprovalManager;
use crate::capability::plugin::{registry_json, registry_json_for_control_plane};
use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::remote::client::inner_admin_base;
use crate::skill::catalog_json;
use crate::store::credentials::{
    clear_local_inner_admin_credentials, local_login_status_json, local_unity_editor_settings,
    save_local_inner_admin_credentials, save_local_unity_editor_path,
};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

#[derive(Clone)]
pub(crate) struct AgentState {
    pub worker_status: Arc<Mutex<LocalWorkerStatus>>,
    pub approval_manager: Arc<ApprovalManager>,
    pub capability_gateway: CapabilityGateway,
    pub port: u16,
    pub dashboard_base: String,
    pub state_path: PathBuf,
    pub options: Options,
    pub dashboard_authorization: Arc<Mutex<crate::app::identity::DashboardAuthorizationFlow>>,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct BuiltinAIRuntimeInstallationStatus {
    pub state: String,
    pub operation: String,
    pub stage: String,
    pub progress_percent: u8,
    pub message: String,
    pub error: String,
    pub runtime: crate::runtime::builtin::BuiltinAIRuntimeStatus,
    pub update_available: bool,
    pub available_version: String,
    pub release_notes: String,
    pub mandatory_update: bool,
}

static BUILTIN_AI_RUNTIME_INSTALLATION: OnceLock<Mutex<BuiltinAIRuntimeInstallationStatus>> =
    OnceLock::new();

fn builtin_ai_runtime_installation() -> &'static Mutex<BuiltinAIRuntimeInstallationStatus> {
    BUILTIN_AI_RUNTIME_INSTALLATION.get_or_init(|| {
        let runtime = crate::runtime::builtin::status();
        let ready = runtime.compatible;
        Mutex::new(BuiltinAIRuntimeInstallationStatus {
            state: if ready { "ready" } else { "idle" }.to_string(),
            operation: "none".to_string(),
            stage: if ready { "ready" } else { "idle" }.to_string(),
            progress_percent: if ready { 100 } else { 0 },
            message: if ready {
                "HiMind AI 运行时已就绪".to_string()
            } else {
                "尚未安装 HiMind AI 运行时".to_string()
            },
            error: String::new(),
            runtime,
            update_available: false,
            available_version: String::new(),
            release_notes: String::new(),
            mandatory_update: false,
        })
    })
}

fn builtin_ai_runtime_installation_snapshot() -> BuiltinAIRuntimeInstallationStatus {
    builtin_ai_runtime_installation()
        .lock()
        .map(|status| status.clone())
        .unwrap_or_else(|_| BuiltinAIRuntimeInstallationStatus {
            state: "failed".to_string(),
            operation: "none".to_string(),
            stage: "failed".to_string(),
            progress_percent: 0,
            message: "无法读取 HiMind AI 运行时安装状态".to_string(),
            error: "运行时安装状态不可用".to_string(),
            runtime: crate::runtime::builtin::status(),
            update_available: false,
            available_version: String::new(),
            release_notes: String::new(),
            mandatory_update: false,
        })
}

fn update_builtin_ai_runtime_installation(
    operation: &str,
    stage: &str,
    progress_percent: u8,
    message: &str,
) {
    if let Ok(mut status) = builtin_ai_runtime_installation().lock() {
        status.state = "working".to_string();
        status.operation = operation.to_string();
        status.stage = stage.to_string();
        status.progress_percent = progress_percent.min(100);
        status.message = message.to_string();
        status.error.clear();
    }
}

fn dashboard_agent_user_client(
    state: &AgentState,
    required_scope: &str,
) -> Result<(String, String, reqwest::blocking::Client), String> {
    require_dashboard(state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if agent_id.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let access = crate::api::oauth::platform_access_token(&state.options, required_scope)
        .map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    Ok((agent_id, access.token, client))
}

fn require_dashboard(state: &AgentState) -> Result<(), String> {
    if state.options.mode().dashboard_enabled() {
        Ok(())
    } else {
        Err(crate::app::runtime_mode::control_plane_required_error())
    }
}

#[tauri::command]
pub(crate) async fn get_dashboard_identity_status(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardIdentityStatus, String> {
    if !state.options.mode().dashboard_enabled() {
        // Independent mode has no Dashboard identity of its own. Clear only a
        // leftover Dashboard binding (e.g. after switching from connected
        // mode); a locally confirmed, unbounded full_access approval posture
        // must survive restarts instead of being reset on every identity poll.
        let settings = state.approval_manager.get_settings();
        if !settings.owner_user_id.trim().is_empty() || !settings.agent_id.trim().is_empty() {
            state.approval_manager.clear_identity()?;
        }
        return Ok(crate::app::identity::independent_status(&state.options));
    }
    let options = state.options.clone();
    let manager = Arc::clone(&state.approval_manager);
    tauri::async_runtime::spawn_blocking(move || {
        let status = crate::app::identity::identity_status(&options);
        if !status.user_id.trim().is_empty() && !status.agent_id.trim().is_empty() {
            manager.bind_identity(&status.user_id, &status.agent_id)?;
        } else {
            manager.clear_identity()?;
        }
        Ok(status)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_builtin_ai_activity(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::AI_CONVERSATION_SCOPE)?;
    let response = client
        .get(format!(
            "{}/api/integrations/ai/runtime/sessions/activity",
            state.dashboard_base.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .send()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("Dashboard returned HTTP {}", response.status()));
    }
    response.json().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn start_dashboard_authorization(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardAuthorizationProgress, String> {
    require_dashboard(&state)?;
    crate::app::identity::start_authorization(
        state.options.clone(),
        Arc::clone(&state.dashboard_authorization),
        Arc::clone(&state.approval_manager),
    )
}

#[tauri::command]
pub(crate) fn get_dashboard_authorization_progress(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardAuthorizationProgress, String> {
    require_dashboard(&state)?;
    Ok(crate::app::identity::authorization_progress(
        &state.dashboard_authorization,
    ))
}

#[tauri::command]
pub(crate) fn cancel_dashboard_authorization(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardAuthorizationProgress, String> {
    require_dashboard(&state)?;
    crate::app::identity::cancel_authorization(&state.dashboard_authorization)
}

#[tauri::command]
pub(crate) fn open_dashboard_authorization_page(
    state: State<'_, AgentState>,
) -> Result<(), String> {
    require_dashboard(&state)?;
    let progress = crate::app::identity::authorization_progress(&state.dashboard_authorization);
    if progress.verification_uri_complete.trim().is_empty() {
        return Err("当前没有可打开的 Dashboard 授权页面".to_string());
    }
    open_url(&progress.verification_uri_complete).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn revoke_dashboard_authorization(
    state: State<'_, AgentState>,
) -> Result<(), String> {
    require_dashboard(&state)?;
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::api::oauth::revoke_authorization(&options).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    state
        .approval_manager
        .add_log("info", "已退出 Dashboard 账号授权");
    state.approval_manager.clear_identity()?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn test_mcp_connection(
    state: State<'_, AgentState>,
) -> Result<crate::app::ai_clients::McpConnectionTestResult, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::ai_clients::test_connection(&options).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_mcp_registry_snapshot(
    state: State<'_, AgentState>,
) -> Result<crate::app::mcp_registry::McpRegistrySnapshot, String> {
    crate::app::mcp_registry::public_snapshot(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_mcp_targets(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::app::mcp_targets::McpTargetDescriptor>, String> {
    crate::app::mcp_targets::list(&state.options).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn inspect_mcp_target(
    state: State<'_, AgentState>,
    target_id: String,
) -> Result<serde_json::Value, String> {
    crate::app::mcp_targets::inspect(&state.options, &target_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn plan_mcp_registration(
    state: State<'_, AgentState>,
    target_id: String,
) -> Result<crate::app::mcp_registry::McpRegistrationPlan, String> {
    crate::app::mcp_targets::plan(&state.options, &target_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn apply_mcp_registration(
    state: State<'_, AgentState>,
    target_id: String,
    reset_invalid: Option<bool>,
) -> Result<crate::app::mcp_targets::McpTargetOperationResult, String> {
    let result =
        crate::app::mcp_targets::apply(&state.options, &target_id, reset_invalid.unwrap_or(false))
            .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已应用 MCP 注册目标: {target_id}"));
    Ok(result)
}

#[tauri::command]
pub(crate) fn apply_all_mcp_registrations(
    state: State<'_, AgentState>,
    detected_only: Option<bool>,
    reset_invalid: Option<bool>,
) -> Result<crate::app::mcp_targets::McpTargetBatchResult, String> {
    let result = crate::app::mcp_targets::apply_all(
        &state.options,
        detected_only.unwrap_or(true),
        reset_invalid.unwrap_or(false),
    )
    .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!(
            "已批量应用 MCP 注册目标: {} 成功, {} 失败",
            result.results.len(),
            result.failures.len()
        ),
    );
    Ok(result)
}

#[tauri::command]
pub(crate) fn remove_mcp_registration(
    state: State<'_, AgentState>,
    target_id: String,
) -> Result<crate::app::mcp_targets::McpTargetOperationResult, String> {
    let result = crate::app::mcp_targets::remove(&state.options, &target_id)
        .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已移除 MCP 注册目标: {target_id}"));
    Ok(result)
}

#[tauri::command]
pub(crate) fn remove_all_mcp_registrations(
    state: State<'_, AgentState>,
    detected_only: Option<bool>,
) -> Result<crate::app::mcp_targets::McpTargetBatchResult, String> {
    let result = crate::app::mcp_targets::remove_all(&state.options, detected_only.unwrap_or(true))
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!(
            "已批量移除 MCP 注册目标: {} 成功, {} 失败",
            result.results.len(),
            result.failures.len()
        ),
    );
    Ok(result)
}

#[tauri::command]
pub(crate) async fn test_mcp_server(
    state: State<'_, AgentState>,
    server_id: String,
) -> Result<crate::app::mcp_probe::McpProbeResult, String> {
    let state_path = state.state_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let server = crate::app::mcp_registry::get(&state_path, &server_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("MCP server not found: {server_id}"))?;
        Ok(crate::app::mcp_probe::probe_report(&server))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_agent_status(state: State<'_, AgentState>) -> Result<serde_json::Value, String> {
    let worker = local_worker_snapshot(&state.worker_status);
    let executable = local_agent_executable_metadata();
    let pending = state.approval_manager.list_pending();
    let login = local_login_status_json();
    let current_task =
        state
            .options
            .task_execution()
            .map(|(task_id, task_type, execution_id, _)| {
                json!({
                    "task_id": task_id,
                    "task_type": task_type,
                    "execution_id": execution_id,
                    "status": "running",
                })
            });

    Ok(json!({
        "status": "online",
        "version": VERSION,
        "profile": crate::store::paths::profile_name(),
        "mode": state.options.mode().as_str(),
        "effective_mode": state.options.mode().as_str(),
        "pending_mode": state.options.pending_mode().as_str(),
        "requires_restart": state.options.mode() != state.options.pending_mode(),
        "dashboard_enabled": state.options.mode().dashboard_enabled(),
        "control_plane": {
            "kind": state.options.mode().control_plane(),
            "enabled": state.options.mode().control_plane_enabled(),
            "available": state.options.mode().control_plane_enabled(),
            "worker_state": worker["dashboard_worker_state"],
            "worker_expected": worker["dashboard_worker_expected"],
            "worker_reason_code": worker["dashboard_worker_reason_code"],
        },
        "local_port": state.port,
        "dashboard_base": state.dashboard_base,
        "executable_name": executable["name"],
        "executable_path": executable["path"],
        "login_status": login["status"],
        "login_label": login["label"],
        "login_account": login["account"],
        "dashboard_worker_online": worker["dashboard_worker_online"],
        "dashboard_agent_id": worker["dashboard_agent_id"],
        "dashboard_worker_error": worker["dashboard_worker_error"],
        "dashboard_worker_state": worker["dashboard_worker_state"],
        "dashboard_worker_expected": worker["dashboard_worker_expected"],
        "dashboard_worker_reason_code": worker["dashboard_worker_reason_code"],
        "mcp_transport": worker["mcp_transport"],
        "local_service_expected": worker["local_service_expected"],
        "local_service_online": worker["local_service_online"],
        "local_service_error": worker["local_service_error"],
        "pending_approvals": pending.len(),
        "current_task": current_task,
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AgentModeSettings {
    /// The value shown in settings. This is the pending value when a restart
    /// is required, so the panel can reflect the user's choice immediately.
    pub mode: String,
    pub effective_mode: String,
    pub pending_mode: String,
    pub dashboard_enabled: bool,
    pub requires_restart: bool,
}

#[tauri::command]
pub(crate) fn get_agent_mode(state: State<'_, AgentState>) -> Result<AgentModeSettings, String> {
    let effective = state.options.mode();
    let pending = state.options.pending_mode();
    Ok(AgentModeSettings {
        mode: pending.as_str().to_string(),
        effective_mode: effective.as_str().to_string(),
        pending_mode: pending.as_str().to_string(),
        dashboard_enabled: pending.dashboard_enabled(),
        requires_restart: effective != pending,
    })
}

#[tauri::command]
pub(crate) fn set_agent_mode(
    state: State<'_, AgentState>,
    mode: String,
) -> Result<AgentModeSettings, String> {
    let previous = state.options.mode();
    let mode = crate::app::runtime_mode::AgentMode::parse(&mode)
        .ok_or_else(|| "运行模式只能是 connected 或 independent".to_string())?;
    crate::app::runtime_mode::save(&state.state_path, mode).map_err(|error| error.to_string())?;
    if previous != mode {
        // Do not leave a session started under the previous control-plane
        // policy running while the user is switching modes.
        crate::app::ui::stop_builtin_ai_process();
    }
    state.approval_manager.add_log(
        "info",
        &format!("Agent 运行模式已设置为 {}，重启后生效", mode.as_str()),
    );
    Ok(AgentModeSettings {
        mode: mode.as_str().to_string(),
        effective_mode: previous.as_str().to_string(),
        pending_mode: mode.as_str().to_string(),
        dashboard_enabled: mode.dashboard_enabled(),
        requires_restart: previous != mode,
    })
}

#[tauri::command]
pub(crate) fn get_agent_update_status(
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    crate::app::update_manager::load(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn check_agent_update(
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    let options = state.options.clone();
    let logs = Arc::clone(&state.approval_manager);
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::update_manager::check_now(&options)
            .inspect(|status| {
                logs.add_log(
                    "info",
                    if status.available_version.is_empty() {
                        "软件更新检查完成，当前已是最新版本".to_string()
                    } else {
                        format!("软件更新检查完成，发现 v{}", status.available_version)
                    }
                    .as_str(),
                )
            })
            .map_err(|error| {
                logs.add_log("error", &format!("软件更新检查失败: {error}"));
                error.to_string()
            })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn download_agent_update(
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    let options = state.options.clone();
    let logs = Arc::clone(&state.approval_manager);
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::update_manager::download(&options)
            .inspect(|status| {
                logs.add_log(
                    "info",
                    &format!("软件更新下载完成: v{}", status.available_version),
                )
            })
            .map_err(|error| {
                logs.add_log("error", &format!("软件更新下载失败: {error}"));
                error.to_string()
            })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_agent_update_download(
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    let status = crate::app::update_manager::cancel_download(&state.state_path)
        .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", "已请求取消软件更新下载");
    Ok(status)
}

#[tauri::command]
pub(crate) fn set_agent_update_preferences(
    auto_check: bool,
    auto_download: bool,
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    let status =
        crate::app::update_manager::set_preferences(&state.state_path, auto_check, auto_download)
            .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!(
            "软件更新策略已调整: 自动检查={}，自动下载={}",
            if status.auto_check {
                "开启"
            } else {
                "关闭"
            },
            if status.auto_download {
                "开启"
            } else {
                "关闭"
            },
        ),
    );
    Ok(status)
}

#[tauri::command]
pub(crate) fn install_agent_update(
    state: State<'_, AgentState>,
) -> Result<crate::app::update_manager::AgentUpdateStatus, String> {
    crate::app::update_manager::install(&state.options).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_pending_approvals(
    state: State<'_, AgentState>,
) -> Result<Vec<serde_json::Value>, String> {
    let approvals = state.approval_manager.list_pending();
    Ok(approvals
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "request_type": a.request_type,
                "title": a.title,
                "description": a.description,
                "timeout_seconds": a.timeout_seconds,
                "remaining_seconds": a.remaining_seconds,
                "created_at": a.created_at,
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) fn get_approval_history(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::approval::types::ApprovalFact>, String> {
    Ok(state.approval_manager.list_recent_facts())
}

#[tauri::command]
pub(crate) fn respond_approval(
    state: State<'_, AgentState>,
    id: String,
    approved: bool,
) -> Result<(), String> {
    state.approval_manager.respond(&id, approved)
}

#[tauri::command]
pub(crate) async fn get_approval_settings(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || approval_settings_snapshot(&state))
        .await
        .map_err(|error| error.to_string())?
}

fn approval_settings_snapshot(state: &AgentState) -> Result<serde_json::Value, String> {
    match crate::api::oauth::persisted_authorization_identity(&state.state_path) {
        Some((agent_id, user_id)) => state.approval_manager.bind_identity(&user_id, &agent_id)?,
        None => state.approval_manager.clear_identity()?,
    };
    let settings = state.approval_manager.get_settings();
    let effective_r1 = state.approval_manager.effective_mode_for_risk("R1");
    let effective_r2 = state.approval_manager.effective_mode_for_risk("R2");
    let effective_r3 = state.approval_manager.effective_mode_for_risk("R3");
    let effective_r4 = state.approval_manager.effective_mode_for_risk("R4");
    let now_unix = crate::approval::manager::unix_now();
    let acknowledged_valid = settings.risk_acknowledgement_valid(now_unix);
    let acknowledged_remaining =
        if acknowledged_valid && settings.risk_acknowledged_duration_seconds > 0 {
            settings
                .risk_acknowledged_at
                .saturating_add(settings.risk_acknowledged_duration_seconds)
                .saturating_sub(now_unix)
        } else {
            0
        };
    let auto_start =
        is_agent_auto_start_enabled(&state.dashboard_base, state.port, &state.state_path)
            .unwrap_or(false);
    Ok(json!({
        "rules": settings.rules,
        "timeout_seconds": settings.timeout_seconds,
        "profile": settings.profile,
        "notification_mode": settings.notification_mode,
        "owner_user_id": settings.owner_user_id,
        "agent_id": settings.agent_id,
        "binding_updated_at": settings.binding_updated_at,
        "risk_acknowledged_at": settings.risk_acknowledged_at,
        "risk_acknowledged": acknowledged_valid,
        "risk_acknowledged_duration_seconds": settings.risk_acknowledged_duration_seconds,
        "risk_acknowledged_remaining_seconds": acknowledged_remaining,
        "effective_modes": {
            "read": effective_r1,
            "write": effective_r2,
            "high_risk": effective_r3,
            "system": effective_r4,
        },
        "auto_start": auto_start,
        "editors": local_unity_editor_settings().map_err(|error| error.to_string())?,
    }))
}

#[tauri::command]
pub(crate) fn set_approval_profile(
    state: State<'_, AgentState>,
    profile: String,
    confirmed: bool,
    duration_seconds: Option<u64>,
) -> Result<serde_json::Value, String> {
    state
        .approval_manager
        .update_profile_with_duration(&profile, confirmed, duration_seconds)?;
    state
        .approval_manager
        .add_log("warn", &format!("审批档位已调整为: {}", profile.trim()));
    let settings = state.approval_manager.get_settings();
    let acknowledged_valid =
        settings.risk_acknowledgement_valid(crate::approval::manager::unix_now());
    Ok(serde_json::json!({
        "profile": settings.profile,
        "notification_mode": settings.notification_mode,
        "owner_user_id": settings.owner_user_id,
        "agent_id": settings.agent_id,
        "risk_acknowledged": acknowledged_valid,
        "risk_acknowledged_duration_seconds": settings.risk_acknowledged_duration_seconds,
    }))
}

#[tauri::command]
pub(crate) fn set_approval_notification_mode(
    state: State<'_, AgentState>,
    mode: String,
) -> Result<serde_json::Value, String> {
    state.approval_manager.update_notification_mode(&mode)?;
    state
        .approval_manager
        .add_log("info", &format!("审批提醒方式已调整为: {}", mode.trim()));
    Ok(serde_json::json!({
        "profile": state.approval_manager.get_settings().profile,
        "notification_mode": state.approval_manager.get_settings().notification_mode,
    }))
}

#[tauri::command]
pub(crate) async fn get_remote_execution_settings(
    state: State<'_, AgentState>,
) -> Result<crate::app::remote_execution::RemoteExecutionSettings, String> {
    let state_path = state.state_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::remote_execution::load(&state_path).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn save_remote_execution_settings(
    state: State<'_, AgentState>,
    settings: crate::app::remote_execution::RemoteExecutionSettings,
    full_access_confirmed: Option<bool>,
) -> Result<crate::app::remote_execution::RemoteExecutionSettings, String> {
    let current =
        crate::app::remote_execution::load(&state.state_path).map_err(|error| error.to_string())?;
    // `full_access_confirmed` is the compatibility name of the Tauri payload;
    // it confirms the remote runtime sandbox change, not approval.profile.
    let entering_machine_unrestricted = settings.access_mode
        == crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED
        && (current.access_mode != crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED
            || (!current.enabled && settings.enabled));
    if entering_machine_unrestricted && full_access_confirmed != Some(true) {
        return Err("解除远程 AI 工作区限制必须在本机明确确认".to_string());
    }
    crate::app::remote_execution::save(&state.state_path, &settings)
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        if settings.enabled {
            "已更新远程任务设置"
        } else {
            "已关闭远程任务"
        },
    );
    Ok(settings)
}

#[tauri::command]
pub(crate) async fn get_remote_clients(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let state_path = state.state_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        remote_clients::overview(&state_path).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn detect_remote_clients(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let state_path = state.state_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        remote_clients::detect(&state_path).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn configure_remote_client(
    state: State<'_, AgentState>,
    vendor: String,
    path: String,
) -> Result<serde_json::Value, String> {
    remote_clients::configure(&vendor, &path, &state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn pick_remote_client(vendor: String) -> Result<serde_json::Value, String> {
    let title = if vendor.to_ascii_lowercase().contains("todesk") {
        "选择 ToDesk 客户端程序"
    } else {
        "选择向日葵客户端程序"
    };
    let path = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("Windows 可执行文件", &["exe"])
        .pick_file()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) async fn get_builtin_ai_runtime_status(
    _state: State<'_, AgentState>,
) -> Result<crate::runtime::builtin::BuiltinAIRuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(crate::runtime::builtin::status)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn pick_runtime_manifest() -> Result<serde_json::Value, String> {
    let path = rfd::FileDialog::new()
        .set_title("选择 Runtime 发布清单")
        .add_filter("Runtime Release Manifest", &["json"])
        .pick_file()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) async fn get_builtin_ai_runtime_installation_status(
    _state: State<'_, AgentState>,
) -> Result<BuiltinAIRuntimeInstallationStatus, String> {
    let installation = builtin_ai_runtime_installation();
    let mut status = installation
        .lock()
        .map_err(|_| "HiMind AI 运行时安装状态不可用".to_string())?;
    if status.state != "working" && status.state != "failed" {
        status.runtime = crate::runtime::builtin::status();
        if status.runtime.compatible {
            status.state = "ready".to_string();
            status.operation = "none".to_string();
            status.stage = "ready".to_string();
            status.progress_percent = 100;
            status.message = "HiMind AI 运行时已就绪".to_string();
            status.error.clear();
        } else {
            status.state = "idle".to_string();
            status.operation = "none".to_string();
            status.stage = "idle".to_string();
            status.progress_percent = 0;
            status.message = "尚未安装 HiMind AI 运行时".to_string();
        }
    }
    Ok(status.clone())
}

#[tauri::command]
pub(crate) async fn start_builtin_ai_runtime_install(
    state: State<'_, AgentState>,
    operation: Option<String>,
    manifest_path: Option<String>,
) -> Result<BuiltinAIRuntimeInstallationStatus, String> {
    let operation = operation
        .unwrap_or_else(|| "install".to_string())
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        operation.as_str(),
        "install" | "update" | "repair" | "local" | "uninstall"
    ) {
        return Err("不支持的 HiMind AI 运行时操作".to_string());
    }
    let local_manifest_guard = if operation == "local" {
        let manifest_path = manifest_path
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or("本地安装需要选择 Runtime 发布清单")?;
        Some(
            crate::runtime::distribution::use_local_manifest_scoped(manifest_path)
                .map_err(|error| error.to_string())?,
        )
    } else if manifest_path.is_some() {
        return Err("只有本地安装可以指定 Runtime 发布清单".to_string());
    } else {
        None
    };
    let installation = builtin_ai_runtime_installation();
    {
        let mut current = installation
            .lock()
            .map_err(|_| "HiMind AI 运行时安装状态不可用".to_string())?;
        if current.state == "working" {
            return Ok(current.clone());
        }
        current.runtime = crate::runtime::builtin::status();
        if operation == "install" && current.runtime.compatible {
            current.state = "ready".to_string();
            current.operation = "none".to_string();
            current.stage = "ready".to_string();
            current.progress_percent = 100;
            current.message = "HiMind AI 运行时已就绪".to_string();
            current.error.clear();
            return Ok(current.clone());
        }
        if operation == "update" && !current.runtime.compatible {
            return Err("HiMind AI 运行时尚未安装，请先安装运行时".to_string());
        }
        current.state = "working".to_string();
        current.operation = operation.clone();
        current.stage = if operation == "uninstall" {
            "uninstalling".to_string()
        } else {
            "resolving".to_string()
        };
        current.progress_percent = if operation == "uninstall" { 10 } else { 5 };
        current.message = match operation.as_str() {
            "update" => "正在检查 HiMind AI 运行时更新".to_string(),
            "repair" => "正在准备修复 HiMind AI 运行时".to_string(),
            "local" => "正在读取本地 Runtime 发布清单".to_string(),
            "uninstall" => "正在准备卸载 HiMind AI 运行时".to_string(),
            _ => "正在检查可用的 HiMind AI 运行时".to_string(),
        };
        current.error.clear();
        current.update_available = false;
        current.available_version.clear();
        current.release_notes.clear();
        current.mandatory_update = false;
    }

    crate::app::ui::stop_builtin_ai_process();
    let options = state.options.clone();
    let client_instance_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("himind-agent-{}", crate::store::paths::profile_name()));
    let logs = Arc::clone(&state.approval_manager);
    let operation_for_thread = operation.clone();
    thread::spawn(move || {
        let _local_manifest_guard = local_manifest_guard;
        let mut report_progress = |stage: &str, progress_percent: u8, message: &str| {
            update_builtin_ai_runtime_installation(
                &operation_for_thread,
                stage,
                progress_percent,
                message,
            );
        };
        let result = match operation_for_thread.as_str() {
            "update" => crate::runtime::builtin::update_with_progress(
                &options,
                &client_instance_id,
                &mut report_progress,
            ),
            "uninstall" => crate::runtime::builtin::uninstall_with_progress(&mut report_progress),
            _ => crate::runtime::builtin::install_with_progress(
                &options,
                &client_instance_id,
                &mut report_progress,
            ),
        };
        if let Ok(mut current) = builtin_ai_runtime_installation().lock() {
            match result {
                Ok(runtime) => {
                    current.state = if operation_for_thread == "uninstall" {
                        "idle".to_string()
                    } else {
                        "ready".to_string()
                    };
                    current.operation = operation_for_thread.clone();
                    current.stage = if operation_for_thread == "uninstall" {
                        "idle".to_string()
                    } else {
                        "ready".to_string()
                    };
                    current.progress_percent = if operation_for_thread == "uninstall" {
                        0
                    } else {
                        100
                    };
                    current.message = match operation_for_thread.as_str() {
                        "update" => "HiMind AI 运行时已更新".to_string(),
                        "repair" => "HiMind AI 运行时已修复".to_string(),
                        "local" => "HiMind AI 运行时本地安装完成".to_string(),
                        "uninstall" => "HiMind AI 运行时已卸载".to_string(),
                        _ => "HiMind AI 运行时已就绪".to_string(),
                    };
                    current.error.clear();
                    current.runtime = runtime;
                    current.update_available = false;
                    current.available_version.clear();
                    current.release_notes.clear();
                    current.mandatory_update = false;
                    logs.add_log("info", &current.message);
                }
                Err(error) => {
                    current.state = "failed".to_string();
                    current.operation = operation_for_thread.clone();
                    current.stage = "failed".to_string();
                    current.message = format!(
                        "HiMind AI 运行时{}失败",
                        runtime_operation_label(&operation_for_thread)
                    );
                    current.error = error.clone();
                    current.runtime = crate::runtime::builtin::status();
                    logs.add_log("error", &format!("{}: {error}", current.message));
                }
            }
        }
    });
    Ok(builtin_ai_runtime_installation_snapshot())
}

fn runtime_operation_label(operation: &str) -> &'static str {
    match operation {
        "update" => "更新",
        "repair" => "修复",
        "local" => "本地安装",
        "uninstall" => "卸载",
        _ => "安装",
    }
}

#[tauri::command]
pub(crate) async fn check_builtin_ai_runtime_update(
    state: State<'_, AgentState>,
) -> Result<BuiltinAIRuntimeInstallationStatus, String> {
    let current = builtin_ai_runtime_installation_snapshot();
    if current.state == "working" {
        return Ok(current);
    }
    if !current.runtime.compatible {
        return Err("HiMind AI 运行时尚未安装".to_string());
    }
    let options = state.options.clone();
    let client_instance_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("himind-agent-{}", crate::store::paths::profile_name()));
    let update = tauri::async_runtime::spawn_blocking(move || {
        crate::runtime::builtin::check_update(&options, &client_instance_id)
    })
    .await
    .map_err(|error| error.to_string())??;
    let mut status = builtin_ai_runtime_installation()
        .lock()
        .map_err(|_| "HiMind AI 运行时安装状态不可用".to_string())?;
    status.runtime = crate::runtime::builtin::status();
    status.state = "ready".to_string();
    status.operation = "none".to_string();
    status.stage = "ready".to_string();
    status.progress_percent = 100;
    status.update_available = update.update_available;
    status.available_version = update.available_version;
    status.release_notes = update.release_notes;
    status.mandatory_update = update.mandatory;
    status.message = if status.update_available {
        format!("有新的 HiMind AI 运行时版本 v{}", status.available_version)
    } else {
        "HiMind AI 运行时已是最新版本".to_string()
    };
    status.error.clear();
    Ok(status.clone())
}

#[tauri::command]
pub(crate) async fn install_builtin_ai_runtime(
    state: State<'_, AgentState>,
) -> Result<crate::runtime::builtin::BuiltinAIRuntimeStatus, String> {
    let options = state.options.clone();
    let client_instance_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("himind-agent-{}", crate::store::paths::profile_name()));
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::runtime::builtin::install(&options, &client_instance_id)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", "HiMind AI 运行时已完成安装或修复");
    Ok(result)
}

#[tauri::command]
pub(crate) fn set_approval_rule(
    state: State<'_, AgentState>,
    request_type: String,
    mode: String,
) -> Result<(), String> {
    state.approval_manager.update_rule(&request_type, &mode)?;
    state.approval_manager.add_log(
        "info",
        &format!("审批策略已调整: {} -> {}", request_type, mode),
    );
    Ok(())
}

#[tauri::command]
pub(crate) fn set_approval_timeout(
    state: State<'_, AgentState>,
    seconds: u64,
) -> Result<(), String> {
    state.approval_manager.update_timeout(seconds)?;
    state
        .approval_manager
        .add_log("info", &format!("审批超时已调整为 {seconds} 秒"));
    Ok(())
}

#[tauri::command]
pub(crate) fn get_local_login_status() -> Result<serde_json::Value, String> {
    Ok(local_login_status_json())
}

#[tauri::command]
pub(crate) fn save_local_login(
    state: State<'_, AgentState>,
    username: String,
    password: String,
) -> Result<serde_json::Value, String> {
    save_local_inner_admin_credentials(&username, &password).map_err(|e| e.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!("已更新内网平台登录账号: {}", username.trim()),
    );
    Ok(local_login_status_json())
}

#[tauri::command]
pub(crate) fn logout_local_login(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    clear_local_inner_admin_credentials().map_err(|e| e.to_string())?;
    state
        .approval_manager
        .add_log("info", "已清除内网平台本地登录凭据");
    Ok(local_login_status_json())
}

#[tauri::command]
pub(crate) fn open_dashboard_page(state: State<'_, AgentState>) -> Result<(), String> {
    open_url(&state.dashboard_base).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn start_builtin_ai_session(
    state: State<'_, AgentState>,
    project_id: Option<String>,
    extension_workspace: Option<bool>,
) -> Result<String, String> {
    if !crate::runtime::builtin::status().compatible {
        return Err("HiMind AI 运行时尚未安装，请先安装 HiMind AI 运行时".to_string());
    }
    let extension_workspace = extension_workspace.unwrap_or(false);
    if extension_workspace && project_id.is_some() {
        return Err("不能同时指定扩展项目和扩展聚合仓库".to_string());
    }
    let project = project_id
        .as_deref()
        .map(crate::extension_projects::get)
        .transpose()
        .map_err(|error| error.to_string())?;
    if project
        .as_ref()
        .is_some_and(|item| !item.workspace_available)
    {
        return Err("扩展项目目录当前不可用".to_string());
    }
    let workspace = if extension_workspace {
        let settings = crate::extension_workspace::settings();
        if !settings.valid {
            let message = if settings.error.trim().is_empty() {
                "扩展聚合仓库当前不可用，请先在扩展页面选择有效目录。".to_string()
            } else {
                settings.error
            };
            return Err(message);
        }
        Some(PathBuf::from(settings.root))
    } else {
        project
            .as_ref()
            .map(|item| PathBuf::from(&item.workspace_path))
    };
    let project_name = project
        .as_ref()
        .map(|item| item.name.clone())
        .or_else(|| extension_workspace.then(|| "扩展聚合仓库".to_string()));
    let options = state.options.clone();
    let logs = Arc::clone(&state.approval_manager);
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::app::ui::start_builtin_ai_session(&options, workspace.as_deref())
    })
    .await
    .map_err(|error| error.to_string())?;
    match result {
        Ok(session_url) => {
            logs.add_log(
                "info",
                &project_name
                    .map(|name| format!("HiMind AI 已进入扩展项目: {name}"))
                    .unwrap_or_else(|| "HiMind AI 会话已启动".to_string()),
            );
            Ok(session_url)
        }
        Err(error) => {
            logs.add_log("error", &format!("HiMind AI 会话启动失败: {error}"));
            Err(present_builtin_ai_start_error(&error))
        }
    }
}

#[tauri::command]
pub(crate) async fn open_builtin_ai_web(
    state: State<'_, AgentState>,
    project_id: Option<String>,
    extension_workspace: Option<bool>,
) -> Result<String, String> {
    let session_url = start_builtin_ai_session(state, project_id, extension_workspace).await?;
    open_url(&session_url).map_err(|error| error.to_string())?;
    Ok(session_url)
}

#[tauri::command]
pub(crate) async fn sync_builtin_ai_models(
    state: State<'_, AgentState>,
) -> Result<crate::app::builtin_ai_model_sync::BuiltinAiModelSyncResult, String> {
    require_dashboard(&state)?;
    let options = state.options.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::app::ui::sync_builtin_ai_models(&options)
    })
    .await
    .map_err(|error| error.to_string())?;
    match &result {
        Ok(value) => state.approval_manager.add_log(
            "info",
            &format!(
                "HiMind AI 模型同步完成：{} 个模型，状态={}",
                value.model_count, value.status
            ),
        ),
        Err(error) => state
            .approval_manager
            .add_log("warn", &format!("HiMind AI 模型同步失败：{error}")),
    }
    result
}

#[tauri::command]
pub(crate) async fn get_builtin_ai_tool_context_summary(
    state: State<'_, AgentState>,
) -> Result<crate::runtime::builtin::BuiltinAIToolContextSummary, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::runtime::builtin::interactive_tool_context_summary(&options)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_builtin_ai_mcp_servers(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::app::mcp_registry::McpServerConfig>, String> {
    crate::app::mcp_registry::list_configs(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_builtin_ai_mcp_server(
    state: State<'_, AgentState>,
    server: crate::app::mcp_registry::McpServerConfig,
) -> Result<crate::app::mcp_registry::McpServerConfig, String> {
    let server = crate::app::mcp_registry::upsert_config(&state.state_path, server)
        .map_err(|error| error.to_string())?;
    crate::app::ui::stop_builtin_ai_process();
    state.approval_manager.add_log(
        "info",
        &format!("已保存 HiMind AI MCP 服务: {}", server.server_name),
    );
    Ok(server)
}

#[tauri::command]
pub(crate) fn delete_builtin_ai_mcp_server(
    state: State<'_, AgentState>,
    server_name: String,
) -> Result<bool, String> {
    let removed = crate::app::mcp_registry::remove_config(&state.state_path, &server_name)
        .map_err(|error| error.to_string())?;
    if removed {
        crate::app::ui::stop_builtin_ai_process();
        state
            .approval_manager
            .add_log("info", &format!("已删除 HiMind AI MCP 服务: {server_name}"));
    }
    Ok(removed)
}

#[tauri::command]
pub(crate) fn validate_builtin_ai_mcp_server(
    server: crate::app::mcp_registry::McpServerConfig,
) -> Result<(), String> {
    crate::app::mcp_registry::validate_config(&server)
}

#[tauri::command]
pub(crate) fn reload_builtin_ai_tool_context(state: State<'_, AgentState>) {
    crate::app::ui::stop_builtin_ai_process();
    state
        .approval_manager
        .add_log("info", "HiMind AI 工具上下文已更新");
}

fn present_builtin_ai_start_error(error: &str) -> String {
    let normalized = error.to_lowercase();
    if normalized.contains("运行时尚未安装")
        || normalized.contains("runtime is not installed")
        || normalized.contains("runtime is unavailable")
    {
        return "请先安装 HiMind AI 运行时，再开始对话。".to_string();
    }
    if normalized.contains("请先登录")
        || normalized.contains("授权已失效")
        || normalized.contains("授权已过期")
        || normalized.contains("missing scope")
    {
        return "需要登录 HiMind 账号后才能开始对话".to_string();
    }
    if normalized.contains("尚未生成 ai 凭证")
        || normalized.contains("没有可用的 ai 服务")
        || normalized.contains("没有可用渠道")
    {
        return "当前账号暂未分配可用 AI 服务".to_string();
    }
    if normalized.contains("尚未安装") || normalized.contains("组件状态") {
        return "HiMind AI 运行时需要修复，请在设置中处理".to_string();
    }
    if normalized.contains("扩展项目工作区")
        || normalized.contains("dsh")
        || normalized.contains("workspace")
        || normalized.contains("session")
    {
        let detail = error
            .trim()
            .strip_prefix("无法进入扩展项目工作区：")
            .unwrap_or(error.trim());
        return format!("无法进入项目工作区：{detail}");
    }
    "HiMind AI 暂时无法启动，请稍后重试".to_string()
}

#[tauri::command]
pub(crate) fn open_inner_admin_page() -> Result<(), String> {
    open_url(&format!(
        "{}/admin/personal/software_code",
        inner_admin_base()
    ))
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn open_agent_directory() -> Result<(), String> {
    open_agent_install_directory().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn show_main_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn quit_agent(app: AppHandle) -> Result<(), String> {
    crate::app::ui::stop_builtin_ai_process();
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub(crate) fn set_auto_start(
    state: State<'_, AgentState>,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    let auto_start = set_agent_auto_start(
        enabled,
        &state.dashboard_base,
        state.port,
        &state.state_path,
    )
    .map_err(|e| e.to_string())?;
    state.approval_manager.add_log(
        "info",
        if auto_start {
            "已启用 Agent 开机自启"
        } else {
            "已关闭 Agent 开机自启"
        },
    );
    Ok(json!({ "auto_start": auto_start }))
}

#[tauri::command]
pub(crate) fn pick_unity_editor() -> Result<serde_json::Value, String> {
    let path = rfd::FileDialog::new()
        .set_title("选择 Unity 编辑器")
        .add_filter("Unity 编辑器", &["exe"])
        .pick_file()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) fn save_unity_editor(
    path: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let settings = save_local_unity_editor_path(&path).map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        if path.trim().is_empty() {
            "Unity 编辑器已恢复为工作流默认值"
        } else {
            "Unity 编辑器本机覆盖已更新"
        },
    );
    Ok(settings)
}

#[tauri::command]
pub(crate) fn get_agent_logs(
    state: State<'_, AgentState>,
) -> Result<Vec<serde_json::Value>, String> {
    let logs = state.approval_manager.get_logs();
    Ok(logs
        .iter()
        .map(|l| {
            json!({
                "time": l.time,
                "level": l.level,
                "message": l.message,
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) fn export_agent_diagnostics(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let file_name = format!("himind-agent-diagnostics-{}.zip", diagnostics_unix_now());
    let Some(destination) = rfd::FileDialog::new()
        .set_title("导出 HiMind Agent 诊断包")
        .set_file_name(&file_name)
        .add_filter("ZIP 诊断包", &["zip"])
        .save_file()
    else {
        return Ok(json!({ "canceled": true }));
    };
    let worker = state
        .worker_status
        .lock()
        .map_err(|_| "Agent Worker 状态不可用".to_string())?;
    let path = crate::app::diagnostics::export_bundle(&destination, &state.options, &worker)
        .map_err(|error| error.to_string())?;
    drop(worker);
    state
        .approval_manager
        .add_log("info", "已导出脱敏 Agent 诊断包");
    Ok(json!({ "canceled": false, "path": path.to_string_lossy() }))
}

fn diagnostics_unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[tauri::command]
pub(crate) fn get_svn_connections() -> Result<serde_json::Value, String> {
    let items = crate::svn::service::list_connections().map_err(|error| error.to_string())?;
    Ok(json!({ "items": items }))
}

#[tauri::command]
pub(crate) fn save_svn_connection(
    request: crate::svn::types::SaveSvnConnectionRequest,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let connection =
        crate::svn::service::save_connection(request).map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!("已保存公司 SVN 凭据: {}", connection.username),
    );
    Ok(json!({ "connection": connection }))
}

#[tauri::command]
pub(crate) fn remove_svn_connection(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let removed = crate::svn::service::remove_connection().map_err(|error| error.to_string())?;
    if removed {
        state
            .approval_manager
            .add_log("info", "已删除公司 SVN 凭据");
    }
    Ok(json!({ "removed": removed }))
}

#[tauri::command]
pub(crate) fn test_svn_connection(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let result = crate::svn::service::test_connection().map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", "公司 SVN 连接验证成功");
    Ok(result)
}

#[tauri::command]
pub(crate) async fn get_plugin_registry(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let control_plane_enabled = state.options.mode().control_plane_enabled();
    tauri::async_runtime::spawn_blocking(move || {
        registry_json_for_control_plane(control_plane_enabled).map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_extension_sources(
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::settings().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn add_extension_source(
    name: String,
    repository: String,
    reference: String,
    catalog_path: Option<String>,
    verification: Option<String>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::app::extension_source::add_github_source(
            &name,
            &repository,
            &reference,
            catalog_path.as_deref(),
            verification.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        Ok(settings)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn add_local_extension_source(
    name: String,
    root: String,
    catalog_path: Option<String>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings =
            crate::app::extension_source::add_local_source(&name, &root, catalog_path.as_deref())
                .map_err(|error| error.to_string())?;
        let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        Ok(settings)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn pick_local_extension_source_dir() -> Result<Option<String>, String> {
    let path = rfd::FileDialog::new()
        .set_title("选择本地扩展源聚合目录")
        .pick_folder()
        .ok_or("已取消选择本地扩展源目录")?;
    let root = path.canonicalize().map_err(|error| error.to_string())?;
    Ok(Some(crate::extension_workspace::display_path(&root)))
}

#[tauri::command]
pub(crate) async fn update_extension_source(
    source_id: String,
    enabled: bool,
    auto_update: bool,
    verification: Option<String>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::app::extension_source::update_source(
            &source_id,
            enabled,
            auto_update,
            verification.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        Ok(settings)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn remove_extension_source(
    source_id: String,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::app::extension_source::remove_source(&source_id)
            .map_err(|error| error.to_string())?;
        let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        Ok(settings)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_extension_source_snapshot(
) -> Result<crate::app::extension_source::ExtensionSourceSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| {
        crate::app::extension_source::refresh_snapshot().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn set_extension_unit_acquisition(
    unit_key: String,
    acquisition: String,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let acquisition = match acquisition.as_str() {
            "remote" => crate::app::extension_source::ExtensionSourceAcquisition::Remote,
            "local" => crate::app::extension_source::ExtensionSourceAcquisition::Local,
            other => return Err(format!("取用模式无效: {other}")),
        };
        crate::app::extension_source::set_unit_acquisition(&unit_key, acquisition)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn install_extension_unit(
    state: State<'_, AgentState>,
    unit_key: String,
    source_id: String,
) -> Result<crate::app::extension_source::ExtensionUnitInstallReport, String> {
    let report = tauri::async_runtime::spawn_blocking(move || {
        crate::app::extension_source::install_unit_bound(&unit_key, Some(&source_id))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    let capability_facts = skill_capability_facts(&state)?;
    for skill in &report.skills {
        if let Ok(Some(record)) = crate::skill::store::SkillStore::new().get_record(&skill.asset_id)
        {
            let _ =
                crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts);
        }
    }
    let _ = crate::app::extension_source::reconcile_dsh_presets_now();
    Ok(report)
}

#[tauri::command]
pub(crate) fn get_extension_provenance(
) -> Result<Vec<crate::app::extension_source::ExtensionProvenance>, String> {
    crate::app::extension_source::list_provenance().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_lock() -> Result<crate::app::extension_lock::ExtensionLockFile, String>
{
    crate::app::extension_lock::load().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_local_plugin(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入本地 HiMind 插件")
        .add_filter("HiMind 插件", &["hmpkg"])
        .pick_file()
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("选择 HiMind 插件目录")
                .pick_folder()
        })
    else {
        return Err("已取消导入插件".to_string());
    };
    crate::app::plugin_manager::install_local_package_from_source(&path, ADHOC_SOURCE)
        .map_err(|error| error.to_string())?;
    registry_json_for_control_plane(state.options.mode().control_plane_enabled())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_github_plugin(
    repository: String,
    reference: String,
    subpath: Option<String>,
) -> Result<serde_json::Value, String> {
    crate::app::github_source::import_plugin(
        &repository,
        &reference,
        subpath.as_deref().unwrap_or(""),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_github_plugin_url(source_url: String) -> Result<serde_json::Value, String> {
    crate::app::github_source::import_plugin(&source_url, "", "").map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn get_extension_desired_state(
    state: State<'_, AgentState>,
) -> Result<ExtensionDesiredState, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        require_dashboard(&state)?;
        let agent_id = local_worker_snapshot(&state.worker_status)
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        let credential = state.options.agent_credential();
        if agent_id.is_empty() || credential.trim().is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::distribution::extension_desired_state(
            &client,
            &state.dashboard_base,
            &agent_id,
            &credential,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_agent_task_history(
    state: State<'_, AgentState>,
    limit: Option<usize>,
) -> Result<Vec<AgentTaskHistoryItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        require_dashboard(&state)?;
        let agent_id = local_worker_snapshot(&state.worker_status)
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        let credential = state.options.agent_credential();
        if agent_id.is_empty() || credential.trim().is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::client::list_task_history(
            &client,
            &state.dashboard_base,
            &agent_id,
            &credential,
            limit.unwrap_or(50).clamp(1, 100),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_agent_capabilities(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::capability::types::CapabilityDescriptor>, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        gateway
            .list_capabilities(&InvocationContext::tauri())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn list_ai_services(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let custom = crate::store::ai_services::public_snapshot().map_err(|e| e.to_string())?;
        let clients = crate::app::ai_provider_import::status(&options);
        let managed = if options.mode().dashboard_enabled() {
            // This summary may require OAuth refresh and network I/O. It runs
            // off the desktop event loop and degrades to an unavailable state.
            let user_id = crate::app::identity::identity_status(&options).user_id;
            crate::api::ai::managed_ai_service_summary(&options, &user_id)
        } else {
            serde_json::json!({ "available": false, "reason": "independent" })
        };
        Ok(json!({
            "custom": custom,
            "managed": managed,
            "clients": serde_json::to_value(clients).map_err(|e| e.to_string())?,
        }))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn list_acp_runtime_profiles() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(json!({
            "profiles": crate::store::acp_profiles::list().map_err(|error| error.to_string())?,
            "providers": crate::runtime::probe_installations(),
        }))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_projection_sync_status(
    state: State<'_, AgentState>,
) -> Result<crate::agent_core_projection::ProjectionSyncStatus, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::agent_core_projection::projection_sync_status(&options)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn save_acp_runtime_profile(
    state: State<'_, AgentState>,
    provider_id: String,
    display_name: String,
    executable: String,
    args: Vec<String>,
    version: String,
    permission_policy: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    let profile =
        crate::store::acp_profiles::upsert(crate::store::acp_profiles::AcpRuntimeProfileRecord {
            provider_id,
            display_name,
            executable,
            args,
            version,
            permission_policy,
            enabled,
        })
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!("已保存 ACP Runtime Profile: {}", profile.provider_id),
    );
    serde_json::to_value(profile).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_acp_runtime_profile_enabled(
    state: State<'_, AgentState>,
    provider_id: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    let profile = crate::store::acp_profiles::set_enabled(&provider_id, enabled)
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!(
            "{} ACP Runtime Profile: {}",
            if enabled { "已启用" } else { "已停用" },
            profile.provider_id
        ),
    );
    serde_json::to_value(profile).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn remove_acp_runtime_profile(
    state: State<'_, AgentState>,
    provider_id: String,
) -> Result<serde_json::Value, String> {
    let removed =
        crate::store::acp_profiles::remove(&provider_id).map_err(|error| error.to_string())?;
    if removed {
        state.approval_manager.add_log(
            "info",
            &format!(
                "已删除 ACP Runtime Profile: {}",
                crate::store::acp_profiles::normalize_provider_id(&provider_id)
            ),
        );
    }
    Ok(json!({
        "provider_id": crate::store::acp_profiles::normalize_provider_id(&provider_id),
        "removed": removed,
    }))
}

#[tauri::command]
pub(crate) fn save_ai_service(
    state: State<'_, AgentState>,
    id: String,
    display_name: String,
    base_url: String,
    protocol: String,
    model: String,
    models: Vec<String>,
    api_key: String,
) -> Result<serde_json::Value, String> {
    let protocol = match protocol.as_str() {
        "openai-chat" => crate::store::ai_services::AIServiceProtocol::OpenaiChat,
        "openai-responses" => crate::store::ai_services::AIServiceProtocol::OpenaiResponses,
        _ => return Err("protocol 只支持 openai-chat 或 openai-responses".to_string()),
    };
    let service =
        crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
            id,
            display_name,
            base_url,
            protocol,
            model,
            models,
            api_key,
        })
        .map_err(|e| e.to_string())?;
    if crate::store::ai_services::active_id()
        .map_err(|e| e.to_string())?
        .as_deref()
        == Some(service.id.as_str())
    {
        crate::app::ui::stop_builtin_ai_process();
    }
    state.approval_manager.add_log(
        "info",
        &format!("已保存自定义 AI 服务: {}", service.display_name),
    );
    Ok(service.public_json())
}

#[tauri::command]
pub(crate) fn remove_ai_service(state: State<'_, AgentState>, id: String) -> Result<bool, String> {
    crate::app::ai_provider_import::ensure_service_not_in_use(&state.options, &id)
        .map_err(|error| error.to_string())?;
    let removed = crate::store::ai_services::remove(&id).map_err(|e| e.to_string())?;
    if removed {
        crate::app::ui::stop_builtin_ai_process();
        state
            .approval_manager
            .add_log("info", &format!("已删除自定义 AI 服务: {id}"));
    }
    Ok(removed)
}

#[tauri::command]
pub(crate) fn set_active_ai_service(
    state: State<'_, AgentState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let selected = crate::store::ai_services::set_active(&id).map_err(|e| e.to_string())?;
    crate::app::ui::stop_builtin_ai_process();
    if let Some(service) = selected.as_ref() {
        state.approval_manager.add_log(
            "info",
            &format!(
                "已将本机 AI 服务设为 HiMind AI 默认服务: {}",
                service.display_name
            ),
        );
    } else {
        state
            .approval_manager
            .add_log("info", "已取消 HiMind AI 本机默认服务，恢复 DSH 原生设置");
    }
    Ok(json!({
        "active_service_id": selected.map(|service| service.id).unwrap_or_default(),
    }))
}

#[tauri::command]
pub(crate) fn fetch_ai_service_models(
    base_url: String,
    api_key: String,
) -> Result<serde_json::Value, String> {
    let models =
        crate::store::ai_services::fetch_models(&base_url, &api_key).map_err(|e| e.to_string())?;
    Ok(json!({ "models": models }))
}

#[tauri::command]
pub(crate) fn fetch_saved_ai_service_models(
    id: String,
    base_url: String,
) -> Result<serde_json::Value, String> {
    let (_, api_key) = crate::store::ai_services::load_secret(&id).map_err(|e| e.to_string())?;
    let models =
        crate::store::ai_services::fetch_models(&base_url, &api_key).map_err(|e| e.to_string())?;
    Ok(json!({ "models": models }))
}

#[tauri::command]
pub(crate) fn import_ai_client(
    state: State<'_, AgentState>,
    target: String,
    service: Option<String>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    let request = serde_json::json!({
        "target": target,
        "service": service.unwrap_or_else(|| "managed".to_string()),
    });
    let result = gateway
        .invoke(&InvocationContext::tauri(), "ai.client.import", request)
        .map_err(|e| e.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已注册 AI 客户端: {target}"));
    Ok(result)
}

#[tauri::command]
pub(crate) fn remove_ai_client(
    state: State<'_, AgentState>,
    target: String,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    let result = gateway
        .invoke(
            &InvocationContext::tauri(),
            "ai.client.remove",
            serde_json::json!({ "target": target }),
        )
        .map_err(|e| e.to_string())?;
    state
        .approval_manager
        .add_log("info", "已取消 AI 客户端注册");
    Ok(result)
}

fn skill_capability_facts(
    state: &AgentState,
) -> Result<Vec<crate::skill::resolver::CapabilityFact>, String> {
    state
        .capability_gateway
        .list_capabilities(&InvocationContext::tauri())
        .map(|items| {
            items
                .into_iter()
                .map(|descriptor| crate::skill::resolver::CapabilityFact {
                    id: descriptor.id,
                    version: descriptor.version,
                    source: descriptor.source,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn get_skill_catalog(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let descriptors = gateway
            .list_capabilities(&InvocationContext::tauri())
            .map_err(|e| e.to_string())?;
        let capability_facts = descriptors
            .into_iter()
            .map(|descriptor| crate::skill::resolver::CapabilityFact {
                id: descriptor.id,
                version: descriptor.version,
                source: descriptor.source,
            })
            .collect::<Vec<_>>();
        catalog_json(VERSION, "codex", &capability_facts).map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn import_local_skill(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入本地 HiMind Skill")
        .add_filter("HiMind Skill", &["hmskill", "zip"])
        .pick_file()
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("选择 HiMind Skill 目录")
                .pick_folder()
        })
    else {
        return Err("已取消导入 Skill".to_string());
    };
    let record = crate::app::skill_manager::install_local_package_from_source(&path, ADHOC_SOURCE)
        .map_err(|error| error.to_string())?;
    imported_skill_result(&state, record)
}

#[tauri::command]
pub(crate) fn get_skill_workspace() -> crate::skill::target::SkillWorkspaceStatus {
    crate::skill::target::workspace_status()
}

#[tauri::command]
pub(crate) fn set_skill_workspace(
    path: Option<String>,
) -> Result<crate::skill::target::SkillWorkspaceStatus, String> {
    crate::skill::target::set_workspace(path.as_deref()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_skill_workspace_enabled(
    skill_id: String,
    enabled: bool,
    state: State<'_, AgentState>,
) -> Result<bool, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let value = crate::skill::set_workspace_skill_enabled_json(
        &skill_id,
        enabled,
        VERSION,
        &capability_facts,
    )
    .map_err(|error| error.to_string())?;
    Ok(value
        .get("updated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

#[tauri::command]
pub(crate) fn pick_skill_workspace() -> Result<crate::skill::target::SkillWorkspaceStatus, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择项目 Skill 工作区")
        .pick_folder()
    else {
        return Err("已取消选择项目 Skill 工作区".to_string());
    };
    crate::skill::target::set_workspace(Some(&path.to_string_lossy()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_github_skill(
    repository: String,
    reference: String,
    subpath: Option<String>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let value = crate::app::github_source::import_skill(
        &repository,
        &reference,
        subpath.as_deref().unwrap_or(""),
    )
    .map_err(|error| error.to_string())?;
    let record = serde_json::from_value(value).map_err(|error| error.to_string())?;
    imported_skill_result(&state, record)
}

#[tauri::command]
pub(crate) fn import_github_skill_url(
    source_url: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let value = crate::app::github_source::import_skill(&source_url, "", "")
        .map_err(|error| error.to_string())?;
    let record = serde_json::from_value(value).map_err(|error| error.to_string())?;
    imported_skill_result(&state, record)
}

fn imported_skill_result(
    state: &AgentState,
    record: crate::skill::types::SkillRecord,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(state)?;
    let clients =
        crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts)
            .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "record": record,
        "clients": clients,
        "deployment": "current-target",
    }))
}

#[tauri::command]
pub(crate) async fn get_organization_skill_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || merged_skill_catalog(&state))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn query_organization_skill_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::SkillCatalogPage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let items = filter_skill_catalog(merged_skill_catalog(&state)?, &q, &category);
        Ok(catalog_page(items, page, page_size))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn list_skill_drafts() -> Result<Vec<crate::skill::authoring::AuthoringDraft>, String> {
    crate::skill::authoring::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_extension_projects(
) -> Result<Vec<crate::extension_projects::ExtensionProject>, String> {
    crate::extension_projects::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_workspace() -> crate::extension_workspace::ExtensionWorkspaceSettings {
    crate::extension_workspace::settings()
}

#[tauri::command]
pub(crate) fn set_extension_workspace(
    root: String,
) -> Result<crate::extension_workspace::ExtensionWorkspaceSettings, String> {
    crate::extension_workspace::select(std::path::Path::new(root.trim()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn open_extension_projects(
) -> Result<Vec<crate::extension_projects::ExtensionProject>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind 项目或扩展聚合仓库")
        .pick_folder()
    else {
        return Err("已取消打开扩展项目".to_string());
    };
    if path.join("extensions.json").is_file() {
        crate::extension_workspace::select(&path).map_err(|error| error.to_string())?;
        return crate::extension_projects::list().map_err(|error| error.to_string());
    }
    crate::extension_projects::register(&path)
        .map(|project| vec![project])
        .map_err(|error| {
            format!("请选择包含 plugin.json、skill.json 或 extensions.json 的目录：{error}")
        })
}

#[tauri::command]
pub(crate) fn associate_extension_project(
    input: crate::extension_projects::AssociateExtensionProjectInput,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择协作项目的本地目录")
        .pick_folder()
    else {
        return Err("已取消关联扩展项目".to_string());
    };
    crate::extension_projects::associate(&path, input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn create_extension_project(
    input: crate::extension_projects::CreateExtensionProjectInput,
    state: State<'_, AgentState>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let identity = crate::app::identity::authoring_identity(&state.options);
    let Some(parent) = rfd::FileDialog::new()
        .set_title("选择项目保存位置")
        .pick_folder()
    else {
        return Err("已取消新建扩展项目".to_string());
    };
    crate::extension_projects::create(&parent, input, &identity.user_name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn build_extension_project(
    project_id: String,
) -> Result<crate::extension_projects::ExtensionCandidate, String> {
    crate::extension_projects::build(&project_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn prepare_extension_authoring() -> Result<(), String> {
    crate::app::extension_source::ensure_authoring_feature().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn remove_extension_project(project_id: String) -> Result<(), String> {
    crate::extension_projects::remove(&project_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_extension_collaboration_projects(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::AgentExtensionProject>, String> {
    require_dashboard(&state)?;
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::PROFILE_SCOPE)?;
    crate::api::distribution::extension_projects(&client, &state.dashboard_base, &agent_id, &token)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn update_extension_project_source(
    project_id: String,
    input: crate::extension_projects::ExtensionProjectSourceInput,
    sync_remote: Option<bool>,
    state: State<'_, AgentState>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    if sync_remote.unwrap_or(true) {
        require_dashboard(&state)?;
    }
    let project = crate::extension_projects::get(&project_id).map_err(|error| error.to_string())?;
    if sync_remote.unwrap_or(true) {
        let (agent_id, token, client) =
            dashboard_agent_user_client(&state, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
        crate::api::distribution::upsert_extension_source(
            &client,
            &state.dashboard_base,
            &agent_id,
            &token,
            &project,
            &input,
        )
        .map_err(|error| error.to_string())?;
    }
    crate::extension_projects::update_source(&project_id, input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_collaboration(
    product_key: String,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::ExtensionCollaboration, String> {
    require_dashboard(&state)?;
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::PROFILE_SCOPE)?;
    crate::api::distribution::extension_collaboration(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &product_key,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_extension_collaborator_options(
    product_key: String,
    query: Option<String>,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::ExtensionCollaboratorOption>, String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
    crate::api::distribution::extension_collaborator_options(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &product_key,
        query.as_deref().unwrap_or_default(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn invite_extension_collaborator(
    product_key: String,
    user_id: String,
    role: String,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::ExtensionCollaborationMember, String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
    crate::api::distribution::invite_extension_collaborator(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &product_key,
        &user_id,
        &role,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn update_extension_collaborator(
    product_key: String,
    user_id: String,
    role: String,
    state: State<'_, AgentState>,
) -> Result<(), String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
    crate::api::distribution::update_extension_collaborator(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &product_key,
        &user_id,
        &role,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn delete_extension_collaborator(
    product_key: String,
    user_id: String,
    state: State<'_, AgentState>,
) -> Result<(), String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
    crate::api::distribution::delete_extension_collaborator(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &product_key,
        &user_id,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_extension_collaboration_invitations(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::ExtensionCollaborationInvitation>, String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::PROFILE_SCOPE)?;
    crate::api::distribution::extension_collaboration_invitations(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn respond_extension_collaboration_invitation(
    invitation_id: String,
    action: String,
    state: State<'_, AgentState>,
) -> Result<(), String> {
    let (agent_id, token, client) =
        dashboard_agent_user_client(&state, crate::api::oauth::PROFILE_SCOPE)?;
    crate::api::distribution::respond_extension_collaboration_invitation(
        &client,
        &state.dashboard_base,
        &agent_id,
        &token,
        &invitation_id,
        &action,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_skill_candidate(
    revision_of_version: Option<String>,
    parent_submission_id: Option<String>,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind Skill 候选包")
        .add_filter("HiMind Skill 包", &["hmskill", "zip"])
        .pick_file()
    else {
        return Err("已取消选择 Skill 候选包".to_string());
    };
    crate::skill::authoring::import_package(crate::skill::authoring::SkillPackageInput {
        package_path: path,
        revision_of_version,
        parent_submission_id,
    })
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_plugin_drafts() -> Result<Vec<crate::plugin_authoring::PluginDraft>, String> {
    crate::plugin_authoring::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_plugin_candidate(
    revision_of_version: Option<String>,
    parent_submission_id: Option<String>,
) -> Result<crate::plugin_authoring::PluginDraft, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind 插件候选包")
        .add_filter("HiMind 插件包", &["hmpkg"])
        .pick_file()
    else {
        return Err("已取消选择插件候选包".to_string());
    };
    crate::plugin_authoring::save(crate::plugin_authoring::PluginDraftInput {
        package_path: path,
        revision_of_version,
        parent_submission_id,
    })
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn create_plugin_revision(
    plugin_id: String,
    version: String,
) -> Result<crate::plugin_authoring::PluginDraft, String> {
    crate::plugin_authoring::create_revision(&plugin_id, &version)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn test_plugin_draft(
    plugin_id: String,
    version: String,
) -> Result<crate::plugin_authoring::PluginDraft, String> {
    crate::plugin_authoring::test(&plugin_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn confirm_plugin_draft(
    plugin_id: String,
    version: String,
) -> Result<crate::plugin_authoring::PluginDraft, String> {
    crate::plugin_authoring::confirm(&plugin_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_workflow_drafts() -> Result<Vec<crate::workflow::WorkflowDraft>, String> {
    crate::workflow::list_authoring_drafts().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn test_workflow_draft(
    workflow_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::workflow::WorkflowDraft, String> {
    let capabilities = state
        .capability_gateway
        .list_capabilities(&InvocationContext::tauri())
        .map_err(|error| error.to_string())?;
    crate::workflow::test_authoring_candidate_with_capabilities(
        &workflow_id,
        &version,
        &capabilities,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn confirm_workflow_draft(
    workflow_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::workflow::WorkflowDraft, String> {
    let capabilities = state
        .capability_gateway
        .list_capabilities(&InvocationContext::tauri())
        .map_err(|error| error.to_string())?;
    crate::workflow::confirm_authoring_candidate_with_capabilities(
        &workflow_id,
        &version,
        &capabilities,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn submit_workflow_draft(
    workflow_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::workflow::WorkflowDraft, String> {
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if agent_id.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    crate::workflow::submit_authoring_candidate(&state.options, &agent_id, &workflow_id, &version)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_workflow_submissions(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if agent_id.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let access = crate::api::oauth::platform_access_token(
        &state.options,
        crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
    )
    .map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::workflow_submissions(
        &client,
        &state.dashboard_base,
        &agent_id,
        &access.token,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_plugin_submissions(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginSubmissionStatus>, String> {
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if agent_id.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let access =
        crate::api::oauth::platform_access_token(&state.options, crate::api::oauth::PROFILE_SCOPE)
            .map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::plugin_submissions(
        &client,
        &state.dashboard_base,
        &agent_id,
        &access.token,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn submit_plugin_draft(
    plugin_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::plugin_authoring::PluginDraft, String> {
    require_dashboard(&state)?;
    let draft =
        crate::plugin_authoring::read(&plugin_id, &version).map_err(|error| error.to_string())?;
    if !confirm_authoring_submission(
        "插件",
        &draft.manifest.name,
        &version,
        &draft.candidate_sha256,
    ) {
        return Err("用户取消了插件提审".to_string());
    }
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::plugin_authoring::submit(&state.options, &agent_id, &plugin_id, &version)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_skill_submissions(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillSubmissionStatus>, String> {
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if agent_id.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let access =
        crate::api::oauth::platform_access_token(&state.options, crate::api::oauth::PROFILE_SCOPE)
            .map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::skill_submissions(
        &client,
        &state.dashboard_base,
        &agent_id,
        &access.token,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_skill_draft(
    mut input: crate::skill::authoring::SkillDraftInput,
    state: State<'_, AgentState>,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    require_dashboard(&state)?;
    let identity = crate::app::identity::identity_status(&state.options);
    if !identity.authorized || identity.user_name.trim().is_empty() {
        return Err("请先授权 HiMind 工作台账号，再保存 Skill 候选".to_string());
    }
    input.author = identity.user_name;
    crate::skill::authoring::save(input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn create_skill_revision(
    skill_id: String,
    version: String,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    crate::skill::authoring::create_revision(&skill_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn test_skill_draft(
    skill_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::skill::authoring::AuthoringTestResult, String> {
    let capability_facts = skill_capability_facts(&state)?;
    crate::skill::authoring::test(&skill_id, &version, &capability_facts)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn confirm_skill_draft(
    skill_id: String,
    version: String,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    crate::skill::authoring::confirm(&skill_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn submit_skill_draft(
    skill_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    require_dashboard(&state)?;
    let draft =
        crate::skill::authoring::read(&skill_id, &version).map_err(|error| error.to_string())?;
    if !confirm_authoring_submission(
        "Skill",
        &draft.manifest.name,
        &version,
        &draft.candidate_sha256,
    ) {
        return Err("用户取消了 Skill 提审".to_string());
    }
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::skill::authoring::submit(&state.options, &agent_id, &skill_id, &version)
        .map_err(|error| error.to_string())
}

fn confirm_authoring_submission(kind: &str, name: &str, version: &str, sha256: &str) -> bool {
    matches!(
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title(format!("确认提交{kind}审核"))
            .set_description(format!(
                "名称：{name}\n版本：{version}\nSHA-256：{sha256}\n\n提交后候选制品不可变，是否继续？"
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show(),
        rfd::MessageDialogResult::Yes
    )
}

#[tauri::command]
pub(crate) fn install_organization_skill(
    skill_id: String,
    version: Option<String>,
    optional_plugin_ids: Option<Vec<String>>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    let use_public_source = public_source_id.is_some()
        || (source.is_none()
            && merged_skill_catalog(&state)?.into_iter().any(|item| {
                item.skill_id == skill_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }));
    if use_public_source {
        let (catalog_item, record) = crate::app::extension_source::install_skill_bound(
            &skill_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let capability_facts = skill_capability_facts(&state)?;
        let rendered =
            crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts)
                .map_err(|error| error.to_string())?;
        let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        return Ok(serde_json::json!({
            "catalog_item": catalog_item,
            "record": record,
            "codex": rendered.get("codex"),
            "github_copilot": rendered.get("github-copilot"),
            "workbuddy": rendered.get("workbuddy"),
            "clients": rendered,
        }));
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let (catalog_item, record) = crate::app::skill_manager::install_with_dependencies_bound(
        &state.options,
        &agent_id,
        &skill_id,
        version.as_deref(),
        optional_plugin_ids.as_deref().unwrap_or_default(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let capability_facts = skill_capability_facts(&state)?;
    let rendered =
        crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts)
            .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "catalog_item": catalog_item,
        "record": record,
        "codex": rendered.get("codex"),
        "github_copilot": rendered.get("github-copilot"),
        "workbuddy": rendered.get("workbuddy"),
        "clients": rendered,
    }))
}

#[tauri::command]
pub(crate) fn plan_organization_skill_install(
    skill_id: String,
    version: Option<String>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    state: State<'_, AgentState>,
) -> Result<crate::app::skill_manager::SkillInstallPlan, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_skill_catalog(&state)?.into_iter().any(|item| {
                item.skill_id == skill_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        return crate::app::extension_source::plan_skill_bound(
            &skill_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::app::skill_manager::plan_install_bound(
        &state.options,
        &agent_id,
        &skill_id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_skill_versions(
    skill_id: String,
    source: Option<String>,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    if public_extension_source_id(source.as_deref()).is_some()
        || (source.is_none()
            && merged_skill_catalog(&state)?.into_iter().any(|item| {
                item.skill_id == skill_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        let mut versions = crate::app::extension_source::skill_versions(&skill_id)
            .map_err(|error| error.to_string())?;
        if let Some(source) = source.as_deref() {
            versions.retain(|item| item.source == source);
        }
        return Ok(versions);
    }
    require_dashboard(&state)?;
    let snapshot = local_worker_snapshot(&state.worker_status);
    let agent_id = snapshot
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let credential = state.options.agent_credential();
    if agent_id.is_empty() || credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::skill_versions(
        &client,
        &state.dashboard_base,
        agent_id,
        &credential,
        &skill_id,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_codex_skill_status(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let clients = crate::skill::client_status_json(VERSION, &capability_facts)
        .map_err(|error| error.to_string())?;
    codex_compatible_client_result(clients)
}

#[tauri::command]
pub(crate) fn get_skill_sync_settings() -> Result<crate::skill::store::SkillSyncSettings, String> {
    crate::skill::store::SkillStore::new()
        .sync_settings()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_skill_sync_mode(
    mode: String,
) -> Result<crate::skill::store::SkillSyncSettings, String> {
    crate::skill::store::SkillStore::new()
        .set_sync_mode(&mode)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn sync_codex_skills(state: State<'_, AgentState>) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let clients = crate::skill::client_sync_json(VERSION, &capability_facts)
        .map_err(|error| error.to_string())?;
    codex_compatible_client_result(clients)
}

#[tauri::command]
pub(crate) fn sync_codex_skill(
    skill_id: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let store = crate::skill::store::SkillStore::new();
    store
        .bootstrap_builtin_skills()
        .map_err(|error| error.to_string())?;
    let record = store
        .get_record(&skill_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    crate::skill::ensure_workspace_record_is_current(&record).map_err(|error| error.to_string())?;
    let clients =
        crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts)
            .map_err(|error| error.to_string())?;
    let mut primary = primary_skill_client(&clients)
        .ok_or_else(|| "该 Skill 未声明任何 Agent 支持的 AI 客户端".to_string())?;
    if let Some(object) = primary.as_object_mut() {
        object.insert("clients".to_string(), serde_json::json!(clients));
    }
    Ok(primary)
}

/// Explicitly advance the selected project to the Store's current Skill
/// version.  Ordinary sync/repair operations remain pinned to the workspace
/// lock; this command is the only per-Skill action that changes that lock.
#[tauri::command]
pub(crate) fn update_skill_workspace(
    skill_id: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let value = crate::skill::update_workspace_skill_json(&skill_id, VERSION, &capability_facts)
        .map_err(|error| error.to_string())?;
    let clients: std::collections::BTreeMap<String, serde_json::Value> = value
        .get("clients")
        .and_then(serde_json::Value::as_object)
        .map(|items| {
            items
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut primary = primary_skill_client(&clients)
        .ok_or_else(|| "该 Skill 未声明任何 Agent 支持的 AI 客户端".to_string())?;
    if let Some(object) = primary.as_object_mut() {
        object.insert("clients".to_string(), serde_json::json!(clients));
        object.insert("lock_updated".to_string(), serde_json::Value::Bool(true));
        for key in ["previous_version", "workspace_root", "lock_path"] {
            if let Some(entry) = value.get(key) {
                object.insert(key.to_string(), entry.clone());
            }
        }
    }
    Ok(primary)
}

#[tauri::command]
pub(crate) fn sync_skill_client(
    skill_id: String,
    client_id: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    crate::skill::sync_skill_client_json(&skill_id, &client_id, VERSION, &capability_facts)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn repair_codex_skill(
    skill_id: String,
    preserve_modified: Option<bool>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    let store = crate::skill::store::SkillStore::new();
    store
        .bootstrap_builtin_skills()
        .map_err(|error| error.to_string())?;
    let record = store
        .get_record(&skill_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let clients = crate::skill::repair_record_for_supported_clients(
        &record,
        preserve_modified.unwrap_or(true),
        VERSION,
        &capability_facts,
    )
    .map_err(|error| error.to_string())?;
    let mut primary = primary_skill_client(&clients)
        .ok_or_else(|| "该 Skill 未声明任何 Agent 支持的 AI 客户端".to_string())?;
    let backup_root = clients.values().find_map(|client| {
        client
            .get("backup_root")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    });
    if let Some(object) = primary.as_object_mut() {
        object.insert("clients".to_string(), serde_json::json!(clients));
        object.insert("backup_root".to_string(), serde_json::json!(backup_root));
    }
    Ok(primary)
}

#[tauri::command]
pub(crate) fn uninstall_codex_skill(skill_id: String) -> Result<serde_json::Value, String> {
    let clients = crate::skill::uninstall_supported_clients_json(&skill_id)
        .map_err(|error| error.to_string())?;
    if let Some(workspace_root) =
        crate::skill::target::resolve_workspace_root(None).map_err(|error| error.to_string())?
    {
        // A full uninstall also forgets the project assignment, including a
        // disabled entry the user left behind.
        if let Ok(target) = crate::skill::target::SkillTarget::workspace(
            &workspace_root,
            ".agents/skills",
            "workspace",
        ) {
            let _ = crate::skill::target::remove_workspace_skill_entry(&target, &skill_id);
        }
        let target_root = clients
            .get("clients")
            .and_then(serde_json::Value::as_object)
            .and_then(|items| items.get("codex").or_else(|| items.values().next()))
            .and_then(|client| client.get("target_root"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| workspace_root.to_string_lossy().to_string());
        let removed = clients
            .get("clients")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|items| {
                items.values().any(|client| {
                    client
                        .get("removed")
                        .and_then(|value| value.get("removed"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
            });
        return Ok(serde_json::json!({
            "client_id": "agent",
            "target_root": target_root,
            "target_source": "workspace",
            "target_configured": true,
            "target_kind": crate::skill::target::TARGET_KIND_WORKSPACE,
            "workspace_root": workspace_root.to_string_lossy().to_string(),
            "workspace_id": crate::skill::target::workspace_id(&workspace_root),
            "package_removed": false,
            "removed": {
                "skill_id": skill_id,
                "removed": removed,
            },
            "clients": clients.get("clients").cloned().unwrap_or_default(),
        }));
    }
    let store = crate::skill::store::SkillStore::new();
    let global_target = crate::skill::target::SkillTarget::global(
        store.root().to_path_buf(),
        "agent-skill-store",
        true,
    );
    let remaining_deployments =
        crate::skill::target::other_deployments_for_skill(&skill_id, &global_target)
            .map_err(|error| error.to_string())?;
    let removed = if remaining_deployments.is_empty() {
        let removed = store
            .remove_installed_skill(&skill_id)
            .map_err(|error| error.to_string())?;
        if removed {
            crate::app::plugin_manager::remove_owner_references(&format!("skill:{skill_id}"));
        }
        removed
    } else {
        false
    };
    Ok(serde_json::json!({
        "client_id": "agent",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "target_kind": crate::skill::target::TARGET_KIND_GLOBAL,
        "workspace_root": serde_json::Value::Null,
        "workspace_id": serde_json::Value::Null,
        "package_removed": removed,
        "package_retained": !remaining_deployments.is_empty(),
        "remaining_deployments": remaining_deployments,
        "removed": {
            "skill_id": skill_id,
            "removed": removed,
        },
        "clients": clients.get("clients").cloned().unwrap_or_default(),
    }))
}

#[tauri::command]
pub(crate) fn unregister_skill_client(
    skill_id: String,
    client_id: String,
) -> Result<serde_json::Value, String> {
    crate::skill::unregister_skill_client_json(&skill_id, &client_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn unregister_skill_clients(skill_id: String) -> Result<serde_json::Value, String> {
    crate::skill::unregister_skill_clients_json(&skill_id).map_err(|error| error.to_string())
}

fn codex_compatible_client_result(clients: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut codex = clients
        .get("codex")
        .cloned()
        .ok_or_else(|| "Agent 未返回 Codex 客户端状态".to_string())?;
    let mut project_skills = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    if let Some(client_map) = clients.as_object() {
        for client in client_map.values() {
            let Some(items) = client
                .get("project_skills")
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for item in items {
                let path = item
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if seen_paths.insert(path.to_string()) {
                    project_skills.push(item.clone());
                }
            }
        }
    }
    project_skills.sort_by(|left, right| {
        left.get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .cmp(
                right
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            )
    });
    if let Some(object) = codex.as_object_mut() {
        object.insert(
            "project_skills".to_string(),
            serde_json::Value::Array(project_skills),
        );
        object.insert("clients".to_string(), clients);
    }
    Ok(codex)
}

fn primary_skill_client(
    clients: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    ["codex", "himind-ai"]
        .into_iter()
        .find_map(|client_id| clients.get(client_id).cloned())
        .or_else(|| clients.values().next().cloned())
}

fn public_extension_source_id(source: Option<&str>) -> Option<&str> {
    source.and_then(|value| {
        value
            .strip_prefix("local:")
            .or_else(|| value.strip_prefix("github:"))
    })
}

fn merged_plugin_catalog(
    state: &AgentState,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    let source_snapshot =
        crate::app::extension_source::snapshot().map_err(|error| error.to_string())?;
    let mut items = source_snapshot.plugins;
    if state.options.mode().dashboard_enabled() {
        let worker = local_worker_snapshot(&state.worker_status);
        let agent_id = worker
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let credential = state.options.agent_credential();
        if !agent_id.is_empty() && !credential.is_empty() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build();
            let dashboard = match client {
                Ok(client) => crate::api::distribution::plugin_catalog(
                    &client,
                    &state.dashboard_base,
                    agent_id,
                    &credential,
                )
                .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            match dashboard {
                Ok(catalog) => {
                    items.extend(catalog);
                }
                Err(error) if items.is_empty() => return Err(error.to_string()),
                Err(_) => {}
            }
        } else if items.is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
    }
    let mut seen = HashSet::new();
    items.retain(|item| {
        seen.insert((
            item.plugin_id.clone(),
            item.source.clone(),
            item.version.clone(),
            item.artifact_id.clone(),
            item.sha256.clone(),
        ))
    });
    let mut result = items;
    result.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| left.source.cmp(&right.source))
    });
    // 发布元数据可能缺少分类（旧制品或未声明 categories 的清单），按
    // Capability 命名空间兜底，与工作流保持同一套规则。
    for item in result.iter_mut() {
        crate::extension_category::fill_missing_categories(
            &item.capability_ids,
            &mut item.categories,
        );
    }
    Ok(result)
}

fn merged_skill_catalog(
    state: &AgentState,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    let source_snapshot =
        crate::app::extension_source::snapshot().map_err(|error| error.to_string())?;
    let mut items = source_snapshot.skills;
    if state.options.mode().dashboard_enabled() {
        let worker = local_worker_snapshot(&state.worker_status);
        let agent_id = worker
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let credential = state.options.agent_credential();
        if !agent_id.is_empty() && !credential.is_empty() {
            match crate::app::skill_manager::catalog(&state.options, agent_id) {
                Ok(catalog) => {
                    items.extend(catalog);
                }
                Err(error) if items.is_empty() => return Err(error.to_string()),
                Err(_) => {}
            }
        } else if items.is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
    }
    let mut seen = HashSet::new();
    items.retain(|item| {
        seen.insert((
            item.skill_id.clone(),
            item.source.clone(),
            item.version.clone(),
            item.artifact_id.clone(),
            item.sha256.clone(),
        ))
    });
    let mut result = items;
    result.sort_by(|left, right| {
        left.skill_id
            .cmp(&right.skill_id)
            .then_with(|| left.source.cmp(&right.source))
    });
    for item in result.iter_mut() {
        crate::extension_category::fill_missing_categories(
            &item.capability_ids,
            &mut item.categories,
        );
    }
    Ok(result)
}

fn filter_plugin_catalog(
    items: Vec<crate::api::distribution::PluginCatalogItem>,
    query: &str,
    category: &str,
) -> Vec<crate::api::distribution::PluginCatalogItem> {
    let query = query.trim().to_ascii_lowercase();
    items
        .into_iter()
        .filter(|item| {
            (query.is_empty()
                || format!("{} {} {}", item.plugin_id, item.name, item.description)
                    .to_ascii_lowercase()
                    .contains(&query))
                && (category.is_empty()
                    || category == "all"
                    || item.categories.iter().any(|value| value == category))
        })
        .collect()
}

fn filter_skill_catalog(
    items: Vec<crate::api::distribution::SkillCatalogItem>,
    query: &str,
    category: &str,
) -> Vec<crate::api::distribution::SkillCatalogItem> {
    let query = query.trim().to_ascii_lowercase();
    items
        .into_iter()
        .filter(|item| {
            (query.is_empty()
                || format!("{} {} {}", item.skill_id, item.name, item.description)
                    .to_ascii_lowercase()
                    .contains(&query))
                && (category.is_empty()
                    || category == "all"
                    || item.categories.iter().any(|value| value == category))
        })
        .collect()
}

/// Resolve the Workflow catalog the same way `get_workflow_center` always has:
/// local/GitHub extension sources first, then the organization catalog.
///
/// The error string is only meaningful when nothing could be resolved at all.
/// A partially reachable catalog must not present itself as an empty one, so a
/// non-empty result always reports an empty error.
fn merged_workflow_catalog(
    options: &crate::Options,
) -> Result<(Vec<crate::api::distribution::WorkflowCatalogItem>, String), String> {
    let mut catalog = Vec::new();
    let mut errors = Vec::new();
    match crate::app::extension_source::snapshot() {
        Ok(snapshot) => catalog.extend(snapshot.workflows),
        Err(error) => errors.push(error.to_string()),
    }
    if options.mode().dashboard_enabled() {
        match crate::api::client::load_agent_state(&options.state_path) {
            Ok(state) => match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
            {
                Ok(client) => match crate::api::distribution::workflow_catalog(
                    &client,
                    &options.api_base,
                    &state.agent_id,
                    &state.credential,
                ) {
                    Ok(items) => catalog.extend(items),
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(error.to_string()),
            },
            Err(error) => errors.push(error.to_string()),
        }
    }
    dedupe_workflow_catalog(&mut catalog);
    // Dashboard 发布的工作流同样不带分类，这里统一按 Capability 命名空间补齐；
    // 显式声明的分类不会被覆盖。与扩展源走同一条规则，避免两个来源分叉。
    for item in catalog.iter_mut() {
        crate::extension_category::fill_missing_categories(
            &item.capability_ids,
            &mut item.categories,
        );
    }
    let error = if catalog.is_empty() {
        errors.join("; ")
    } else {
        String::new()
    };
    Ok((catalog, error))
}

fn dedupe_workflow_catalog(items: &mut Vec<crate::api::distribution::WorkflowCatalogItem>) {
    let mut seen = HashSet::new();
    items.retain(|item| {
        seen.insert((
            item.workflow_id.clone(),
            item.source.clone(),
            item.version.clone(),
            item.artifact_id.clone(),
            item.sha256.clone(),
        ))
    });
}

fn filter_workflow_catalog(
    items: Vec<crate::api::distribution::WorkflowCatalogItem>,
    query: &str,
    category: &str,
) -> Vec<crate::api::distribution::WorkflowCatalogItem> {
    let query = query.trim().to_ascii_lowercase();
    items
        .into_iter()
        .filter(|item| {
            (query.is_empty()
                || format!(
                    "{} {} {} {} {}",
                    item.workflow_id,
                    item.name,
                    item.description,
                    item.author_name,
                    item.capability_ids.join(" ")
                )
                .to_ascii_lowercase()
                .contains(&query))
                && (category.is_empty()
                    || category == "all"
                    || item.categories.iter().any(|value| value == category))
        })
        .collect()
}

fn catalog_page<T>(
    items: Vec<T>,
    page: usize,
    page_size: usize,
) -> crate::api::distribution::CatalogPage<T> {
    let page = page.clamp(1, 10_000);
    let page_size = page_size.clamp(1, 100);
    let total = items.len();
    let offset = (page - 1).saturating_mul(page_size);
    let items = items.into_iter().skip(offset).take(page_size).collect();
    crate::api::distribution::CatalogPage {
        items,
        total,
        page,
        page_size,
    }
}

#[tauri::command]
pub(crate) fn open_folder(state: State<'_, AgentState>, path: String) -> Result<(), String> {
    state
        .capability_gateway
        .invoke(
            &InvocationContext::tauri(),
            "system.open_folder",
            json!({ "path": path }),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn get_plugin_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || merged_plugin_catalog(&state))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn query_plugin_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::PluginCatalogPage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let items = filter_plugin_catalog(merged_plugin_catalog(&state)?, &q, &category);
        Ok(catalog_page(items, page, page_size))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_plugin_versions(
    plugin_id: String,
    source: Option<String>,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_plugin_catalog(&state)?.into_iter().any(|item| {
                item.plugin_id == plugin_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        let mut versions = crate::app::extension_source::plugin_versions(&plugin_id)
            .map_err(|error| error.to_string())?;
        if let Some(source) = source.as_deref() {
            versions.retain(|item| item.source == source);
        }
        return Ok(versions);
    }
    require_dashboard(&state)?;
    let snapshot = local_worker_snapshot(&state.worker_status);
    let agent_id = snapshot
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let credential = state.options.agent_credential();
    if agent_id.is_empty() || credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::plugin_versions(
        &client,
        &state.dashboard_base,
        agent_id,
        &credential,
        &plugin_id,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn plan_plugin_install(
    state: State<'_, AgentState>,
    plugin_id: String,
    version: Option<String>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
) -> Result<crate::app::plugin_manager::PluginInstallPlan, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_plugin_catalog(&state)?.iter().any(|item| {
                item.plugin_id == plugin_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        return crate::app::extension_source::plan_plugin_bound(
            &plugin_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let snapshot = local_worker_snapshot(&state.worker_status);
    let agent_id = snapshot
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    crate::app::plugin_manager::plan_install_bound(
        &state.options,
        agent_id,
        &plugin_id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn install_plugin(
    state: State<'_, AgentState>,
    plugin_id: String,
    version: Option<String>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
) -> Result<(), String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_plugin_catalog(&state)?.iter().any(|item| {
                item.plugin_id == plugin_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        return crate::app::extension_source::install_plugin_bound(
            &plugin_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map(|_| {
            let _ = crate::app::extension_source::reconcile_dsh_presets_now();
        })
        .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let previous = crate::app::plugin_manager::local_status(&plugin_id).current_version;
    let result = crate::app::plugin_manager::install_bound(
        &state.options,
        &agent_id,
        &plugin_id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    );
    let report_error = result
        .as_ref()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    let _ = crate::app::plugin_manager::report_status(
        &state.options,
        &agent_id,
        &plugin_id,
        if previous.is_empty() {
            "install"
        } else {
            "upgrade"
        },
        &previous,
        &report_error,
    );
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn uninstall_plugin(
    state: State<'_, AgentState>,
    plugin_id: String,
) -> Result<(), String> {
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let previous = crate::app::plugin_manager::local_status(&plugin_id).current_version;
    let result = crate::app::plugin_manager::uninstall(&plugin_id);
    let report_error = result
        .as_ref()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    let _ = crate::app::plugin_manager::report_status(
        &state.options,
        &agent_id,
        &plugin_id,
        "uninstall",
        &previous,
        &report_error,
    );
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn rollback_plugin(
    state: State<'_, AgentState>,
    plugin_id: String,
) -> Result<(), String> {
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let previous = crate::app::plugin_manager::local_status(&plugin_id).current_version;
    let result = crate::app::plugin_manager::rollback(&plugin_id);
    let report_error = result
        .as_ref()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    let _ = crate::app::plugin_manager::report_status(
        &state.options,
        &agent_id,
        &plugin_id,
        "rollback",
        &previous,
        &report_error,
    );
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_plugin_enabled(
    state: State<'_, AgentState>,
    plugin_id: String,
    enabled: bool,
) -> Result<(), String> {
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let result = crate::app::plugin_manager::set_enabled(&plugin_id, enabled);
    let report_error = result
        .as_ref()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    let _ = crate::app::plugin_manager::report_status(
        &state.options,
        &agent_id,
        &plugin_id,
        if enabled { "enable" } else { "disable" },
        "",
        &report_error,
    );
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn open_plugin_directory() -> Result<(), String> {
    let registry = registry_json().map_err(|e| e.to_string())?;
    let path = registry
        .get("registry_dir")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "plugin registry directory is unavailable".to_string())?;
    open_system_folder(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn register_development_plugin() -> Result<String, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind 插件工程目录")
        .pick_folder()
    else {
        return Err("已取消选择插件工程".to_string());
    };
    crate::capability::plugin::register_development_plugin(&path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn unregister_development_plugin(plugin_id: String) -> Result<(), String> {
    crate::capability::plugin::unregister_development_plugin(&plugin_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn invoke_development_plugin(
    state: State<'_, AgentState>,
    plugin_id: String,
    capability_id: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let started = Instant::now();
    let plugin = crate::capability::plugin::find_plugin(&plugin_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "开发插件不存在".to_string())?;
    if !plugin.development {
        return Err("仅开发插件可使用调试调用".to_string());
    }
    if !plugin
        .capabilities
        .iter()
        .any(|item| item.id == capability_id)
    {
        return Err("Capability 未在插件 Manifest 中声明".to_string());
    }
    let capability = plugin
        .capabilities
        .iter()
        .find(|item| item.id == capability_id)
        .expect("capability existence checked above");
    let control_plane_capability = matches!(
        capability.availability.trim().to_ascii_lowercase().as_str(),
        "control_plane" | "dashboard"
    );
    if control_plane_capability && !state.options.mode().control_plane_enabled() {
        return Err(crate::app::runtime_mode::control_plane_required_error());
    }
    let trusted_dashboard_url = state
        .options
        .mode()
        .control_plane_enabled()
        .then_some(state.options.api_base.as_str());
    let result = crate::capability::plugin::invoke_plugin_capability_for_plugin(
        &plugin_id,
        &capability_id,
        input,
        trusted_dashboard_url,
    );
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(match result {
        Ok(value) => json!({
            "ok": true,
            "duration_ms": duration_ms,
            "result": value,
            "error": null,
        }),
        Err(error) => json!({
            "ok": false,
            "duration_ms": duration_ms,
            "result": null,
            "error": error.to_string(),
        }),
    })
}

#[tauri::command]
pub(crate) async fn open_plugin_view(
    app: AppHandle,
    plugin_id: String,
    view_id: String,
) -> Result<(), String> {
    super::ui::open_plugin_view(&app, &plugin_id, &view_id)
}

/// Return the context supplied to a plugin view without coupling the Agent to
/// any particular plugin.  Plugin UIs can use this to resolve the currently
/// bound authoring/project workspace and keep their own recent-path fallback.
#[tauri::command]
pub(crate) fn get_plugin_view_context(window: WebviewWindow) -> Result<serde_json::Value, String> {
    let label = window.label().to_string();
    if !label.starts_with("plugin-view-") {
        return Err("only plugin windows can use this command".to_string());
    }
    let (plugin_id, view_id) = crate::capability::plugin::scan_plugins()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find_map(|plugin| {
            plugin.views.iter().find_map(|view| {
                (crate::app::ui::plugin_view_window_label(&plugin.id, &view.id) == label)
                    .then(|| (plugin.id.clone(), view.id.clone()))
            })
        })
        .ok_or_else(|| "plugin view identity is unavailable".to_string())?;
    let workspace =
        crate::extension_projects::current_workspace().map_err(|error| error.to_string())?;
    let workspace_root = workspace
        .get("workspace_root")
        .and_then(|value| value.as_str())
        .filter(|path| !path.trim().is_empty())
        .filter(|path| {
            !crate::extension_workspace::is_agent_managed_path(std::path::Path::new(path))
        })
        .map(str::to_string);
    Ok(serde_json::json!({
        "plugin_id": plugin_id,
        "view_id": view_id,
        // The private data directory belongs to this extension only; the Agent
        // derives it from the plugin id so a view cannot ask for another one.
        "data_root": crate::capability::plugin::plugin_data_dir(&plugin_id)
            .to_string_lossy()
            .to_string(),
        "workspace_root": workspace_root,
        "workspace_source": workspace.get("source").cloned().unwrap_or(serde_json::Value::Null),
        "workspace_bound": workspace.get("bound").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "workspace_kind": workspace.get("kind").cloned().unwrap_or(serde_json::Value::String("directory".to_string())),
    }))
}

#[tauri::command]
pub(crate) fn pick_workspace_directory() -> Result<serde_json::Value, String> {
    let path = rfd::FileDialog::new()
        .set_title("选择本机目录")
        .pick_folder()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) fn create_plugin_view_shortcut(
    plugin_id: String,
    view_id: String,
    title: String,
) -> Result<(), String> {
    let Some((_plugin, view, _entry)) =
        crate::capability::plugin::plugin_view_entry(&plugin_id, &view_id)
            .map_err(|error| error.to_string())?
    else {
        return Err(format!("plugin view not found: {plugin_id}/{view_id}"));
    };
    let shortcut_title = if title.trim().is_empty() {
        view.title
    } else {
        title
    };
    crate::app::system::create_plugin_view_shortcut(&plugin_id, &view_id, &shortcut_title)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn close_plugin_view(window: WebviewWindow) -> Result<(), String> {
    if !window.label().starts_with("plugin-view-") {
        return Err("only plugin windows can use this command".to_string());
    }
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(20));
        let _ = window.destroy();
    });
    Ok(())
}

#[tauri::command]
pub(crate) async fn invoke_plugin_view_capability(
    window: WebviewWindow,
    state: State<'_, AgentState>,
    capability_id: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let label = window.label().to_string();
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let plugin = crate::capability::plugin::scan_plugins()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|plugin| {
                plugin.enabled
                    && plugin.views.iter().any(|view| {
                        super::ui::plugin_view_window_label(&plugin.id, &view.id) == label
                    })
            })
            .ok_or_else(|| "plugin view identity is unavailable".to_string())?;
        if !plugin
            .capabilities
            .iter()
            .any(|capability| capability.id == capability_id)
        {
            return Err(format!(
                "capability is not declared by plugin {}: {}",
                plugin.id, capability_id
            ));
        }
        gateway
            .invoke(
                &InvocationContext::new(
                    crate::capability::types::InvocationSource::Tauri,
                    format!("plugin-view:{}", plugin.id),
                ),
                &capability_id,
                input,
            )
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
/// 定时任务的读/写入口。
///
/// UI 只做展示与编辑，调度语义完全落在 `crate::scheduler`：到点由 Agent
/// 调度线程按 `kind` 派发目标，UI 不参与执行。
pub(crate) fn list_schedules() -> Result<serde_json::Value, String> {
    crate::scheduler::list(crate::scheduler::now_epoch()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_schedule(input: serde_json::Value) -> Result<serde_json::Value, String> {
    crate::scheduler::set(&input, crate::scheduler::now_epoch()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn delete_schedule(id: String) -> Result<serde_json::Value, String> {
    crate::scheduler::delete(&id).map_err(|error| error.to_string())
}

/// 启动预设：把一套启动参数存下来，跨工作区复用时只改工作区。
#[tauri::command]
pub(crate) fn list_workflow_presets(
    workflow_id: Option<String>,
) -> Result<serde_json::Value, String> {
    crate::workflow::list_run_presets(workflow_id.as_deref().unwrap_or_default())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_workflow_preset(input: serde_json::Value) -> Result<serde_json::Value, String> {
    crate::workflow::set_run_preset(&input, crate::scheduler::now_epoch())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn delete_workflow_preset(id: String) -> Result<serde_json::Value, String> {
    crate::workflow::delete_run_preset(&id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_skill_runs(limit: Option<usize>) -> Result<serde_json::Value, String> {
    crate::skill_run::list(limit.unwrap_or(20)).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn run_skill(
    state: State<'_, AgentState>,
    skillId: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let options = state.capability_gateway.options().clone();
    crate::skill_run::start(&options, "", &skillId, &input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn reveal_skill_run(run_id: String) -> Result<(), String> {
    let record = crate::skill_run::get(&run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("技能运行不存在：{run_id}"))?;
    // 结果文件可能还没生成（运行中/失败），此时退回到运行目录，保证“定位”始终有落点。
    let target = if record.output_path.is_empty() {
        crate::skill_run::runs_root().join(&record.run_id)
    } else {
        std::path::PathBuf::from(&record.output_path)
    };
    open_system_folder(&target.to_string_lossy()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_workflow_center(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    workflow_center_snapshot(state.capability_gateway.options()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn query_workflow_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::WorkflowCatalogPage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (items, error) = merged_workflow_catalog(&state.options)?;
        if items.is_empty() && !error.is_empty() {
            return Err(error);
        }
        let items = filter_workflow_catalog(items, &q, &category);
        Ok(catalog_page(items, page, page_size))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// List the versions a user can choose for one Workflow.
///
/// `source` is part of each item on purpose: the install command routes
/// local/GitHub items through the extension source and everything else through
/// the organization catalog, so the version picker has to keep that identity.
#[tauri::command]
pub(crate) fn get_workflow_versions(
    workflow_id: String,
    source: Option<String>,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::WorkflowCatalogItem>, String> {
    let mut versions = Vec::new();
    let mut errors = Vec::new();
    if source
        .as_deref()
        .is_none_or(|value| value.starts_with("local:") || value.starts_with("github:"))
    {
        match crate::app::extension_source::workflow_versions(&workflow_id) {
            Ok(mut items) => {
                if let Some(source) = source.as_deref() {
                    items.retain(|item| item.source == source);
                }
                versions.extend(items)
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    if state.options.mode().dashboard_enabled()
        && source
            .as_deref()
            .is_none_or(|value| !value.starts_with("local:") && !value.starts_with("github:"))
    {
        let snapshot = local_worker_snapshot(&state.worker_status);
        let agent_id = snapshot
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let credential = state.options.agent_credential();
        if !agent_id.is_empty() && !credential.is_empty() {
            match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
            {
                Ok(client) => match crate::api::distribution::workflow_versions(
                    &client,
                    &state.dashboard_base,
                    agent_id,
                    &credential,
                    &workflow_id,
                ) {
                    Ok(items) => versions.extend(items),
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(error.to_string()),
            }
        }
    }
    dedupe_workflow_catalog(&mut versions);
    versions.sort_by(|left, right| {
        crate::skill::resolver::compare_versions(&right.version, &left.version)
    });
    if versions.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(versions)
}

#[derive(Debug, Serialize)]
pub(crate) struct ConnectorStateItem {
    id: String,
    name: String,
    version: String,
    availability: String,
    credential_ownership: String,
    enabled: bool,
    revoked: bool,
    source: String,
    remote_revision: u64,
    reason: String,
    updated_at: String,
    credential_count: usize,
}

fn connector_state_snapshot() -> Result<Vec<ConnectorStateItem>, Box<dyn Error>> {
    let store = crate::workflow::WorkflowStore::open_default()?;
    let mut connectors = std::collections::BTreeMap::new();
    for installed in store.list()? {
        for connector in installed.package.connectors {
            connectors.entry(connector.id.clone()).or_insert(connector);
        }
    }
    let states = crate::store::connector_state::list()?
        .into_iter()
        .map(|state| (state.connector_id.clone(), state))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut credential_counts = std::collections::BTreeMap::<String, usize>::new();
    for credential in crate::store::connector_credentials::list()? {
        *credential_counts
            .entry(credential.connector_id)
            .or_default() += 1;
    }
    for connector_id in states.keys() {
        connectors.entry(connector_id.clone()).or_insert_with(|| {
            crate::workflow::WorkflowConnectorManifest {
                schema_version: "connector_manifest.v1".to_string(),
                id: connector_id.clone(),
                version: "0.0.0".to_string(),
                name: connector_id.clone(),
                description: String::new(),
                availability: "local".to_string(),
                credential_ownership: "agent".to_string(),
                auth: vec!["none".to_string()],
                capabilities: Vec::new(),
                scopes: Vec::new(),
                supported_platforms: Vec::new(),
                health_check: Value::Null,
                credentials: Vec::new(),
            }
        });
    }
    Ok(connectors
        .into_values()
        .map(|connector| {
            let state = states.get(&connector.id).cloned().unwrap_or_else(|| {
                crate::store::connector_state::ConnectorState {
                    connector_id: connector.id.clone(),
                    enabled: true,
                    revoked: false,
                    source: "local".to_string(),
                    remote_revision: 0,
                    reason: String::new(),
                    updated_at: String::new(),
                }
            });
            ConnectorStateItem {
                id: connector.id,
                name: connector.name,
                version: connector.version,
                availability: connector.availability,
                credential_ownership: connector.credential_ownership,
                enabled: state.enabled,
                revoked: state.revoked,
                source: state.source,
                remote_revision: state.remote_revision,
                reason: state.reason,
                updated_at: state.updated_at,
                credential_count: credential_counts
                    .get(&state.connector_id)
                    .copied()
                    .unwrap_or_default(),
            }
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn get_connector_states() -> Result<Vec<ConnectorStateItem>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        connector_state_snapshot().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn set_connector_enabled(
    connector_id: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = crate::store::connector_state::set_enabled(&connector_id, enabled)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(state).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn revoke_connector(
    connector_id: String,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = crate::store::connector_state::revoke(
            &connector_id,
            reason.as_deref().unwrap_or_default(),
        )
        .map_err(|error| error.to_string())?;
        serde_json::to_value(state).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn restore_connector(connector_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = crate::store::connector_state::restore(&connector_id)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(state).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn set_connector_file_credential(
    connector_id: String,
    handle: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let credential_spec = declared_connector_credential(&connector_id, &handle)?;
        if credential_spec.kind != "file_path" {
            return Err(format!(
                "connector credential {handle} is not a file_path credential"
            ));
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择 Connector 凭据文件")
            .pick_file()
        else {
            return Ok(json!({
                "cancelled": true,
                "credential": null,
            }));
        };
        let credential =
            crate::store::connector_credentials::set_file_path(&handle, &connector_id, &path)
                .map_err(|error| error.to_string())?;
        Ok(json!({
            "cancelled": false,
            "credential": credential,
        }))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn set_connector_secret_credential(
    connector_id: String,
    handle: String,
    secret: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let credential_spec = declared_connector_credential(&connector_id, &handle)?;
        if credential_spec.kind != "secret" {
            return Err(format!(
                "connector credential {handle} is not a secret credential"
            ));
        }
        let credential =
            crate::store::connector_credentials::set_secret(&handle, &connector_id, &secret)
                .map_err(|error| error.to_string())?;
        serde_json::to_value(credential).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn declared_connector_credential(
    connector_id: &str,
    handle: &str,
) -> Result<crate::workflow::WorkflowConnectorCredential, String> {
    let store =
        crate::workflow::WorkflowStore::open_default().map_err(|error| error.to_string())?;
    for installed in store.list().map_err(|error| error.to_string())? {
        for connector in installed.package.connectors {
            if connector.id != connector_id {
                continue;
            }
            if let Some(credential) = connector
                .credentials
                .into_iter()
                .find(|credential| credential.handle == handle)
            {
                return Ok(credential);
            }
        }
    }
    Err(format!(
        "connector credential {handle} is not declared by connector {connector_id}"
    ))
}

#[tauri::command]
pub(crate) async fn remove_connector_credential(handle: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::store::connector_credentials::remove(&handle).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn get_workflow_run(run_id: String) -> Result<serde_json::Value, String> {
    workflow_run_snapshot(&run_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn verify_workflow_run(run_id: String) -> Result<serde_json::Value, String> {
    workflow_run_verification(&run_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn reveal_workflow_artifact(run_id: String, artifact_id: String) -> Result<(), String> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()
        .map_err(|error| error.to_string())?;
    let run = ledger
        .get_run(&run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    let interaction = ledger
        .get_interaction(&run.interaction_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "workflow run interaction is missing".to_string())?;
    let package = crate::workflow::WorkflowStore::open_default()
        .and_then(|store| store.load_for_run_interaction(&interaction))
        .map_err(|error| error.to_string())?;
    let verification =
        crate::workflow::verify_run(&package, &run).map_err(|error| error.to_string())?;
    let artifact = verification
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
        .ok_or_else(|| format!("workflow artifact not found: {artifact_id}"))?;
    if artifact.uri.trim().is_empty() {
        return Err("workflow artifact has no local file location".to_string());
    }
    let path = PathBuf::from(artifact.uri.trim())
        .canonicalize()
        .map_err(|error| format!("workflow artifact file is unavailable: {error}"))?;
    if !path.is_file() {
        return Err("workflow artifact location is not a file".to_string());
    }
    open_system_folder(&path.to_string_lossy()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn approve_workflow_step(
    run_id: String,
    step_id: String,
) -> Result<serde_json::Value, String> {
    decide_workflow_step(&run_id, &step_id, true).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn reject_workflow_step(
    run_id: String,
    step_id: String,
) -> Result<serde_json::Value, String> {
    decide_workflow_step(&run_id, &step_id, false).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn cancel_workflow_run(run_id: String) -> Result<serde_json::Value, String> {
    let runner =
        crate::workflow::WorkflowRunner::open_default().map_err(|error| error.to_string())?;
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()
        .map_err(|error| error.to_string())?;
    let run = ledger
        .get_run(&run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    let approval_id = if run.current_step_id.trim().is_empty() {
        String::new()
    } else {
        crate::workflow::workflow_approval_id(&run.run_id, &run.current_step_id)
    };
    let run = runner
        .cancel(run, "workflow canceled from Agent UI")
        .map_err(|error| error.to_string())?;
    if !approval_id.is_empty() {
        let _ = ApprovalManager::global().interrupt(&approval_id, "workflow_canceled");
    }
    serde_json::to_value(run).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn resume_workflow_run(
    state: State<'_, AgentState>,
    run_id: String,
    feedback: Option<String>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        resume_workflow_with_gateway(gateway, &run_id, feedback.as_deref())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn start_workflow_run(
    state: State<'_, AgentState>,
    package_id: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_workflow_with_gateway(gateway, &package_id, input).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn preflight_workflow_run(
    state: State<'_, AgentState>,
    package_id: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        preflight_workflow_with_gateway(gateway, &package_id, input)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn install_workflow_catalog_item(
    state: State<'_, AgentState>,
    workflow_id: String,
    version: Option<String>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if source
            .as_deref()
            .is_some_and(|value| value.starts_with("local:") || value.starts_with("github:"))
        {
            let source_id = public_extension_source_id(source.as_deref());
            let (_, installed) = crate::app::extension_source::install_workflow_bound(
                &workflow_id,
                version.as_deref(),
                source_id,
                sha256.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            return serde_json::to_value(installed).map_err(|error| error.to_string());
        }
        let options = gateway.options().clone();
        let installed = crate::app::workflow_manager::install_dashboard_catalog_workflow_bound(
            &options,
            &workflow_id,
            version.as_deref(),
            artifact_id.as_deref(),
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        serde_json::to_value(installed).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn pick_workflow_archive() -> Result<serde_json::Value, String> {
    let path = rfd::FileDialog::new()
        .set_title("选择 Workflow Package")
        .add_filter("HiMind Workflow", &["hmwf", "zip"])
        .pick_file()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) async fn install_local_workflow_archive(
    archive_path: String,
    require_signature: bool,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let installed = crate::app::workflow_manager::install_local_archive(
            PathBuf::from(archive_path).as_path(),
            require_signature,
        )
        .map_err(|error| error.to_string())?;
        serde_json::to_value(installed).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn set_workflow_enabled(
    package_id: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store =
            crate::workflow::WorkflowStore::open_default().map_err(|error| error.to_string())?;
        let installed = store
            .set_enabled(&package_id, enabled)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(installed).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn rollback_workflow(package_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store =
            crate::workflow::WorkflowStore::open_default().map_err(|error| error.to_string())?;
        let installed = store
            .rollback(&package_id)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(installed).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn remove_workflow(package_id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::workflow::WorkflowStore::open_default()
            .and_then(|store| store.remove(&package_id))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn decide_workflow_step(
    run_id: &str,
    step_id: &str,
    approved: bool,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let approval_id = crate::workflow::workflow_approval_id(run_id, step_id);
    let approval_manager = ApprovalManager::global();
    let mut decision_error = None;
    for attempt in 0..20 {
        match approval_manager.respond(&approval_id, approved) {
            Ok(()) => {
                decision_error = None;
                break;
            }
            Err(error) => {
                decision_error = Some(error);
                if attempt < 19 {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    if let Some(error) = decision_error {
        return Err(error.into());
    }

    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let mut last_run = None;
    for _ in 0..40 {
        let run = ledger
            .get_run(run_id)?
            .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
        if run.status != crate::agent_core_contracts::LocalRunStatus::Waiting
            || run.current_step_id != step_id
        {
            return Ok(serde_json::to_value(run)?);
        }
        thread::sleep(Duration::from_millis(50));
        last_run = Some(run);
    }
    let run = last_run.ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    Ok(serde_json::to_value(run)?)
}

pub(crate) fn resume_workflow_with_gateway(
    gateway: CapabilityGateway,
    run_id: &str,
    feedback: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let run = ledger
        .get_run(run_id)?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    let interaction = ledger
        .get_interaction(&run.interaction_id)?
        .ok_or("workflow run interaction is missing")?;
    let mut input = interaction
        .business_context
        .get("input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let package =
        crate::workflow::WorkflowStore::open_default()?.load_for_run_interaction(&interaction)?;
    let context = InvocationContext::new(
        crate::capability::types::InvocationSource::Workflow,
        "workflow-ui",
    );
    let capabilities = gateway.list_capabilities(&context)?;
    let probe_context = context.clone().without_agent_core_run();
    let report = crate::workflow::preflight_with_connector_probes(
        &package,
        VERSION,
        &capabilities,
        &input,
        |capability_id, input| {
            let mut context = probe_context.clone();
            context.request_id = format!("{}:health:{capability_id}", context.request_id);
            gateway.invoke(&context, capability_id, input)
        },
    );
    if !report.ready {
        return Err(format!("workflow preflight failed: {}", report.blockers.join("; ")).into());
    }
    if let Some(feedback) = feedback
        .map(str::trim)
        .filter(|feedback| !feedback.is_empty())
    {
        if let Some(object) = input.as_object_mut() {
            object.insert("feedback".to_string(), json!(feedback));
        }
    }
    let runner = crate::workflow::WorkflowRunner::open_default()?;
    let run = if let Some(feedback) = feedback
        .map(str::trim)
        .filter(|feedback| !feedback.is_empty())
    {
        runner.record_loop_feedback(&package, run, feedback)?
    } else {
        run
    };
    let executor = crate::workflow::WorkflowGatewayExecutor::new(
        gateway,
        context,
        ledger.clone(),
        run.run_id.clone(),
    );
    let outcome = runner.run_ready(&package, run, &input, &executor)?;
    Ok(serde_json::to_value(outcome)?)
}

/// 后台执行一个已经创建（Queued）的运行。
///
/// 「受理即返回」是这条链路的硬要求：界面必须立刻接手渲染实时过程，
/// 而不是等执行结束。启动、定时、恢复（含 `workflow.run.start` 能力）共用这一个
/// 实现，避免某条路径再退回同步执行——UI 启动此前正是这么退化的：
/// 点一次启动，弹窗要挂到整条工作流跑完，期间界面拿不到任何过程反馈。
pub(crate) fn dispatch_run_in_background(
    gateway: CapabilityGateway,
    context: InvocationContext,
    package: crate::workflow::WorkflowPackage,
    run_id: String,
    input: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    thread::Builder::new()
        .name(format!("workflow-run-{run_id}"))
        .spawn(move || {
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
                let executor = crate::workflow::WorkflowGatewayExecutor::new(
                    gateway,
                    context,
                    ledger,
                    run_id.clone(),
                );
                let queued = crate::store::local_runs::LocalRunLedger::open_default()?
                    .get_run(&run_id)?
                    .ok_or("workflow run disappeared before execution")?;
                crate::workflow::WorkflowRunner::open_default()?
                    .run_ready(&package, queued, &input, &executor)?;
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("workflow run {run_id} failed to start: {error}");
            }
        })?;
    Ok(())
}

fn start_workflow_with_gateway(
    gateway: CapabilityGateway,
    package_id: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let context = InvocationContext::new(
        crate::capability::types::InvocationSource::Workflow,
        "workflow-ui",
    );
    let (package, run) =
        prepare_workflow_run(gateway.clone(), package_id, input.clone(), &context)?;
    dispatch_run_in_background(gateway, context, package, run.run_id.clone(), input)?;
    Ok(json!({
        "accepted": true,
        "run": run,
        "blocked_step_id": "",
        "completed_steps": [],
    }))
}

pub(crate) fn schedule_workflow_with_gateway(
    gateway: CapabilityGateway,
    package_id: &str,
    mut input: serde_json::Value,
    context: InvocationContext,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if let Some(object) = input.as_object_mut() {
        object.insert(
            "_origin".to_string(),
            json!({
                "source": context.source.as_str(),
                "ai_client_id": &context.ai_client_id,
                "session_id_hash": &context.session_id_hash,
                "request_id": &context.request_id,
            }),
        );
    }
    let (package, run) =
        prepare_workflow_run(gateway.clone(), package_id, input.clone(), &context)?;
    dispatch_run_in_background(gateway, context, package, run.run_id.clone(), input)?;
    Ok(json!({
        "accepted": true,
        "run": run,
    }))
}

pub(crate) fn schedule_resume_workflow_with_gateway(
    gateway: CapabilityGateway,
    run_id: &str,
    feedback: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let run = ledger
        .get_run(run_id)?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    let thread_run_id = run_id.to_string();
    let thread_feedback = feedback.map(str::to_string);
    thread::Builder::new()
        .name(format!("workflow-resume-{run_id}"))
        .spawn(move || {
            if let Err(error) =
                resume_workflow_with_gateway(gateway, &thread_run_id, thread_feedback.as_deref())
            {
                eprintln!("workflow resume {thread_run_id} failed: {error}");
            }
        })?;
    Ok(json!({
        "accepted": true,
        "run": run,
    }))
}

fn prepare_workflow_run(
    gateway: CapabilityGateway,
    package_id: &str,
    input: serde_json::Value,
    context: &InvocationContext,
) -> Result<
    (
        crate::workflow::WorkflowPackage,
        crate::agent_core_contracts::LocalRun,
    ),
    Box<dyn std::error::Error>,
> {
    let installed =
        crate::workflow::WorkflowStore::open_default()?.load_enabled_for_run(package_id)?;
    let package = installed.package.clone();
    let capabilities = gateway.list_capabilities(context)?;
    let probe_context = context.clone().without_agent_core_run();
    let report = crate::workflow::preflight_with_connector_probes(
        &package,
        VERSION,
        &capabilities,
        &input,
        |capability_id, input| {
            let mut context = probe_context.clone();
            context.request_id = format!("{}:health:{capability_id}", context.request_id);
            gateway.invoke(&context, capability_id, input)
        },
    );
    let mut report = report;
    if let Some(lock) = installed.extension_lock.as_ref() {
        if let Err(error) = crate::workflow::validate_environment_lock(lock, &capabilities, &report)
        {
            report.push_blocker(
                "environment.lock_mismatch",
                "dependencies",
                error.to_string(),
                "恢复锁定的 Capability、Connector 和 Runtime 版本，或重新生成 Environment Lock。",
                true,
            );
        }
    }
    if !report.ready {
        return Err(format!("workflow preflight failed: {}", report.blockers.join("; ")).into());
    }
    let runner = crate::workflow::WorkflowRunner::open_default()?;
    let request_id = format!("{}:{}", context.request_id, crate::workflow_request_id());
    let run = runner.start("local-agent", &package, &request_id, &input)?;
    Ok((package, run))
}

fn preflight_workflow_with_gateway(
    gateway: CapabilityGateway,
    package_id: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let installed =
        crate::workflow::WorkflowStore::open_default()?.load_enabled_for_run(package_id)?;
    let package = installed.package.clone();
    let context = InvocationContext::new(
        crate::capability::types::InvocationSource::Workflow,
        "workflow-ui-preflight",
    );
    let capabilities = gateway.list_capabilities(&context)?;
    let probe_context = context.clone().without_agent_core_run();
    let report = crate::workflow::preflight_with_connector_probes(
        &package,
        VERSION,
        &capabilities,
        &input,
        |capability_id, input| {
            let mut context = probe_context.clone();
            context.request_id = format!("{}:health:{capability_id}", context.request_id);
            gateway.invoke(&context, capability_id, input)
        },
    );
    let mut report = report;
    if let Some(lock) = installed.extension_lock.as_ref() {
        if let Err(error) = crate::workflow::validate_environment_lock(lock, &capabilities, &report)
        {
            report.push_blocker(
                "environment.lock_mismatch",
                "dependencies",
                error.to_string(),
                "恢复锁定的 Capability、Connector 和 Runtime 版本，或重新生成 Environment Lock。",
                true,
            );
        }
    }
    Ok(serde_json::to_value(report)?)
}

fn workflow_center_snapshot(
    options: &crate::Options,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let store = crate::workflow::WorkflowStore::open_default()?;
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let metrics = crate::workflow::workflow_metrics_by_package(&ledger)?;
    let mut workflows = Vec::new();
    let mut workflow_names = std::collections::HashMap::<String, String>::new();
    for item in store.list()? {
        let view = store.view_json(&item.package)?;
        workflow_names.insert(item.package.id.clone(), item.package.name.clone());
        workflows.push(json!({
            "package": item.package,
            "enabled": item.enabled,
            "previous_version": item.previous_version,
            "package_digest": item.package_digest,
            "source": item.source,
            "installed_at": item.installed_at,
            "updated_at": item.updated_at,
            "view": view,
            "metrics": metrics.get(&item.package.id).cloned().unwrap_or_default(),
        }));
    }
    let mut runs = Vec::new();
    for run in ledger
        .list_runs(100)?
        .into_iter()
        .filter(|run| run.source == crate::agent_core_contracts::InteractionSource::Workflow)
    {
        let interaction = ledger.get_interaction(&run.interaction_id)?;
        let workflow_ref = interaction
            .as_ref()
            .and_then(|interaction| interaction.business_context.get("workflow"));
        let input = interaction
            .as_ref()
            .and_then(|interaction| interaction.business_context.get("input"));
        let workflow_id = workflow_ref
            .and_then(|workflow| workflow.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let workflow_version = workflow_ref
            .and_then(|workflow| workflow.get("version"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let current_step = run
            .steps
            .iter()
            .find(|step| step.step_id == run.current_step_id);
        let interaction_request =
            if run.status == crate::agent_core_contracts::LocalRunStatus::Waiting {
                workflow_interaction_request(&run, &ledger.list_events(&run.run_id)?)
            } else {
                None
            };
        let waiting_kind = interaction_request
            .as_ref()
            .and_then(|request| request.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let waiting_reason = interaction_request
            .as_ref()
            .and_then(|request| request.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let required_action = interaction_request
            .as_ref()
            .and_then(|request| request.get("required_action"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let projections = ledger.projections_for_aggregate(&run.run_id, 100)?;
        let projection_status = projections
            .first()
            .map(|projection| projection.status.clone())
            .unwrap_or_else(|| "none".to_string());
        runs.push(json!({
            "workflow_id": workflow_id,
            "workflow_name": workflow_names.get(workflow_id).cloned().unwrap_or_default(),
            "workflow_version": workflow_version,
            "business_stage": workflow_business_stage(&run.current_step_id),
            "current_step_title": current_step.map(|step| step.title.clone()).unwrap_or_default(),
            "waiting_kind": waiting_kind,
            "waiting_reason": waiting_reason,
            "required_action": required_action,
            "interaction_request": interaction_request,
            "project_root": input.and_then(|input| input.get("project_root")).and_then(Value::as_str).unwrap_or_default(),
            "workspace_root": input.and_then(|input| input.get("workspace_root")).and_then(Value::as_str).unwrap_or_default(),
            "app_id": input.and_then(|input| input.get("app_id")).and_then(Value::as_str).unwrap_or_default(),
            "run": run,
            "projection_count": projections.len(),
            "projection_status": projection_status,
        }));
    }
    let (catalog, catalog_error) = merged_workflow_catalog(options)?;
    Ok(json!({
        "workflows": workflows,
        "runs": runs,
        "catalog": catalog,
        "catalog_error": catalog_error,
    }))
}

fn workflow_business_stage(step_id: &str) -> &'static str {
    match step_id {
        "" => "",
        "WX-REQUIREMENTS" => "需求",
        "WX-PREPARE" => "准备",
        "DEV-LOOP" | "DEV-CODE" | "DEV-TEST" | "DEV-BUILD" | "DEV-REVIEW" => "开发",
        "WX-CONTEXT" => "工程校验",
        "WX-CANDIDATE" => "候选冻结",
        "WX-PREVIEW" => "预览",
        "WX-UPLOAD" => "体验版",
        "WX-ACCEPTANCE" => "验收",
        "WX-ORGANIZATION-APPROVAL" => "组织审批",
        "WX-REVIEW-PREPARE" | "WX-REVIEW-SUBMIT" => "微信审核",
        "WX-RELEASE" => "发布",
        "WX-ROLLBACK" => "回滚",
        value if value.starts_with("DEV-") => "开发",
        _ => "执行",
    }
}

/// 将 Runner 已经写入 Ledger 的等待事件投影为稳定的 UI 交互契约。
///
/// 这不是新的事实源：审批/反馈仍由 Runtime Event 决定，UI 只消费这个
/// 可解释的 View Model。后续新增 form/evidence/external_wait 时只需扩展
/// kind、schema 和 required_action，不再让每个页面猜 payload 字段。
fn workflow_interaction_request(
    run: &crate::agent_core_contracts::LocalRun,
    events: &[crate::agent_core_contracts::RuntimeEvent],
) -> Option<Value> {
    if run.status != crate::agent_core_contracts::LocalRunStatus::Waiting
        || run.current_step_id.trim().is_empty()
    {
        return None;
    }
    let step_title = run
        .steps
        .iter()
        .find(|step| step.step_id == run.current_step_id)
        .map(|step| step.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(run.current_step_id.as_str());
    let mut pending_feedback: Option<&crate::agent_core_contracts::RuntimeEvent> = None;
    let mut pending_approval: Option<&crate::agent_core_contracts::RuntimeEvent> = None;
    for event in events
        .iter()
        .filter(|event| event.step_id == run.current_step_id)
    {
        match event.event_type {
            crate::agent_core_contracts::RuntimeEventType::QuestionRequested => {
                pending_feedback = Some(event);
            }
            crate::agent_core_contracts::RuntimeEventType::Progress
                if event
                    .payload
                    .get("waiting_for_feedback")
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                pending_feedback = Some(event);
            }
            crate::agent_core_contracts::RuntimeEventType::QuestionResolved => {
                let question_event_id = event
                    .payload
                    .get("question_event_id")
                    .and_then(Value::as_str);
                if question_event_id.is_none()
                    || pending_feedback
                        .as_ref()
                        .is_some_and(|pending| Some(pending.event_id.as_str()) == question_event_id)
                {
                    pending_feedback = None;
                }
            }
            crate::agent_core_contracts::RuntimeEventType::ApprovalRequested => {
                pending_approval = Some(event);
            }
            crate::agent_core_contracts::RuntimeEventType::ApprovalResolved => {
                pending_approval = None;
            }
            _ => {}
        }
    }
    if let Some(event) = pending_feedback {
        let prompt = event
            .payload
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("请提供下一轮开发需要处理的反馈");
        return Some(json!({
            "schema_version": "interaction_request.v1",
            "id": event.event_id,
            "run_id": run.run_id,
            "step_id": run.current_step_id,
            "kind": "feedback",
            "title": format!("{}需要反馈", step_title),
            "description": prompt,
            "required_action": "submit_feedback",
            "status": "pending",
            "source_event_id": event.event_id,
            "created_at": event.occurred_at,
            "schema": {
                "type": "object",
                "required": ["feedback"],
                "properties": {
                    "feedback": {"type": "string", "minLength": 1, "maxLength": 8000}
                }
            },
            "metadata": {
                "iteration": event.payload.get("iteration").cloned().unwrap_or(Value::Null),
                "loop_id": event.payload.get("loop_id").cloned().unwrap_or(Value::Null)
            }
        }));
    }
    if let Some(event) = pending_approval {
        let risk_level = event
            .payload
            .get("risk_level")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Some(json!({
            "schema_version": "interaction_request.v1",
            "id": format!("{}:{}:approval", run.run_id, run.current_step_id),
            "run_id": run.run_id,
            "step_id": run.current_step_id,
            "kind": "approval",
            "title": format!("{}等待审批", step_title),
            "description": if risk_level.is_empty() { "确认后继续执行此步骤".to_string() } else { format!("风险等级 {}，确认后继续执行此步骤", risk_level) },
            "required_action": "approve_or_reject",
            "status": "pending",
            "source_event_id": event.event_id,
            "created_at": event.occurred_at,
            "risk_level": risk_level,
            "schema": {
                "type": "object",
                "required": ["decision"],
                "properties": {
                    "decision": {"type": "string", "enum": ["approve", "reject"]}
                }
            }
        }));
    }
    Some(json!({
        "schema_version": "interaction_request.v1",
        "id": format!("{}:{}:waiting", run.run_id, run.current_step_id),
        "run_id": run.run_id,
        "step_id": run.current_step_id,
        "kind": "external_wait",
        "title": format!("{}等待处理", step_title),
        "description": "运行正在等待外部状态或人工操作",
        "required_action": "inspect_run",
        "status": "pending"
    }))
}

pub(crate) fn workflow_run_snapshot(
    run_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let run = ledger
        .get_run(run_id)?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    let interaction = ledger.get_interaction(&run.interaction_id)?;
    let events = ledger.list_events(run_id)?;
    let projections = ledger.projections_for_aggregate(run_id, 100)?;
    let package_id = interaction
        .as_ref()
        .and_then(|interaction| interaction.business_context.get("workflow"))
        .and_then(|workflow| workflow.get("id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let store = crate::workflow::WorkflowStore::open_default()?;
    let workflow = store
        .list()?
        .into_iter()
        .find(|item| item.package.id == package_id)
        .map(|item| {
            let view = store.view_json(&item.package)?;
            Ok::<_, Box<dyn std::error::Error>>(json!({
                "package": item.package,
                "enabled": item.enabled,
                "view": view,
            }))
        })
        .transpose()?;
    Ok(json!({
        "run": run,
        "interaction": interaction,
        "events": events,
        "projections": projections,
        "interaction_request": workflow_interaction_request(&run, &events),
        "workflow": workflow,
    }))
}

fn workflow_run_verification(
    run_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let run = ledger
        .get_run(run_id)?
        .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
    if run.status != crate::agent_core_contracts::LocalRunStatus::Succeeded {
        return Err(format!(
            "workflow run is not succeeded: {} ({:?})",
            run.run_id, run.status
        )
        .into());
    }
    let interaction = ledger
        .get_interaction(&run.interaction_id)?
        .ok_or("workflow run interaction is missing")?;
    let package =
        crate::workflow::WorkflowStore::open_default()?.load_for_run_interaction(&interaction)?;
    Ok(serde_json::to_value(crate::workflow::verify_run(
        &package, &run,
    )?)?)
}

#[cfg(test)]
mod tests {
    use super::{dedupe_workflow_catalog, present_builtin_ai_start_error};
    use crate::api::distribution::WorkflowCatalogItem;

    fn catalog_item(
        workflow_id: &str,
        name: &str,
        description: &str,
        author_name: &str,
        categories: &[&str],
        capability_ids: &[&str],
    ) -> WorkflowCatalogItem {
        WorkflowCatalogItem {
            workflow_id: workflow_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            author_name: author_name.to_string(),
            categories: categories.iter().map(|value| value.to_string()).collect(),
            version: "1.0.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: "0.3.0".to_string(),
            capability_ids: capability_ids
                .iter()
                .map(|value| value.to_string())
                .collect(),
            channel: "stable".to_string(),
            artifact_id: String::new(),
            file_name: "test.hmwf".to_string(),
            file_size: 0,
            sha256: String::new(),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: String::new(),
            source: "organization".to_string(),
            assignment: "optional".to_string(),
            management: "user_managed".to_string(),
            install_mode: "prompt".to_string(),
            organization_reason: String::new(),
            managed: false,
            allow_disable: true,
            allow_uninstall: true,
            extension_lock: None,
        }
    }

    #[test]
    fn workflow_catalog_search_matches_name_author_and_capability() {
        let items = vec![
            catalog_item(
                "com.himind.workflow.wechat",
                "微信小程序开发闭环",
                "从需求到体验版",
                "马宝全",
                &["engineering"],
                &["wechat.miniprogram.build"],
            ),
            catalog_item(
                "com.himind.workflow.contract",
                "合同评审",
                "法务流程",
                "李四",
                &["legal"],
                &["document.inspect"],
            ),
        ];
        assert_eq!(
            super::filter_workflow_catalog(items.clone(), "wechat.miniprogram.build", "").len(),
            1
        );
        assert_eq!(
            super::filter_workflow_catalog(items.clone(), "马宝全", "").len(),
            1
        );
        assert_eq!(
            super::filter_workflow_catalog(items.clone(), "", "legal").len(),
            1
        );
        assert_eq!(
            super::filter_workflow_catalog(items.clone(), "体验版", "").len(),
            1
        );
        assert_eq!(
            super::filter_workflow_catalog(items.clone(), "", "all").len(),
            2
        );
        assert_eq!(
            super::filter_workflow_catalog(items, "missing", "").len(),
            0
        );
    }

    #[test]
    fn workflow_catalog_keeps_distinct_source_and_artifact_candidates() {
        let mut local = catalog_item("com.himind.workflow.same", "同一工作流", "", "", &[], &[]);
        local.source = "local:workspace-a".to_string();
        local.sha256 = "a".repeat(64);
        let mut github = local.clone();
        github.source = "github:release-a".to_string();
        let mut dashboard = local.clone();
        dashboard.source = "organization".to_string();
        dashboard.artifact_id = "artifact-1".to_string();
        let duplicate = dashboard.clone();
        let mut candidates = vec![local, github, dashboard, duplicate];

        dedupe_workflow_catalog(&mut candidates);

        assert_eq!(candidates.len(), 3);
        assert!(candidates
            .iter()
            .any(|item| item.source.starts_with("local:")));
        assert!(candidates
            .iter()
            .any(|item| item.source.starts_with("github:")));
        assert!(candidates
            .iter()
            .any(|item| item.artifact_id == "artifact-1"));
    }

    #[test]
    fn connected_ai_errors_keep_existing_login_guidance() {
        assert_eq!(
            present_builtin_ai_start_error("AI credential missing scope"),
            "需要登录 HiMind 账号后才能开始对话"
        );
    }

    #[test]
    fn workspace_start_errors_keep_dsh_diagnostics() {
        assert_eq!(
            present_builtin_ai_start_error(
                "无法进入扩展项目工作区：注册 DSH 工作区失败（workspace-invalid-path）：path is invalid"
            ),
            "无法进入项目工作区：注册 DSH 工作区失败（workspace-invalid-path）：path is invalid"
        );
    }
}
