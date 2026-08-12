use reqwest::blocking::Client;
use serde_json::json;
use std::error::Error;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
use crate::app::ai_provider_import::{
    cancel as cancel_ai_provider_import, consume_vscode_enrollment, import as import_ai_provider,
    status as ai_provider_import_status, AIProviderImportRequest,
};
use crate::app::http::{
    local_tree_json, query_param, set_response_origin, split_target, write_local_response,
};
use crate::app::security::LocalRequestSecurity;
use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    capture_browser_page_text, inspect_project_workspace, launch_project_workspace,
    launch_remote_connection, open_url,
};
use crate::app::types::{
    AgentEnrollmentRequest, BrowserTextCaptureRequest, EngineeringSyncRequest, LocalLoginRequest,
    ProjectWorkspaceRequest, RemoteConnectRequest,
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
    let mut local_principal = None;
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
        match verify_local_agent_ticket(
            &Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            &options.api_base,
            agent_id,
            ticket,
            operation,
            &options.agent_credential(),
        ) {
            Ok(principal) => local_principal = Some(principal),
            Err(error) => {
                return write_local_response(
                    &mut stream,
                    403,
                    &json!({ "error": format!("local Agent ticket rejected: {error}") })
                        .to_string(),
                    "application/json",
                );
            }
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
                    crate::api::oauth::cache_registration_access(&options, &state);
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
        ("POST", "/vscode/enrollment/exchange") => {
            #[derive(serde::Deserialize)]
            struct ExchangeRequest {
                code: String,
            }
            let payload: ExchangeRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "error": "VS Code 授权请求格式无效" }).to_string(),
                        "application/json",
                    )
                }
            };
            match consume_vscode_enrollment(payload.code.trim()) {
                Ok(value) => write_local_response(
                    &mut stream,
                    200,
                    &serde_json::to_string(&value)?,
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
            let upload_offset = crate::app::security::header_value(&request, "x-upload-offset")
                .and_then(|value| value.parse::<u64>().ok());
            let upload_size = crate::app::security::header_value(&request, "x-upload-size")
                .and_then(|value| value.parse::<u64>().ok());
            let final_chunk = crate::app::security::header_value(&request, "x-upload-final").unwrap_or_default() == "1";
            let (upload_offset, upload_size) = match (upload_offset, upload_size) {
                (Some(offset), Some(size)) => (offset, size),
                _ => return write_staging_error(&mut stream, StagingError::invalid_headers()),
            };
            let staging_root = std::env::temp_dir().join("himind-resource-staging");
            match stage_file_chunk(
                &staging_root,
                &upload_id,
                &file_name,
                upload_offset,
                upload_size,
                final_chunk,
                body_bytes,
            ) {
                Ok(staged) => write_local_response(
                    &mut stream,
                    200,
                    &json!({
                        "path": staged.path.to_string_lossy(),
                        "file_name": file_name,
                        "final": final_chunk,
                        "bytes_received": staged.bytes_received,
                    })
                    .to_string(),
                    "application/json",
                ),
                Err(error) => {
                    eprintln!(
                        "stage-file failed upload_id={} offset={} total_size={} code={} error={}",
                        upload_id, upload_offset, upload_size, error.code, error.message
                    );
                    write_staging_error(&mut stream, error)
                }
            }
        }
        ("POST", "/stream-smb-file") => {
            let upload_id = crate::app::security::header_value(&request, "x-upload-id").unwrap_or_default();
            let file_name = crate::app::http::percent_decode(
                crate::app::security::header_value(&request, "x-file-name").unwrap_or_default(),
            );
            let target_dir = crate::app::http::percent_decode(
                crate::app::security::header_value(&request, "x-target-dir").unwrap_or_default(),
            );
            let relative_path = crate::app::http::percent_decode(
                crate::app::security::header_value(&request, "x-relative-path").unwrap_or_default(),
            );
            let upload_offset = crate::app::security::header_value(&request, "x-upload-offset")
                .and_then(|value| value.parse::<u64>().ok());
            let upload_size = crate::app::security::header_value(&request, "x-upload-size")
                .and_then(|value| value.parse::<u64>().ok());
            let final_chunk = crate::app::security::header_value(&request, "x-upload-final").unwrap_or_default() == "1";
            let conflict_policy = crate::app::security::header_value(&request, "x-conflict-policy")
                .unwrap_or("skip");
            let (upload_offset, upload_size) = match (upload_offset, upload_size) {
                (Some(offset), Some(size)) => (offset, size),
                _ => return write_stream_smb_error(&mut stream, StreamSMBError::invalid_headers()),
            };
            if !is_unc_path(&target_dir) {
                return write_stream_smb_error(&mut stream, StreamSMBError::invalid_target());
            }
            match stream_smb_file_chunk(
                Path::new(&target_dir),
                &upload_id,
                &file_name,
                &relative_path,
                upload_offset,
                upload_size,
                final_chunk,
                &conflict_policy,
                body_bytes,
            ) {
                Ok(result) => write_local_response(
                    &mut stream,
                    200,
                    &json!({
                        "bytes_received": result.bytes_received,
                        "file_name": result.file_name,
                        "relative_path": result.relative_path,
                        "file_size": result.file_size,
                        "action": result.action,
                        "final": result.complete,
                    })
                    .to_string(),
                    "application/json",
                ),
                Err(error) => {
                    eprintln!(
                        "stream-smb-file failed upload_id={} relative_path={} offset={} total_size={} code={} error={}",
                        upload_id, relative_path, upload_offset, upload_size, error.code, error.message
                    );
                    write_stream_smb_error(&mut stream, error)
                }
            }
        }
        ("GET", "/tree") => {
            let tree_path = query_param(&query, "path").unwrap_or_default();
            let depth = local_tree_depth(&query);
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
        ("POST", "/ai-provider-import") => {
            let payload: AIProviderImportRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "ok": false, "error": format!("invalid AI provider import payload: {error}") }).to_string(),
                        "application/json",
                    )
                }
            };
            let expected_user_id = local_principal
                .as_ref()
                .map(|principal| principal.user_id.as_str())
                .unwrap_or_default();
            match import_ai_provider(&options, expected_user_id, &payload) {
                Ok(value) => write_local_response(
                    &mut stream,
                    200,
                    &serde_json::to_string(&value)?,
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "target": payload.target, "error": error.to_string() }).to_string(),
                    "application/json",
                ),
            }
        }
        ("GET", "/ai-provider-import/status") => write_local_response(
            &mut stream,
            200,
            &serde_json::to_string(&ai_provider_import_status(&options))?,
            "application/json",
        ),
        ("POST", "/ai-provider-import/cancel") => {
            let payload: AIProviderImportRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(error) => {
                    return write_local_response(
                        &mut stream,
                        400,
                        &json!({ "ok": false, "error": format!("invalid AI provider cancellation payload: {error}") }).to_string(),
                        "application/json",
                    )
                }
            };
            match cancel_ai_provider_import(&options, &payload.target) {
                Ok(value) => write_local_response(
                    &mut stream,
                    200,
                    &serde_json::to_string(&value)?,
                    "application/json",
                ),
                Err(error) => write_local_response(
                    &mut stream,
                    400,
                    &json!({ "ok": false, "target": payload.target, "error": error.to_string() }).to_string(),
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
        _ => write_local_response(
            &mut stream,
            404,
            &json!({ "error": "not found" }).to_string(),
            "application/json",
        ),
    }
}

