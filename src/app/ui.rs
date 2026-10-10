use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use tauri::http::{Request, Response, StatusCode};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

use crate::app::builtin_ai_gateway::BuiltinAiCommandGateway;
use crate::app::builtin_ai_model_sync::{
    BuiltinAiModelSync, BuiltinAiModelSyncResult, ModelSyncSnapshot,
};
use crate::app::builtin_ai_proxy::BuiltinAiProxy;
use crate::app::builtin_ai_sync::BuiltinAiEventSync;
use crate::app::commands::AgentState;
use crate::approval::manager::ApprovalManager;
use crate::store::types::LocalWorkerStatus;
use crate::Options;

struct BuiltinAiSession {
    child: Child,
    home: PathBuf,
    workspace: PathBuf,
    focus_workspace: bool,
    /// 本会话的降级提示。多会话并发之后提示必须跟着会话走，否则 B 会话的
    /// 「模型凭据来自本机 AI 服务」会盖到 A 会话的头部。
    notice: Option<String>,
    proxy: BuiltinAiProxy,
    event_sync: BuiltinAiEventSync,
    model_sync: Arc<BuiltinAiModelSync>,
    command_gateway: Option<BuiltinAiCommandGateway>,
}

/// 一个 Agent 进程同时服务多个工作区的 HiMind AI 会话。
///
/// 用户的实际场景是「同时用 AI 开发两个扩展」：两个工作区各有一个 DSH 进程，
/// 各自的 MCP 伴生进程带着自己的 `HIMIND_AI_WORKSPACE`。旧实现把会话存成
/// `Option<BuiltinAiSession>` 单例，开第二个工作区必须先杀掉第一个 —— 正在跑的
/// 那一轮就没了。现在按工作区目录索引，增删一个会话不碰其它会话。
static BUILTIN_AI_SESSIONS: OnceLock<Mutex<HashMap<String, BuiltinAiSession>>> = OnceLock::new();
/// 正在启动的工作区集合，避免同一个目录被并发启动两次。
static BUILTIN_AI_STARTING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn builtin_ai_sessions() -> &'static Mutex<HashMap<String, BuiltinAiSession>> {
    BUILTIN_AI_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn builtin_ai_starting() -> &'static Mutex<HashSet<String>> {
    BUILTIN_AI_STARTING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 会话身份就是工作区目录本身：一个目录一个 DSH 进程，不同目录互不干扰。
