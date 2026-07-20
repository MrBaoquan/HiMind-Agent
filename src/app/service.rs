use reqwest::blocking::Client;
use serde_json::json;
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[cfg(windows)]
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
};
#[cfg(windows)]
use std::num::NonZeroIsize;

use crate::api::client::verify_local_agent_ticket;
use crate::app::http::{
    local_tree_json, query_param, set_response_origin, split_target, write_local_response,
};
use crate::app::security::LocalRequestSecurity;
use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    capture_browser_page_text, inspect_project_workspace, launch_project_workspace,
    launch_remote_connection, open_url, trigger_local_agent_update,
};
use crate::app::types::{
    AgentEnrollmentRequest, BrowserTextCaptureRequest, EngineeringSyncRequest,
    LocalAgentUpdateRequest, LocalLoginRequest, ProjectWorkspaceRequest, RemoteConnectRequest,
};
use crate::capability::service::CapabilityGateway;
use crate::capability::types::{CapabilityInvokeRequest, InvocationContext};
use crate::remote::client::inner_admin_base;
use crate::remote::sync::{fetch_engineering_projects, fetch_selected_engineering_exhibits};
use crate::store::credentials::{
    clear_local_inner_admin_credentials, local_login_status_json,
    save_local_inner_admin_credentials,
};
use crate::store::types::LocalWorkerStatus;
use crate::{worker, Options};

use crate::approval::manager::ApprovalManager;

#[cfg(windows)]
struct ForegroundDialogParent {
    hwnd: NonZeroIsize,
}

#[cfg(windows)]
impl HasWindowHandle for ForegroundDialogParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = Win32WindowHandle::new(self.hwnd);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

#[cfg(windows)]
impl HasDisplayHandle for ForegroundDialogParent {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()))
        })
    }
}

#[cfg(windows)]
fn foreground_dialog_parent() -> Option<ForegroundDialogParent> {
    extern "system" {
        fn GetForegroundWindow() -> isize;
    }
    NonZeroIsize::new(unsafe { GetForegroundWindow() }).map(|hwnd| ForegroundDialogParent { hwnd })
}