#[derive(Debug)]
struct StagedFile {
    path: PathBuf,
    bytes_received: u64,
}

#[derive(Debug)]
struct StagingError {
    status: u16,
    code: &'static str,
    message: String,
    bytes_received: Option<u64>,
    expected_offset: Option<u64>,
    expected_size: Option<u64>,
}

impl StagingError {
    fn invalid_headers() -> Self {
        Self {
            status: 400,
            code: "invalid_staging_headers",
            message: "文件暂存参数不完整，请重新拖入文件".to_string(),
            bytes_received: None,
            expected_offset: None,
            expected_size: None,
        }
    }

    fn invalid_range(offset: u64, size: u64) -> Self {
        Self {
            status: 400,
            code: "invalid_staging_range",
            message: "文件分块范围无效，请重新拖入文件".to_string(),
            bytes_received: None,
            expected_offset: Some(offset),
            expected_size: Some(size),
        }
    }

    fn io(action: &str, error: std::io::Error, offset: u64, size: u64) -> Self {
        Self {
            status: 500,
            code: "staging_io_error",
            message: format!("{action}失败：{error}"),
            bytes_received: None,
            expected_offset: Some(offset),
            expected_size: Some(size),
        }
    }
}

fn write_staging_error(stream: &mut TcpStream, error: StagingError) -> Result<(), Box<dyn Error>> {
    write_local_response(
        stream,
        error.status,
        &json!({
            "error": error.message,
            "code": error.code,
            "bytes_received": error.bytes_received,
            "expected_offset": error.expected_offset,
            "expected_size": error.expected_size,
        })
        .to_string(),
        "application/json",
    )
}