/// Windows 路径大小写不敏感，统一按小写收敛，避免同一个目录开出两个会话。
fn builtin_ai_session_key(workspace: &Path) -> String {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

/// 会话的降级提示：控制面本该提供模型凭据但拿不到时，说明真实原因。
/// 会话本身正常启动，提示只是把「模型凭据从哪来」这件事说清楚。
pub(crate) fn current_builtin_ai_notice(workspace: Option<&Path>) -> Option<String> {
    let sessions = builtin_ai_sessions().lock().ok()?;
    match workspace {
        Some(path) => sessions
            .get(&builtin_ai_session_key(path))
            .and_then(|session| session.notice.clone()),
        None => sessions.values().find_map(|session| session.notice.clone()),
    }
}

pub(crate) fn run_tauri_app(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let port = options.local_port;
    // WebView2 的用户数据目录是进程级独占的：开发实例（或并行实例）跟正在
    // 运行的生产实例共用一个目录时，后启动的进程会静默建不出主窗口，只剩
    // 托盘图标。必须在 Tauri 创建任何 WebView 之前把目录按 profile 定下来。
    let webview_data_dir = crate::store::paths::apply_webview_user_data_dir(port);
    println!("webview user data dir: {}", webview_data_dir.display());
    let initial_plugin_view = options.plugin_view_launch();
    let initial_open_target = options.protocol_open_target();

    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: if options.mode().dashboard_enabled() {
            "正在连接 Dashboard 任务 Worker".to_string()
        } else {
            String::new()
        },
        dashboard_worker_state: if options.mode().dashboard_enabled() {
            "connecting".to_string()
        } else {
            "not_applicable".to_string()
        },
        dashboard_worker_reason_code: if options.mode().dashboard_enabled() {
            "connected_agent_app_starting".to_string()
        } else {
            "independent_mode_no_control_plane".to_string()
        },
        worker_transport: "local_http".to_string(),
        local_service_online: false,
        local_service_error: String::new(),
        distribution_update_available: false,
        distribution_update_version: String::new(),
        distribution_update_url: String::new(),
        distribution_update_sha256: String::new(),
        distribution_update_signature: String::new(),
        distribution_update_signature_key_id: String::new(),
        distribution_update_signature_algorithm: String::new(),
    }));
    let approval_manager = ApprovalManager::global();
    let capability_gateway =
        crate::capability::service::CapabilityGateway::new_with_approval_manager(
            options.clone(),
            Arc::clone(&worker_status),
            Arc::clone(&approval_manager),
        );
    let service_options = options.clone();
    let service_worker_status = Arc::clone(&worker_status);
    let service_approval_manager = Arc::clone(&approval_manager);

    let state = AgentState {
        worker_status,
        approval_manager,
        capability_gateway,
        port,
        state_path: options.state_path.clone(),
        options: options.clone(),
        dashboard_authorization: Arc::new(Mutex::new(
            crate::app::identity::DashboardAuthorizationFlow::default(),
        )),
    };
    let popup_approval_manager = Arc::clone(&state.approval_manager);

    // 本机推理网关（ADR 0113）：进程级单例，绑定在每次请求时解析，
    // 因此客户端切换注入模式后无需重启网关。
    let gateway_options = options.clone();
    match crate::app::inference_gateway::ensure_started(
        Some(crate::app::inference_gateway::configured_port(
            &options.state_path,
        )),
        Box::new(move || crate::app::ai_provider_import::gateway_bindings(&gateway_options)),
    ) {
        Ok(()) => println!(
            "inference gateway listening on {}",
            crate::app::inference_gateway::url().unwrap_or_default()
        ),
        Err(error) => eprintln!("本机推理网关未启动：{error}"),
    }

    let builder = tauri::Builder::default();
    // 单实例键 = identifier + profile（见 app::single_instance）：生产 profile 与
    // 历史键逐字一致，其它 profile 各占一个键，开发实例与已安装产品互不顶替。
    // HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE=1 只留给并行验证脚本，跳过守卫。
    let builder = if std::env::var("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE").as_deref() == Ok("1") {
        builder
    } else {
        builder.plugin(crate::app::single_instance::init(|app, args, _cwd| {
            if let Some(launch) = crate::parse_plugin_view_launch(&args) {
                let app = app.clone();
                let _ = thread::Builder::new()
                    .name("himind-plugin-view-open".to_string())
                    .spawn(move || {
                        let result = open_plugin_view(&app, &launch.plugin_id, &launch.view_id);
                        if let Some(state) = app.try_state::<AgentState>() {
                            match result {
                                Ok(()) => state.approval_manager.add_log(
                                    "info",
                                    &format!(
                                        "已打开插件窗口: {}/{}",
                                        launch.plugin_id, launch.view_id
                                    ),
                                ),
                                Err(error) => state.approval_manager.add_log(
                                    "error",
                                    &format!(
                                        "打开插件窗口失败: {}/{}: {error}",
                                        launch.plugin_id, launch.view_id
                                    ),
                                ),
                            }
                        }
                    });
            } else if let Some(target) = crate::parse_protocol_open(&args) {
                open_agent_open_target(app, target);
            } else if !args.iter().any(|argument| argument == "--protocol-url") {
                show_main_window(app);
            }
        }))
    };
    let builder = builder
        .register_uri_scheme_protocol("plugin-ui", |_ctx, request| plugin_ui_response(request))
        .manage(state)
        .on_window_event(|window, event| match (window.label(), event) {
            ("main", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            ("approval-popup", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            (label, WindowEvent::CloseRequested { api, .. })
                if label.starts_with("plugin-view-") =>
            {
                api.prevent_close();
                let plugin_window = window.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(20));
                    let _ = plugin_window.destroy();
                });
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            super::commands::get_agent_status,
            super::commands::get_agent_mode,
            super::commands::set_agent_mode,
            super::commands::get_agent_update_status,
            super::commands::check_agent_update,
            super::commands::download_agent_update,
            super::commands::cancel_agent_update_download,
            super::commands::set_agent_update_preferences,
            super::commands::install_agent_update,
            super::commands::get_dashboard_identity_status,
            super::workbench_connections::list_workbench_connections,
            super::workbench_connections::add_workbench_connection,
            super::workbench_connections::rename_workbench_connection,
            super::workbench_connections::remove_workbench_connection,
            super::workbench_connections::probe_workbench_connection,
            super::workbench_connections::switch_workbench_connection,
            super::workbench_connections::enroll_workbench_connection,
            super::commands::get_builtin_ai_activity,
            super::commands::get_local_usage_overview,
            super::commands::get_inference_gateway_status,
            super::commands::restart_inference_gateway,
            super::commands::stop_inference_gateway_and_unbind,
            super::commands::set_inference_gateway_port,
            super::commands::set_provider_binding_mode,
            super::commands::start_dashboard_authorization,
            super::commands::get_dashboard_authorization_progress,
            super::commands::cancel_dashboard_authorization,
            super::commands::open_dashboard_authorization_page,
            super::commands::revoke_dashboard_authorization,
            super::commands::test_mcp_connection,
            super::commands::get_mcp_registry_snapshot,
            super::commands::list_experts,
            super::commands::active_expert,
            super::commands::activate_expert,
            super::commands::save_expert,
            super::commands::pick_expert_package,
            super::commands::import_expert_package,
            super::commands::export_expert_package,
            super::commands::project_expert_to_client,
            super::commands::materialize_expert_project,
            super::commands::list_expert_drafts,
            super::commands::materialize_instruction_project,
            super::commands::test_expert_draft,
            super::commands::confirm_expert_draft,
            super::commands::submit_expert_draft,
            super::commands::get_mcp_targets,
            super::commands::get_instruction_targets,
            super::commands::get_workspace_instruction_context,
            super::commands::save_workspace_instruction_selection,
            super::commands::inspect_ecc_repository,
            super::commands::plan_instruction_projection,
            super::commands::apply_instruction_projection,
            super::commands::rollback_instruction_projection,
            super::commands::list_instruction_pack_drafts,
            super::commands::save_instruction_pack_draft,
            super::commands::import_instruction_file,
            super::commands::import_instruction_package,
            super::commands::test_instruction_pack_draft,
            super::commands::confirm_instruction_pack_draft,
            super::commands::publish_instruction_pack_locally,
            super::commands::pick_instruction_package,
            super::commands::inspect_mcp_target,
            super::commands::plan_mcp_registration,
            super::commands::apply_mcp_registration,
            super::commands::apply_all_mcp_registrations,
            super::commands::remove_mcp_registration,
            super::commands::remove_all_mcp_registrations,
            super::commands::test_mcp_server,
            super::commands::get_pending_approvals,
            super::commands::get_approval_history,
            super::commands::respond_approval,
            super::commands::get_approval_settings,
            super::commands::get_remote_execution_settings,
            super::commands::save_remote_execution_settings,
            super::commands::get_remote_clients,
            super::commands::detect_remote_clients,
            super::commands::configure_remote_client,
            super::commands::pick_remote_client,
            super::commands::get_builtin_ai_runtime_status,
            super::commands::pick_runtime_manifest,
            super::commands::get_builtin_ai_runtime_installation_status,
            super::commands::check_builtin_ai_runtime_update,
            super::commands::get_builtin_ai_tool_context_summary,
            super::commands::get_builtin_ai_mcp_servers,
            super::commands::save_builtin_ai_mcp_server,
            super::commands::delete_builtin_ai_mcp_server,
            super::commands::validate_builtin_ai_mcp_server,
            super::commands::get_mcp_runtime_requirements,
            super::commands::get_mcp_catalog,
            super::commands::refresh_mcp_catalog,
            super::commands::install_mcp_catalog_entry,
            super::commands::reload_builtin_ai_tool_context,
            super::commands::install_builtin_ai_runtime,
            super::commands::start_builtin_ai_runtime_install,
            super::commands::start_builtin_ai_session,
            super::commands::get_builtin_ai_session_notice,
            super::commands::list_builtin_ai_sessions,
            super::commands::stop_builtin_ai_session,
            super::commands::open_builtin_ai_web,
            super::commands::sync_builtin_ai_models,
            super::commands::set_approval_rule,
            super::commands::set_approval_profile,
            super::commands::set_approval_notification_mode,
            super::commands::set_approval_timeout,
            super::commands::get_local_login_status,
            super::commands::save_local_login,
            super::commands::logout_local_login,
            super::commands::open_dashboard_page,
            super::commands::open_inner_admin_page,
            super::commands::open_agent_directory,
            super::commands::show_main_window,
            super::commands::open_settings_window,
            super::commands::window_start_dragging,
            super::commands::window_minimize,
            super::commands::window_toggle_maximize,
            super::commands::window_close,
            super::commands::quit_agent,
            super::commands::set_auto_start,
            super::commands::pick_unity_editor,
            super::commands::save_unity_editor,
            super::commands::pick_engine_editor,
            super::commands::save_engine_editor,
            super::commands::list_engine_installations,
            super::commands::get_agent_logs,
            super::commands::export_agent_diagnostics,
            super::commands::get_agent_backup_scope,
            super::commands::export_agent_backup,
            super::commands::inspect_agent_backup,
            super::commands::import_agent_backup,
            super::commands::get_svn_connections,
            super::commands::save_svn_connection,
            super::commands::remove_svn_connection,
            super::commands::test_svn_connection,
            super::commands::get_plugin_registry,
            super::commands::get_workflow_center,
            super::commands::list_schedules,
            super::commands::set_schedule,
            super::commands::delete_schedule,
            super::commands::list_skill_runs,
            super::commands::list_workflow_presets,
            super::commands::set_workflow_preset,
            super::commands::delete_workflow_preset,
            super::commands::run_skill,
            super::commands::reveal_skill_run,
            super::commands::query_workflow_catalog,
            super::commands::get_workflow_versions,
            super::commands::get_connector_states,
            super::commands::set_connector_enabled,
            super::commands::revoke_connector,
            super::commands::restore_connector,
            super::commands::set_connector_file_credential,
            super::commands::set_connector_secret_credential,
            super::commands::remove_connector_credential,
            super::commands::get_workflow_run,
            super::commands::verify_workflow_run,
            super::commands::reveal_workflow_artifact,
            super::commands::approve_workflow_step,
            super::commands::reject_workflow_step,
            super::commands::cancel_workflow_run,
            super::commands::resume_workflow_run,
            super::commands::start_workflow_run,
            super::commands::preflight_workflow_run,
            super::commands::install_workflow_catalog_item,
            super::commands::pick_workflow_archive,
            super::commands::install_local_workflow_archive,
            super::commands::set_workflow_enabled,
            super::commands::rollback_workflow,
            super::commands::remove_workflow,
            super::commands::get_extension_sources,
            super::commands::add_extension_source,
            super::commands::add_local_extension_source,
            super::commands::pick_local_extension_source_dir,
            super::commands::update_extension_source,
            super::commands::remove_extension_source,
            super::commands::get_extension_source_snapshot,
            super::commands::set_extension_unit_acquisition,
            super::commands::install_extension_unit,
            super::commands::get_extension_provenance,
            super::commands::get_extension_lock,
            super::commands::plan_extension_updates,
            super::commands::apply_extension_updates,
            super::commands::cancel_extension_updates,
            super::commands::import_local_plugin,
            super::commands::import_github_plugin,
            super::commands::import_github_plugin_url,
            super::commands::get_extension_desired_state,
            super::commands::get_agent_task_history,
            super::commands::list_local_activity,
            super::commands::get_plugin_catalog,
            super::commands::query_plugin_catalog,
            super::commands::get_plugin_versions,
            super::commands::plan_plugin_install,
            super::commands::install_plugin,
            super::commands::uninstall_plugin,
            super::commands::rollback_plugin,
            super::commands::set_plugin_enabled,
            super::commands::repair_plugin,
            super::commands::get_agent_capabilities,
            super::commands::get_projection_sync_status,
            super::commands::requeue_projection_dead_letters,
            super::commands::list_ai_services,
            super::commands::list_ai_service_templates,
            super::commands::list_acp_runtime_profiles,
            super::commands::save_acp_runtime_profile,
            super::commands::set_acp_runtime_profile_enabled,
            super::commands::remove_acp_runtime_profile,
            super::commands::save_ai_service,
            super::commands::set_active_ai_service,
            super::commands::remove_ai_service,
            super::commands::import_ai_client,
            super::commands::remove_ai_client,
            super::commands::fetch_ai_service_models,
            super::commands::fetch_saved_ai_service_models,
            super::commands::get_skill_catalog,
            super::commands::import_local_skill,
            super::commands::import_github_skill,
            super::commands::import_github_skill_url,
            super::commands::get_organization_skill_catalog,
            super::commands::get_instruction_pack_catalog,
            super::commands::get_expert_catalog,
            super::commands::get_instruction_pack_versions,
            super::commands::install_instruction_pack_market,
            super::commands::install_expert_market,
            super::commands::query_organization_skill_catalog,
            super::commands::get_skill_versions,
            super::commands::install_organization_skill,
            super::commands::plan_organization_skill_install,
            super::commands::list_extension_projects,
            super::commands::get_extension_workspace,
            super::commands::set_extension_workspace,
            super::commands::list_extension_workspaces,
            super::commands::pick_extension_workspace_dir,
            super::commands::pick_instruction_file,
            super::commands::add_extension_workspace,
            super::commands::remove_extension_workspace,
            super::commands::open_extension_projects,
            super::commands::associate_extension_project,
            super::commands::create_extension_project,
            super::commands::build_extension_project,
            super::commands::set_extension_project_distribution_targets,
            super::commands::set_extension_unit_distribution_targets,
            super::commands::get_github_distribution_account,
            super::commands::set_github_distribution_account,
            super::commands::remove_github_distribution_account,
            super::commands::start_github_app_authorization,
            super::commands::poll_github_app_authorization,
            super::commands::list_github_app_installations,
            super::commands::select_github_app_installation,
            super::commands::import_github_app_private_key,
            super::commands::open_github_authorization_page,
            super::commands::preview_extension_distribution,
            super::commands::publish_extension_distribution,
            super::commands::get_extension_distribution_state,
            super::commands::prepare_extension_authoring,
            super::commands::remove_extension_project,
            super::commands::list_extension_collaboration_projects,
            super::commands::update_extension_project_source,
            super::commands::get_extension_collaboration,
            super::commands::list_extension_collaborator_options,
            super::commands::invite_extension_collaborator,
            super::commands::update_extension_collaborator,
            super::commands::delete_extension_collaborator,
            super::commands::list_extension_collaboration_invitations,
            super::commands::respond_extension_collaboration_invitation,
            super::commands::list_skill_drafts,
            super::commands::import_skill_candidate,
            super::commands::list_plugin_drafts,
            super::commands::import_plugin_candidate,
            super::commands::create_plugin_revision,
            super::commands::test_plugin_draft,
            super::commands::confirm_plugin_draft,
            super::commands::list_workflow_drafts,
            super::commands::test_workflow_draft,
            super::commands::confirm_workflow_draft,
            super::commands::submit_workflow_draft,
            super::commands::list_workflow_submissions,
            super::commands::list_plugin_submissions,
            super::commands::submit_plugin_draft,
            super::commands::list_skill_submissions,
            super::commands::save_skill_draft,
            super::commands::create_skill_revision,
            super::commands::test_skill_draft,
            super::commands::confirm_skill_draft,
            super::commands::submit_skill_draft,
            super::commands::get_codex_skill_status,
            super::commands::get_client_capability_matrix,
            super::commands::get_skill_workspace,
            super::commands::set_skill_workspace,
            super::commands::set_skill_workspace_enabled,
            super::commands::pick_skill_workspace,
            super::commands::pick_skill_location,
            super::commands::deploy_skill_to_location,
            super::commands::remove_skill_from_location,
            super::commands::get_skill_sync_settings,
            super::commands::set_skill_sync_mode,
            super::commands::sync_codex_skills,
            super::commands::sync_codex_skill,
            super::commands::update_skill_workspace,
            super::commands::sync_skill_client,
            super::commands::repair_codex_skill,
            super::commands::uninstall_codex_skill,
            super::commands::unregister_skill_client,
            super::commands::unregister_skill_clients,
            super::commands::open_folder,
            super::commands::open_plugin_directory,
            super::commands::register_development_plugin,
            super::commands::unregister_development_plugin,
            super::commands::invoke_development_plugin,
            super::commands::open_plugin_view,
            super::commands::create_plugin_view_shortcut,
            super::commands::close_plugin_view,
            super::commands::get_plugin_view_context,
            super::commands::pick_workspace_directory,
            super::commands::invoke_plugin_view_capability,
        ])
        .setup(move |app| {
            super::service::start_background_services(
                &service_options,
                service_worker_status,
                Some(Arc::clone(&service_approval_manager)),
                app.state::<AgentState>().capability_gateway.clone(),
            )?;
            let migration_approval_manager = Arc::clone(&service_approval_manager);
            let _ = thread::Builder::new()
                .name("himind-ai-client-migration".to_string())
                .spawn(
                    move || match super::ai_clients::migrate_legacy_agent_commands() {
                        Ok(count) if count > 0 => migration_approval_manager.add_log(
                            "info",
                            &format!("已将 {count} 个 AI 客户端连接迁移到稳定 Agent 入口"),
                        ),
                        Ok(_) => {}
                        Err(error) => migration_approval_manager
                            .add_log("warn", &format!("AI 客户端连接迁移未完成：{error}")),
                    },
                );
            start_pending_updater_repair(Arc::clone(&service_approval_manager));
            service_approval_manager
                .add_log("info", &format!("Agent 已启动，本地服务: 127.0.0.1:{port}"));
            println!("local agent app service listening on http://127.0.0.1:{port}");
            setup_tray(app, port)?;
            setup_approval_popup(app)?;
            start_internal_window_filter();
            start_approval_popup_watcher(app.handle().clone(), Arc::clone(&popup_approval_manager));
            // 平台级定时任务由桌面 Agent 自己的调度线程驱动；到点后按目标类型
            // 派发（Workflow 目标走的仍是与手动启动完全相同的 Run 路径）。
            crate::scheduler::start_scheduler(app.state::<AgentState>().capability_gateway.clone());
            // 启动时先收尾上一次进程留下的僵尸运行，再开始调度。
            if let Err(error) = crate::scheduler::abandon_stale_runs() {
                eprintln!("stale workflow run sweep failed: {error}");
            }
            start_client_skill_hygiene();
            start_extension_storage_hygiene();
            start_mcp_catalog_refresh(app.state::<AgentState>().state_path.clone());
            fit_main_window_to_monitor(app);
            if let Some(launch) = initial_plugin_view.as_ref() {
                open_plugin_view(app.handle(), &launch.plugin_id, &launch.view_id)?;
            } else if let Some(target) = initial_open_target {
                open_agent_open_target(app.handle(), target);
            }
            Ok(())
        });

    builder.run(tauri::generate_context!())?;

    Ok(())
}