// Tray is now managed by Tauri (src/app/ui.rs). This legacy entry point is retained
// for headless testing but no longer starts a GUI event loop.
pub(crate) fn run_local_app(options: Options) -> Result<(), Box<dyn Error>> {
    let port = options.local_port;
    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: "正在连接 Dashboard 任务 Worker".to_string(),
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

    start_background_services(&options, Arc::clone(&worker_status), None)?;

    println!(
        "local agent app service listening on http://127.0.0.1:{}",
        port
    );
    // Block forever (headless fallback). Use Tauri entry point (--local-app) for GUI.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

pub(crate) fn start_background_services(
    options: &Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    approval_mgr: Option<Arc<ApprovalManager>>,
) -> Result<(), Box<dyn Error>> {
    let listener = match TcpListener::bind(("127.0.0.1", options.local_port)) {
        Ok(listener) => listener,
        Err(error) => {
            if let Ok(mut state) = worker_status.lock() {
                state.local_service_online = false;
                state.local_service_error = format!("本地服务监听失败：{error}");
            }
            return Err(error.into());
        }
    };
    if let Ok(mut state) = worker_status.lock() {
        state.local_service_online = true;
        state.local_service_error.clear();
    }

    let worker_opts = options.clone();
    let ws = Arc::clone(&worker_status);
    let mgr = approval_mgr.clone();
    thread::spawn(move || {
        worker::run_supervisor(worker_opts, ws, mgr);
    });

    let http_ws = Arc::clone(&worker_status);
    let http_opts = options.clone();
    thread::spawn(move || {
        if let Err(error) = run_local_http_service(listener, http_ws, http_opts) {
            eprintln!("local agent service failed: {error}");
        }
    });

    Ok(())
}

fn run_local_http_service(
    listener: TcpListener,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    options: Options,
) -> Result<(), Box<dyn Error>> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let worker_status = Arc::clone(&worker_status);
                let options = options.clone();
                thread::spawn(|| {
                    if let Err(error) = handle_local_http(stream, worker_status, options) {
                        eprintln!("local agent request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("local agent accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle_local_http(
    mut stream: TcpStream,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    options: Options,
) -> Result<(), Box<dyn Error>> {
    let request_bytes = read_http_request(&mut stream)?;
    let request = String::from_utf8_lossy(&request_bytes);
    let security = LocalRequestSecurity::new(&options.api_base, options.local_port);
    let response_origin = match security.validate(&request) {
        Ok(origin) => origin,
        Err(message) => {
            set_response_origin(None);
            return write_local_response(
                &mut stream,
                403,
                &json!({ "error": message }).to_string(),
                "application/json",
            );
        }
    };
    set_response_origin(response_origin);
    let first_line = request.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or("/");
    if method == "OPTIONS" {
        return write_local_response(&mut stream, 204, "", "text/plain");
    }
    let (path, query) = split_target(target);
    let body = request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let body_bytes = request
        .find("\r\n\r\n")
        .map(|index| &request_bytes[index + 4..])
        .unwrap_or(&[]);
    if method == "POST" && path == "/enroll" && response_origin.is_none() {
        return write_local_response(
            &mut stream,
            403,
            &json!({ "error": "browser origin is required" }).to_string(),
            "application/json",
        );
    }
    if let Some(operation) = local_operation(method, &path) {
        let ticket = crate::app::security::header_value(&request, "x-himind-local-ticket")
            .unwrap_or_default();
        if ticket.is_empty() {
            return write_local_response(
                &mut stream,
                401,
                &json!({ "error": "local Agent ticket is required" }).to_string(),
                "application/json",
            );
        }
        let snapshot = local_worker_snapshot(&worker_status);
        let agent_id = snapshot
            .get("dashboard_agent_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if agent_id.is_empty() || options.agent_credential().is_empty() {
            return write_local_response(
                &mut stream,
                503,
                &json!({ "error": "Dashboard Agent is not authenticated" }).to_string(),
                "application/json",
            );
        }
        if let Err(error) = verify_local_agent_ticket(
            &Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            &options.api_base,
            agent_id,
            ticket,
            operation,
            &options.agent_credential(),
        ) {
            return write_local_response(
                &mut stream,
                403,
                &json!({ "error": format!("local Agent ticket rejected: {error}") }).to_string(),
                "application/json",
            );
        }
    }
    let gateway = CapabilityGateway::new(options.clone(), Arc::clone(&worker_status));
    match (method, path.as_str()) {
        ("POST", "/enroll") => {
            let payload: AgentEnrollmentRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid enrollment payload: {error}") }).to_string(),
                        "application/json",
                    )
                }
            };
            if payload.enrollment_token.trim().is_empty() {
                return write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": "enrollment token is required" }).to_string(),
                    "application/json",
                );
            }
            match crate::api::client::register_agent(
                &Client::builder()
                    .timeout(std::time::Duration::from_secs(20))
                    .build()?,
                &options.api_base,
                &options.state_path,
                crate::VERSION,
                &payload.enrollment_token,
            ) {
                Ok(state) => {
                    options.set_agent_credential(&state.credential);
                    if let Ok(mut status) = worker_status.lock() {
                        status.dashboard_agent_id = state.agent_id.clone();
                        status.dashboard_worker_error = "正在完成连接".to_string();
                    }
                    write_local_response(
                        &mut stream,
                        200,
                        &json!({ "ok": true, "status": "registered", "agent_id": state.agent_id }).to_string(),
                        "application/json",
                    )
                }
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "status": "failed", "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("GET", "/health") => {
            write_local_response(
                &mut stream,
                200,
                &gateway.health(&InvocationContext::local_http()).to_string(),
                "application/json",
            )
        }
        ("GET", "/capabilities") => {
            match gateway.list_capabilities(&InvocationContext::local_http()) {
            Ok(items) => write_local_response(
                &mut stream,
                200,
                &json!({ "items": items }).to_string(),
                "application/json",
            ),
            Err(error) => write_local_response(
                &mut stream,
                400,
                &json!({ "error": error.to_string() }).to_string(),
                "application/json",
            ),
            }
        }
        ("POST", "/capabilities/invoke") => {
            let payload: CapabilityInvokeRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid capability payload: {error}") })
                            .to_string(),
                        "application/json",
                    )
                }
            };
            if payload.ticket.trim().is_empty() {
                return write_local_response(
                    &mut stream,
                    401,
                    &json!({ "error": "local Agent ticket is required" }).to_string(),
                    "application/json",
                );
            }
            let snapshot = local_worker_snapshot(&worker_status);
            let agent_id = snapshot
                .get("dashboard_agent_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if agent_id.is_empty() || options.agent_credential().is_empty() {
                return write_local_response(
                    &mut stream,
                    503,
                    &json!({ "error": "Dashboard Agent is not authenticated" }).to_string(),
                    "application/json",
                );
            }
            let principal = match verify_local_agent_ticket(
                &Client::builder().timeout(std::time::Duration::from_secs(10)).build()?,
                &options.api_base,
                agent_id,
                &payload.ticket,
                &payload.capability_id,
                &options.agent_credential(),
            ) {
                Ok(principal) => principal,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        403,
                        &json!({ "error": format!("local Agent ticket rejected: {error}") }).to_string(),
                        "application/json",
                    )
                }
            };
            match gateway.invoke(
                &InvocationContext::dashboard_user(&principal.user_id, &principal.session_id_hash),
                &payload.capability_id,
                payload.input,
            ) {
                Ok(value) => write_local_response(
                    &mut stream,
                    200,
                    &json!({ "ok": true, "capability_id": payload.capability_id, "result": value })
                        .to_string(),
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "capability_id": payload.capability_id, "error": error.to_string() })
                        .to_string(),
                    "application/json",
                ),
            }
        }
        ("GET", "/pick-folder") | ("POST", "/pick-folder") => {
            let title = query_param(&query, "title").unwrap_or_else(|| "选择文件夹".to_string());
            let multi = query_param(&query, "multi")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            #[cfg(windows)]
            let dialog_parent = foreground_dialog_parent();
            let paths: Vec<String> = if multi {
                let dialog = rfd::FileDialog::new().set_title(&title);
                let dialog = if let Some(parent) = dialog_parent.as_ref() {
                    dialog.set_parent(parent)
                } else {
                    dialog
                };
                dialog
                    .pick_folders()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| item.to_string_lossy().to_string())
                    .collect()
            } else {
                let dialog = rfd::FileDialog::new().set_title(&title);
                let dialog = if let Some(parent) = dialog_parent.as_ref() {
                    dialog.set_parent(parent)
                } else {
                    dialog
                };
                dialog
                    .pick_folder()
                    .map(|item| vec![item.to_string_lossy().to_string()])
                    .unwrap_or_default()
            };
            let path = paths.first().cloned();
            write_local_response(
                &mut stream,
                200,
                &json!({ "path": path, "paths": paths }).to_string(),
                "application/json",
            )
        }
        ("GET", "/pick-files") | ("POST", "/pick-files") => {
            let title = query_param(&query, "title").unwrap_or_else(|| "选择资源文件".to_string());
            #[cfg(windows)]
            let dialog_parent = foreground_dialog_parent();
            let dialog = rfd::FileDialog::new().set_title(&title);
            let dialog = if let Some(parent) = dialog_parent.as_ref() {
                dialog.set_parent(parent)
            } else {
                dialog
            };
            let paths = dialog
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|item| item.to_string_lossy().to_string())
                .collect::<Vec<_>>();
            write_local_response(
                &mut stream,
                200,
                &json!({ "paths": paths }).to_string(),
                "application/json",
            )
        }
        ("POST", "/stage-file") => {
            let upload_id = crate::app::security::header_value(&request, "x-upload-id").unwrap_or_default();
            let encoded_file_name = crate::app::security::header_value(&request, "x-file-name").unwrap_or_default();
            let file_name = crate::app::http::percent_decode(encoded_file_name);
            let final_chunk = crate::app::security::header_value(&request, "x-upload-final").unwrap_or_default() == "1";
            if upload_id.is_empty() || file_name.is_empty() || upload_id.contains(['\\', '/', ':']) || file_name.contains(['\\', '/', ':']) {
                return write_local_response(&mut stream, 400, &json!({"error":"invalid staging headers"}).to_string(), "application/json");
            }
            let root = std::env::temp_dir().join("himind-resource-staging").join(upload_id);
            fs::create_dir_all(&root)?;
            let path = root.join(&file_name);
            let mut file = if path.exists() { fs::OpenOptions::new().append(true).open(&path)? } else { fs::File::create(&path)? };
            file.write_all(body_bytes)?;
            write_local_response(&mut stream, 200, &json!({"path":path.to_string_lossy(),"file_name":file_name,"final":final_chunk}).to_string(), "application/json")
        }
        ("GET", "/tree") => {
            let tree_path = query_param(&query, "path").unwrap_or_default();
            let depth = query_param(&query, "depth")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1)
                .min(2);
            let body = local_tree_json(&tree_path, depth)?;
            write_local_response(&mut stream, 200, &body.to_string(), "application/json")
        }
        ("GET", "/plugins") => match gateway.invoke(
            &InvocationContext::local_http(),
            "plugin.list",
            json!({}),
        ) {
            Ok(value) => {
                write_local_response(&mut stream, 200, &value.to_string(), "application/json")
            }
            Err(error) => write_local_response(
                &mut stream,
                400,
                &json!({ "error": error.to_string() }).to_string(),
                "application/json",
            ),
        },
        ("GET", "/plugins/manifest") => {
            let plugin_id = query_param(&query, "plugin_id").unwrap_or_default();
            match gateway.invoke(
                &InvocationContext::local_http(),
                "plugin.manifest",
                json!({ "plugin_id": plugin_id }),
            ) {
                Ok(value) => {
                    write_local_response(&mut stream, 200, &value.to_string(), "application/json")
                }
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        },
        ("POST", "/plugins/install")
        | ("POST", "/plugins/update")
        | ("POST", "/plugins/uninstall")
        | ("POST", "/plugins/enable")
        | ("POST", "/plugins/disable") => write_local_response(
            &mut stream,
            400,
            &json!({
                "ok": false,
                "status": "not_implemented",
                "message": "插件安装、升级、卸载和启停需要等待 Distribution 策略与制品校验接入后开放。"
            })
            .to_string(),
            "application/json",
        ),
        ("GET", "/open-folder") | ("POST", "/open-folder") => {
            let folder_path = query_param(&query, "path").unwrap_or_default();
            match gateway.invoke(
                &InvocationContext::local_http(),
                "system.open_folder",
                json!({ "path": folder_path }),
            ) {
                Ok(value) => {
                    write_local_response(&mut stream, 200, &value.to_string(), "application/json")
                }
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("POST", "/workspace-status") => {
            let payload: ProjectWorkspaceRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => return write_local_response(&mut stream, 400, &json!({ "error": format!("invalid workspace payload: {error}") }).to_string(), "application/json"),
            };
            match inspect_project_workspace(&payload.path, payload.engine_type.as_deref(), payload.engine_version.as_deref()) {
                Ok(value) => write_local_response(&mut stream, 200, &value.to_string(), "application/json"),
                Err(error) => write_local_response(&mut stream, 400, &json!({ "error": error.to_string() }).to_string(), "application/json"),
            }
        }
        ("POST", "/open-project") => {
            let payload: ProjectWorkspaceRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => return write_local_response(&mut stream, 400, &json!({ "error": format!("invalid workspace payload: {error}") }).to_string(), "application/json"),
            };
            match launch_project_workspace(&payload.path, payload.engine_type.as_deref(), payload.engine_version.as_deref()) {
                Ok(value) => write_local_response(&mut stream, 200, &value.to_string(), "application/json"),
                Err(error) => write_local_response(&mut stream, 400, &json!({ "error": error.to_string() }).to_string(), "application/json"),
            }
        }
        ("POST", "/remote-connect") => {
            let payload: RemoteConnectRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid remote connect payload: {error}") })
                            .to_string(),
                        "application/json",
                    )
                }
            };
            match launch_remote_connection(&payload) {
                Ok(value) => {
                    write_local_response(&mut stream, 200, &value.to_string(), "application/json")
                }
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("GET", "/login-status") => write_local_response(
            &mut stream,
            200,
            &local_login_status_json().to_string(),
            "application/json",
        ),
        ("GET", "/engineering-projects") => match fetch_engineering_projects() {
            Ok(items) => write_local_response(
                &mut stream,
                200,
                &json!({ "items": items, "total": items.len() }).to_string(),
                "application/json",
            ),
            Err(error) => write_local_response(
                &mut stream,
                400,
                &json!({ "error": error.to_string() }).to_string(),
                "application/json",
            ),
        },
        ("POST", "/engineering-exhibits") => {
            let payload: EngineeringSyncRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid engineering sync payload: {error}") })
                            .to_string(),
                        "application/json",
                    )
                }
            };
            match fetch_selected_engineering_exhibits(&payload.project_ids) {
                Ok(items) => write_local_response(
                    &mut stream,
                    200,
                    &json!({ "items": items, "total": items.len() }).to_string(),
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("POST", "/extract-web-text") => {
            let payload: BrowserTextCaptureRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid capture payload: {error}") })
                            .to_string(),
                        "application/json",
                    )
                }
            };
            match capture_browser_page_text(&payload.source_url) {
                Ok(value) => write_local_response(
                    &mut stream,
                    200,
                    &value.to_string(),
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "error": error.to_string(), "source_url": payload.source_url }).to_string(),
                    "application/json",
                ),
            }
        }
        ("GET", "/open-login") | ("POST", "/open-login") => {
            open_url(&format!(
                "{}/admin/personal/software_code",
                inner_admin_base()
            ))?;
            write_local_response(
                &mut stream,
                200,
                &local_login_status_json().to_string(),
                "application/json",
            )
        }
        ("POST", "/login") => {
            let payload: LocalLoginRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": format!("invalid login payload: {error}") }).to_string(),
                        "application/json",
                    )
                }
            };
            match save_local_inner_admin_credentials(&payload.username, &payload.password) {
                Ok(()) => write_local_response(
                    &mut stream,
                    200,
                    &local_login_status_json().to_string(),
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("POST", "/logout") => match clear_local_inner_admin_credentials() {
            Ok(()) => write_local_response(
                &mut stream,
                200,
                &local_login_status_json().to_string(),
                "application/json",
            ),
            Err(error) => write_local_response(
                &mut stream,
                400,
                &json!({ "error": error.to_string() }).to_string(),
                "application/json",
            ),
        },
        ("POST", "/update-agent") => {
            let payload = if body.trim().is_empty() {
                LocalAgentUpdateRequest::default()
            } else {
                match serde_json::from_str::<LocalAgentUpdateRequest>(body) {
                    Ok(value) => value,
                    Err(error) => {
                        return write_local_response(
                            &mut stream,
                            400,
                            &json!({ "ok": false, "status": "failed", "message": format!("invalid update payload: {error}") }).to_string(),
                            "application/json",
                        )
                    }
                }
            };
            match trigger_local_agent_update(&options, &payload) {
                Ok(message) => write_local_response(
                    &mut stream,
                    200,
                    &json!({ "ok": true, "status": "restarting", "message": message }).to_string(),
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "status": "failed", "message": error.to_string() })
                        .to_string(),
                    "application/json",
                ),
            }
        }
        _ => write_local_response(
            &mut stream,
            404,
            &json!({ "error": "not found" }).to_string(),
            "application/json",
        ),
    }
}