fn stage_file_chunk(
    staging_root: &Path,
    upload_id: &str,
    file_name: &str,
    upload_offset: u64,
    upload_size: u64,
    final_chunk: bool,
    body: &[u8],
) -> Result<StagedFile, StagingError> {
    if upload_id.is_empty()
        || file_name.is_empty()
        || upload_id.contains(['\\', '/', ':', '\0'])
        || file_name.contains(['\\', '/', ':', '\0'])
        || matches!(upload_id, "." | "..")
        || matches!(file_name, "." | "..")
    {
        return Err(StagingError::invalid_headers());
    }
    let body_size = body.len() as u64;
    if upload_offset > upload_size || upload_offset.saturating_add(body_size) > upload_size {
        return Err(StagingError::invalid_range(upload_offset, upload_size));
    }

    let root = staging_root.join(upload_id);
    fs::create_dir_all(&root)
        .map_err(|error| StagingError::io("创建文件暂存目录", error, upload_offset, upload_size))?;
    let path = root.join(file_name);
    let current_size = match fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(StagingError::io(
                "读取暂存文件状态",
                error,
                upload_offset,
                upload_size,
            ))
        }
    };
    let expected_after = upload_offset.saturating_add(body_size);

    if current_size == upload_offset {
        let mut file = if upload_offset == 0 {
            fs::File::create(&path)
        } else {
            fs::OpenOptions::new().append(true).open(&path)
        }
        .map_err(|error| StagingError::io("打开暂存文件", error, upload_offset, upload_size))?;
        file.write_all(body)
            .and_then(|_| file.flush())
            .map_err(|error| StagingError::io("写入暂存文件", error, upload_offset, upload_size))?;
    } else if current_size < expected_after {
        return Err(StagingError {
            status: 409,
            code: "staging_offset_mismatch",
            message: format!("文件暂存进度不一致，Agent 已接收 {current_size} 字节"),
            bytes_received: Some(current_size),
            expected_offset: Some(upload_offset),
            expected_size: Some(upload_size),
        });
    }

    let bytes_received = fs::metadata(&path)
        .map(|metadata| metadata.len())
        .map_err(|error| StagingError::io("确认暂存文件状态", error, upload_offset, upload_size))?;
    if final_chunk && bytes_received != upload_size {
        return Err(StagingError {
            status: 409,
            code: "staged_file_incomplete",
            message: format!("文件暂存不完整，已接收 {bytes_received} / {upload_size} 字节"),
            bytes_received: Some(bytes_received),
            expected_offset: Some(upload_offset),
            expected_size: Some(upload_size),
        });
    }

    Ok(StagedFile {
        path,
        bytes_received,
    })
}

#[derive(Debug)]
struct StreamedSMBFile {
    bytes_received: u64,
    file_name: String,
    relative_path: String,
    file_size: u64,
    action: &'static str,
    complete: bool,
}

#[derive(Debug)]
struct StreamSMBError {
    status: u16,
    code: &'static str,
    message: String,
    bytes_received: Option<u64>,
    expected_offset: Option<u64>,
    expected_size: Option<u64>,
}

impl StreamSMBError {
    fn invalid_headers() -> Self {
        Self::new(
            400,
            "invalid_stream_headers",
            "直传参数不完整，请重新拖入文件",
        )
    }

    fn invalid_target() -> Self {
        Self::new(400, "invalid_smb_target", "直传目标必须是 SMB UNC 路径")
    }

    fn invalid_path() -> Self {
        Self::new(
            400,
            "invalid_relative_path",
            "附件相对路径无效或包含越级目录",
        )
    }

