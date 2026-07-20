use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tauri::http::{Request, Response, StatusCode};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

use crate::app::commands::AgentState;
use crate::approval::manager::ApprovalManager;
use crate::store::types::LocalWorkerStatus;
use crate::Options;

pub(crate) fn run_tauri_app(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let port = options.local_port;
    let initial_plugin_view = options.plugin_view_launch();

    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: "正在连接 Dashboard 任务 Worker".to_string(),
        local_service_online: false,
        local_service_error: String::new(),
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
    };
    let popup_approval_manager = Arc::clone(&state.approval_manager);

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
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
            } else {
                show_main_window(app);
            }
        }))
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
            super::commands::get_pending_approvals,
            super::commands::respond_approval,
            super::commands::get_approval_settings,
            super::commands::set_approval_rule,
            super::commands::set_approval_timeout,
            super::commands::get_local_login_status,
            super::commands::save_local_login,
            super::commands::logout_local_login,
            super::commands::open_dashboard_page,
            super::commands::open_inner_admin_page,
            super::commands::open_agent_directory,
            super::commands::show_main_window,
            super::commands::set_auto_start,
            super::commands::get_agent_logs,
            super::commands::get_plugin_registry,
            super::commands::get_plugin_catalog,
            super::commands::install_plugin,
            super::commands::uninstall_plugin,
            super::commands::rollback_plugin,
            super::commands::set_plugin_enabled,
            super::commands::get_agent_capabilities,
            super::commands::get_skill_catalog,
            super::commands::get_organization_skill_catalog,
            super::commands::install_organization_skill,
            super::commands::plan_organization_skill_install,
            super::commands::list_skill_drafts,
            super::commands::list_skill_submissions,
            super::commands::save_skill_draft,
            super::commands::test_skill_draft,
            super::commands::confirm_skill_draft,
            super::commands::submit_skill_draft,
            super::commands::get_codex_skill_status,
            super::commands::sync_codex_skills,
            super::commands::sync_codex_skill,
            super::commands::repair_codex_skill,
            super::commands::uninstall_codex_skill,
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
            service_approval_manager
                .add_log("info", &format!("Agent 已启动，本地服务: 127.0.0.1:{port}"));
            println!("local agent app service listening on http://127.0.0.1:{port}");
            setup_tray(app, port)?;
            setup_approval_popup(app)?;
            start_internal_window_filter();
            start_approval_popup_watcher(app.handle().clone(), Arc::clone(&popup_approval_manager));
            if let Some(launch) = initial_plugin_view.as_ref() {
                open_plugin_view(app.handle(), &launch.plugin_id, &launch.view_id)?;
            }
            Ok(())
        });

    builder.run(tauri::generate_context!())?;

    Ok(())
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
    let quit_item = MenuItem::with_id(handle, "quit", "退出 Agent", true, None::<&str>)?;

    let menu = Menu::with_items(handle, &[&open_item, &status_item, &quit_item])?;

    let icon = make_tray_icon();

    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .tooltip("HiMind Agent")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                show_main_window(app);
            }
            "quit" => {
                app.exit(0);
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
        if class_name == "Tao Thread Event Target" || class_name.ends_with("-sic") {
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

fn make_tray_icon() -> tauri::image::Image<'static> {
    let width: u32 = 32;
    let height: u32 = 32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let inside = x > 4 && x < 27 && y > 4 && y < 27;
            let accent = x > 9 && x < 23 && y > 9 && y < 23;
            let (r, g, b, a) = if accent {
                (37u8, 99u8, 235u8, 255u8)
            } else if inside {
                (8u8, 145u8, 178u8, 255u8)
            } else {
                (0u8, 0u8, 0u8, 0u8)
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    tauri::image::Image::new_owned(rgba, width, height)
}