/// 启动后台把客户端技能目录扫一遍：清掉旧版 `<id>/current` 布局与渲染中断
/// 留下的 staging。客户端靠递归发现 `**/SKILL.md`，这些残留会让同一个技能
/// 在客户端里出现两次，所以每次启动都收尾一次。
fn start_client_skill_hygiene() {
    thread::spawn(crate::skill::sweep_client_skill_directories);
}

/// 启动后台把本机的扩展版本目录扫一遍：插件与技能的 `versions/` 只保留
/// `current` / `previous` 两版。安装完成时也会打扫，但那一次只覆盖刚装过的
/// 那一个扩展，装完就不再更新的扩展会一直带着历史版本，目录随版本累积。
fn start_extension_storage_hygiene() {
    thread::spawn(|| {
        let removed = crate::app::plugin_manager::sweep_plugin_versions()
            + crate::skill::store::sweep_skill_versions();
        if removed == 0 {
            return;
        }
        crate::approval::manager::ApprovalManager::global().add_log(
            "info",
            &format!("已清理 {removed} 个历史版本目录，仅保留可回退的两版"),
        );
    });
}

/// 目录快照过期时在后台补一次。失败只记 warn：目录扫不到不影响已经装好的工具，
/// 也不该在启动时把界面卡住。
fn start_mcp_catalog_refresh(state_path: std::path::PathBuf) {
    thread::spawn(move || {
        crate::app::mcp_catalog::refresh_if_stale(&state_path);
    });
}