    fn invalid_range(offset: u64, size: u64) -> Self {
        let mut error = Self::new(
            400,
            "invalid_stream_range",
            "文件分块范围无效，请重新拖入文件",
        );
        error.expected_offset = Some(offset);
        error.expected_size = Some(size);
        error
    }

    fn io(action: &str, error: std::io::Error, offset: u64, size: u64) -> Self {
        let mut result = Self::new(
            500,
            "smb_stream_io_error",
            &format!("{action}失败：{error}"),
        );
        result.expected_offset = Some(offset);
        result.expected_size = Some(size);
        result
    }

    fn new(status: u16, code: &'static str, message: &str) -> Self {
        Self {
            status,
            code,
            message: message.to_string(),
            bytes_received: None,
            expected_offset: None,
            expected_size: None,
        }
    }
}

fn write_stream_smb_error(
    stream: &mut TcpStream,
    error: StreamSMBError,
) -> Result<(), Box<dyn Error>> {
    write_local_response(
        stream,
        error.status,
        &json!({
            "error": error.message,
            "code": error.code,
            "bytes_received": error.bytes_received,
            "expected_offset": error.expected_offset,
            "expected_size": error.expected_size,
        })
        .to_string(),
        "application/json",
    )
}

fn is_unc_path(value: &str) -> bool {
    let normalized = value.trim().replace('/', "\\");
    if !normalized.starts_with("\\\\") {
        return false;
    }
    let mut components = normalized.trim_start_matches('\\').split('\\');
    components.next().is_some_and(|part| !part.is_empty())
        && components.next().is_some_and(|part| !part.is_empty())
}

fn clean_stream_relative_path(value: &str) -> Result<(PathBuf, String), StreamSMBError> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains('\0')
        || normalized.contains(':')
    {
        return Err(StreamSMBError::invalid_path());
    }
    let mut path = PathBuf::new();
    let mut clean = Vec::new();
    for component in normalized.split('/') {
        if component.is_empty() || matches!(component, "." | "..") {
            return Err(StreamSMBError::invalid_path());
        }
        if component
            .chars()
            .any(|value| matches!(value, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(StreamSMBError::invalid_path());
        }
        path.push(component);
        clean.push(component);
    }
    Ok((path, clean.join("/")))
}