fn local_operation(method: &str, path: &str) -> Option<&'static str> {
    match (method, path) {
        ("GET" | "POST", "/pick-folder") => Some("local.file.pick_folder"),
        ("GET" | "POST", "/pick-files") => Some("local.file.pick_files"),
        ("POST", "/stage-file") => Some("local.file.stage"),
        ("GET", "/tree") => Some("local.file.tree"),
        ("GET", "/plugins") => Some("local.plugin.list"),
        ("GET", "/plugins/manifest") => Some("local.plugin.manifest"),
        (
            "POST",
            "/plugins/install" | "/plugins/update" | "/plugins/uninstall" | "/plugins/enable"
            | "/plugins/disable",
        ) => Some("local.plugin.manage"),
        ("GET" | "POST", "/open-folder") => Some("local.file.open_folder"),
        ("POST", "/workspace-status") => Some("local.workspace.inspect"),
        ("POST", "/open-project") => Some("local.workspace.open"),
        ("POST", "/remote-connect") => Some("local.remote.connect"),
        ("GET", "/login-status") => Some("local.inner_admin.login_status"),
        ("GET", "/engineering-projects") => Some("local.inner_admin.projects"),
        ("POST", "/engineering-exhibits") => Some("local.inner_admin.exhibits"),
        ("POST", "/extract-web-text") => Some("local.browser.extract_text"),
        ("GET" | "POST", "/open-login") => Some("local.inner_admin.open_login"),
        ("POST", "/login") => Some("local.inner_admin.login"),
        ("POST", "/logout") => Some("local.inner_admin.logout"),
        ("POST", "/update-agent") => Some("local.agent.update"),
        _ => None,
    }
}

