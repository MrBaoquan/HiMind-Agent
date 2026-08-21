use serde_json::json;
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
use crate::capability::plugin::registry_json;
use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::remote::client::inner_admin_base;
use crate::skill::{catalog_json, codex_repair_json};
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

#[tauri::command]
pub(crate) async fn get_dashboard_identity_status(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardIdentityStatus, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || crate::app::identity::identity_status(&options))
        .await
        .map_err(|error| error.to_string())
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
    Ok(crate::app::identity::authorization_progress(
        &state.dashboard_authorization,
    ))
}

#[tauri::command]
pub(crate) fn cancel_dashboard_authorization(
    state: State<'_, AgentState>,
) -> Result<crate::app::identity::DashboardAuthorizationProgress, String> {
    crate::app::identity::cancel_authorization(&state.dashboard_authorization)
}

#[tauri::command]
pub(crate) fn open_dashboard_authorization_page(
    state: State<'_, AgentState>,
) -> Result<(), String> {
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
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::api::oauth::revoke_authorization(&options).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    state
        .approval_manager
        .add_log("info", "已退出 Dashboard 账号授权");
    Ok(())
}

#[tauri::command]
pub(crate) fn get_ai_integration_overview(
    state: State<'_, AgentState>,
) -> Result<crate::app::ai_clients::AiIntegrationOverview, String> {
    crate::app::ai_clients::overview(&state.options).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn register_ai_client_mcp_server(
    state: State<'_, AgentState>,
    client_id: String,
    reset_invalid: Option<bool>,
) -> Result<crate::app::ai_clients::AiClientConfigurationResult, String> {
    let result = crate::app::ai_clients::configure(
        &state.options,
        &client_id,
        reset_invalid.unwrap_or(false),
    )
    .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已注册 AI 客户端 MCP 服务: {client_id}"));
    Ok(result)
}

#[tauri::command]
pub(crate) fn unregister_ai_client_mcp_server(
    state: State<'_, AgentState>,
    client_id: String,
) -> Result<crate::app::ai_clients::AiClientConfigurationResult, String> {
    let result = crate::app::ai_clients::remove_configuration(&state.options, &client_id)
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!("已取消注册 AI 客户端 MCP 服务: {client_id}"),
    );
    Ok(result)
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
        "current_task": current_task,
    }))
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
        "editors": local_unity_editor_settings().map_err(|error| error.to_string())?,
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
) -> Result<String, String> {
    if !crate::runtime::builtin::status().compatible {
        return Err("HiMind AI 运行时尚未安装，请先安装 HiMind AI 运行时".to_string());
    }
    let options = state.options.clone();
    let logs = Arc::clone(&state.approval_manager);
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::app::ui::start_builtin_ai_session(&options)
    })
    .await
    .map_err(|error| error.to_string())?;
    match result {
        Ok(session_url) => {
            logs.add_log("info", "HiMind AI 会话已启动");
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
) -> Result<Vec<crate::app::mcp_settings::McpServerConfig>, String> {
    crate::app::mcp_settings::load(&state.state_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_builtin_ai_mcp_server(
    state: State<'_, AgentState>,
    server: crate::app::mcp_settings::McpServerConfig,
) -> Result<crate::app::mcp_settings::McpServerConfig, String> {
    let server = crate::app::mcp_settings::upsert(&state.state_path, server)
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
    let removed = crate::app::mcp_settings::remove(&state.state_path, &server_name)
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
    server: crate::app::mcp_settings::McpServerConfig,
) -> Result<(), String> {
    crate::app::mcp_settings::validate_config(&server)
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
pub(crate) fn get_plugin_registry() -> Result<serde_json::Value, String> {
    registry_json().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_desired_state(
    state: State<'_, AgentState>,
) -> Result<ExtensionDesiredState, String> {
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
pub(crate) fn get_organization_skill_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::app::skill_manager::catalog(&state.options, &agent_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn query_organization_skill_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::SkillCatalogPage, String> {
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
    crate::api::distribution::skill_catalog_page(
        &client,
        &state.dashboard_base,
        agent_id,
        &credential,
        &q,
        &category,
        page.clamp(1, 10_000),
        page_size.clamp(1, 100),
    )
    .map_err(|error| error.to_string())
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
pub(crate) fn open_extension_project() -> Result<crate::extension_projects::ExtensionProject, String>
{
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 HiMind 插件或技能项目")
        .pick_folder()
    else {
        return Err("已取消打开扩展项目".to_string());
    };
    crate::extension_projects::register(&path).map_err(|error| error.to_string())
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
    let identity = crate::app::identity::identity_status(&state.options);
    if !identity.authorized || identity.user_name.trim().is_empty() {
        return Err("请先授权 HiMind 工作台账号，再新建扩展项目".to_string());
    }
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
pub(crate) fn remove_extension_project(project_id: String) -> Result<(), String> {
    crate::extension_projects::remove(&project_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_extension_collaboration_projects(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::AgentExtensionProject>, String> {
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
    state: State<'_, AgentState>,
) -> Result<crate::skill::authoring::AuthoringDraft, String> {
    let identity = crate::app::identity::identity_status(&state.options);
    if !identity.authorized || identity.user_name.trim().is_empty() {
        return Err("请先授权 HiMind 工作台账号，再导入 Skill 候选".to_string());
    }
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
    let mut codex = clients
        .get("codex")
        .cloned()
        .ok_or_else(|| "该 Skill 未声明支持 Codex".to_string())?;
    if let Some(object) = codex.as_object_mut() {
        object.insert("clients".to_string(), serde_json::json!(clients));
    }
    Ok(codex)
}

#[tauri::command]
pub(crate) fn repair_codex_skill(
    skill_id: String,
    preserve_modified: Option<bool>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let capability_facts = skill_capability_facts(&state)?;
    codex_repair_json(
        &skill_id,
        preserve_modified.unwrap_or(true),
        VERSION,
        &capability_facts,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn uninstall_codex_skill(skill_id: String) -> Result<serde_json::Value, String> {
    let removed = crate::skill::uninstall_supported_clients_json(&skill_id)
        .map_err(|error| error.to_string())?;
    crate::app::plugin_manager::remove_owner_references(&format!("skill:{skill_id}"));
    let mut codex = removed
        .get("clients")
        .and_then(|clients| clients.get("codex"))
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "client_id": "codex",
                "removed": {"skill_id": skill_id, "removed": false}
            })
        });
    if let Some(object) = codex.as_object_mut() {
        object.insert(
            "clients".to_string(),
            removed.get("clients").cloned().unwrap_or_default(),
        );
    }
    Ok(codex)
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

#[tauri::command]
pub(crate) fn open_folder(path: String) -> Result<(), String> {
    open_system_folder(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_plugin_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
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
    crate::api::distribution::plugin_catalog(&client, &state.dashboard_base, agent_id, &credential)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn query_plugin_catalog(
    q: String,
    category: String,
    page: usize,
    page_size: usize,
    state: State<'_, AgentState>,
) -> Result<crate::api::distribution::PluginCatalogPage, String> {
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
    crate::api::distribution::plugin_catalog_page(
        &client,
        &state.dashboard_base,
        agent_id,
        &credential,
        &q,
        &category,
        page.clamp(1, 10_000),
        page_size.clamp(1, 100),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_plugin_versions(
    plugin_id: String,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::PluginCatalogItem>, String> {
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
    let result = crate::capability::plugin::invoke_plugin_capability_for_plugin(
        &plugin_id,
        &capability_id,
        input,
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