fn stream_smb_file_chunk(
    target_dir: &Path,
    upload_id: &str,
    file_name: &str,
    relative_path: &str,
    upload_offset: u64,
    upload_size: u64,
    final_chunk: bool,
    conflict_policy: &str,
    body: &[u8],
) -> Result<StreamedSMBFile, StreamSMBError> {
    if upload_id.is_empty()
        || upload_id.contains(['\\', '/', ':', '\0'])
        || matches!(upload_id, "." | "..")
        || !matches!(conflict_policy, "replace" | "skip")
    {
        return Err(StreamSMBError::invalid_headers());
    }
    let (relative, clean_relative) = clean_stream_relative_path(relative_path)?;
    let clean_file_name = relative
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if file_name.is_empty() || file_name != clean_file_name {
        return Err(StreamSMBError::invalid_path());
    }
    let body_size = body.len() as u64;
    if upload_offset > upload_size || upload_offset.saturating_add(body_size) > upload_size {
        return Err(StreamSMBError::invalid_range(upload_offset, upload_size));
    }

    let destination = target_dir.join(&relative);
    let parent = destination
        .parent()
        .ok_or_else(StreamSMBError::invalid_path)?;
    fs::create_dir_all(parent).map_err(|error| {
        StreamSMBError::io("创建 SMB 目标目录", error, upload_offset, upload_size)
    })?;
    let temporary = parent.join(format!(".{file_name}.{upload_id}.uploading"));
    let destination_existed = destination.exists();
    if conflict_policy == "skip" && destination_existed && !temporary.exists() {
        return Ok(StreamedSMBFile {
            bytes_received: upload_size,
            file_name: file_name.to_string(),
            relative_path: clean_relative,
            file_size: upload_size,
            action: "skipped",
            complete: true,
        });
    }
    if conflict_policy == "replace"
        && final_chunk
        && !temporary.exists()
        && upload_offset.saturating_add(body_size) == upload_size
        && destination
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == upload_size)
        && file_range_matches(&destination, upload_offset, body)
    {
        return Ok(StreamedSMBFile {
            bytes_received: upload_size,
            file_name: file_name.to_string(),
            relative_path: clean_relative,
            file_size: upload_size,
            action: "replaced",
            complete: true,
        });
    }

    let current_size = match fs::metadata(&temporary) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(StreamSMBError::io(
                "读取 SMB 临时文件状态",
                error,
                upload_offset,
                upload_size,
            ))
        }
    };
    let expected_after = upload_offset.saturating_add(body_size);
    if current_size == upload_offset {
        let mut file = if upload_offset == 0 {
            fs::File::create(&temporary)
        } else {
            fs::OpenOptions::new().append(true).open(&temporary)
        }
        .map_err(|error| {
            StreamSMBError::io("打开 SMB 临时文件", error, upload_offset, upload_size)
        })?;
        file.write_all(body)
            .and_then(|_| {
                if final_chunk {
                    file.sync_all()
                } else {
                    file.flush()
                }
            })
            .map_err(|error| {
                StreamSMBError::io("写入 SMB 临时文件", error, upload_offset, upload_size)
            })?;
    } else if current_size < expected_after {
        let mut error = StreamSMBError::new(
            409,
            "smb_stream_offset_mismatch",
            &format!("SMB 直传进度不一致，Agent 已接收 {current_size} 字节"),
        );
        error.bytes_received = Some(current_size);
        error.expected_offset = Some(upload_offset);
        error.expected_size = Some(upload_size);
        return Err(error);
    }

    let bytes_received = fs::metadata(&temporary)
        .map(|metadata| metadata.len())
        .map_err(|error| {
            StreamSMBError::io("确认 SMB 临时文件状态", error, upload_offset, upload_size)
        })?;
    if final_chunk && bytes_received != upload_size {
        let mut error = StreamSMBError::new(
            409,
            "smb_stream_incomplete",
            &format!("SMB 直传文件不完整，已接收 {bytes_received} / {upload_size} 字节"),
        );
        error.bytes_received = Some(bytes_received);
        error.expected_offset = Some(upload_offset);
        error.expected_size = Some(upload_size);
        return Err(error);
    }
    if final_chunk {
        commit_streamed_file(&temporary, &destination).map_err(|error| {
            StreamSMBError::io("提交 SMB 文件", error, upload_offset, upload_size)
        })?;
    }
    Ok(StreamedSMBFile {
        bytes_received,
        file_name: file_name.to_string(),
        relative_path: clean_relative,
        file_size: upload_size,
        action: if destination_existed {
            "replaced"
        } else {
            "new"
        },
        complete: final_chunk,
    })
}

fn file_range_matches(path: &Path, offset: u64, expected: &[u8]) -> bool {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return false;
    }
    let mut actual = vec![0_u8; expected.len()];
    file.read_exact(&mut actual).is_ok() && actual == expected
}

#[cfg(windows)]
fn commit_streamed_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn commit_streamed_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(source, destination)
}

fn local_operation(method: &str, path: &str) -> Option<&'static str> {
    match (method, path) {
        ("GET" | "POST", "/pick-folder") => Some("local.file.pick_folder"),
        ("GET" | "POST", "/pick-files") => Some("local.file.pick_files"),
        ("POST", "/stage-file") => Some("local.file.stage"),
        ("POST", "/stream-smb-file") => Some("local.file.stream_smb"),
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
        ("POST", "/ai-provider-import") => Some("local.ai.provider_import"),
        ("GET", "/ai-provider-import/status") => Some("local.ai.provider_import_status"),
        ("POST", "/ai-provider-import/cancel") => Some("local.ai.provider_import_cancel"),
        ("GET", "/login-status") => Some("local.inner_admin.login_status"),
        ("GET", "/engineering-projects") => Some("local.inner_admin.projects"),
        ("POST", "/engineering-exhibits") => Some("local.inner_admin.exhibits"),
        ("POST", "/extract-web-text") => Some("local.browser.extract_text"),
        ("GET" | "POST", "/open-login") => Some("local.inner_admin.open_login"),
        ("POST", "/login") => Some("local.inner_admin.login"),
        ("POST", "/logout") => Some("local.inner_admin.logout"),
        _ => None,
    }
}

