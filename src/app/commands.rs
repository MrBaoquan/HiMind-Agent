use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    is_agent_auto_start_enabled, local_agent_executable_metadata, open_agent_install_directory,
    open_folder, open_url, set_agent_auto_start,
};
use crate::approval::manager::ApprovalManager;
use crate::capability::plugin::registry_json;
use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::remote::client::inner_admin_base;
use crate::store::credentials::{
    clear_local_inner_admin_credentials, local_login_status_json,
    save_local_inner_admin_credentials,
};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

pub(crate) struct AgentState {
    pub worker_status: Arc<Mutex<LocalWorkerStatus>>,
    pub approval_manager: Arc<ApprovalManager>,
    pub port: u16,
    pub dashboard_base: String,
    pub state_path: PathBuf,
}

#[tauri::command]
pub(crate) fn get_agent_status(state: State<'_, AgentState>) -> Result<serde_json::Value, String> {
    let worker = local_worker_snapshot(&state.worker_status);
    let executable = local_agent_executable_metadata();
    let pending = state.approval_manager.list_pending();
    let login = local_login_status_json();

    Ok(json!({
        "status": "online",
        "version": VERSION,
        "mode": "local-app",
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
        "local_service_online": worker["local_service_online"],
        "local_service_error": worker["local_service_error"],
        "pending_approvals": pending.len(),
    }))
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
    let settings = state.approval_manager.get_settings();
    let auto_start =
        is_agent_auto_start_enabled(&state.dashboard_base, state.port, &state.state_path)
            .unwrap_or(false);
    Ok(json!({
        "rules": settings.rules,
        "timeout_seconds": settings.timeout_seconds,
        "auto_start": auto_start,
    }))
}

#[tauri::command]
pub(crate) fn set_approval_rule(
    state: State<'_, AgentState>,
    request_type: String,
    mode: String,
) -> Result<(), String> {
    state.approval_manager.update_rule(&request_type, &mode)
}

#[tauri::command]
pub(crate) fn set_approval_timeout(
    state: State<'_, AgentState>,
    seconds: u64,
) -> Result<(), String> {
    state.approval_manager.update_timeout(seconds)
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
pub(crate) fn get_plugin_registry() -> Result<serde_json::Value, String> {
    registry_json().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_agent_capabilities(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::capability::types::CapabilityDescriptor>, String> {
    let options = Options {
        api_base: state.dashboard_base.clone(),
        state_path: state.state_path.clone(),
        once: false,
        interval_seconds: 10,
        local_app: true,
        local_port: state.port,
        enrollment_token: std::env::var("HIMIND_AGENT_ENROLLMENT_TOKEN")
            .unwrap_or_default(),
        agent_credential: Arc::new(std::sync::RwLock::new(String::new())),
    };
    let gateway = CapabilityGateway::new(options, Arc::clone(&state.worker_status));
    gateway
        .list_capabilities(&InvocationContext::tauri())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn open_plugin_directory() -> Result<(), String> {
    let registry = registry_json().map_err(|e| e.to_string())?;
    let path = registry
        .get("registry_dir")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "plugin registry directory is unavailable".to_string())?;
    open_folder(path).map_err(|e| e.to_string())
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
    window.destroy().map_err(|error| error.to_string())
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
        once: false,
        interval_seconds: 10,
        local_app: true,
        local_port: state.port,
        enrollment_token: std::env::var("HIMIND_AGENT_ENROLLMENT_TOKEN")
            .unwrap_or_default(),
        agent_credential: Arc::new(std::sync::RwLock::new(String::new())),
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