fn start_pending_updater_repair(approval_manager: Arc<ApprovalManager>) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    thread::spawn(move || {
        for attempt in 0..30 {
            thread::sleep(Duration::from_secs(1));
            match crate::install_layout::repair_pending_updater(&executable) {
                Ok(true) => {
                    approval_manager.add_log("info", "Agent updater 已完成后台修复");
                    return;
                }
                Ok(false) => return,
                Err(error) if attempt < 29 => {
                    let _ = error;
                }
                Err(error) => {
                    approval_manager
                        .add_log("warn", &format!("Agent updater 后台修复未完成：{error}"));
                }
            }
        }
    });
}

fn setup_tray(app: &tauri::App, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle();

    let open_item = MenuItem::with_id(handle, "open", "打开主窗口", true, None::<&str>)?;
    // 托盘是"一眼看状态"的地方：先给运行模式，再给待办，最后才是动作与退出。
    // 端口、版本这类诊断信息放进浮动提示，不占用菜单行。
    let mode_label = app
        .try_state::<AgentState>()
        .map(|state| {
            if state.options.mode().dashboard_enabled() {
                "已连接工作台"
            } else {
                "独立运行"
            }
        })
        .unwrap_or("运行中");
    let status_item = MenuItem::with_id(
        handle,
        "status",
        format!("状态：{mode_label} · 本地服务 127.0.0.1:{port}"),
        false,
        None::<&str>,
    )?;
    let approval_item = MenuItem::with_id(handle, "approvals", "待审批：无", false, None::<&str>)?;
    let check_update_item =
        MenuItem::with_id(handle, "check-update", "检查更新", true, None::<&str>)?;
    let update_status = handle
        .try_state::<AgentState>()
        .and_then(|state| crate::app::update_manager::load(&state.state_path).ok());
    let install_update_item = MenuItem::with_id(
        handle,
        "install-update",
        update_status
            .as_ref()
            .filter(|status| status.status == "ready")
            .map(|status| format!("重启并更新到 v{}", status.available_version))
            .unwrap_or_else(|| "重启并更新".to_string()),
        update_status
            .as_ref()
            .map(|status| status.status == "ready")
            .unwrap_or(false),
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(handle, "quit", "退出 Agent", true, None::<&str>)?;

    let state_separator = PredefinedMenuItem::separator(handle)?;
    let action_separator = PredefinedMenuItem::separator(handle)?;
    let quit_separator = PredefinedMenuItem::separator(handle)?;

    let menu = Menu::with_items(
        handle,
        &[
            &open_item,
            &status_item,
            &state_separator,
            &approval_item,
            &action_separator,
            &check_update_item,
            &install_update_item,
            &quit_separator,
            &quit_item,
        ],
    )?;

    let icon = make_tray_icon()?;
    start_update_tray_watcher(handle.clone(), install_update_item.clone());
    start_approval_tray_watcher(handle.clone(), approval_item.clone());

    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .tooltip(format!("HiMind Agent · {mode_label}"))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => {
                show_main_window(app);
            }
            "approvals" => {
                if let Some(window) = app.get_webview_window("approval-popup") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                } else {
                    show_main_window(app);
                }
            }
            "quit" => {
                stop_builtin_ai_process();
                app.exit(0);
            }
            "check-update" => {
                let app = app.clone();
                let check_item = check_update_item.clone();
                let install_item = install_update_item.clone();
                let _ = check_item.set_enabled(false);
                let _ = check_item.set_text("正在检查更新...");
                thread::spawn(move || {
                    let result = app
                        .try_state::<AgentState>()
                        .map(|state| crate::app::update_manager::check_now(&state.options));
                    let _ = check_item.set_text("检查更新");
                    let _ = check_item.set_enabled(true);
                    if let Some(Ok(status)) = result {
                        let ready = status.status == "ready";
                        let _ = install_item.set_enabled(ready);
                        let _ = install_item.set_text(if ready {
                            format!("重启并更新到 v{}", status.available_version)
                        } else {
                            "重启并更新".to_string()
                        });
                        show_main_window(&app);
                    }
                });
            }
            "install-update" => {
                if let Some(state) = app.try_state::<AgentState>() {
                    let _ = crate::app::update_manager::install(&state.options);
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(handle)?;

    Ok(())
}

fn start_update_tray_watcher(app: tauri::AppHandle, install_item: MenuItem<tauri::Wry>) {
    thread::spawn(move || {
        let mut previous = String::new();
        loop {
            let Some(state) = app.try_state::<AgentState>() else {
                return;
            };
            let Ok(status) = crate::app::update_manager::load(&state.state_path) else {
                thread::sleep(Duration::from_secs(5));
                continue;
            };
            let signature = format!("{}:{}", status.status, status.available_version);
            if signature != previous {
                let ready = status.status == "ready";
                let _ = install_item.set_enabled(ready);
                let _ = install_item.set_text(if ready {
                    format!("重启并更新到 v{}", status.available_version)
                } else {
                    "重启并更新".to_string()
                });
                previous = signature;
            }
            thread::sleep(Duration::from_secs(5));
        }
    });
}

fn start_approval_tray_watcher(app: tauri::AppHandle, approval_item: MenuItem<tauri::Wry>) {
    thread::spawn(move || {
        let mut previous = usize::MAX;
        loop {
            let Some(state) = app.try_state::<AgentState>() else {
                return;
            };
            let count = state.approval_manager.list_pending().len();
            if count != previous {
                let text = if count == 0 {
                    "待审批：无".to_string()
                } else {
                    format!("待审批：{count}")
                };
                let _ = approval_item.set_text(text);
                // 没有待办时不给入口，避免点开空弹窗。
                let _ = approval_item.set_enabled(count > 0);
                previous = count;
            }
            thread::sleep(Duration::from_millis(700));
        }
    });
}

fn setup_approval_popup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle();
    if handle.get_webview_window("approval-popup").is_some() {
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        handle,
        "approval-popup",
        WebviewUrl::App("approval-popup.html".into()),
    )
    .title("审批提醒")
    .visible(false)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .inner_size(390.0, 280.0)
    .build()?;

    position_approval_popup(&app.handle(), &window);
    Ok(())
}

fn start_approval_popup_watcher(app: tauri::AppHandle, approval_manager: Arc<ApprovalManager>) {
    thread::spawn(move || {
        let mut last_signature = String::new();
        let mut popup_visible = false;

        loop {
            let pending = approval_manager.list_pending();
            let Some(window) = app.get_webview_window("approval-popup") else {
                break;
            };

            if pending.is_empty() || !approval_manager.should_show_popup() {
                if popup_visible {
                    let _ = window.hide();
                    popup_visible = false;
                }
                last_signature.clear();
                thread::sleep(Duration::from_millis(700));
                continue;
            }

            let latest = pending
                .first()
                .map(|item| item.id.clone())
                .unwrap_or_default();
            let signature = format!("{}:{}", pending.len(), latest);
            if !popup_visible || signature != last_signature {
                position_approval_popup(&app, &window);
                let _ = window.show();
                popup_visible = true;
                last_signature = signature;
            }

            thread::sleep(Duration::from_millis(700));
        }
    });
}

fn position_approval_popup(manager: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = manager.primary_monitor() else {
        return;
    };

    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let popup_width: i32 = 390;
    let popup_height: i32 = 280;
    let margin: i32 = 18;
    let x = monitor_position.x + monitor_size.width as i32 - popup_width - margin;
    let y = monitor_position.y + monitor_size.height as i32 - popup_height - margin;
    let _ = window.set_position(tauri::Position::Physical(PhysicalPosition::new(x, y)));
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 深链唤起落点（ADR 0118 写通道②）：`Main` 只把主窗口带到前台；`SettingsAi`
/// 额外把设置窗口开到「AI 连接」面板，让用户在本机完成客户端注册。设置窗口
/// 不存在时 `open_settings_window` 会按同一路由新建。
fn open_agent_open_target(app: &tauri::AppHandle, target: crate::AgentOpenTarget) {
    match target {
        crate::AgentOpenTarget::Main => show_main_window(app),
        crate::AgentOpenTarget::SettingsAi => {
            if let Err(error) = open_settings_window(app, Some("ai"), None, None, None) {
                eprintln!("deep link open ai failed: {error}");
            }
        }
    }
}

/// 默认窗口按 1280×820 设计；屏幕更小时收进可用区域，最小不低于
/// 1024×680（与 tauri.conf.json 的 minWidth/minHeight 一致）。
fn fit_main_window_to_monitor(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let scale = monitor.scale_factor();
    let available_width = monitor.size().width as f64 / scale;
    let available_height = monitor.size().height as f64 / scale;
    // 预留任务栏与窗口边缘，避免默认尺寸刚好压在屏幕边界上。
    let width = (available_width - 120.0).clamp(1024.0, 1280.0);
    let height = (available_height - 120.0).clamp(680.0, 820.0);
    let _ = window.set_size(tauri::LogicalSize::new(width, height));
    let _ = window.center();
}

pub(crate) fn open_settings_window(
    app: &tauri::AppHandle,
    panel: Option<&str>,
    section: Option<&str>,
    tab: Option<&str>,
    ai_tab: Option<&str>,
) -> Result<(), String> {
    let panel = match panel.unwrap_or("settings") {
        "settings" | "ai" | "logs" => panel.unwrap_or("settings"),
        _ => "settings",
    };
    // Legacy section keys are forwarded verbatim so the renderer can remap
    // old deep links (remote/connectors/tools/skills/backup/logs) onto the
    // consolidated rail without a silent fall back to "general".
    let section = section
        .filter(|value| {
            matches!(
                *value,
                "accounts"
                    | "ai"
                    | "services"
                    | "automation"
                    | "approval"
                    | "general"
                    | "tooling"
                    | "diagnostics"
                    | "remote"
                    | "remote-tools"
                    | "connectors"
                    | "tools"
                    | "skills"
                    | "backup"
                    | "logs"
            )
        })
        .unwrap_or("general");
    let tab = tab
        .filter(|value| {
            matches!(
                *value,
                "connectors" | "remote-clients" | "tools" | "skills" | "backup" | "logs"
            )
        })
        .unwrap_or("");
    let ai_tab = ai_tab
        .filter(|value| matches!(*value, "mcp" | "services" | "acp"))
        .unwrap_or("mcp");
    let payload =
        serde_json::json!({ "panel": panel, "section": section, "tab": tab, "aiTab": ai_tab });

    if let Some(window) = app.get_webview_window("settings") {
        window
            .emit("himind:settings-navigate", payload)
            .map_err(|error| error.to_string())?;
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }

    // Keep the same entry point as the main window and pass the initial route
    // through an initialization script. The renderer also recognizes the
    // native window label, so the settings shell does not depend on this
    // single signal.
    let bootstrap = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .initialization_script(format!("window.__HIMIND_SETTINGS_WINDOW__ = {bootstrap};"))
        .title("HiMind Agent 设置")
        .inner_size(980.0, 720.0)
        .min_inner_size(760.0, 520.0)
        .resizable(true)
        .center()
        .build()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(any(target_os = "windows", test))]
fn should_hide_internal_window(class_name: &str) -> bool {
    // Tauri delivers AppHandle::exit through the Tao event target window.
    class_name.ends_with("-sic")
}

#[cfg(target_os = "windows")]
fn start_internal_window_filter() {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;

    unsafe extern "system" {
        fn EnumWindows(
            callback: unsafe extern "system" fn(Hwnd, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetCurrentProcessId() -> u32;
        fn GetWindowThreadProcessId(window: Hwnd, process_id: *mut u32) -> u32;
        fn GetClassNameW(window: Hwnd, class_name: *mut u16, max_count: i32) -> i32;
        fn ShowWindow(window: Hwnd, command: i32) -> i32;
    }

    unsafe extern "system" fn hide_helper(window: Hwnd, process_id: isize) -> i32 {
        let mut owner_process_id = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner_process_id) };
        if owner_process_id != process_id as u32 {
            return 1;
        }

        let mut class_name = [0_u16; 256];
        let length =
            unsafe { GetClassNameW(window, class_name.as_mut_ptr(), class_name.len() as i32) };
        let class_name = String::from_utf16_lossy(&class_name[..length.max(0) as usize]);
        if should_hide_internal_window(&class_name) {
            unsafe { ShowWindow(window, 0) };
        }
        1
    }

    thread::spawn(|| {
        let process_id = unsafe { GetCurrentProcessId() as isize };
        for _ in 0..50 {
            unsafe { EnumWindows(hide_helper, process_id) };
            thread::sleep(Duration::from_millis(100));
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn start_internal_window_filter() {}

pub(crate) fn open_plugin_view(
    app: &tauri::AppHandle,
    plugin_id: &str,
    view_id: &str,
) -> Result<(), String> {
    let Some((plugin, view, entry)) =
        crate::capability::plugin::plugin_view_entry(plugin_id, view_id)
            .map_err(|error| error.to_string())?
    else {
        return Err(format!("plugin view not found: {plugin_id}/{view_id}"));
    };
    let label = plugin_view_window_label(plugin_id, view_id);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        if window.is_visible().unwrap_or(false) {
            let _ = window.show();
            let _ = window.set_focus();
            return Ok(());
        }
        let _ = window.destroy();
    }
    let root = crate::capability::plugin::plugin_execution_dir(&plugin)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let relative_entry = entry
        .strip_prefix(&root)
        .map_err(|_| "plugin view entry is outside plugin directory")?
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string();
    let resource = format!(
        "plugin-ui://localhost/{}/{}/{}",
        plugin.id, view.id, relative_entry
    );
    let plugin_log_name = format!("{}/{}", plugin.id, view.id);
    WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::CustomProtocol(resource.parse().map_err(|_| "plugin view URL is invalid")?),
    )
    .title(view.title)
    .inner_size(1100.0, 760.0)
    .resizable(true)
    .on_navigation(crate::capability::plugin::is_plugin_ui_navigation)
    .on_page_load(move |window, payload| {
        if let Some(state) = window.app_handle().try_state::<AgentState>() {
            state.approval_manager.add_log(
                "info",
                &format!(
                    "插件窗口页面 {:?}: {} ({plugin_log_name})",
                    payload.event(),
                    payload.url()
                ),
            );
        }
    })
    .build()
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(crate) fn start_builtin_ai_session(
    options: &Options,
    workspace: Option<&Path>,
) -> Result<String, String> {
    let focus_workspace = workspace.is_some();
    let workspace = requested_builtin_ai_workspace(workspace)?;
    let key = builtin_ai_session_key(&workspace);
    if let Some(url) = reuse_builtin_ai_session(&key, focus_workspace)? {
        return Ok(url);
    }
    {
        let mut starting = builtin_ai_starting()
            .lock()
            .map_err(|_| "HiMind AI 会话状态不可用")?;
        if !starting.insert(key.clone()) {
            return Err("HiMind AI 正在启动，请稍后重试".to_string());
        }
    }

    let result = start_builtin_ai_session_inner(options, &workspace, focus_workspace);
    if let Ok(mut starting) = builtin_ai_starting().lock() {
        starting.remove(&key);
    }
    let session = result?;
    let session_url = session.proxy.url().to_string();
    builtin_ai_sessions()
        .lock()
        .map_err(|_| "HiMind AI 会话状态不可用")?
        .insert(key, session);
    Ok(session_url)
}

/// 这个工作区已经在跑就复用它，而不是重启进程。
///
/// 项目入口要求「进入后落在该项目」时，才需要把分组里的会话行补上；补失败只是
/// 分组不好看，不影响这个已经能用的会话。
fn reuse_builtin_ai_session(key: &str, focus_workspace: bool) -> Result<Option<String>, String> {
    let mut sessions = builtin_ai_sessions()
        .lock()
        .map_err(|_| "HiMind AI 会话状态不可用")?;
    let Some(session) = sessions.get_mut(key) else {
        return Ok(None);
    };
    if focus_workspace && !session.focus_workspace {
        let prepared =
            session
                .proxy
                .control()
                .prepare_rail(&session.home, &session.workspace, true);
        match prepared {
            Ok(_) => session.focus_workspace = true,
            Err(error) => session.notice = Some(error),
        }
    }
    Ok(Some(session.proxy.url().to_string()))
}

/// 启动一条会话要现起一个完整的 DSH host：解包运行时、起本地服务、再把地址打到
/// stdout。机器忙或同时在开多条会话时，冷启动十几秒是常态。
///
/// 固定预算会把「慢」误判成「坏」——超时分支会直接杀掉进程，用户看到的是
/// 「启动失败」而不是「还在启动」。所以预算随已运行的会话数增长：并发越多，
/// 每条新会话允许的等待越宽，同时仍保留上限，避免真卡死时无限等下去。
/// 真崩溃仍由 `child.try_wait()` 立即返回，不靠超时兜底。
fn builtin_ai_startup_budget(active_sessions: usize) -> Duration {
    const BASE_SECONDS: u64 = 45;
    const PER_SESSION_SECONDS: u64 = 30;
    const MAX_SECONDS: u64 = 180;
    let seconds = BASE_SECONDS
        .saturating_add(PER_SESSION_SECONDS.saturating_mul(active_sessions as u64))
        .min(MAX_SECONDS);
    Duration::from_secs(seconds)
}

/// 已经在跑的会话条数。键被锁住读不到时按 0 处理：预算退化成最保守的基础值，
/// 不会因为统计失败而放宽限制。
fn active_builtin_ai_session_count() -> usize {
    builtin_ai_sessions()
        .lock()
        .map(|sessions| sessions.len())
        .unwrap_or(0)
}

fn start_builtin_ai_session_inner(
    options: &Options,
    workspace: &Path,
    focus_workspace: bool,
) -> Result<BuiltinAiSession, String> {
    let startup_budget = builtin_ai_startup_budget(active_builtin_ai_session_count());
    let launch = crate::runtime::builtin::prepare_interactive_launch_allow_degraded(
        options,
        Some(workspace),
    )?;
    // 沙箱写权限的第一笔开销是"按工作区一次性"的：ACE 落地要向整棵树传播，实测
    // 十几万文件的工作区要 90 秒。它会算进用户的第一条命令里，所以在这里就用
    // 后台线程先付掉——会话启动本身不等它。
    crate::app::sandbox_warmup::spawn(&launch.executable, &launch.workspace);
    let notice = (!launch.control_plane_notice.trim().is_empty())
        .then(|| launch.control_plane_notice.clone());
    let mut command = if launch
        .executable
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"))
    {
        let mut command = Command::new("cmd.exe");
        command.arg("/D").arg("/C").arg(&launch.executable);
        command
    } else {
        Command::new(&launch.executable)
    };
    command
        .args(["--profile", "himind", "--patch"])
        .arg(&launch.agent_patch)
        .args(["--host", "127.0.0.1", "--port", "0", "--no-open"])
        .current_dir(&launch.workspace)
        .env("DSH_HOME", &launch.home)
        .env("DSH_TELEMETRY_MODE", "DISABLED")
        .env("DSH_PERMISSION_MODE", launch.permission_mode)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::runtime::process::remove_himind_secret_environment(&mut command);
    // 空值也会覆盖子进程环境：把 `""` 写进 `DEEPSEEK_API_KEY` 会让 Runtime
    // 报「凭据缺失」，而它本来可以读自己 settings.yaml / .credentials.yaml 里
    // 的配置。没有密钥时就不要设置这个变量。
    if !launch.api_key.trim().is_empty() {
        if let Some(api_key_env) = launch.api_key_env.as_deref() {
            command.env(api_key_env, &launch.api_key);
        }
    }
    if !launch.base_url.trim().is_empty() {
        command.env("DEEPSEEK_BASE_URL", &launch.base_url);
    }
    crate::runtime::process::configure_hidden_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 HiMind AI：{error}"))?;
    let Some(stdout) = child.stdout.take() else {
        crate::runtime::process::terminate_process_tree(&mut child);
        let _ = child.wait();
        return Err("HiMind AI 没有返回启动输出".to_string());
    };
    let stderr = child.stderr.take();
    let (url_sender, url_receiver) = std::sync::mpsc::channel::<String>();
    let diagnostics = Arc::new(Mutex::new(Vec::<String>::new()));
    let stdout_diagnostics = Arc::clone(&diagnostics);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().flatten() {
            append_builtin_ai_diagnostic(&stdout_diagnostics, &line);
            if let Some(url) = line
                .split_whitespace()
                .find(|value| value.starts_with("http://127.0.0.1:"))
            {
                let _ = url_sender.send(url.trim_end_matches(')').to_string());
            }
        }
    });
    if let Some(stderr) = stderr {
        let stderr_diagnostics = Arc::clone(&diagnostics);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().flatten() {
                append_builtin_ai_diagnostic(&stderr_diagnostics, &line);
            }
        });
    }
    let deadline = Instant::now() + startup_budget;
    let url = loop {
        if let Ok(url) = url_receiver.recv_timeout(Duration::from_millis(200)) {
            break url;
        }
        if Instant::now() >= deadline {
            crate::runtime::process::terminate_process_tree(&mut child);
            let _ = child.wait();
            return Err(builtin_ai_startup_error(
                &format!(
                    "HiMind AI 启动超时（等待 {} 秒），请检查运行时状态",
                    startup_budget.as_secs()
                ),
                &diagnostics,
                &launch.api_key,
            ));
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(builtin_ai_startup_error(
                &format!("HiMind AI 启动失败（退出码 {:?}）", status.code()),
                &diagnostics,
                &launch.api_key,
            ));
        }
    };
    let (event_sync, observer) = BuiltinAiEventSync::start(
        options.clone(),
        crate::app::builtin_ai_gateway::RuntimeCapabilities::conservative(),
    );
    let origin_key = launch.home.to_string_lossy().to_string();
    let mut proxy =
        BuiltinAiProxy::start(&url, Some(observer), Some(&origin_key)).map_err(|error| {
            crate::runtime::process::terminate_process_tree(&mut child);
            let _ = child.wait();
            error
        })?;
    if let Err(error) = proxy.control().verify_browser_entry() {
        proxy.stop();
        crate::runtime::process::terminate_process_tree(&mut child);
        let _ = child.wait();
        return Err(format!("HiMind AI 页面认证失败：{error}"));
    }
    // The rail groups Sessions by Workspace and paints nothing for a closed
    // group, so the entry registers its own directory and opens the recorded
    // groups before the page's own scripts boot. Only an explicit project entry
    // claims a Session row in its group; the default entry reuses whatever the
    // rail already remembers. A refused Workspace is a degraded rail, not a
    // reason to keep the user out of HiMind AI.
    if let Err(error) =
        proxy
            .control()
            .prepare_rail(&launch.home, &launch.workspace, focus_workspace)
    {
        append_builtin_ai_diagnostic(&diagnostics, &error);
    }
    let runtime_capabilities =
        crate::app::builtin_ai_gateway::probe_builtin_ai_capabilities(&proxy.control());
    event_sync.set_capabilities(runtime_capabilities);
    // 只有真的拿到工作台凭据时才把目录推给 DSH：降级会话用的是本机 AI 服务，
    // 把它的地址与模型写进工作台的 `himind-proxy` 路由会污染用户 settings.yaml。
    if launch.service_source == "managed" {
        let _ = proxy.control().sync_model_catalog(
            &launch.default_model,
            &launch.base_url,
            &launch.models,
        );
    }
    let model_sync = BuiltinAiModelSync::start(ModelSyncSnapshot {
        user_id: launch.user_id,
        default_model: launch.default_model,
        base_url: launch.base_url.clone(),
        models: launch.models,
        credential_fingerprint: launch.credential_fingerprint,
        catalog_fingerprint: launch.catalog_fingerprint,
    });
    let command_gateway = options.mode().dashboard_enabled().then(|| {
        BuiltinAiCommandGateway::start(
            options.clone(),
            proxy.control(),
            event_sync.capabilities_state(),
        )
    });
    Ok(BuiltinAiSession {
        child,
        home: launch.home.clone(),
        workspace: launch.workspace.clone(),
        focus_workspace,
        notice,
        proxy,
        event_sync,
        model_sync: Arc::new(model_sync),
        command_gateway,
    })
}

fn requested_builtin_ai_workspace(workspace: Option<&Path>) -> Result<PathBuf, String> {
    let workspace = match workspace {
        Some(path) => path.to_path_buf(),
        // 主入口没有指定目录时，先复用 DSH 上次使用的工作区，再退到进程当前目录，
        // 避免每次都在启动器的 `logs` 目录里新开工作区、让历史会话看起来消失。
        None => crate::runtime::builtin::recent_interactive_workspace()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| "无法确定 HiMind AI 工作目录".to_string())?,
    };
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("HiMind AI 工作目录不可用: {error}"))?;
    if !workspace.is_dir() {
        return Err("HiMind AI 工作目录必须是本机目录".to_string());
    }
    Ok(workspace)
}