fn local_tree_depth(query: &str) -> usize {
    query_param(query, "depth")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .min(8)
}

#[cfg(test)]
mod local_operation_tests {
    use super::{
        clean_stream_relative_path, is_unc_path, local_operation, local_tree_depth,
        stage_file_chunk, stream_smb_file_chunk,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn staging_test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "himind-stage-test-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

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
        assert_eq!(
            local_operation("POST", "/ai-provider-import"),
            Some("local.ai.provider_import")
        );
        assert_eq!(
            local_operation("GET", "/ai-provider-import/status"),
            Some("local.ai.provider_import_status")
        );
        assert_eq!(
            local_operation("POST", "/ai-provider-import/cancel"),
            Some("local.ai.provider_import_cancel")
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
        assert_eq!(local_operation("GET", "/ai-provider-import"), None);
    }

    #[test]
    fn supports_nested_resource_conflict_scans_without_unbounded_walks() {
        assert_eq!(local_tree_depth(""), 1);
        assert_eq!(local_tree_depth("depth=8"), 8);
        assert_eq!(local_tree_depth("depth=99"), 8);
    }

    #[test]
    fn stages_unicode_file_and_resumes_idempotently() {
        let root = staging_test_root("unicode");
        let name = "第二展厅：奋力探索 当家作主.mp4";
        let first =
            stage_file_chunk(&root, "upload-1", name, 0, 6, false, b"abc").expect("first chunk");
        assert_eq!(first.bytes_received, 3);

        let repeated =
            stage_file_chunk(&root, "upload-1", name, 0, 6, false, b"abc").expect("repeated chunk");
        assert_eq!(repeated.bytes_received, 3);

        let final_chunk =
            stage_file_chunk(&root, "upload-1", name, 3, 6, true, b"def").expect("final chunk");
        assert_eq!(final_chunk.bytes_received, 6);
        assert_eq!(fs::read(final_chunk.path).expect("staged file"), b"abcdef");
        fs::remove_dir_all(root).expect("cleanup staging test");
    }

    #[test]
    fn reports_offset_mismatch_and_incomplete_final_chunk() {
        let root = staging_test_root("ranges");
        stage_file_chunk(&root, "upload-2", "video.mp4", 0, 6, false, b"ab").expect("first chunk");
        let mismatch = stage_file_chunk(&root, "upload-2", "video.mp4", 3, 6, true, b"def")
            .expect_err("offset mismatch");
        assert_eq!(mismatch.code, "staging_offset_mismatch");
        assert_eq!(mismatch.bytes_received, Some(2));

        let incomplete = stage_file_chunk(&root, "upload-2", "video.mp4", 2, 6, true, b"c")
            .expect_err("incomplete final range");
        assert_eq!(incomplete.code, "staged_file_incomplete");
        fs::remove_dir_all(root).expect("cleanup staging test");
    }

    #[test]
    fn rejects_path_traversal_and_reports_file_system_failures() {
        let root = staging_test_root("invalid");
        let traversal = stage_file_chunk(&root, "upload-3", "../video.mp4", 0, 1, true, b"x")
            .expect_err("path traversal");
        assert_eq!(traversal.code, "invalid_staging_headers");

        fs::write(&root, b"not a directory").expect("blocking file");
        let io_error = stage_file_chunk(&root, "upload-3", "video.mp4", 0, 1, true, b"x")
            .expect_err("file system error");
        assert_eq!(io_error.status, 500);
        assert_eq!(io_error.code, "staging_io_error");
        assert!(io_error.message.contains("创建文件暂存目录失败"));
        fs::remove_file(root).expect("cleanup staging test");
    }

    #[test]
    fn validates_unc_targets_and_stream_relative_paths() {
        assert!(is_unc_path(r"\\fileserver\media"));
        assert!(is_unc_path("//fileserver/media/exhibits"));
        assert!(!is_unc_path(r"C:\media"));
        assert!(!is_unc_path(r"\\fileserver"));
        let (_, clean) = clean_stream_relative_path(r"后期\视频\成片.mp4").expect("valid path");
        assert_eq!(clean, "后期/视频/成片.mp4");
        for invalid in [
            r"..\secret.mp4",
            r"folder\..\secret.mp4",
            r"C:\secret.mp4",
            r"\secret.mp4",
        ] {
            assert!(
                clean_stream_relative_path(invalid).is_err(),
                "expected rejection: {invalid}"
            );
        }
    }

    #[test]
    fn streams_unicode_file_resumes_and_replays_chunks() {
        let root = staging_test_root("smb-stream-unicode");
        let relative = "后期/第二展厅：奋力探索 当家作主.mp4";
        let name = "第二展厅：奋力探索 当家作主.mp4";
        let first = stream_smb_file_chunk(
            &root, "direct-1", name, relative, 0, 6, false, "replace", b"abc",
        )
        .expect("first chunk");
        assert_eq!(first.bytes_received, 3);
        let replay = stream_smb_file_chunk(
            &root, "direct-1", name, relative, 0, 6, false, "replace", b"abc",
        )
        .expect("idempotent replay");
        assert_eq!(replay.bytes_received, 3);
        let completed = stream_smb_file_chunk(
            &root, "direct-1", name, relative, 3, 6, true, "replace", b"def",
        )
        .expect("final chunk");
        assert!(completed.complete);
        assert_eq!(completed.action, "new");
        assert_eq!(
            fs::read(root.join("后期").join(name)).expect("destination"),
            b"abcdef"
        );
        let final_replay = stream_smb_file_chunk(
            &root, "direct-1", name, relative, 3, 6, true, "replace", b"def",
        )
        .expect("lost final response replay");
        assert!(final_replay.complete);
        assert_eq!(final_replay.bytes_received, 6);
        fs::remove_dir_all(root).expect("cleanup stream test");
    }

    #[test]
    fn streams_replace_and_skip_conflicts() {
        let root = staging_test_root("smb-stream-conflicts");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("existing.txt"), b"old").expect("existing file");
        let skipped = stream_smb_file_chunk(
            &root,
            "direct-skip",
            "existing.txt",
            "existing.txt",
            0,
            3,
            true,
            "skip",
            b"new",
        )
        .expect("skip conflict");
        assert_eq!(skipped.action, "skipped");
        assert_eq!(
            fs::read(root.join("existing.txt")).expect("skipped file"),
            b"old"
        );
        let replaced = stream_smb_file_chunk(
            &root,
            "direct-replace",
            "existing.txt",
            "existing.txt",
            0,
            3,
            true,
            "replace",
            b"new",
        )
        .expect("replace conflict");
        assert_eq!(replaced.action, "replaced");
        assert_eq!(
            fs::read(root.join("existing.txt")).expect("replaced file"),
            b"new"
        );
        fs::remove_dir_all(root).expect("cleanup conflict test");
    }