#[cfg(test)]
mod local_operation_tests {
    use super::local_operation;

    #[test]
    fn maps_sensitive_local_routes_to_stable_operations() {
        assert_eq!(
            local_operation("POST", "/remote-connect"),
            Some("local.remote.connect")
        );
        assert_eq!(
            local_operation("POST", "/plugins/update"),
            Some("local.plugin.manage")
        );
        assert_eq!(
            local_operation("GET", "/login-status"),
            Some("local.inner_admin.login_status")
        );
    }

    #[test]
    fn leaves_discovery_and_preflight_routes_public() {
        assert_eq!(local_operation("GET", "/health"), None);
        assert_eq!(local_operation("GET", "/capabilities"), None);
        assert_eq!(local_operation("OPTIONS", "/remote-connect"), None);
    }

    #[test]
    fn does_not_authorize_wrong_http_methods() {
        assert_eq!(local_operation("GET", "/remote-connect"), None);
        assert_eq!(local_operation("POST", "/login-status"), None);
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut request = Vec::with_capacity(8192);
    let mut buffer = [0_u8; 4096];
    let mut expected_size = None;
    loop {
        let size = stream.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..size]);
        if expected_size.is_none() {
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end + 4]);
                let content_length = crate::app::security::header_value(&headers, "content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                expected_size = Some(header_end + 4 + content_length);
            }
        }
        if expected_size.is_some_and(|value| request.len() >= value) {
            break;
        }
        if request.len() > 1_048_576 {
            return Err("local request is too large".into());
        }
    }
    Ok(request)
}
