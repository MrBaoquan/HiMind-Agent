use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::error::Error;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

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
    pub state_path: PathBuf,
    pub options: Options,
    pub dashboard_authorization: Arc<Mutex<crate::app::identity::DashboardAuthorizationFlow>>,
}

#[tauri::command]
pub(crate) fn list_experts() -> Result<Vec<crate::expert::ExpertSummary>, String> {
    crate::expert::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn active_expert(
    workspace_root: Option<String>,
) -> Result<Option<crate::expert::ExpertActivation>, String> {
    let workspace = workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from);
    crate::expert::active_for_workspace(workspace.as_deref()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn activate_expert(
    expert_id: String,
    version: Option<String>,
    workspace_root: Option<String>,
) -> Result<crate::expert::ExpertActivation, String> {
    let workspace = workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from);
    crate::expert::activate(&expert_id, version.as_deref(), workspace.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_expert(
    input: crate::expert::ExpertDraftInput,
) -> Result<crate::expert::ExpertSummary, String> {
    crate::expert::save(input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn pick_expert_package() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 HiMind 专家包")
        .add_filter("HiMind 专家包", &["hmexpert", "zip"])
        .pick_file()
        .map(|path| crate::extension_workspace::display_path(&path))
}

#[tauri::command]
pub(crate) fn import_expert_package(path: String) -> Result<crate::expert::ExpertSummary, String> {
    crate::expert::import_package(std::path::Path::new(path.trim()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn export_expert_package(
    expert_id: String,
    version: String,
) -> Result<crate::expert::ExpertPackageResult, String> {
    let file_name = format!("{}-{}.hmexpert", expert_id.trim(), version.trim());
    let Some(destination) = rfd::FileDialog::new()
        .set_title("导出专家包")
        .set_file_name(&file_name)
        .add_filter("HiMind 专家包", &["hmexpert"])
        .save_file()
    else {
        return Err("已取消导出".into());
    };
    crate::expert::export_package(&expert_id, &version, &destination)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn project_expert_to_client(
    expert_id: String,
    version: Option<String>,
    client_id: String,
    workspace_root: String,
) -> Result<crate::expert::ExpertProjectionReceipt, String> {
    crate::expert::project_to_client(
        &expert_id,
        version.as_deref(),
        &client_id,
        std::path::Path::new(workspace_root.trim()),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn materialize_expert_project(
    workspace_root: String,
    expert_id: String,
    version: Option<String>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let parent = crate::extension_workspace::validate_authoring_root(workspace_root.trim())?;
    crate::extension_projects::materialize_expert_project(&parent, &expert_id, version.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn materialize_instruction_project(
    workspace_root: String,
    instruction_pack_id: String,
    version: Option<String>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let parent = crate::extension_workspace::validate_authoring_root(workspace_root.trim())?;
    crate::extension_projects::materialize_instruction_project(
        &parent,
        &instruction_pack_id,
        version.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn list_expert_drafts() -> Result<Vec<crate::expert::ExpertAuthoringDraft>, String> {
    crate::expert_authoring::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn test_expert_draft(
    expert_id: String,
    version: String,
) -> Result<crate::expert::ExpertAuthoringDraft, String> {
    crate::expert_authoring::test(&expert_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn confirm_expert_draft(
    expert_id: String,
    version: String,
) -> Result<crate::expert::ExpertAuthoringDraft, String> {
    crate::expert_authoring::confirm(&expert_id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn submit_expert_draft(
    expert_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<crate::expert::ExpertAuthoringDraft, String> {
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::expert_authoring::submit(&state.options, &agent_id, &expert_id, &version)
        .map_err(|error| error.to_string())
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
        return Err("HiMind 账号尚未授权".to_string());
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
        match status.state.as_str() {
            // 读取本地授权文件失败（破损、被占用、正在被其它进程重写）不等于账号
            // 被撤销。以前这里会顺手 clear_identity()，于是瞬时的读失败会把本地
            // 授权姿态整块抹掉，用户被迫重新授权——一次读不到文件就清绑定是本末
            // 倒置。这里保留既有绑定，由 UI 提示「本地授权异常」并引导重新授权。
            "invalid_local_authorization" => {}
            _ if !status.user_id.trim().is_empty() && !status.agent_id.trim().is_empty() => {
                manager.bind_identity(&status.user_id, &status.agent_id)?;
            }
            _ => {
                // 只有工作台明确回答了「没有授权」时才清理本地绑定。
                manager.clear_identity()?;
            }
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
            state.options.api_base().trim_end_matches('/')
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
    ensure_dashboard_mode_for_authorization(&state)?;
    crate::app::identity::start_authorization(
        state.options.clone(),
        Arc::clone(&state.dashboard_authorization),
        Arc::clone(&state.approval_manager),
    )
}

/// 授权即对接：账号授权是用户侧唯一的开关。
///
/// 独立模式下点「授权」先切回对接状态再发起设备授权；否则用户「取消授权」后
/// 就一直停在独立模式，授权按钮每次都按 `control_plane_required` 失败，形成死路。
fn ensure_dashboard_mode_for_authorization(state: &AgentState) -> Result<(), String> {
    if state.options.mode().dashboard_enabled() {
        return Ok(());
    }
    crate::app::runtime_mode::save(
        &state.state_path,
        crate::app::runtime_mode::AgentMode::Connected,
    )
    .map_err(|error| error.to_string())?;
    state
        .options
        .set_mode(crate::app::runtime_mode::AgentMode::Connected);
    // 独立模式下启动的会话沿用本机配置，切换对接状态后收掉，避免它继续用旧身份。
    crate::app::ui::stop_builtin_ai_process();
    state
        .approval_manager
        .add_log("info", "已开启 AI 工作台对接");
    Ok(())
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
    // 取消授权即终止对接：授权是唯一的用户侧控制，取消后立即停止任务接收
    // 与运行记录同步，重启后也不会自动恢复。
    let _ = crate::app::runtime_mode::save(
        &state.state_path,
        crate::app::runtime_mode::AgentMode::Independent,
    );
    state
        .options
        .set_mode(crate::app::runtime_mode::AgentMode::Independent);
    state
        .approval_manager
        .add_log("info", "已取消 AI 工作台授权，停止对接");
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
pub(crate) async fn get_mcp_targets(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::app::mcp_targets::McpTargetDescriptor>, String> {
    // 探测本机 AI 客户端要遍历安装目录与配置文件，秒级耗时。同步命令会在
    // WebView2 主线程上跑，期间整条 IPC 队列都会堵住，所以这里必须下沉到
    // 阻塞线程池。
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::mcp_targets::list(&options).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_instruction_targets(
    workspace_root: String,
) -> Result<Vec<crate::instruction_targets::InstructionTargetDescriptor>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::instruction_targets::discover_instruction_targets(std::path::Path::new(
            workspace_root.trim(),
        ))
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_workspace_instruction_context(
    workspace_root: String,
) -> Result<crate::instruction_pack::WorkspaceInstructionContext, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let workspace = crate::extension_workspace::validate_authoring_root(workspace_root.trim())?;
        crate::instruction_pack::workspace_context(&workspace).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn save_workspace_instruction_selection(
    state: State<'_, AgentState>,
    workspace_root: String,
    selected: Vec<crate::instruction_pack::InstructionPackRef>,
) -> Result<crate::instruction_pack::WorkspaceInstructionContext, String> {
    let workspace = crate::extension_workspace::validate_authoring_root(workspace_root.trim())?;
    let result = crate::instruction_pack::save_workspace_selection(&workspace, selected)
        .map_err(|error| error.to_string())?;
    // Instruction context is immutable for a running DSH process. Stop only
    // this workspace so other open workspaces keep their sessions untouched.
    crate::app::ui::stop_builtin_ai_session(&workspace);
    state.approval_manager.add_log(
        "info",
        &format!(
            "已更新工作区指令选择，会话将在下次启动时加载: {}",
            crate::extension_workspace::display_path(&workspace)
        ),
    );
    Ok(result)
}

#[tauri::command]
pub(crate) async fn inspect_ecc_repository(
    root: String,
) -> Result<crate::ecc_import::EccInspection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::ecc_import::inspect_repository(std::path::Path::new(root.trim()))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn plan_instruction_projection(
    workspace_root: String,
    target: crate::instruction_projection::ProjectionTarget,
) -> Result<crate::instruction_projection::ProjectionPlan, String> {
    use crate::instruction_projection::InstructionAdapter;
    tauri::async_runtime::spawn_blocking(move || {
        let workspace = std::path::Path::new(workspace_root.trim());
        let home = crate::instruction_targets::instruction_home_for_client(&target.client_id);
        let overlays = crate::instruction_projection::overlays_from_target(&target)
            .map_err(|error| error.to_string())?;
        let snapshot =
            crate::workspace_instructions::resolve_with_overlays(workspace, &home, &overlays)
                .map_err(|error| error.to_string())?;
        let adapter = crate::instruction_projection::ManagedMarkdownAdapter::himind(
            target.adapter_id.clone(),
        );
        let mut observation = adapter
            .inspect(&target)
            .map_err(|error| error.to_string())?;
        if let Some(receipt) = crate::instruction_targets::load_projection_receipt(&target)
            .map_err(|error| error.to_string())?
        {
            observation.expected_managed_digest = receipt.managed_digest;
        }
        adapter
            .plan(&snapshot, &target, &observation)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn apply_instruction_projection(
    state: State<'_, AgentState>,
    plan: crate::instruction_projection::ProjectionPlan,
) -> Result<crate::instruction_projection::ProjectionReceipt, String> {
    use crate::instruction_projection::InstructionAdapter;
    let adapter =
        crate::instruction_projection::ManagedMarkdownAdapter::himind(plan.adapter_id.clone());
    let receipt = adapter.apply(&plan).map_err(|error| error.to_string())?;
    crate::instruction_targets::save_projection_receipt(&receipt)
        .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已应用工作区指令投影: {}", plan.client_id));
    Ok(receipt)
}

#[tauri::command]
pub(crate) fn rollback_instruction_projection(
    state: State<'_, AgentState>,
    receipt: crate::instruction_projection::ProjectionReceipt,
) -> Result<(), String> {
    use crate::instruction_projection::InstructionAdapter;
    let adapter =
        crate::instruction_projection::ManagedMarkdownAdapter::himind(receipt.adapter_id.clone());
    adapter
        .rollback(&receipt)
        .map_err(|error| error.to_string())?;
    crate::instruction_targets::remove_projection_receipt(&receipt)
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!("已回滚工作区指令投影: {}", receipt.client_id),
    );
    Ok(())
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
        "dashboard_base": state.options.api_base(),
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
        .ok_or_else(|| "AI 工作台开关只能是 connected 或 independent".to_string())?;
    crate::app::runtime_mode::save(&state.state_path, mode).map_err(|error| error.to_string())?;
    // 立即作用于运行时：Worker、投影与能力可见性都读同一个共享状态，
    // 因此不需要重启 Agent。
    state.options.set_mode(mode);
    if previous != mode {
        // 切换对接状态时收掉按旧策略启动的会话，避免它继续沿用旧的身份。
        crate::app::ui::stop_builtin_ai_process();
    }
    state.approval_manager.add_log(
        "info",
        if mode.dashboard_enabled() {
            "已开启 AI 工作台对接"
        } else {
            "已关闭 AI 工作台对接，Agent 继续在本机运行"
        },
    );
    Ok(AgentModeSettings {
        mode: mode.as_str().to_string(),
        effective_mode: mode.as_str().to_string(),
        pending_mode: mode.as_str().to_string(),
        dashboard_enabled: mode.dashboard_enabled(),
        // 开关直接作用于运行时，不再存在“重启后生效”。
        requires_restart: false,
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
        is_agent_auto_start_enabled(&state.options.api_base(), state.port, &state.state_path)
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
    open_url(&state.options.api_base()).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn start_builtin_ai_session(
    state: State<'_, AgentState>,
    project_id: Option<String>,
    workspace_root: Option<String>,
) -> Result<String, String> {
    if !crate::runtime::builtin::status().compatible {
        return Err("HiMind AI 运行时尚未安装，请先安装 HiMind AI 运行时".to_string());
    }
    if project_id.is_some() && workspace_root.is_some() {
        return Err("不能同时指定扩展项目和扩展工作区目录".to_string());
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
    // 工作区按目录寻址：调用方说得出目录就用它，不再读「当前工作区」这个全局
    // 单值 —— 那正是并发场景下 A 会话把 B 会话的目录传下去的原因。
    let requested_root = workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let workspace = if let Some(root) = requested_root {
        Some(crate::extension_workspace::validate_authoring_root(root)?)
    } else {
        project
            .as_ref()
            .map(|item| PathBuf::from(&item.workspace_path))
    };
    let project_name = project.as_ref().map(|item| item.name.clone());
    let options = state.options.clone();
    let logs = Arc::clone(&state.approval_manager);
    let log_workspace = workspace.clone();
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
                    .unwrap_or_else(|| match log_workspace.as_deref() {
                        Some(root) => format!(
                            "HiMind AI 已进入扩展工作区: {}",
                            crate::extension_workspace::display_path(root)
                        ),
                        None => "HiMind AI 会话已启动".to_string(),
                    }),
            );
            if let Some(notice) = log_workspace
                .as_deref()
                .and_then(|root| crate::app::ui::current_builtin_ai_notice(Some(root)))
            {
                logs.add_log("warn", &notice);
            }
            Ok(session_url)
        }
        Err(error) => {
            logs.add_log("error", &format!("HiMind AI 会话启动失败: {error}"));
            Err(present_builtin_ai_start_error(&error))
        }
    }
}

#[tauri::command]
pub(crate) fn get_builtin_ai_session_notice(workspace_root: Option<String>) -> Option<String> {
    let requested = workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    crate::app::ui::current_builtin_ai_notice(requested.as_deref())
}

/// 当前在跑的 HiMind AI 会话（按工作区各一条），界面用来恢复标签页。
#[tauri::command]
pub(crate) fn list_builtin_ai_sessions() -> Vec<crate::app::ui::BuiltinAiSessionSnapshot> {
    crate::app::ui::builtin_ai_session_snapshots()
}

/// 关闭一个工作区的会话，其它工作区的会话继续跑。
#[tauri::command]
pub(crate) fn stop_builtin_ai_session(workspace_root: String) -> Result<bool, String> {
    let root = crate::extension_workspace::validate_authoring_root(&workspace_root)?;
    Ok(crate::app::ui::stop_builtin_ai_session(&root))
}

#[tauri::command]
pub(crate) async fn open_builtin_ai_web(
    state: State<'_, AgentState>,
    project_id: Option<String>,
    workspace_root: Option<String>,
) -> Result<String, String> {
    let session_url = start_builtin_ai_session(state, project_id, workspace_root).await?;
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
pub(crate) fn get_mcp_runtime_requirements() -> serde_json::Value {
    crate::app::mcp_probe::probe_requirements()
}

#[tauri::command]
pub(crate) fn get_mcp_catalog(
    state: State<'_, AgentState>,
) -> crate::app::mcp_catalog::CatalogView {
    crate::app::mcp_catalog::view(&state.state_path)
}

#[tauri::command]
pub(crate) async fn refresh_mcp_catalog(
    state: State<'_, AgentState>,
) -> Result<crate::app::mcp_catalog::CatalogView, String> {
    // 刷新要联网，必须离开主线程，否则窗口会卡住。
    let state_path = state.state_path.clone();
    tauri::async_runtime::spawn_blocking(move || crate::app::mcp_catalog::refresh(&state_path))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn install_mcp_catalog_entry(
    state: State<'_, AgentState>,
    request: crate::app::mcp_catalog::InstallRequest,
) -> Result<crate::app::mcp_registry::McpServerConfig, String> {
    let config = crate::app::mcp_catalog::install(&state.state_path, &request)?;
    crate::app::ui::stop_builtin_ai_process();
    state.approval_manager.add_log(
        "info",
        &format!("已从目录安装 HiMind AI MCP 服务: {}", config.server_name),
    );
    Ok(config)
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
        return "当前账号暂未分配可用模型服务".to_string();
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
pub(crate) async fn open_settings_window(
    app: AppHandle,
    panel: Option<String>,
    section: Option<String>,
    tab: Option<String>,
    ai_tab: Option<String>,
) -> Result<(), String> {
    crate::app::ui::open_settings_window(
        &app,
        panel.as_deref(),
        section.as_deref(),
        tab.as_deref(),
        ai_tab.as_deref(),
    )
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
    // 关闭窗口保留 Agent 常驻托盘，与原生关闭行为一致。
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
        &state.options.api_base(),
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
pub(crate) fn pick_engine_editor(engine: String) -> Result<serde_json::Value, String> {
    let unreal = engine.trim().eq_ignore_ascii_case("unreal");
    let (title, filter) = if unreal {
        ("选择 Unreal 编辑器", "UnrealEditor.exe")
    } else {
        ("选择 Unity 编辑器", "Unity.exe")
    };
    let path = rfd::FileDialog::new()
        .set_title(title)
        .add_filter(filter, &["exe"])
        .pick_file()
        .map(|value| value.to_string_lossy().to_string());
    Ok(json!({ "path": path }))
}

#[tauri::command]
pub(crate) fn save_engine_editor(
    engine: String,
    path: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let label = if engine.trim().eq_ignore_ascii_case("unreal") {
        "Unreal 编辑器"
    } else {
        "Unity 编辑器"
    };
    let settings = crate::store::credentials::save_local_engine_editor_path(&engine, &path)
        .map_err(|error| error.to_string())?;
    let message = if path.trim().is_empty() {
        format!("{label}已恢复为自动发现")
    } else {
        format!("{label}本机覆盖已更新")
    };
    state.approval_manager.add_log("info", &message);
    Ok(settings)
}

#[tauri::command]
pub(crate) fn list_engine_installations() -> serde_json::Value {
    crate::store::credentials::engine_installations_value()
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
pub(crate) fn get_agent_backup_scope() -> Result<serde_json::Value, String> {
    Ok(json!({
        "entries": crate::app::backup::scope_entries(),
        "minPassphraseChars": crate::app::backup::min_passphrase_chars(),
        "format": crate::app::backup::FORMAT_ID,
        "formatVersion": crate::app::backup::FORMAT_VERSION,
    }))
}

#[tauri::command]
pub(crate) fn export_agent_backup(
    state: State<'_, AgentState>,
    passphrase: Option<String>,
    include_device_identity: Option<bool>,
) -> Result<serde_json::Value, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let file_name = format!("himind-agent-backup-{stamp}.zip");
    let Some(destination) = rfd::FileDialog::new()
        .set_title("导出 HiMind Agent 备份包")
        .set_file_name(&file_name)
        .add_filter("HiMind Agent 备份包", &["zip"])
        .save_file()
    else {
        return Ok(json!({ "canceled": true }));
    };

    let passphrase = passphrase.filter(|value| !value.trim().is_empty());
    let request = crate::app::backup::ExportRequest {
        destination,
        passphrase,
        include_device_identity: include_device_identity.unwrap_or(false),
    };
    let report = crate::app::backup::export(&request).map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "info",
        &format!(
            "已导出 Agent 备份包（{} 个文件，{} 项凭据）",
            report.file_count, report.credentials
        ),
    );
    Ok(json!({ "canceled": false, "report": report }))
}

#[tauri::command]
pub(crate) fn inspect_agent_backup(path: Option<String>) -> Result<serde_json::Value, String> {
    let path = match path.filter(|value| !value.trim().is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let Some(picked) = rfd::FileDialog::new()
                .set_title("选择 HiMind Agent 备份包")
                .add_filter("HiMind Agent 备份包", &["zip"])
                .pick_file()
            else {
                return Ok(json!({ "canceled": true }));
            };
            picked
        }
    };
    let report = crate::app::backup::inspect(&path).map_err(|error| error.to_string())?;
    Ok(json!({ "canceled": false, "report": report }))
}

#[tauri::command]
pub(crate) fn import_agent_backup(
    state: State<'_, AgentState>,
    path: Option<String>,
    passphrase: Option<String>,
) -> Result<serde_json::Value, String> {
    let path = match path.filter(|value| !value.trim().is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let Some(picked) = rfd::FileDialog::new()
                .set_title("选择要恢复的 HiMind Agent 备份包")
                .add_filter("HiMind Agent 备份包", &["zip"])
                .pick_file()
            else {
                return Ok(json!({ "canceled": true }));
            };
            picked
        }
    };
    let passphrase = passphrase.filter(|value| !value.trim().is_empty());
    let report = crate::app::backup::restore(&path, passphrase.as_deref())
        .map_err(|error| error.to_string())?;
    state.approval_manager.add_log(
        "warn",
        &format!(
            "已从备份包恢复 {} 个文件，恢复前快照：{}",
            report.restored.len(),
            report.snapshot
        ),
    );
    Ok(json!({ "canceled": false, "report": report }))
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
pub(crate) async fn get_extension_provenance(
) -> Result<Vec<crate::app::extension_source::ExtensionProvenance>, String> {
    // 读本机扩展台账要遍历来源目录，放到阻塞线程池，别占着 WebView 的
    // 主线程把同一时刻的其它请求一起堵住。
    tauri::async_runtime::spawn_blocking(|| {
        crate::app::extension_source::list_provenance().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 列出可更新的扩展并给出批量更新分组。分组依赖本机安装台账，
/// 放在后端算，避免前端再实现一份来源核对逻辑而与安装层不一致。
#[tauri::command]
pub(crate) async fn plan_extension_updates(
) -> Result<Vec<crate::app::extension_source::ExtensionUpdateCandidate>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        crate::app::extension_source::plan_extension_updates().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 一键批量更新。逐项独立成败，并把每一项的进度推给窗口，
/// 让「正在更新第 N/M 项」在长任务里可见。
#[tauri::command]
pub(crate) async fn apply_extension_updates(
    app: AppHandle,
    state: State<'_, AgentState>,
    targets: Vec<crate::app::extension_source::ExtensionUpdateTarget>,
) -> Result<crate::app::extension_source::ExtensionBatchUpdateReport, String> {
    crate::app::extension_source::reset_extension_update_cancel();
    let report = tauri::async_runtime::spawn_blocking(move || {
        crate::app::extension_source::apply_extension_updates(
            &targets,
            crate::app::extension_source::extension_update_cancel_flag(),
            |progress| {
                let _ = app.emit("himind:extension-update-progress", progress);
            },
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    // 技能更新后必须重新投射到本机 AI 客户端（全局目录 / 已投放的项目目录），
    // 否则本机客户端读到的还是旧版本。
    let capability_facts = skill_capability_facts(&state)?;
    for outcome in &report.outcomes {
        if outcome.asset_kind != "skill" || outcome.status != "updated" {
            continue;
        }
        if let Ok(Some(record)) =
            crate::skill::store::SkillStore::new().get_record(&outcome.asset_id)
        {
            let _ =
                crate::skill::sync_record_to_supported_clients(&record, VERSION, &capability_facts);
        }
    }
    let _ = crate::app::extension_source::reconcile_dsh_presets_now();
    Ok(report)
}

/// 批量更新只取消「还没开始安装」的项，不打断正在写入的单个扩展。
#[tauri::command]
pub(crate) fn cancel_extension_updates() {
    crate::app::extension_source::cancel_extension_updates();
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
            return Err("HiMind 账号尚未授权".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::distribution::extension_desired_state(
            &client,
            &state.options.api_base(),
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
            return Err("HiMind 账号尚未授权".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::client::list_task_history(
            &client,
            &state.options.api_base(),
            &agent_id,
            &credential,
            limit.unwrap_or(50).clamp(1, 100),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 本机活动列表：工作流运行与技能运行统一成一份只读视图。
/// 工作台下发任务仍走工作台历史接口，两种模式都能读到本机这部分。
#[tauri::command]
pub(crate) async fn list_local_activity(
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::local_activity::list(limit).map_err(|error| error.to_string())
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

/// 本机推理网关的用量（ADR 0113）。来源是网关台账，与平台口径完全分开：
/// 这里只有 Token 与调用次数，没有金额。
#[tauri::command]
pub(crate) async fn get_local_usage_overview(
    range: Option<String>,
) -> Result<serde_json::Value, String> {
    let days = match range.as_deref().unwrap_or("7d") {
        "today" => 1,
        "30d" => 30,
        _ => 7,
    };
    tauri::async_runtime::spawn_blocking(move || Ok(crate::store::local_usage::overview(days)))
        .await
        .map_err(|error| error.to_string())?
}

/// 网关状态与注入矩阵的只读快照。
#[tauri::command]
pub(crate) fn get_inference_gateway_status(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    Ok(inference_gateway_status_value(&state.options))
}

fn inference_gateway_status_value(options: &Options) -> serde_json::Value {
    let (running, url, port, last_error, notice) = crate::app::inference_gateway::status();
    let bindings = crate::app::ai_provider_import::gateway_bindings(options);
    json!({
        "running": running,
        "url": url,
        "port": port,
        "preferred_port": crate::app::inference_gateway::configured_port(&options.state_path),
        "last_error": last_error,
        "notice": notice,
        "gateway_clients": bindings
            .iter()
            .map(|binding| json!({
                "client": binding.client,
                "service": binding.service,
                "protocol": binding.protocol,
                "models": binding.models,
            }))
            .collect::<Vec<serde_json::Value>>(),
        "direct_clients": crate::app::ai_provider_import::direct_bound_clients(options),
    })
}

/// 重启本机网关：端口被释放、或异常退出后用它恢复。
#[tauri::command]
pub(crate) async fn restart_inference_gateway(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let resolver_options = options.clone();
        crate::app::inference_gateway::restart(
            Some(crate::app::inference_gateway::configured_port(&options.state_path)),
            Box::new(move || crate::app::ai_provider_import::gateway_bindings(&resolver_options)),
        )
        .map_err(|error| error.to_string())?;
        Ok(inference_gateway_status_value(&options))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 停用网关：先把所有走网关的客户端切回直连，全部成功后才停监听。
///
/// 顺序不能反——先停监听会留下一批指向空端口、连不上上游的客户端，
/// 而用户看不出这两件事的因果关系。
#[tauri::command]
pub(crate) async fn stop_inference_gateway_and_unbind(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let user_id = crate::app::identity::identity_status(&options).user_id;
        let clients = crate::app::ai_provider_import::gateway_bindings(&options)
            .into_iter()
            .map(|binding| binding.client)
            .collect::<Vec<String>>();
        let mut switched = Vec::new();
        let mut failures = Vec::new();
        for client in &clients {
            match crate::app::ai_provider_import::disable_gateway_binding(
                &options, &user_id, client,
            ) {
                Ok(_) => switched.push(client.clone()),
                Err(error) => {
                    failures.push(json!({ "client": client, "error": error.to_string() }))
                }
            }
        }
        let stopped = if failures.is_empty() {
            crate::app::inference_gateway::stop()
        } else {
            false
        };
        Ok(json!({
            "stopped": stopped,
            "switched": switched,
            "failures": failures,
        }))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 改本机网关端口。
///
/// 端口是写进各客户端配置的地址，改它会连带让所有引用它的客户端失效，
/// 所以只在**没有网关绑定**时允许；有绑定时先要求切回直连，避免留下
/// 指向死端口的配置（这正是界面上不让随手改端口的原因）。
#[tauri::command]
pub(crate) async fn set_inference_gateway_port(
    state: State<'_, AgentState>,
    port: u16,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let bound = crate::app::ai_provider_import::gateway_bindings(&options);
        if !bound.is_empty() {
            let clients = bound
                .iter()
                .map(|binding| binding.client.clone())
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!(
                "还有工具走网关（{clients}）。改端口会让它们的配置指向死端口，请先切回直连再改。"
            ));
        }
        crate::app::inference_gateway::set_configured_port(&options.state_path, port)?;
        let resolver_options = options.clone();
        crate::app::inference_gateway::restart(
            Some(crate::app::inference_gateway::configured_port(&options.state_path)),
            Box::new(move || crate::app::ai_provider_import::gateway_bindings(&resolver_options)),
        )?;
        Ok(inference_gateway_status_value(&options))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 切换某个客户端的模型注入模式：`gateway` 走本机网关（用量计入本机口径），
/// `direct` 写真实凭据（用量不计入）。P1 只支持 Codex。
#[tauri::command]
pub(crate) async fn set_provider_binding_mode(
    state: State<'_, AgentState>,
    target: String,
    mode: String,
    service: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let user_id = crate::app::identity::identity_status(&options).user_id;
        let result = match mode.trim() {
            "gateway" => {
                let service = service
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| crate::app::ai_provider_import::binding_service(&options, &target))
                    .unwrap_or_else(|| "managed".to_string());
                crate::app::ai_provider_import::enable_gateway_binding(
                    &options, &user_id, &target, &service,
                )
            }
            "direct" => {
                crate::app::ai_provider_import::disable_gateway_binding(&options, &user_id, &target)
            }
            other => return Err(format!("注入模式只支持 gateway 或 direct，收到：{other}")),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_value(result).map_err(|error| error.to_string())
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
pub(crate) async fn list_ai_service_templates(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_value(crate::app::ai_service_templates::list(&options))
            .map_err(|error| error.to_string())
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
            // 桌面端要先知道 npx / node / opencode 在不在，才能把「一键接入」
            // 做成一步，而不是让用户接入完再自己排查为什么不可用。
            "executables": crate::runtime::probe_acp_executables(),
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

/// 手工重投同步失败记录。`reason` 为空表示全部死信，否则只重投命中的错误片段。
#[tauri::command]
pub(crate) async fn requeue_projection_dead_letters(
    reason: Option<String>,
) -> Result<crate::agent_core_projection::ProjectionRequeueReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let fragment = reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        crate::agent_core_projection::requeue_dead_letter_projections(fragment)
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
    // 桌面端表单当前不编辑环境变量，保存时保留既有配置，避免 UI 一次编辑把
    // 通过 CLI / 分发写入的 env 抹掉。
    let environment = crate::store::acp_profiles::list()
        .ok()
        .and_then(|profiles| {
            let target = crate::store::acp_profiles::normalize_provider_id(&provider_id);
            profiles
                .into_iter()
                .find(|profile| profile.provider_id == target)
                .map(|profile| profile.env)
        })
        .unwrap_or_default();
    let profile =
        crate::store::acp_profiles::upsert(crate::store::acp_profiles::AcpRuntimeProfileRecord {
            provider_id,
            display_name,
            executable,
            args,
            env: environment,
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
    let protocol = crate::store::ai_services::AIServiceProtocol::parse(&protocol)?;
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
                "已将本机模型服务设为 HiMind AI 默认服务: {}",
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
    protocol: Option<String>,
) -> Result<serde_json::Value, String> {
    let protocol = match protocol.as_deref() {
        Some(value) => crate::store::ai_services::AIServiceProtocol::parse(value)?,
        // 历史调用方不带 protocol，保持既有 OpenAI Responses 行为。
        None => crate::store::ai_services::AIServiceProtocol::OpenaiResponses,
    };
    let models = crate::store::ai_services::fetch_models(&base_url, &api_key, protocol)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "models": models }))
}

#[tauri::command]
pub(crate) fn fetch_saved_ai_service_models(
    id: String,
    base_url: String,
) -> Result<serde_json::Value, String> {
    let (service, api_key) =
        crate::store::ai_services::load_secret(&id).map_err(|e| e.to_string())?;
    let models = crate::store::ai_services::fetch_models(&base_url, &api_key, service.protocol)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "models": models }))
}

#[tauri::command]
pub(crate) fn import_ai_client(
    state: State<'_, AgentState>,
    target: String,
    service: Option<String>,
    replace: Option<bool>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    let request = serde_json::json!({
        "target": target,
        "service": service.unwrap_or_else(|| "managed".to_string()),
        "replace": replace.unwrap_or(false),
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
    capability_facts_for(&state.capability_gateway)
}

/// 与 [`skill_capability_facts`] 同源，只依赖网关句柄，方便在
/// `spawn_blocking` 里跑重活。
fn capability_facts_for(
    gateway: &CapabilityGateway,
) -> Result<Vec<crate::skill::resolver::CapabilityFact>, String> {
    gateway
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
pub(crate) async fn get_instruction_pack_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::InstructionPackCatalogItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        require_dashboard(&state)?;
        let snapshot = local_worker_snapshot(&state.worker_status);
        let agent_id = snapshot
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if agent_id.is_empty() || state.options.agent_credential().is_empty() {
            return Err("HiMind 账号尚未授权".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::distribution::instruction_pack_catalog(
            &client,
            &state.options.api_base(),
            agent_id,
            &state.options.agent_credential(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_expert_catalog(
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::ExpertCatalogItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        require_dashboard(&state)?;
        let snapshot = local_worker_snapshot(&state.worker_status);
        let agent_id = snapshot
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if agent_id.is_empty() || state.options.agent_credential().is_empty() {
            return Err("HiMind 账号尚未授权".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::distribution::expert_catalog(
            &client,
            &state.options.api_base(),
            agent_id,
            &state.options.agent_credential(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_instruction_pack_versions(
    instruction_pack_id: String,
    state: State<'_, AgentState>,
) -> Result<Vec<crate::api::distribution::InstructionPackCatalogItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        require_dashboard(&state)?;
        let snapshot = local_worker_snapshot(&state.worker_status);
        let agent_id = snapshot
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if agent_id.is_empty() || state.options.agent_credential().is_empty() {
            return Err("HiMind 账号尚未授权".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        crate::api::distribution::instruction_pack_versions(
            &client,
            &state.options.api_base(),
            agent_id,
            &state.options.agent_credential(),
            &instruction_pack_id,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn install_instruction_pack_market(
    instruction_pack_id: String,
    version: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let options = gateway.options().clone();
        let state = crate::api::client::load_agent_state(&options.state_path).map_err(|error| error.to_string())?;
        options.set_agent_credential(&state.credential);
        let input = serde_json::json!({"kind":"instruction_pack","id":instruction_pack_id,"version":version,"artifact_id":artifact_id,"sha256":sha256});
        crate::app::market::install(&options, &state.agent_id, &input, crate::capability::types::InvocationSource::Tauri)
            .map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn install_expert_market(
    expert_id: String,
    version: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let options = gateway.options().clone();
        if let Ok(snapshot) = crate::app::extension_source::snapshot() {
            if let Some(item) = snapshot.experts.iter().find(|item| {
                item.expert_id == expert_id
                    && version.as_deref().map(|value| value == item.version).unwrap_or(true)
                    && item.source.starts_with("local:")
            }) {
                let source_id = item.source.strip_prefix("local:").unwrap_or_default();
                let summary = crate::app::extension_source::install_expert_bound(
                    &item.expert_id,
                    &item.version,
                    source_id,
                )
                .map_err(|error| error.to_string())?;
                return Ok(serde_json::json!({"expert": summary, "projection_required": false, "activated": false}));
            }
        }
        let state = crate::api::client::load_agent_state(&options.state_path).map_err(|error| error.to_string())?;
        options.set_agent_credential(&state.credential);
        let input = serde_json::json!({"kind":"expert","id":expert_id,"version":version,"artifact_id":artifact_id,"sha256":sha256});
        crate::app::market::install(&options, &state.agent_id, &input, crate::capability::types::InvocationSource::Tauri).map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
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
pub(crate) fn list_instruction_pack_drafts(
) -> Result<Vec<crate::instruction_pack::InstructionPackDraft>, String> {
    crate::instruction_pack::list().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_instruction_pack_draft(
    input: crate::instruction_pack::InstructionPackDraftInput,
) -> Result<crate::instruction_pack::InstructionPackDraft, String> {
    crate::instruction_pack::save(input).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_instruction_file(
    path: String,
) -> Result<crate::instruction_pack::InstructionPackDraft, String> {
    crate::instruction_pack::import_file(std::path::Path::new(path.trim()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn import_instruction_package(
    path: String,
) -> Result<crate::instruction_pack::InstructionPackDraft, String> {
    crate::instruction_pack::import_package(crate::instruction_pack::InstructionPackImportInput {
        package_path: std::path::PathBuf::from(path.trim()),
        source: "local_package".to_string(),
    })
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn test_instruction_pack_draft(
    id: String,
    version: String,
) -> Result<crate::instruction_pack::InstructionPackTestResult, String> {
    crate::instruction_pack::test(&id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn confirm_instruction_pack_draft(
    id: String,
    version: String,
) -> Result<crate::instruction_pack::InstructionPackDraft, String> {
    crate::instruction_pack::confirm(&id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn publish_instruction_pack_locally(
    id: String,
    version: String,
) -> Result<crate::instruction_pack::InstructionPackDraft, String> {
    crate::instruction_pack::publish_local(&id, &version).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn list_extension_projects(
) -> Result<Vec<crate::extension_projects::ExtensionProject>, String> {
    // 开发项目列表要扫描各工作区目录，同样下沉到阻塞线程池，
    // 免得占用 WebView 主线程。
    tauri::async_runtime::spawn_blocking(|| {
        crate::extension_projects::list().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
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

/// 界面里维护的开发目录清单。列表本身是权威数据，目录不可用时也要返回，
/// 否则用户没法把失效登记移除。
#[tauri::command]
pub(crate) fn list_extension_workspaces() -> Vec<crate::extension_workspace::ExtensionWorkspaceEntry>
{
    crate::extension_workspace::workspace_entries()
}

#[tauri::command]
pub(crate) fn pick_extension_workspace_dir() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择扩展开发目录")
        .pick_folder()
        .map(|path| crate::extension_workspace::display_path(&path))
}

#[tauri::command]
pub(crate) fn pick_instruction_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 AGENTS.md 或 CLAUDE.md")
        .add_filter("客户端指令", &["md"])
        .pick_file()
        .map(|path| crate::extension_workspace::display_path(&path))
}

#[tauri::command]
pub(crate) fn pick_instruction_package() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 HiMind 指令包")
        .add_filter("HiMind 指令包", &["hminstruction", "zip"])
        .pick_file()
        .map(|path| crate::extension_workspace::display_path(&path))
}

/// 登记一个开发目录。任何真实目录都可以：目录里暂时没有 `extensions.json`
/// 只是"还没有可整体分发的扩展"，不影响开发。
#[tauri::command]
pub(crate) fn add_extension_workspace(
    root: String,
) -> Result<Vec<crate::extension_workspace::ExtensionWorkspaceEntry>, String> {
    let path =
        crate::extension_workspace::register_root(&root).map_err(|error| error.to_string())?;
    let display = crate::extension_workspace::display_path(&path);
    // 记成兜底工作区：AI 会话与 MCP 创作链路在没有显式按次指定时才有落点。
    let _ = crate::extension_workspace::bind(&path);
    // 目录自带聚合清单时同步一个本地来源，市场侧据此提供免安装预览。
    if path.join("extensions.json").is_file() {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        let _ = crate::app::extension_source::add_local_source(&name, &display, None);
    } else if ["plugin.json", "skill.json", "workflow.json"]
        .iter()
        .any(|manifest| path.join(manifest).is_file())
    {
        // 目录本身就是一个扩展（单项目工作区）：登记成项目，列表里才会出现它。
        let _ = crate::extension_projects::register(&path);
    }
    Ok(crate::extension_workspace::workspace_entries())
}

#[tauri::command]
pub(crate) fn remove_extension_workspace(
    root: String,
) -> Result<Vec<crate::extension_workspace::ExtensionWorkspaceEntry>, String> {
    crate::extension_workspace::unregister_root(&root).map_err(|error| error.to_string())?;
    let _ = crate::extension_workspace::unbind(Some(std::path::Path::new(root.trim())));
    // 旧版"当前选中的聚合仓库"如果就是它，一并清掉，否则下次刷新会从配置里复活。
    let current = crate::extension_workspace::settings();
    if current.configured
        && !current.root.trim().is_empty()
        && crate::extension_workspace::same_root(&current.root, &root)
    {
        let _ = crate::extension_workspace::clear();
    }
    // 指向这个目录的本地来源一并移除，市场里不留空壳。
    if let Ok(sources) = crate::app::extension_source::settings() {
        let detached: Vec<String> = sources
            .sources
            .iter()
            .filter(|source| {
                source.kind == crate::app::extension_source::ExtensionSourceKind::Local
                    && crate::extension_workspace::same_root(&source.repository, &root)
            })
            .map(|source| source.id.clone())
            .collect();
        for source_id in detached {
            let _ = crate::app::extension_source::remove_source(&source_id);
        }
    }
    Ok(crate::extension_workspace::workspace_entries())
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
    parent_dir: Option<String>,
    state: State<'_, AgentState>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let identity = crate::app::identity::authoring_identity(&state.options);
    // 指定了工作区就直接建进去：项目落在哪里是"这个扩展属于哪个仓库"的一部分，
    // 不该让用户在弹框里自己找路径。没指定才退回目录选择。
    let parent = match parent_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(directory) => crate::extension_workspace::validate_authoring_root(directory)?,
        None => {
            let Some(selected) = rfd::FileDialog::new()
                .set_title("选择项目保存位置")
                .pick_folder()
            else {
                return Err("已取消新建扩展项目".to_string());
            };
            selected
        }
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

/// 设置扩展项目的分发目标覆盖；`targets = None` 表示回到分发单元默认。
#[tauri::command]
pub(crate) fn set_extension_project_distribution_targets(
    kind: String,
    extension_id: String,
    targets: Option<Vec<crate::extension_contracts::DistributionTarget>>,
) -> Result<crate::extension_projects::ExtensionProject, String> {
    let kind = crate::extension_projects::ExtensionProjectKind::parse(&kind)
        .map_err(|error| error.to_string())?;
    crate::extension_projects::set_distribution_targets(kind, &extension_id, targets.as_deref())
        .map_err(|error| error.to_string())
}

/// 设置分发单元级默认分发目标，供单元内全部扩展继承；`None` 表示回到继承。
#[tauri::command]
pub(crate) fn set_extension_unit_distribution_targets(
    unit_key: String,
    targets: Option<Vec<crate::extension_contracts::DistributionTarget>>,
) -> Result<crate::app::extension_source::ExtensionSourceSettings, String> {
    crate::app::extension_source::set_unit_distribution_targets(&unit_key, targets.as_deref())
        .map_err(|error| error.to_string())
}

/// GitHub 分发账号：状态查询不返回 token，写入前先调用 GitHub 校验登录名。
#[tauri::command]
pub(crate) fn get_github_distribution_account(
) -> Result<crate::store::github_credentials::GithubAccountStatus, String> {
    crate::store::github_credentials::status().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_github_distribution_account(
    token: String,
    token_kind: Option<String>,
    repositories: Option<Vec<String>>,
) -> Result<crate::store::github_credentials::GithubAccountStatus, String> {
    let identity = crate::app::github_publisher::verify_token(token.trim())
        .map_err(|error| error.to_string())?;
    crate::store::github_credentials::set_account(
        &identity.login,
        token.trim(),
        token_kind.as_deref().unwrap_or(""),
        repositories.as_deref().unwrap_or(&[]),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn remove_github_distribution_account() -> Result<bool, String> {
    crate::store::github_credentials::remove().map_err(|error| error.to_string())
}

/// GitHub App 设备流第一步：申请设备码。client_id 由注册 App 的组织提供，可从 UI 传入；
/// 没传时回退到环境变量或上次授权保存的值。
#[tauri::command]
pub(crate) fn start_github_app_authorization(
    client_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let client_id = client_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(crate::app::github_app::configured_client_id);
    let authorization =
        crate::app::github_app::start_device_flow(&client_id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "client_id": client_id, "authorization": authorization }))
}

/// 轮询设备授权结果。授权成功后立刻保存授权事实并列出可绑定的安装，
/// 省掉「再调一次列安装」的往返，UI 拿到结果就能让用户选。
#[tauri::command]
pub(crate) fn poll_github_app_authorization(
    client_id: String,
    device_code: String,
) -> Result<serde_json::Value, String> {
    use crate::app::github_app::DevicePollOutcome;
    let outcome = crate::app::github_app::poll_device_flow(client_id.trim(), device_code.trim())
        .map_err(|error| error.to_string())?;
    let (state, installations) = match outcome {
        DevicePollOutcome::Authorized(token) => {
            // 登录名取自 GET /user，而不是用户输入；失败不阻断授权，只是暂时没有可展示的名字。
            let login = crate::app::github_publisher::verify_token(&token.access_token)
                .map(|identity| identity.login)
                .unwrap_or_default();
            let record = crate::store::github_credentials::GithubAppRecord {
                login,
                client_id: client_id.trim().to_string(),
                installation_id: String::new(),
                installation_account: String::new(),
                user_token: token.access_token.clone(),
                refresh_token: token.refresh_token.clone(),
                user_token_expires_at: (crate::app::github_app::now_epoch() + token.expires_in)
                    .to_string(),
            };
            crate::store::github_credentials::save_app_state(&record)
                .map_err(|error| error.to_string())?;
            let installations = crate::app::github_app::list_installations(&record.user_token)
                .map_err(|error| error.to_string())?;
            ("authorized", Some(installations))
        }
        DevicePollOutcome::Pending => ("pending", None),
        DevicePollOutcome::SlowDown => ("slow_down", None),
        DevicePollOutcome::Expired => ("expired", None),
        DevicePollOutcome::Denied => ("denied", None),
    };
    Ok(serde_json::json!({ "state": state, "installations": installations }))
}

#[tauri::command]
pub(crate) fn list_github_app_installations() -> Result<serde_json::Value, String> {
    let state = crate::store::github_credentials::app_state()
        .map_err(|error| error.to_string())?
        .ok_or("GitHub App 尚未授权，请先完成授权")?;
    let installations = crate::app::github_app::list_installations(&state.user_token)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "installations": installations }))
}

/// 绑定发布用的安装：之后发布走该安装的短期令牌。
#[tauri::command]
pub(crate) fn select_github_app_installation(
    installation_id: String,
) -> Result<crate::store::github_credentials::GithubAccountStatus, String> {
    let state = crate::store::github_credentials::app_state()
        .map_err(|error| error.to_string())?
        .ok_or("GitHub App 尚未授权，请先完成授权")?;
    let installations = crate::app::github_app::list_installations(&state.user_token)
        .map_err(|error| error.to_string())?;
    let selected = installations
        .into_iter()
        .find(|item| item.id == installation_id.trim())
        .ok_or_else(|| "未找到该安装，请刷新后重新选择".to_string())?;
    crate::app::github_app::select_installation(&selected).map_err(|error| error.to_string())?;
    crate::store::github_credentials::status().map_err(|error| error.to_string())
}

/// 导入 App 私钥（PKCS#1 / PKCS#8 PEM）。私钥只在签发安装令牌时读取，DPAPI 加密落盘。
#[tauri::command]
pub(crate) fn import_github_app_private_key(
) -> Result<crate::store::github_credentials::GithubAccountStatus, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择 GitHub App 私钥（.pem）")
        .add_filter("PEM 私钥", &["pem", "key"])
        .pick_file()
    else {
        return Err("已取消选择私钥文件".to_string());
    };
    let pem = std::fs::read_to_string(&path).map_err(|error| format!("读取私钥失败：{error}"))?;
    crate::store::github_credentials::save_app_private_key(&pem)
        .map_err(|error| error.to_string())?;
    crate::store::github_credentials::status().map_err(|error| error.to_string())
}

/// 打开 GitHub 设备授权页。只放行 github.com，避免这个命令被当成任意 URL 的跳板。
#[tauri::command]
pub(crate) fn open_github_authorization_page(verification_uri: String) -> Result<(), String> {
    let target = verification_uri.trim();
    if !target.starts_with("https://github.com/") {
        return Err("只允许打开 github.com 的授权页面".to_string());
    }
    open_url(target).map_err(|error| error.to_string())
}

/// 发布预览：只读，用于 UI 展示这次会发到哪里、发什么。
#[tauri::command]
pub(crate) fn preview_extension_distribution(
    kind: String,
    extension_id: String,
    version: String,
) -> Result<serde_json::Value, String> {
    let kind = crate::extension_projects::ExtensionProjectKind::parse(&kind)
        .map_err(|error| error.to_string())?;
    let preview = crate::app::distribution_publish::preview(kind, &extension_id, &version)
        .map_err(|error| error.to_string())?;
    with_operation_plan(
        &preview,
        &crate::app::operation_plan::distribution_publish(&preview),
    )
}

/// 按生效目标发布。与 CLI 共用同一编排，失败也会写入分发台账。
#[tauri::command]
pub(crate) fn publish_extension_distribution(
    kind: String,
    extension_id: String,
    version: String,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let kind = crate::extension_projects::ExtensionProjectKind::parse(&kind)
        .map_err(|error| error.to_string())?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    crate::app::distribution_publish::publish(
        &state.options,
        &agent_id,
        kind,
        &extension_id,
        &version,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_extension_distribution_state(
    kind: Option<String>,
    extension_id: Option<String>,
) -> Result<Vec<crate::app::distribution_state::DistributionStateEntry>, String> {
    let view = crate::app::distribution_state::load().map_err(|error| error.to_string())?;
    match (kind, extension_id) {
        (Some(kind), Some(extension_id)) => Ok(view.for_asset(kind.trim(), extension_id.trim())),
        _ => Ok(view.all()),
    }
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
    crate::api::distribution::extension_projects(
        &client,
        &state.options.api_base(),
        &agent_id,
        &token,
    )
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
            &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        &state.options.api_base(),
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
        return Err("HiMind 账号尚未授权".to_string());
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
        return Err("HiMind 账号尚未授权".to_string());
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
        &state.options.api_base(),
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
        return Err("HiMind 账号尚未授权".to_string());
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
        &state.options.api_base(),
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
        return Err("HiMind 账号尚未授权".to_string());
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
        &state.options.api_base(),
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

/// 选择技能安装位置（不产生任何持久化副作用，只返回所选目录）。
#[tauri::command]
pub(crate) fn pick_skill_location() -> Result<String, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("选择技能安装位置")
        .pick_folder()
    else {
        return Err("已取消选择安装位置".to_string());
    };
    Ok(path.to_string_lossy().to_string())
}

/// 只从指定目录移除某个技能的副本（全局投放与其他目录不受影响）。
///
/// 会一并清掉该目录 `.himind/skills.lock.json` 里的对应条目，不留"半管理"残留。
#[tauri::command]
pub(crate) fn remove_skill_from_location(
    skill_id: String,
    location: String,
) -> Result<serde_json::Value, String> {
    // 目录还在：按正常流程移除副本（清文件 + 收据 + 台账 + 该目录的锁条目）。
    if std::path::Path::new(&location).is_dir() {
        return with_skill_location(Some(&location), || {
            crate::skill::unregister_skill_clients_json(&skill_id)
                .map_err(|error| error.to_string())
        });
    }
    // 目录已被删除（用户删了或移走了）：只清理安装台账，让界面不再显示这条幽灵记录。
    let removed = crate::skill::target::purge_deployments_at(&skill_id, &location)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "skill_id": skill_id,
        "location": location,
        "removed_count": removed,
        "purged": true,
    }))
}

/// 把技能库里已有的一份技能写到指定目录（不经过"重新安装"，也不改动任何持久设置）。
///
/// 与安装的区别：安装解决"库里有没有这份技能"，这里解决"把它落到哪个目录"。
/// 目标目录只对本次调用生效，"当前项目"之类的全局状态不再参与。
#[tauri::command]
pub(crate) fn deploy_skill_to_location(
    skill_id: String,
    location: String,
    clients: Option<Vec<String>>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    with_skill_location(Some(&location), || {
        let capability_facts = skill_capability_facts(&state)?;
        let store = crate::skill::store::SkillStore::new();
        store
            .bootstrap_builtin_skills()
            .map_err(|error| error.to_string())?;
        let record = store
            .get_record(&skill_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("技能库里没有这份技能: {skill_id}"))?;
        let rendered = crate::skill::sync_record_to_clients(
            &record,
            VERSION,
            &capability_facts,
            clients.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({
            "record": record,
            "clients": rendered,
            "location": location,
        }))
    })
}

/// 在指定安装位置（或全局）下执行一次技能操作。
///
/// 位置只对本次调用生效：通过进程环境告诉渲染层"这次写到哪儿"，调用结束后原样恢复，
/// 既不写任何持久配置，也不会被"上次选过的项目"改道。这与 CLI 的
/// `--workspace` / `--global` 语义一致。
pub(crate) fn with_skill_location<T>(
    location: Option<&str>,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let previous_target = std::env::var_os("HIMIND_SKILL_TARGET");
    let previous_workspace = std::env::var_os("HIMIND_SKILL_WORKSPACE");
    let canonical = match location.map(str::trim).filter(|value| !value.is_empty()) {
        Some(path) => Some(
            crate::skill::target::canonical_workspace_root(std::path::Path::new(path))
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    std::env::remove_var("HIMIND_SKILL_TARGET");
    match canonical.as_ref() {
        Some(root) => std::env::set_var("HIMIND_SKILL_WORKSPACE", root),
        None => {
            std::env::set_var(
                "HIMIND_SKILL_TARGET",
                crate::skill::target::TARGET_KIND_GLOBAL,
            );
            std::env::remove_var("HIMIND_SKILL_WORKSPACE");
        }
    }
    let result = action();
    match previous_target {
        Some(value) => std::env::set_var("HIMIND_SKILL_TARGET", value),
        None => std::env::remove_var("HIMIND_SKILL_TARGET"),
    }
    match previous_workspace {
        Some(value) => std::env::set_var("HIMIND_SKILL_WORKSPACE", value),
        None => std::env::remove_var("HIMIND_SKILL_WORKSPACE"),
    }
    result
}

#[tauri::command]
pub(crate) fn install_organization_skill(
    skill_id: String,
    version: Option<String>,
    optional_plugin_ids: Option<Vec<String>>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    clients: Option<Vec<String>>,
    location: Option<String>,
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    // 安装位置只对本次调用生效：不改动任何持久设置，也不受"上次选过的项目"影响。
    // 不指定位置时就是全局安装，这一点与 CLI 的行为一致。
    with_skill_location(location.as_deref(), || {
        install_organization_skill_at(
            skill_id,
            version,
            optional_plugin_ids,
            source,
            artifact_id,
            sha256,
            clients,
            state,
        )
    })
}

fn install_organization_skill_at(
    skill_id: String,
    version: Option<String>,
    optional_plugin_ids: Option<Vec<String>>,
    source: Option<String>,
    artifact_id: Option<String>,
    sha256: Option<String>,
    clients: Option<Vec<String>>,
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
        let rendered = crate::skill::sync_record_to_clients(
            &record,
            VERSION,
            &capability_facts,
            clients.as_deref(),
        )
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
    let rendered = crate::skill::sync_record_to_clients(
        &record,
        VERSION,
        &capability_facts,
        clients.as_deref(),
    )
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
) -> Result<serde_json::Value, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_skill_catalog(&state)?.into_iter().any(|item| {
                item.skill_id == skill_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        let plan = crate::app::extension_source::plan_skill_bound(
            &skill_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        return skill_plan_payload(&plan);
    }
    require_dashboard(&state)?;
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let plan = crate::app::skill_manager::plan_install_bound(
        &state.options,
        &agent_id,
        &skill_id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    skill_plan_payload(&plan)
}

/// 技能安装计划：保留原有的 `skill` / `plugin_actions` 字段（既有界面照旧读），
/// 同时附上统一的 [`crate::app::operation_plan::OperationPlan`]，让"会发生什么"
/// 只有一套解释。
fn skill_plan_payload(
    plan: &crate::app::skill_manager::SkillInstallPlan,
) -> Result<serde_json::Value, String> {
    let dependencies = plan
        .plugin_actions
        .iter()
        .map(|action| crate::app::operation_plan::PlanDependency {
            kind: "plugin".to_string(),
            id: action.plugin_id.clone(),
            name: action.plugin_name.clone(),
            required: action.required,
            current_version: action.current_version.clone(),
            target_version: action.target_version.clone(),
            action: action.action.clone(),
            reason: action.reason.clone(),
        })
        .collect();
    let unified = crate::app::operation_plan::skill_install(
        &plan.skill,
        plan.blocked_reasons.clone(),
        dependencies,
    );
    with_operation_plan(plan, &unified)
}

fn plugin_plan_payload(
    plan: &crate::app::plugin_manager::PluginInstallPlan,
) -> Result<serde_json::Value, String> {
    let dependencies = plan
        .dependency_actions
        .iter()
        .map(|action| crate::app::operation_plan::PlanDependency {
            kind: "plugin".to_string(),
            id: action.plugin_id.clone(),
            name: action.plugin_name.clone(),
            required: action.required,
            current_version: action.current_version.clone(),
            target_version: action.target_version.clone(),
            action: action.action.clone(),
            reason: action.reason.clone(),
        })
        .collect();
    let unified = crate::app::operation_plan::plugin_install(
        &plan.plugin,
        plan.blocked_reasons.clone(),
        dependencies,
    );
    with_operation_plan(plan, &unified)
}

fn with_operation_plan<T: serde::Serialize>(
    original: &T,
    unified: &crate::app::operation_plan::OperationPlan,
) -> Result<serde_json::Value, String> {
    let mut payload = serde_json::to_value(original).map_err(|error| error.to_string())?;
    let plan = serde_json::to_value(unified).map_err(|error| error.to_string())?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("plan".to_string(), plan);
    }
    Ok(payload)
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
        return Err("HiMind 账号尚未授权".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::skill_versions(
        &client,
        &state.options.api_base(),
        agent_id,
        &credential,
        &skill_id,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn get_codex_skill_status(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    // 这份快照要给 20 个客户端 × 全部技能逐个核对渲染收据（含文件校验和），
    // 单次十秒量级。放在主线程上会把同一时刻的插件、技能、工作台请求全部
    // 拖到超时，因此整体下沉到阻塞线程池。
    let gateway = state.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let capability_facts = capability_facts_for(&gateway)?;
        let clients = crate::skill::client_status_json(VERSION, &capability_facts)
            .map_err(|error| error.to_string())?;
        codex_compatible_client_result(clients)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 客户端 × 作用域 × 能力矩阵：静态能力 + 本机可用性。
///
/// 这是"某个 AI 工具能不能用某类能力"的唯一答案来源，替代各页面各自维护的
/// 客户端清单。
#[tauri::command]
pub(crate) async fn get_client_capability_matrix(
    state: State<'_, AgentState>,
) -> Result<serde_json::Value, String> {
    let options = state.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(crate::app::client_matrix::matrix_json(&options))
    })
    .await
    .map_err(|error| error.to_string())?
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

/// 每个技能"文件写到过哪些位置"（全局 / 指定目录），来自部署台账而不是当前目标。
///
/// 技能可以同时装在全局和若干目录里，状态查询本身只反映"当前目标"，所以这里单独聚合一
/// 份位置清单，前端据此展示与管理（更新 / 移除）。
fn skill_locations_payload(clients: &serde_json::Value) -> serde_json::Value {
    let mut skill_ids: Vec<String> = Vec::new();
    if let Some(client_map) = clients.as_object() {
        for client in client_map.values() {
            let Some(items) = client.get("items").and_then(serde_json::Value::as_array) else {
                continue;
            };
            for item in items {
                let Some(skill_id) = item
                    .get("record")
                    .and_then(|record| record.get("manifest"))
                    .and_then(|manifest| manifest.get("id"))
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                if !skill_ids.iter().any(|known| known == skill_id) {
                    skill_ids.push(skill_id.to_string());
                }
            }
        }
    }
    let mut payload = serde_json::Map::new();
    // 台账整份读一次后再按技能分组。此前是每个技能各读一次同一份文件，
    // 技能数一多这段纯读盘就成了这个接口的主要耗时。
    let deployments = crate::skill::target::all_deployments().unwrap_or_default();
    for skill_id in skill_ids {
        let mut groups: Vec<(String, bool, String, Vec<String>, String, bool, Vec<String>)> =
            Vec::new();
        for deployment in deployments.iter().filter(|item| item.skill_id == skill_id) {
            let (root, is_directory) = match deployment.workspace_root.as_deref() {
                Some(root) if !root.trim().is_empty() => (root.to_string(), true),
                _ => (String::new(), false),
            };
            let key = if is_directory {
                root.clone()
            } else {
                String::new()
            };
            let present = std::path::Path::new(&deployment.rendered_root).exists();
            // 落盘策略（复制 / 软链接）来自台账本身：界面能如实说明"这一份是怎么装上去的"。
            let strategies: Vec<String> = if deployment.strategy.trim().is_empty() {
                Vec::new()
            } else {
                vec![deployment.strategy.trim().to_string()]
            };
            match groups.iter_mut().find(|entry| entry.0 == key) {
                Some(entry) => {
                    if !entry.3.contains(&deployment.client_id) {
                        entry.3.push(deployment.client_id.clone());
                    }
                    if entry.4.is_empty() {
                        entry.4 = deployment.version.clone();
                    }
                    entry.5 = entry.5 || present;
                    for strategy in strategies {
                        if !entry.6.contains(&strategy) {
                            entry.6.push(strategy);
                        }
                    }
                }
                None => groups.push((
                    key,
                    is_directory,
                    root,
                    vec![deployment.client_id.clone()],
                    deployment.version.clone(),
                    present,
                    strategies,
                )),
            }
        }
        let rows: Vec<serde_json::Value> = groups
            .into_iter()
            .map(
                |(_, is_directory, root, clients, version, present, strategies)| {
                    // 目录还在但副本已被删掉也算"记录已失效"：如实标出来，前端只提供"清理记录"。
                    let missing = is_directory && !present;
                    let root = if is_directory {
                        crate::skill::target::display_path(std::path::Path::new(&root))
                    } else {
                        root
                    };
                    serde_json::json!({
                        "scope": if is_directory { "directory" } else { "global" },
                        "root": root,
                        "missing": missing,
                        "clients": clients,
                        "version": version,
                        "strategies": strategies,
                    })
                },
            )
            .collect();
        payload.insert(skill_id, serde_json::Value::Array(rows));
    }
    serde_json::Value::Object(payload)
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
    let skill_locations = skill_locations_payload(&clients);
    if let Some(object) = codex.as_object_mut() {
        object.insert(
            "project_skills".to_string(),
            serde_json::Value::Array(project_skills),
        );
        object.insert("clients".to_string(), clients);
        object.insert("skill_locations".to_string(), skill_locations);
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
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let (items, errors) = merged_plugin_catalog_for(&state.options, &agent_id);
    if items.is_empty() {
        if let Some(error) = errors.into_iter().next() {
            return Err(error);
        }
    }
    Ok(items)
}

/// 与 [`merged_plugin_catalog`] 同一份解析规则，但不依赖 Tauri 层的
/// `AgentState`：能力面（市场搜索 / 计划 / 安装）只能拿到 [`Options`]。
///
/// 返回 `(目录项, 错误)`：任何一个来源不可达都不该让整个市场变成空的，调用方
/// 用错误列表给出"结果可能不完整"的提示。
pub(crate) fn merged_plugin_catalog_for(
    options: &Options,
    agent_id: &str,
) -> (
    Vec<crate::api::distribution::PluginCatalogItem>,
    Vec<String>,
) {
    let mut errors = Vec::new();
    let mut items = match crate::app::extension_source::snapshot() {
        Ok(snapshot) => snapshot.plugins,
        Err(error) => {
            errors.push(error.to_string());
            Vec::new()
        }
    };
    if options.mode().dashboard_enabled() {
        let credential = options.agent_credential();
        if !agent_id.trim().is_empty() && !credential.trim().is_empty() {
            let dashboard = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|error| error.to_string())
                .and_then(|client| {
                    crate::api::distribution::plugin_catalog(
                        &client,
                        &options.api_base(),
                        agent_id,
                        &credential,
                    )
                    .map_err(|error| error.to_string())
                });
            match dashboard {
                Ok(catalog) => items.extend(catalog),
                Err(error) => errors.push(error),
            }
        } else if items.is_empty() {
            errors.push("HiMind 账号尚未授权".to_string());
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
    (result, errors)
}

fn merged_skill_catalog(
    state: &AgentState,
) -> Result<Vec<crate::api::distribution::SkillCatalogItem>, String> {
    let agent_id = local_worker_snapshot(&state.worker_status)
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let (items, errors) = merged_skill_catalog_for(&state.options, &agent_id);
    if items.is_empty() {
        if let Some(error) = errors.into_iter().next() {
            return Err(error);
        }
    }
    Ok(items)
}

/// 技能目录的共享解析规则，见 [`merged_plugin_catalog_for`]。
pub(crate) fn merged_skill_catalog_for(
    options: &Options,
    agent_id: &str,
) -> (Vec<crate::api::distribution::SkillCatalogItem>, Vec<String>) {
    let mut errors = Vec::new();
    let mut items = match crate::app::extension_source::snapshot() {
        Ok(snapshot) => snapshot.skills,
        Err(error) => {
            errors.push(error.to_string());
            Vec::new()
        }
    };
    if options.mode().dashboard_enabled() {
        let credential = options.agent_credential();
        if !agent_id.trim().is_empty() && !credential.trim().is_empty() {
            match crate::app::skill_manager::catalog(options, agent_id) {
                Ok(catalog) => items.extend(catalog),
                Err(error) => errors.push(error.to_string()),
            }
        } else if items.is_empty() {
            errors.push("HiMind 账号尚未授权".to_string());
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
    (result, errors)
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
pub(crate) fn merged_workflow_catalog(
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
                    &options.api_base(),
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
        return Err("HiMind 账号尚未授权".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    crate::api::distribution::plugin_versions(
        &client,
        &state.options.api_base(),
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
) -> Result<serde_json::Value, String> {
    let public_source_id = public_extension_source_id(source.as_deref());
    if public_source_id.is_some()
        || (source.is_none()
            && merged_plugin_catalog(&state)?.iter().any(|item| {
                item.plugin_id == plugin_id
                    && (item.source.starts_with("local:") || item.source.starts_with("github:"))
            }))
    {
        let plan = crate::app::extension_source::plan_plugin_bound(
            &plugin_id,
            version.as_deref(),
            public_source_id,
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        return plugin_plan_payload(&plan);
    }
    require_dashboard(&state)?;
    let snapshot = local_worker_snapshot(&state.worker_status);
    let agent_id = snapshot
        .get("dashboard_agent_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let plan = crate::app::plugin_manager::plan_install_bound(
        &state.options,
        agent_id,
        &plugin_id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    plugin_plan_payload(&plan)
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

/// 清除插件失败/熔断记录，让它立刻重新参与能力发现。
///
/// 只影响本机的运行健康记录，不改变安装版本与启停状态，因此不进 Dashboard 上报队列
/// （没有需要同步的事实变化）。
#[tauri::command]
pub(crate) fn repair_plugin(plugin_id: String) -> Result<(), String> {
    crate::app::plugin_manager::repair(&plugin_id).map_err(|error| error.to_string())
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
    let api_base = state.options.api_base();
    let trusted_dashboard_url = state
        .options
        .mode()
        .control_plane_enabled()
        .then_some(api_base.as_str());
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
        crate::extension_projects::current_workspace(None).map_err(|error| error.to_string())?;
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
    light: Option<bool>,
) -> Result<serde_json::Value, String> {
    workflow_center_snapshot(state.capability_gateway.options(), light.unwrap_or(false))
        .map_err(|error| error.to_string())
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
                    &state.options.api_base(),
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
            let result = catch_unwind(AssertUnwindSafe(|| {
                (|| -> Result<(), Box<dyn std::error::Error>> {
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
                })()
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("workflow run {run_id} failed: {error}");
                    record_background_workflow_failure(&run_id, &error.to_string());
                }
                Err(payload) => {
                    let reason = panic_reason(payload);
                    eprintln!("workflow run {run_id} panicked: {reason}");
                    record_background_workflow_failure(&run_id, &reason);
                }
            }
        })?;
    Ok(())
}

fn record_background_workflow_failure(run_id: &str, reason: &str) {
    match crate::store::local_runs::LocalRunLedger::open_default()
        .and_then(|ledger| ledger.mark_run_failed(run_id, reason))
    {
        Ok(true) | Ok(false) => {}
        Err(error) => eprintln!("workflow run {run_id} failure could not be persisted: {error}"),
    }
}

fn panic_reason(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "workflow execution thread panicked".to_string()
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
    light: bool,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let mut workflows = Vec::new();
    let mut workflow_names = std::collections::HashMap::<String, String>::new();
    let library_issues = if light {
        Vec::new()
    } else {
        let store = crate::workflow::WorkflowStore::open_default()?;
        let metrics = crate::workflow::workflow_metrics_by_package(&ledger)?;
        // 坏包降级成一条 issue 输出给 UI，不再让单个读取失败的制品把整份「我的能力」打空。
        let (installed, issues) = store.list_with_issues()?;
        for item in installed {
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
        issues
    };
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
        let projections = if light {
            Vec::new()
        } else {
            ledger.projections_for_aggregate(&run.run_id, 100)?
        };
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
    // 轮询走轻量快照：目录会去控制面拉取，不能每 10 秒（运行中每 2.5 秒）打一次网络。
    let (catalog, catalog_error) = if light {
        (Vec::new(), String::new())
    } else {
        merged_workflow_catalog(options)?
    };
    Ok(json!({
        "workflows": workflows,
        "library_issues": library_issues,
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
        "WX-BUILD" => "构建",
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

    /// 计划面是"加法"：老界面依赖的原字段必须原样保留，计划只能作为附加字段出现，
    /// 否则加了统一计划就会把既有页面读的字段挤掉。
    #[test]
    fn operation_plan_is_attached_without_dropping_legacy_fields() {
        #[derive(serde::Serialize)]
        struct LegacyPreview {
            ready: bool,
            targets: Vec<String>,
        }

        let preview = serde_json::json!({
            "kind": "plugin",
            "id": "com.example.tools",
            "name": "示例插件",
            "version": "1.2.0",
            "targets": ["github", "workbench"],
            "github": {
                "repository": "example/tools",
                "tag": "v1.2.0",
                "asset_name": "tools.hmpkg",
                "manifest_name": "tools.json",
                "size_bytes": 1024,
                "sha256": "a".repeat(64),
                "authorized": true
            },
            "workbench": {
                "catalog_id": "catalog-1",
                "channel": "stable",
                "distribution_id": ""
            }
        });
        let plan = crate::app::operation_plan::distribution_publish(&preview);
        let legacy = LegacyPreview {
            ready: true,
            targets: vec!["github".to_string()],
        };

        let payload = super::with_operation_plan(&legacy, &plan).expect("统一计划载荷");
        assert_eq!(payload["ready"], serde_json::Value::Bool(true));
        assert_eq!(payload["targets"][0], "github");

        let attached = &payload["plan"];
        assert_eq!(
            attached["schema_version"],
            crate::app::operation_plan::PLAN_SCHEMA_VERSION
        );
        assert_eq!(attached["operation"], "publish");
        assert_eq!(attached["capability"], "plugin");
        assert_eq!(attached["item"]["name"], "示例插件");
        // 落点是执行面的同一份规则：GitHub 是发布制品，工作台是提交审核。
        assert_eq!(attached["targets"][0]["strategy"], "release");
        assert_eq!(attached["targets"][1]["strategy"], "submit");
        assert_eq!(attached["ready"], serde_json::Value::Bool(true));
    }
}