    #[test]
    fn reports_stream_offset_incomplete_and_io_failures() {
        let root = staging_test_root("smb-stream-errors");
        stream_smb_file_chunk(
            &root,
            "direct-errors",
            "video.mp4",
            "video.mp4",
            0,
            6,
            false,
            "replace",
            b"ab",
        )
        .expect("first chunk");
        let mismatch = stream_smb_file_chunk(
            &root,
            "direct-errors",
            "video.mp4",
            "video.mp4",
            3,
            6,
            false,
            "replace",
            b"def",
        )
        .expect_err("offset mismatch");
        assert_eq!(mismatch.code, "smb_stream_offset_mismatch");
        let incomplete = stream_smb_file_chunk(
            &root,
            "direct-errors",
            "video.mp4",
            "video.mp4",
            2,
            6,
            true,
            "replace",
            b"c",
        )
        .expect_err("incomplete file");
        assert_eq!(incomplete.code, "smb_stream_incomplete");
        fs::remove_dir_all(&root).expect("cleanup range test");

        fs::write(&root, b"not a directory").expect("blocking file");
        let io_error = stream_smb_file_chunk(
            &root,
            "direct-io",
            "video.mp4",
            "video.mp4",
            0,
            1,
            true,
            "replace",
            b"x",
        )
        .expect_err("io failure");
        assert_eq!(io_error.code, "smb_stream_io_error");
        fs::remove_file(root).expect("cleanup io test");
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
