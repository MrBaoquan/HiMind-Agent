use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Instant;
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::api::distribution::ExtensionDesiredState;
use crate::api::types::AgentTaskHistoryItem;
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

pub(crate) struct AgentState {
    pub worker_status: Arc<Mutex<LocalWorkerStatus>>,
    pub approval_manager: Arc<ApprovalManager>,
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
        state.approval_manager.clear_identity()?;
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
pub(crate) fn get_approval_settings(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    match crate::api::oauth::persisted_authorization_identity(&state.state_path) {
        Some((agent_id, user_id)) => state.approval_manager.bind_identity(&user_id, &agent_id)?,
        None => state.approval_manager.clear_identity()?,
    };
    let settings = state.approval_manager.get_settings();
    let effective_r1 = state.approval_manager.effective_mode_for_risk("R1");
    let effective_r2 = state.approval_manager.effective_mode_for_risk("R2");
    let effective_r3 = state.approval_manager.effective_mode_for_risk("R3");
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
        "risk_acknowledged": settings.risk_acknowledged_at > 0,
        "effective_modes": {
            "read": effective_r1,
            "write": effective_r2,
            "high_risk": effective_r3,
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
) -> Result<serde_json::Value, String> {
    state.approval_manager.update_profile(&profile, confirmed)?;
    state
        .approval_manager
        .add_log("warn", &format!("审批档位已调整为: {}", profile.trim()));
    Ok(serde_json::json!({
        "profile": state.approval_manager.get_settings().profile,
        "notification_mode": state.approval_manager.get_settings().notification_mode,
        "owner_user_id": state.approval_manager.get_settings().owner_user_id,
        "agent_id": state.approval_manager.get_settings().agent_id,
        "risk_acknowledged": state.approval_manager.get_settings().risk_acknowledged_at > 0,
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
pub(crate) fn get_remote_execution_settings(
    state: State<'_, AgentState>,
) -> Result<crate::app::remote_execution::RemoteExecutionSettings, String> {
    crate::app::remote_execution::load(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_remote_execution_settings(
    state: State<'_, AgentState>,
    settings: crate::app::remote_execution::RemoteExecutionSettings,
    full_access_confirmed: Option<bool>,
) -> Result<crate::app::remote_execution::RemoteExecutionSettings, String> {
    let current =
        crate::app::remote_execution::load(&state.state_path).map_err(|error| error.to_string())?;
    let entering_full_access = settings.access_mode
        == crate::app::remote_execution::ACCESS_MODE_FULL_ACCESS
        && (current.access_mode != crate::app::remote_execution::ACCESS_MODE_FULL_ACCESS
            || (!current.enabled && settings.enabled));
    if entering_full_access && full_access_confirmed != Some(true) {
        return Err("启用完全访问此电脑必须在本机明确确认".to_string());
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
pub(crate) fn get_remote_clients(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    remote_clients::overview(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn detect_remote_clients(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    remote_clients::overview(&state.state_path).map_err(|error| error.to_string())
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
) -> Result<BuiltinAIRuntimeInstallationStatus, String> {
    let operation = operation
        .unwrap_or_else(|| "install".to_string())
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        operation.as_str(),
        "install" | "update" | "repair" | "uninstall"
    ) {
        return Err("不支持的 HiMind AI 运行时操作".to_string());
    }
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
pub(crate) fn window_start_dragging(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn window_toggle_maximize(window: WebviewWindow) -> Result<(), String> {
    let maximized = window.is_maximized().map_err(|error| error.to_string())?;
    if maximized {
        window.unmaximize().map_err(|error| error.to_string())
    } else {
        window.maximize().map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub(crate) fn window_close(window: WebviewWindow) -> Result<(), String> {
    // Keep the Agent resident in the tray, matching the native close behavior.
    window.hide().map_err(|error| error.to_string())
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
pub(crate) fn get_plugin_registry(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    registry_json_for_control_plane(state.options.mode().control_plane_enabled())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_sources(
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::settings().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn add_extension_source(
    name: String,
    repository: String,
    reference: String,
    catalog_path: Option<String>,
    verification: Option<String>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::add_github_source(
        &name,
        &repository,
        &reference,
        catalog_path.as_deref(),
        verification.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn update_extension_source(
    source_id: String,
    enabled: bool,
    auto_update: bool,
    verification: Option<String>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::update_source(
        &source_id,
        enabled,
        auto_update,
        verification.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn remove_extension_source(
    source_id: String,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::remove_source(&source_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_source_snapshot(
) -> Result<crate::app::extension_source::ExtensionSourceSnapshot, String> {
    crate::app::extension_source::refresh_snapshot().map_err(|error| error.to_string())
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
    crate::app::plugin_manager::install_local_package(&path).map_err(|error| error.to_string())?;
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
pub(crate) fn get_extension_desired_state(
    state: State<'_, AgentState>,
) -> Result<ExtensionDesiredState, String> {
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
}

#[tauri::command]
pub(crate) fn get_agent_task_history(
    state: State<'_, AgentState>,
    limit: Option<usize>,
) -> Result<Vec<AgentTaskHistoryItem>, String> {
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
}

#[tauri::command]
pub(crate) fn get_agent_capabilities(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::capability::types::CapabilityDescriptor>, String> {
    let gateway = CapabilityGateway::new(state.options.clone(), Arc::clone(&state.worker_status));
    gateway
        .list_capabilities(&InvocationContext::tauri())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn list_ai_services(state: State<'_, AgentState>) -> Result<serde_json::Value, String> {
    let custom = crate::store::ai_services::public_snapshot().map_err(|e| e.to_string())?;
    let clients = crate::app::ai_provider_import::status(&state.options);
    let managed = if state.options.mode().dashboard_enabled() {
        // 使用当前本机绑定用户做摘要一致性校验；未授权时返回 available:false。
        let user_id = crate::app::identity::identity_status(&state.options).user_id;
        crate::api::ai::managed_ai_service_summary(&state.options, &user_id)
    } else {
        serde_json::json!({ "available": false, "reason": "independent" })
    };
    Ok(json!({
        "custom": custom,
        "managed": managed,
        "clients": serde_json::to_value(clients).map_err(|e| e.to_string())?,
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
        state
            .approval_manager
            .add_log("info", &format!("已删除自定义 AI 服务: {id}"));
    }
    Ok(removed)
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
    let gateway = CapabilityGateway::new(state.options.clone(), Arc::clone(&state.worker_status));
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
    let gateway = CapabilityGateway::new(state.options.clone(), Arc::clone(&state.worker_status));
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
    crate::skill::capability_facts_from_gateway(
        &state.options,
        Arc::clone(&state.worker_status),
        &InvocationContext::tauri(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_skill_catalog(state: State<'_, AgentState>) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    catalog_json(VERSION, "codex", &capability_facts).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn import_local_skill() -> Result<serde_json::Value, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入本地 HiMind Skill")
        .add_filter("HiMind Skill", &["hmskill"])
        .pick_file()
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("选择 HiMind Skill 目录")
                .pick_folder()
        })
    else {
        return Err("已取消导入 Skill".to_string());
    };
    let record = crate::app::skill_manager::install_local_package(&path)
        .map_err(|error| error.to_string())?;
    serde_json::to_value(record).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_github_skill(
    repository: String,
    reference: String,
    subpath: Option<String>,
) -> Result<serde_json::Value, String> {
    crate::app::github_source::import_skill(
        &repository,
        &reference,
        subpath.as_deref().unwrap_or(""),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_github_skill_url(source_url: String) -> Result<serde_json::Value, String> {
    crate::app::github_source::import_skill(&source_url, "", "").map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_organization_skill_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    merged_skill_catalog(&state)
}

#[tauri::command]
pub(crate) fn query_organization_skill_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::SkillCatalogPage, String> {
    let items = filter_skill_catalog(merged_skill_catalog(&state)?, &q, &category);
    Ok(catalog_page(items, page, page_size))
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
pub(crate) fn select_extension_workspace(
) -> Result<crate::extension_workspace::ExtensionWorkspaceSettings, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind 扩展聚合仓库")
        .pick_folder()
    else {
        return Err("已取消选择扩展聚合仓库".to_string());
    };
    crate::extension_workspace::select(&path).map_err(|error| error.to_string())
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
        .add_filter("HiMind Skill 包", &["hmskill"])
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
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let source_item = merged_skill_catalog(&state)?
        .into_iter()
        .find(|item| item.skill_id == skill_id && item.source.starts_with("github:"));
    if source_item.is_some() {
        let (catalog_item, record) =
            crate::app::extension_source::install_skill(&skill_id, version.as_deref())
                .map_err(|error| error.to_string())?;
        let capability_facts = skill_capability_facts(&state)?;
        let rendered =
            crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts)
                .map_err(|error| error.to_string())?;
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
    let (catalog_item, record) = crate::app::skill_manager::install_with_dependencies(
        &state.options,
        &agent_id,
        &skill_id,
        version.as_deref(),
        optional_plugin_ids.as_deref().unwrap_or_default(),
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
    state: State<'_, AgentState>,
) -> Result<crate::app::skill_manager::SkillInstallPlan, String> {
    let source_item = merged_skill_catalog(&state)?
        .into_iter()
        .find(|item| item.skill_id == skill_id && item.source.starts_with("github:"));
    if source_item.is_some() {
        return crate::app::extension_source::plan_skill(&skill_id, version.as_deref())
            .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::app::skill_manager::plan_install(
        &state.options,
        &agent_id,
        &skill_id,
        version.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_skill_versions(
    skill_id: String,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    if merged_skill_catalog(&state)?
        .into_iter()
        .any(|item| item.skill_id == skill_id && item.source.starts_with("github:"))
    {
        return crate::app::extension_source::skill_versions(&skill_id)
            .map_err(|error| error.to_string());
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
    let store = crate::skill::store::SkillStore::new();
    let removed = store
        .remove_installed_skill(&skill_id)
        .map_err(|error| error.to_string())?;
    crate::app::plugin_manager::remove_owner_references(&format!("skill:{skill_id}"));
    Ok(serde_json::json!({
        "client_id": "agent",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
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
    if let Some(object) = codex.as_object_mut() {
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

fn merged_plugin_catalog(
    state: &AgentState,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    let source_snapshot =
        crate::app::extension_source::snapshot().map_err(|error| error.to_string())?;
    let mut items = source_snapshot
        .plugins
        .into_iter()
        .map(|item| (item.plugin_id.clone(), item))
        .collect::<HashMap<_, _>>();
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
                    for item in catalog {
                        items.insert(item.plugin_id.clone(), item);
                    }
                }
                Err(error) if items.is_empty() => return Err(error.to_string()),
                Err(_) => {}
            }
        } else if items.is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
    }
    let mut result = items.into_values().collect::<Vec<_>>();
    result.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    Ok(result)
}

fn merged_skill_catalog(
    state: &AgentState,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    let source_snapshot =
        crate::app::extension_source::snapshot().map_err(|error| error.to_string())?;
    let mut items = source_snapshot
        .skills
        .into_iter()
        .map(|item| (item.skill_id.clone(), item))
        .collect::<HashMap<_, _>>();
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
                    for item in catalog {
                        items.insert(item.skill_id.clone(), item);
                    }
                }
                Err(error) if items.is_empty() => return Err(error.to_string()),
                Err(_) => {}
            }
        } else if items.is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".to_string());
        }
    }
    let mut result = items.into_values().collect::<Vec<_>>();
    result.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
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
    CapabilityGateway::new(state.options.clone(), Arc::clone(&state.worker_status))
        .invoke(
            &InvocationContext::tauri(),
            "system.open_folder",
            json!({ "path": path }),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_plugin_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    merged_plugin_catalog(&state)
}

#[tauri::command]
pub(crate) fn query_plugin_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::PluginCatalogPage, String> {
    let items = filter_plugin_catalog(merged_plugin_catalog(&state)?, &q, &category);
    Ok(catalog_page(items, page, page_size))
}

#[tauri::command]
pub(crate) fn get_plugin_versions(
    plugin_id: String,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
    if merged_plugin_catalog(&state)?
        .into_iter()
        .any(|item| item.plugin_id == plugin_id && item.source.starts_with("github:"))
    {
        return crate::app::extension_source::plugin_versions(&plugin_id)
            .map_err(|error| error.to_string());
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
) -> Result<crate::app::plugin_manager::PluginInstallPlan, String> {
    if merged_plugin_catalog(&state)?
        .iter()
        .any(|item| item.plugin_id == plugin_id && item.source.starts_with("github:"))
    {
        return crate::app::extension_source::plan_plugin(&plugin_id, version.as_deref())
            .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let snapshot = local_worker_snapshot(&state.worker_status);
    let agent_id = snapshot
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    crate::app::plugin_manager::plan_install(
        &state.options,
        agent_id,
        &plugin_id,
        version.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn install_plugin(
    state: State<'_, AgentState>,
    plugin_id: String,
    version: Option<String>,
) -> Result<(), String> {
    if merged_plugin_catalog(&state)?
        .iter()
        .any(|item| item.plugin_id == plugin_id && item.source.starts_with("github:"))
    {
        return crate::app::extension_source::install_plugin(&plugin_id, version.as_deref())
            .map(|_| ())
            .map_err(|error| error.to_string());
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let previous = crate::app::plugin_manager::local_status(&plugin_id).current_version;
    let result = crate::app::plugin_manager::install(
        &state.options,
        &agent_id,
        &plugin_id,
        version.as_deref(),
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
pub(crate) fn open_plugin_view(
    app: AppHandle,
    plugin_id: String,
    view_id: String,
) -> Result<(), String> {
    super::ui::open_plugin_view(&app, &plugin_id, &view_id)
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
pub(crate) fn invoke_plugin_view_capability(
    window: WebviewWindow,
    state: State<'_, AgentState>,
    capability_id: String,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = crate::capability::plugin::scan_plugins()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|plugin| {
            plugin.enabled
                && plugin.views.iter().any(|view| {
                    super::ui::plugin_view_window_label(&plugin.id, &view.id) == window.label()
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

    let options = Options {
        api_base: state.dashboard_base.clone(),
        state_path: state.state_path.clone(),
        effective_mode: state.options.mode(),
        once: false,
        interval_seconds: 10,
        local_app: true,
        local_port: state.port,
        reenroll: false,
        enrollment_token: std::env::var("HIMIND_AGENT_ENROLLMENT_TOKEN").unwrap_or_default(),
        agent_credential: Arc::new(std::sync::RwLock::new(String::new())),
        identity_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        platform_access: Arc::new(std::sync::RwLock::new(None)),
        task_execution: Arc::new(std::sync::RwLock::new(None)),
    };
    CapabilityGateway::new(options, Arc::clone(&state.worker_status))
        .invoke(
            &InvocationContext::new(
                crate::capability::types::InvocationSource::Tauri,
                format!("plugin-view:{}", plugin.id),
            ),
            &capability_id,
            input,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::present_builtin_ai_start_error;

    #[test]
    fn connected_ai_errors_keep_existing_login_guidance() {
        assert_eq!(
            present_builtin_ai_start_error("AI credential missing scope"),
            "需要登录 HiMind 账号后才能开始对话"
        );
    }
}
