use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::http::{Request, Response, StatusCode};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent,
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
    workspace: PathBuf,
    focus_workspace: bool,
    proxy: BuiltinAiProxy,
    event_sync: BuiltinAiEventSync,
    model_sync: BuiltinAiModelSync,
    command_gateway: Option<BuiltinAiCommandGateway>,
}

static BUILTIN_AI_SESSION: std::sync::OnceLock<Mutex<Option<BuiltinAiSession>>> =
    std::sync::OnceLock::new();
static BUILTIN_AI_STARTING: std::sync::OnceLock<AtomicBool> = std::sync::OnceLock::new();

fn builtin_ai_session() -> &'static Mutex<Option<BuiltinAiSession>> {
    BUILTIN_AI_SESSION.get_or_init(|| Mutex::new(None))
}

fn builtin_ai_starting() -> &'static AtomicBool {
    BUILTIN_AI_STARTING.get_or_init(|| AtomicBool::new(false))
}

pub(crate) fn run_tauri_app(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let port = options.local_port;
    let initial_plugin_view = options.plugin_view_launch();
    let initial_protocol_open = options.protocol_open_requested();

    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: if options.mode().dashboard_enabled() {
            "正在连接 Dashboard 任务 Worker".to_string()
        } else {
            String::new()
        },
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
    let approval_manager = Arc::new(ApprovalManager::new());
    let service_options = options.clone();
    let service_worker_status = Arc::clone(&worker_status);
    let service_approval_manager = Arc::clone(&approval_manager);

    let state = AgentState {
        worker_status,
        approval_manager,
        port,
        dashboard_base: options.api_base.clone(),
        state_path: options.state_path.clone(),
        options: options.clone(),
        dashboard_authorization: Arc::new(Mutex::new(
            crate::app::identity::DashboardAuthorizationFlow::default(),
        )),
    };
    let popup_approval_manager = Arc::clone(&state.approval_manager);

    let builder = tauri::Builder::default();
    let builder = if std::env::var("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE").as_deref() == Ok("1") {
        builder
    } else {
        builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if let Some(launch) = crate::parse_plugin_view_launch(&args) {
                let result = open_plugin_view(app, &launch.plugin_id, &launch.view_id);
                if let Some(state) = app.try_state::<AgentState>() {
                    match result {
                        Ok(()) => state.approval_manager.add_log(
                            "info",
                            &format!("已打开插件窗口: {}/{}", launch.plugin_id, launch.view_id),
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
            } else if crate::protocol_open_requested(&args)
                || !args.iter().any(|argument| argument == "--protocol-url")
            {
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
            super::commands::get_builtin_ai_activity,
            super::commands::start_dashboard_authorization,
            super::commands::get_dashboard_authorization_progress,
            super::commands::cancel_dashboard_authorization,
            super::commands::open_dashboard_authorization_page,
            super::commands::revoke_dashboard_authorization,
            super::commands::test_mcp_connection,
            super::commands::get_mcp_registry_snapshot,
            super::commands::get_mcp_targets,
            super::commands::inspect_mcp_target,
            super::commands::plan_mcp_registration,
            super::commands::apply_mcp_registration,
            super::commands::apply_all_mcp_registrations,
            super::commands::remove_mcp_registration,
            super::commands::remove_all_mcp_registrations,
            super::commands::test_mcp_server,
            super::commands::get_pending_approvals,
            super::commands::respond_approval,
            super::commands::get_approval_settings,
            super::commands::get_remote_execution_settings,
            super::commands::save_remote_execution_settings,
            super::commands::get_remote_clients,
            super::commands::detect_remote_clients,
            super::commands::configure_remote_client,
            super::commands::pick_remote_client,
            super::commands::get_builtin_ai_runtime_status,
            super::commands::get_builtin_ai_runtime_installation_status,
            super::commands::check_builtin_ai_runtime_update,
            super::commands::get_builtin_ai_tool_context_summary,
            super::commands::get_builtin_ai_mcp_servers,
            super::commands::save_builtin_ai_mcp_server,
            super::commands::delete_builtin_ai_mcp_server,
            super::commands::validate_builtin_ai_mcp_server,
            super::commands::reload_builtin_ai_tool_context,
            super::commands::install_builtin_ai_runtime,
            super::commands::start_builtin_ai_runtime_install,
            super::commands::start_builtin_ai_session,
            super::commands::sync_builtin_ai_models,
            super::commands::set_approval_rule,
            super::commands::set_approval_timeout,
            super::commands::get_local_login_status,
            super::commands::save_local_login,
            super::commands::logout_local_login,
            super::commands::open_dashboard_page,
            super::commands::open_inner_admin_page,
            super::commands::open_agent_directory,
            super::commands::show_main_window,
            super::commands::window_start_dragging,
            super::commands::window_minimize,
            super::commands::window_toggle_maximize,
            super::commands::window_close,
            super::commands::quit_agent,
            super::commands::set_auto_start,
            super::commands::pick_unity_editor,
            super::commands::save_unity_editor,
            super::commands::get_agent_logs,
            super::commands::export_agent_diagnostics,
            super::commands::get_svn_connections,
            super::commands::save_svn_connection,
            super::commands::remove_svn_connection,
            super::commands::test_svn_connection,
            super::commands::get_plugin_registry,
            super::commands::get_extension_sources,
            super::commands::add_extension_source,
            super::commands::update_extension_source,
            super::commands::remove_extension_source,
            super::commands::get_extension_source_snapshot,
            super::commands::get_extension_provenance,
            super::commands::import_local_plugin,
            super::commands::import_github_plugin,
            super::commands::import_github_plugin_url,
            super::commands::get_extension_desired_state,
            super::commands::get_agent_task_history,
            super::commands::get_plugin_catalog,
            super::commands::query_plugin_catalog,
            super::commands::get_plugin_versions,
            super::commands::plan_plugin_install,
            super::commands::install_plugin,
            super::commands::uninstall_plugin,
            super::commands::rollback_plugin,
            super::commands::set_plugin_enabled,
            super::commands::get_agent_capabilities,
            super::commands::get_skill_catalog,
            super::commands::import_local_skill,
            super::commands::import_github_skill,
            super::commands::import_github_skill_url,
            super::commands::get_organization_skill_catalog,
            super::commands::query_organization_skill_catalog,
            super::commands::get_skill_versions,
            super::commands::install_organization_skill,
            super::commands::plan_organization_skill_install,
            super::commands::list_extension_projects,
            super::commands::get_extension_workspace,
            super::commands::select_extension_workspace,
            super::commands::open_extension_projects,
            super::commands::associate_extension_project,
            super::commands::create_extension_project,
            super::commands::build_extension_project,
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
            super::commands::list_plugin_submissions,
            super::commands::submit_plugin_draft,
            super::commands::list_skill_submissions,
            super::commands::save_skill_draft,
            super::commands::create_skill_revision,
            super::commands::test_skill_draft,
            super::commands::confirm_skill_draft,
            super::commands::submit_skill_draft,
            super::commands::get_codex_skill_status,
            super::commands::get_skill_sync_settings,
            super::commands::set_skill_sync_mode,
            super::commands::sync_codex_skills,
            super::commands::sync_codex_skill,
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
            super::commands::invoke_plugin_view_capability,
        ])
        .setup(move |app| {
            super::service::start_background_services(
                &service_options,
                service_worker_status,
                Some(Arc::clone(&service_approval_manager)),
            )?;
            match super::ai_clients::migrate_legacy_agent_commands() {
                Ok(count) if count > 0 => service_approval_manager.add_log(
                    "info",
                    &format!("已将 {count} 个 AI 客户端连接迁移到稳定 Agent 入口"),
                ),
                Ok(_) => {}
                Err(error) => service_approval_manager
                    .add_log("warn", &format!("AI 客户端连接迁移未完成：{error}")),
            }
            start_pending_updater_repair(Arc::clone(&service_approval_manager));
            service_approval_manager
                .add_log("info", &format!("Agent 已启动，本地服务: 127.0.0.1:{port}"));
            println!("local agent app service listening on http://127.0.0.1:{port}");
            setup_tray(app, port)?;
            setup_approval_popup(app)?;
            start_internal_window_filter();
            start_approval_popup_watcher(app.handle().clone(), Arc::clone(&popup_approval_manager));
            if let Some(launch) = initial_plugin_view.as_ref() {
                open_plugin_view(app.handle(), &launch.plugin_id, &launch.view_id)?;
            } else if initial_protocol_open {
                show_main_window(app.handle());
            }
            Ok(())
        });

    builder.run(tauri::generate_context!())?;

    Ok(())
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
    let status_item = MenuItem::with_id(
        handle,
        "status",
        format!("本地服务: 127.0.0.1:{port}"),
        false,
        None::<&str>,
    )?;
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

    let menu = Menu::with_items(
        handle,
        &[
            &open_item,
            &status_item,
            &check_update_item,
            &install_update_item,
            &quit_item,
        ],
    )?;

    let icon = make_tray_icon()?;
    start_update_tray_watcher(handle.clone(), install_update_item.clone());

    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .tooltip("HiMind Agent")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => {
                show_main_window(app);
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

            if pending.is_empty() {
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
    WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::CustomProtocol(resource.parse().map_err(|_| "plugin view URL is invalid")?),
    )
    .title(view.title)
    .inner_size(1100.0, 760.0)
    .resizable(true)
    .on_navigation(crate::capability::plugin::is_plugin_ui_navigation)
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
    {
        let active = builtin_ai_session()
            .lock()
            .map_err(|_| "HiMind AI 会话状态不可用")?;
        if let Some(session) = active.as_ref() {
            if session.workspace == workspace && session.focus_workspace == focus_workspace {
                return Ok(session.proxy.url().to_string());
            }
        }
    }
    if builtin_ai_session()
        .lock()
        .map_err(|_| "HiMind AI 会话状态不可用")?
        .is_some()
    {
        stop_builtin_ai_process();
    }
    if builtin_ai_starting().swap(true, Ordering::AcqRel) {
        return Err("HiMind AI 正在启动，请稍后重试".to_string());
    }

    let result = start_builtin_ai_session_inner(options, &workspace, focus_workspace);
    builtin_ai_starting().store(false, Ordering::Release);
    result
}

fn start_builtin_ai_session_inner(
    options: &Options,
    workspace: &Path,
    focus_workspace: bool,
) -> Result<String, String> {
    let launch = crate::runtime::builtin::prepare_interactive_launch(options, Some(workspace))?;
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
        .args(["--host", "127.0.0.1", "--port", "0"])
        .current_dir(&launch.workspace)
        .env("DSH_HOME", &launch.home)
        .env("DSH_TELEMETRY_MODE", "DISABLED")
        .env("DSH_PERMISSION_MODE", launch.permission_mode)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::runtime::process::remove_himind_secret_environment(&mut command);
    if let Some(api_key_env) = launch.api_key_env.as_deref() {
        command.env(api_key_env, &launch.api_key);
    }
    if !launch.base_url.trim().is_empty() {
        command.env("DEEPSEEK_BASE_URL", &launch.base_url);
    }
    crate::runtime::process::configure_hidden_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 HiMind AI：{error}"))?;
    let stdout = child.stdout.take().ok_or("HiMind AI 没有返回启动输出")?;
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
    let deadline = Instant::now() + Duration::from_secs(20);
    let url = loop {
        if let Ok(url) = url_receiver.recv_timeout(Duration::from_millis(200)) {
            break url;
        }
        if Instant::now() >= deadline {
            crate::runtime::process::terminate_process_tree(&mut child);
            let _ = child.wait();
            return Err(builtin_ai_startup_error(
                "HiMind AI 启动超时，请检查运行时状态",
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
    let mut proxy = BuiltinAiProxy::start(&url, Some(observer)).map_err(|error| {
        crate::runtime::process::terminate_process_tree(&mut child);
        let _ = child.wait();
        error
    })?;
    if focus_workspace {
        if let Err(error) = proxy.control().start_workspace_session(&launch.workspace) {
            proxy.stop();
            crate::runtime::process::terminate_process_tree(&mut child);
            let _ = child.wait();
            return Err(format!("无法进入扩展项目工作区：{error}"));
        }
    }
    let runtime_capabilities =
        crate::app::builtin_ai_gateway::probe_builtin_ai_capabilities(&proxy.control());
    event_sync.set_capabilities(runtime_capabilities);
    // Connected mode owns the Dashboard-backed model catalog. Independent
    // mode leaves provider and model selection entirely in native DSH config.
    if options.mode().dashboard_enabled() {
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
    let session_url = proxy.url().to_string();
    let workspace = launch.workspace.clone();
    *builtin_ai_session()
        .lock()
        .map_err(|_| "HiMind AI 会话状态不可用")? = Some(BuiltinAiSession {
        child,
        workspace,
        focus_workspace,
        proxy,
        event_sync,
        model_sync,
        command_gateway,
    });
    Ok(session_url)
}

fn requested_builtin_ai_workspace(workspace: Option<&Path>) -> Result<PathBuf, String> {
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

pub(crate) fn stop_builtin_ai_process() {
    if let Ok(mut active) = builtin_ai_session().lock() {
        if let Some(mut session) = active.take() {
            if let Some(command_gateway) = session.command_gateway.as_mut() {
                command_gateway.stop();
            }
            session.proxy.stop();
            session.event_sync.stop();
            crate::runtime::process::terminate_process_tree(&mut session.child);
            let _ = session.child.wait();
        }
    }
}

/// Reconcile the active DSH process with the current Dashboard AI service.
/// Model catalog changes are applied live; credential/route changes request a
/// clean process restart so the new environment is used.
pub(crate) fn sync_builtin_ai_models(
    options: &Options,
) -> Result<BuiltinAiModelSyncResult, String> {
    let (result, workspace, focus_workspace) = {
        let active = builtin_ai_session()
            .lock()
            .map_err(|_| "HiMind AI 会话状态不可用")?;
        let session = active
            .as_ref()
            .ok_or_else(|| "HiMind AI 会话尚未启动".to_string())?;
        (
            session
                .model_sync
                .sync_now(options, &session.proxy.control())?,
            session.workspace.clone(),
            session.focus_workspace,
        )
    };
    if result.status == "restart_required" {
        stop_builtin_ai_process();
        let session_url =
            start_builtin_ai_session(options, focus_workspace.then_some(workspace.as_path()))?;
        return Ok(BuiltinAiModelSyncResult {
            status: "restarted".to_string(),
            model_count: result.model_count,
            restarted: true,
            session_url,
        });
    }
    let session_url = builtin_ai_session()
        .lock()
        .map_err(|_| "HiMind AI 会话状态不可用")?
        .as_ref()
        .map(|session| session.proxy.url().to_string())
        .ok_or_else(|| "HiMind AI 会话尚未启动".to_string())?;
    Ok(BuiltinAiModelSyncResult {
        session_url,
        ..result
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
    let result = (|| -> Result<Response<Vec<u8>>, Box<dyn std::error::Error>> {
        let url = url::Url::parse(&request.uri().to_string())?;
        let (path, content_type) = crate::capability::plugin::resolve_plugin_ui_resource(&url)?;
        let body = std::fs::read(path)?;
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header("Content-Security-Policy", "default-src 'self' data: blob: https: http:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; script-src 'self' 'unsafe-inline' https: http:; style-src 'self' 'unsafe-inline' https: http:; connect-src *; img-src 'self' data: blob: https: http:; media-src 'self' data: blob: https: http:")
            .header("X-Content-Type-Options", "nosniff")
            .body(body)?)
    })();
    result.unwrap_or_else(|error| {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
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
    use super::builtin_ai_startup_error;
    use std::sync::{Arc, Mutex};

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