fn append_builtin_ai_diagnostic(diagnostics: &Arc<Mutex<Vec<String>>>, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if let Ok(mut lines) = diagnostics.lock() {
        if lines.len() == 24 {
            lines.remove(0);
        }
        lines.push(line.to_string());
    }
}

fn builtin_ai_startup_error(
    summary: &str,
    diagnostics: &Arc<Mutex<Vec<String>>>,
    api_key: &str,
) -> String {
    let detail = diagnostics
        .lock()
        .map(|lines| lines.join("\n"))
        .unwrap_or_default();
    if detail.is_empty() {
        return summary.to_string();
    }
    let detail = if api_key.is_empty() {
        detail
    } else {
        detail.replace(api_key, "[redacted]")
    };
    format!(
        "{summary}: {}",
        crate::runtime::process::summarize_output(&detail, 1_200)
    )
}

/// 关掉全部会话。用于退出应用、卸载/更新运行时、切换对接模式这类「会话基座
/// 变了」的场景 —— 单个工作区的开关不要走这里，用 `stop_builtin_ai_session`。
pub(crate) fn stop_builtin_ai_process() {
    stop_builtin_ai_sessions(None);
}

/// 关掉一个工作区的会话，其它工作区的会话继续跑。返回是否真的关掉了一个。
pub(crate) fn stop_builtin_ai_session(workspace: &Path) -> bool {
    stop_builtin_ai_sessions(Some(builtin_ai_session_key(workspace)))
}

fn stop_builtin_ai_sessions(key: Option<String>) -> bool {
    let stopped: Vec<BuiltinAiSession> = {
        let Ok(mut sessions) = builtin_ai_sessions().lock() else {
            return false;
        };
        match key {
            Some(key) => sessions.remove(&key).into_iter().collect(),
            None => sessions.drain().map(|(_, session)| session).collect(),
        }
    };
    let stopped_any = !stopped.is_empty();
    for mut session in stopped {
        if let Some(command_gateway) = session.command_gateway.as_mut() {
            command_gateway.stop();
        }
        session.proxy.stop();
        session.event_sync.stop();
        crate::runtime::process::terminate_process_tree(&mut session.child);
        let _ = session.child.wait();
    }
    stopped_any
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct BuiltinAiSessionSnapshot {
    pub workspace_root: String,
    pub url: String,
    pub focus_workspace: bool,
    pub notice: Option<String>,
}

/// 当前在跑的 HiMind AI 会话，按工作区各一条。界面用它恢复标签页。
pub(crate) fn builtin_ai_session_snapshots() -> Vec<BuiltinAiSessionSnapshot> {
    let Ok(sessions) = builtin_ai_sessions().lock() else {
        return Vec::new();
    };
    let mut snapshots: Vec<BuiltinAiSessionSnapshot> = sessions
        .values()
        .map(|session| BuiltinAiSessionSnapshot {
            workspace_root: crate::extension_workspace::display_path(&session.workspace),
            url: session.proxy.url().to_string(),
            focus_workspace: session.focus_workspace,
            notice: session.notice.clone(),
        })
        .collect();
    snapshots.sort_by(|left, right| left.workspace_root.cmp(&right.workspace_root));
    snapshots
}

fn first_builtin_ai_session_url() -> Option<String> {
    let sessions = builtin_ai_sessions().lock().ok()?;
    sessions
        .values()
        .next()
        .map(|session| session.proxy.url().to_string())
}

/// Reconcile the active DSH process with the current Dashboard AI service.
/// Model catalog changes are applied live; credential/route changes request a
/// clean process restart so the new environment is used.
pub(crate) fn sync_builtin_ai_models(
    options: &Options,
) -> Result<BuiltinAiModelSyncResult, String> {
    // 模型目录是全网关共享的：一次同步要把每个在跑的工作区会话都刷一遍，
    // 否则后开的会话看不到新模型。
    let plan: Vec<(String, PathBuf, bool)> = {
        let sessions = builtin_ai_sessions()
            .lock()
            .map_err(|_| "HiMind AI 会话状态不可用")?;
        if sessions.is_empty() {
            return Err("HiMind AI 会话尚未启动".to_string());
        }
        sessions
            .iter()
            .map(|(key, session)| {
                (
                    key.clone(),
                    session.workspace.clone(),
                    session.focus_workspace,
                )
            })
            .collect()
    };
    let mut model_count = 0usize;
    let mut restart_required = false;
    let mut updated = false;
    for (key, _, _) in &plan {
        let Some((model_sync, proxy)) = builtin_ai_sessions()
            .lock()
            .map_err(|_| "HiMind AI 会话状态不可用")?
            .get(key)
            .map(|session| (Arc::clone(&session.model_sync), session.proxy.control()))
        else {
            continue;
        };
        // 远程凭据/模型目录读取不在会话总锁内执行，否则网络抖动会阻塞
        // 其它工作区的启动、停止和状态读取。
        let result = model_sync.sync_now(options, &proxy)?;
        model_count = model_count.max(result.model_count);
        updated |= result.status == "updated";
        restart_required |= result.status == "restart_required";
    }
    if restart_required {
        stop_builtin_ai_process();
        for (_, workspace, focus_workspace) in &plan {
            start_builtin_ai_session(options, focus_workspace.then_some(workspace.as_path()))?;
        }
        return Ok(BuiltinAiModelSyncResult {
            status: "restarted".to_string(),
            model_count,
            restarted: true,
            session_url: first_builtin_ai_session_url().unwrap_or_default(),
        });
    }
    Ok(BuiltinAiModelSyncResult {
        status: if updated { "updated" } else { "unchanged" }.to_string(),
        model_count,
        restarted: false,
        session_url: first_builtin_ai_session_url()
            .ok_or_else(|| "HiMind AI 会话尚未启动".to_string())?,
    })
}

pub(crate) fn plugin_view_window_label(plugin_id: &str, view_id: &str) -> String {
    format!(
        "plugin-view-{}-{}",
        sanitize_window_label(plugin_id),
        sanitize_window_label(view_id)
    )
}

fn sanitize_window_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn plugin_ui_response(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    const MAX_PLUGIN_UI_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;
    let result = (|| -> Result<Response<Vec<u8>>, Box<dyn std::error::Error>> {
        let url = url::Url::parse(&request.uri().to_string())?;
        let (path, content_type) = crate::capability::plugin::resolve_plugin_ui_resource(&url)?;
        let size = std::fs::metadata(&path)?.len();
        if size > MAX_PLUGIN_UI_RESOURCE_BYTES {
            return Err(
                format!("plugin UI resource exceeds {MAX_PLUGIN_UI_RESOURCE_BYTES} bytes").into(),
            );
        }
        let body = std::fs::read(path)?;
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header("Content-Security-Policy", "default-src 'self' data: blob: https: http:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; script-src 'self' 'unsafe-inline' https: http:; style-src 'self' 'unsafe-inline' https: http:; connect-src *; img-src 'self' data: blob: https: http:; media-src 'self' data: blob: https: http:")
            .header("X-Content-Type-Options", "nosniff")
            .body(body)?)
    })();
    result.unwrap_or_else(|error| {
        let status = if error.to_string().contains("exceeds") {
            StatusCode::from_u16(413).unwrap_or(StatusCode::NOT_FOUND)
        } else {
            StatusCode::NOT_FOUND
        };
        Response::builder()
            .status(status)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("X-Content-Type-Options", "nosniff")
            .body(format!(
                "<!doctype html><meta charset=\"utf-8\"><title>插件页面加载失败</title><body style=\"font-family:Segoe UI,Microsoft YaHei,sans-serif;padding:32px;color:#172033\"><h1>插件页面加载失败</h1><pre style=\"white-space:pre-wrap\">{}</pre></body>",
                escape_html(&error.to_string())
            ).into_bytes())
            .unwrap_or_else(|_| Response::new(Vec::new()))
    })
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn make_tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(include_bytes!("../../icons/himind-tray.png"))
}

#[cfg(test)]
mod tray_icon_tests {
    use super::make_tray_icon;

    #[test]
    fn embedded_tray_icon_is_valid_rgba() {
        let icon = make_tray_icon().expect("embedded tray icon should decode");
        assert_eq!((icon.width(), icon.height()), (64, 64));
        assert_eq!(icon.rgba().len(), 64 * 64 * 4);
    }
}

#[cfg(test)]
mod internal_window_filter_tests {
    use super::should_hide_internal_window;

    #[test]
    fn preserves_tao_event_target_used_by_app_exit() {
        assert!(!should_hide_internal_window("Tao Thread Event Target"));
        assert!(should_hide_internal_window("internal-sic"));
    }
}

#[cfg(test)]
mod builtin_ai_startup_tests {
    use super::{builtin_ai_startup_budget, builtin_ai_startup_error};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 并发越多，单条会话的启动预算越宽；同时必须有上限，不能无限等。
    #[test]
    fn startup_budget_grows_with_concurrency_and_stays_bounded() {
        let idle = builtin_ai_startup_budget(0);
        let two = builtin_ai_startup_budget(2);
        let many = builtin_ai_startup_budget(12);

        assert_eq!(idle, Duration::from_secs(45));
        assert_eq!(two, Duration::from_secs(105));
        assert!(many <= Duration::from_secs(180));
        assert!(many > two && two > idle);
    }

    #[test]
    fn startup_diagnostics_redact_the_runtime_api_key() {
        let diagnostics = Arc::new(Mutex::new(vec![
            "runtime failed with api_key=secret-value".to_string()
        ]));

        let error = builtin_ai_startup_error("HiMind AI 启动失败", &diagnostics, "secret-value");

        assert!(error.contains("[redacted]"));
        assert!(!error.contains("secret-value"));
    }
}
