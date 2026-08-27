#![cfg_attr(
    all(not(debug_assertions), not(feature = "mcp-console")),
    windows_subsystem = "windows"
)]

use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

mod api;
mod app;
mod approval;
mod capability;
mod extension_projects;
mod extension_workspace;
mod install_layout;
mod mcp;
mod plugin_authoring;
mod remote;
mod runtime;
mod scan;
mod skill;
mod store;
mod svn;
mod upload;
mod worker;

use api::client::{is_task_canceled_error, TaskCancelGuard};
use api::types::Task;
use approval::manager::ApprovalManager;
use approval::types::RequestType;
use remote::sync::execute_sync_exhibits;
use scan::service::execute_scan;
use store::outbox::{
    list_reports, remove_report, remove_reports_for_execution, store_report, TaskReportRecord,
};
use svn::service::{
    apply_project_acl, clone_exhibit_repository, create_exhibit_repository_path,
    create_repository_with_post_commit_hook, ensure_project_exhibits_access,
    import_local_exhibit_with_cancel_and_progress, initialize_exhibit_repository_with_cancel,
    preview_project_acl, reconcile_project_acl, task_failure_result, SvnDiagnosticContextGuard,
};
use svn::types::{
    ApplyProjectAclRequest, CloneExhibitRepositoryRequest, CreateExhibitRepositoryPathRequest,
    CreateRepositoryRequest, EnsureProjectExhibitsAccessRequest, ImportLocalExhibitRequest,
    InitializeExhibitRepositoryRequest, PreviewProjectAclRequest, ReconcileProjectAclRequest,
};
use upload::smb::execute_smb_upload;
use upload::tasks::{execute_upload_code, execute_upload_placeholder};

// Keep the runtime health version aligned with the version stamped into the
// updater package. Cargo is the source of truth for both binaries.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginViewLaunch {
    pub plugin_id: String,
    pub view_id: String,
}

const AGENT_PROTOCOL_SCHEME: &str = "himind-agent";

fn protocol_open_requested(args: &[String]) -> bool {
    let Some(index) = args.iter().position(|value| value == "--protocol-url") else {
        return false;
    };
    let Some(value) = args.get(index + 1) else {
        return false;
    };
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == AGENT_PROTOCOL_SCHEME
        && url.host_str() == Some("open")
        && (url.path().is_empty() || url.path() == "/")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn main() {
    let options = Options::from_env();
    let mcp_mode = cfg!(feature = "mcp-console") || env::args().any(|argument| argument == "--mcp");
    if !mcp_mode {
        let svn_credentials_from_environment = match svn::service::bootstrap_svn_credentials() {
            Ok(configured) => configured,
            Err(error) => {
                eprintln!("SVN credential initialization failed: {error}");
                std::process::exit(1);
            }
        };
        if let Err(error) = svn::service::bootstrap_svn_admin_credentials() {
            eprintln!("SVN administrator credential initialization failed: {error}");
            std::process::exit(1);
        }
        if !svn_credentials_from_environment {
            if let Ok(Some(snapshot)) = api::oauth::authorization_snapshot(&options.state_path) {
                if !snapshot.display_name.trim().is_empty() {
                    if let Err(error) =
                        svn::service::ensure_default_svn_credentials(&snapshot.display_name)
                    {
                        eprintln!("SVN user credential initialization failed: {error}");
                    }
                }
            }
        }
    }
    if let Some(arguments) = auth_cli_arguments() {
        if let Err(error) = run_auth_cli(&options, &arguments) {
            eprintln!("auth command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = skill_cli_arguments() {
        if let Err(error) = run_skill_cli(&options, &arguments) {
            eprintln!("skill command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = plugin_cli_arguments() {
        if let Err(error) = run_plugin_cli(&options, &arguments) {
            eprintln!("plugin command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = runtime_cli_arguments() {
        if let Err(error) = run_runtime_cli(&options, &arguments) {
            eprintln!("runtime command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = mcp_cli_arguments() {
        if let Err(error) = run_mcp_cli(&options, &arguments) {
            eprintln!("mcp command failed: {error}");
            std::process::exit(1);
        }
    } else if mcp_mode {
        if let Err(error) = mcp::run(options) {
            eprintln!("agent mcp failed: {error}");
            std::process::exit(1);
        }
    } else if options.local_app {
        if let Err(error) = app::ui::run_tauri_app(options) {
            eprintln!("agent ui failed: {error}");
            std::process::exit(1);
        }
    } else if let Err(error) = worker::run_loop(options, None, None) {
        eprintln!("agent failed: {error}");
        std::process::exit(1);
    }
}

fn auth_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "auth")?;
    Some(arguments[index + 1..].to_vec())
}

fn run_auth_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("login") => {
            let authorization = api::oauth::begin_device_authorization(options)?;
            println!("Open {}", authorization.verification_uri_complete);
            println!("Verification page: {}", authorization.verification_uri);
            println!("Authorization code: {}", authorization.user_code);
            let _ = app::system::open_url(&authorization.verification_uri_complete);
            let access = api::oauth::wait_for_device_authorization(options, &authorization)?;
            if let Ok(info) = api::oauth::fetch_user_info(options) {
                let svn_username = if info.svn_username.trim().is_empty() {
                    svn::service::default_svn_username(&info.name)?
                } else {
                    info.svn_username
                };
                if info.svn_provisioning_status == "ready" {
                    svn::service::ensure_default_svn_credentials(&svn_username)?;
                }
            }
            println!(
                "Agent {} is authorized as Dashboard user {} with scopes: {}",
                access.agent_id, access.user_id, access.scope
            );
        }
        Some("status") => {
            let access = api::oauth::platform_access_token(options, api::oauth::PROFILE_SCOPE)?;
            println!(
                "Agent {} represents Dashboard user {} with scopes: {}",
                access.agent_id, access.user_id, access.scope
            );
        }
        Some("logout") => {
            api::oauth::revoke_authorization(options)?;
            println!("Delegated Dashboard authorization revoked");
        }
        Some("logout-local") => {
            api::oauth::clear_authorization(&options.state_path)?;
            if let Ok(mut cache) = options.platform_access.write() {
                *cache = None;
            }
            println!("Local delegated authorization removed without server revocation");
        }
        Some("rotate-device") => {
            let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
            let state = api::client::load_or_register(
                &client,
                &options.api_base,
                &options.state_path,
                VERSION,
                &options.enrollment_token,
            )?;
            let rotated = api::client::rotate_agent_credential(
                &client,
                &options.api_base,
                &options.state_path,
                &state,
            )?;
            options.set_agent_credential(&rotated.credential);
            println!("Agent {} device credential rotated", rotated.agent_id);
        }
        _ => {
            return Err(
                "usage: himind-agent auth <login|status|logout|logout-local|rotate-device>".into(),
            )
        }
    }
    Ok(())
}

fn plugin_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "plugin")?;
    Some(arguments[index + 1..].to_vec())
}

fn skill_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "skill")?;
    Some(arguments[index + 1..].to_vec())
}

fn runtime_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "runtime")?;
    Some(arguments[index + 1..].to_vec())
}

fn mcp_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "mcp")?;
    Some(arguments[index + 1..].to_vec())
}

fn run_mcp_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("list") => {
            let payload = json!({
                "servers": app::mcp_registry::public_snapshot(&options.state_path)?,
                "targets": app::mcp_targets::list(options)?,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Some("targets") if arguments.len() == 1 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_targets::list(options)?)?
            );
        }
        Some("inspect") if arguments.len() == 2 => {
            let id = &arguments[1];
            match app::mcp_registry::inspect(&options.state_path, id) {
                Ok(server) => println!("{}", serde_json::to_string_pretty(&server)?),
                Err(_) => println!(
                    "{}",
                    serde_json::to_string_pretty(&app::mcp_targets::inspect(options, id)?)?
                ),
            }
        }
        Some("plan") if arguments.len() == 2 => println!(
            "{}",
            serde_json::to_string_pretty(&app::mcp_targets::plan(options, &arguments[1])?)?
        ),
        Some("apply") if arguments.len() == 2 || arguments.len() == 3 => {
            let reset_invalid = arguments.get(2).is_some_and(|value| value == "--reset-invalid");
            if arguments.len() == 3 && !reset_invalid {
                return Err("usage: himind-agent mcp apply <target-id> [--reset-invalid]".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_targets::apply(
                    options,
                    &arguments[1],
                    reset_invalid,
                )?)?
            );
        }
        Some("apply-all") => {
            let detected_only = !arguments.iter().any(|value| value == "--include-undetected");
            let reset_invalid = arguments.iter().any(|value| value == "--reset-invalid");
            if arguments.iter().skip(1).any(|value| {
                !matches!(value.as_str(), "--include-undetected" | "--reset-invalid")
            }) {
                return Err("usage: himind-agent mcp apply-all [--include-undetected] [--reset-invalid]".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_targets::apply_all(
                    options,
                    detected_only,
                    reset_invalid,
                )?)?
            );
        }
        Some("remove") if arguments.len() == 2 => println!(
            "{}",
            serde_json::to_string_pretty(&app::mcp_targets::remove(options, &arguments[1])?)?
        ),
        Some("remove-all") => {
            let detected_only = !arguments.iter().any(|value| value == "--include-undetected");
            if arguments.iter().skip(1).any(|value| value != "--include-undetected") {
                return Err("usage: himind-agent mcp remove-all [--include-undetected]".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_targets::remove_all(
                    options,
                    detected_only,
                )?)?
            );
        }
        Some("test") if arguments.len() == 2 => {
            let server = app::mcp_registry::get(&options.state_path, &arguments[1])?
                .ok_or_else(|| format!("MCP server not found: {}", arguments[1]))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_probe::probe_report(&server))?
            );
        }
        _ => {
            return Err("usage: himind-agent mcp <list|targets|inspect server-id|plan target-id|apply target-id [--reset-invalid]|apply-all [--include-undetected] [--reset-invalid]|remove target-id|remove-all [--include-undetected]|test server-id>".into())
        }
    }
    Ok(())
}

fn run_runtime_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("status") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime::builtin::status())?
            );
        }
        Some("install") => {
            let client_instance_id =
                format!("himind-agent-runtime-{}", store::paths::profile_name());
            let status = runtime::builtin::install(options, &client_instance_id)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Some("check-update") => {
            let client_instance_id =
                format!("himind-agent-runtime-{}", store::paths::profile_name());
            let status = runtime::builtin::check_update(options, &client_instance_id)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Some("update") => {
            let client_instance_id =
                format!("himind-agent-runtime-{}", store::paths::profile_name());
            let mut progress = |stage: &str, percent: u8, message: &str| {
                eprintln!("[{percent:>3}%] {stage}: {message}");
            };
            let status =
                runtime::builtin::update_with_progress(options, &client_instance_id, &mut progress)
                    .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Some("uninstall") => {
            let mut progress = |stage: &str, percent: u8, message: &str| {
                eprintln!("[{percent:>3}%] {stage}: {message}");
            };
            let status = runtime::builtin::uninstall_with_progress(&mut progress)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        _ => {
            return Err(
                "usage: himind-agent runtime <status|install|check-update|update|uninstall>".into(),
            )
        }
    }
    Ok(())
}

fn run_skill_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    skill::cli::run(options, arguments)
}

fn run_plugin_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let state = api::client::load_agent_state(&options.state_path).ok();
    if let Some(state) = state.as_ref() {
        options.set_agent_credential(&state.credential);
    }
    match arguments.first().map(String::as_str) {
        Some("list") => println!(
            "{}",
            capability::plugin::registry_json_for_control_plane(
                options.mode().control_plane_enabled(),
            )?
        ),
        Some("import-local") if arguments.len() == 2 => {
            app::plugin_manager::install_local_package(std::path::Path::new(&arguments[1]))?;
            println!(
                "{}",
                capability::plugin::registry_json_for_control_plane(
                    options.mode().control_plane_enabled(),
                )?
            );
        }
        Some("import-github") if (arguments.len() >= 2 && arguments.len() <= 4) => {
            let registry = app::github_source::import_plugin(
                &arguments[1],
                arguments.get(2).map(String::as_str).unwrap_or_default(),
                arguments.get(3).map(String::as_str).unwrap_or_default(),
            )?;
            println!("{}", serde_json::to_string_pretty(&registry)?);
        }
        Some("install") if arguments.len() == 2 => {
            let state = state.as_ref().ok_or("plugin install requires Dashboard enrollment")?;
            app::plugin_manager::install(options, &state.agent_id, &arguments[1], None)?
        }
        Some("uninstall") if arguments.len() == 2 => {
            app::plugin_manager::uninstall(&arguments[1])?
        }
        Some("rollback") if arguments.len() == 2 => {
            app::plugin_manager::rollback(&arguments[1])?
        }
        Some("enable") if arguments.len() == 2 => {
            app::plugin_manager::set_enabled(&arguments[1], true)?
        }
        Some("disable") if arguments.len() == 2 => {
            app::plugin_manager::set_enabled(&arguments[1], false)?
        }
        Some("invoke") if arguments.len() == 3 => {
            let input_path = arguments[2].strip_prefix('@').unwrap_or(&arguments[2]);
            let raw_input = if std::path::Path::new(input_path).is_file() {
                std::fs::read_to_string(input_path)?
            } else {
                arguments[2].clone()
            };
            let input = serde_json::from_str::<Value>(&raw_input)?;
            let gateway = capability::service::CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(store::types::LocalWorkerStatus::default())),
            );
            println!(
                "{}",
                gateway.invoke(
                    &capability::types::InvocationContext::new(
                        capability::types::InvocationSource::Cli,
                        "local-cli",
                    ),
                    &arguments[1],
                    input,
                )?
            );
        }
        _ => {
            return Err("usage: himind-agent plugin <list|import-local path|import-github github-url [ref] [subpath]|install|uninstall|enable|disable|rollback|invoke> [plugin-id|capability-id json]".into())
        }
    }
    if let (Some(state), Some(plugin_id)) = (
        state.as_ref(),
        arguments.get(1).filter(|_| {
            matches!(
                arguments.first().map(String::as_str),
                Some("install" | "uninstall" | "enable" | "disable" | "rollback")
            )
        }),
    ) {
        let action = arguments[0].as_str();
        let _ =
            app::plugin_manager::report_status(options, &state.agent_id, plugin_id, action, "", "");
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Options {
    api_base: String,
    state_path: PathBuf,
    /// The control-plane mode captured when this process starts. Runtime
    /// settings are persisted for the next launch and must not mutate the
    /// live service graph halfway through a process lifetime.
    effective_mode: app::runtime_mode::AgentMode,
    once: bool,
    interval_seconds: u64,
    local_app: bool,
    local_port: u16,
    reenroll: bool,
    enrollment_token: String,
    agent_credential: Arc<RwLock<String>>,
    identity_generation: Arc<AtomicU64>,
    platform_access: Arc<RwLock<Option<api::oauth::AgentAccessToken>>>,
    task_execution: Arc<RwLock<Option<(String, String, String, String)>>>,
}

impl Options {
    /// Returns the mode used by every service in this process.
    pub(crate) fn mode(&self) -> app::runtime_mode::AgentMode {
        self.effective_mode
    }

    /// Returns the mode persisted by the settings panel. It is intentionally
    /// separate from `mode()` because the persisted value becomes effective
    /// only after the next process start.
    pub(crate) fn pending_mode(&self) -> app::runtime_mode::AgentMode {
        app::runtime_mode::load(&self.state_path)
    }

    pub(crate) fn plugin_view_launch(&self) -> Option<PluginViewLaunch> {
        parse_plugin_view_launch(&env::args().collect::<Vec<_>>())
    }

    pub(crate) fn protocol_open_requested(&self) -> bool {
        protocol_open_requested(&env::args().collect::<Vec<_>>())
    }

    fn from_env() -> Self {
        let mut api_base =
            env::var("DASHBOARD_API_BASE").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let mut state_path = default_state_path();
        let mut once = false;
        let mut interval_seconds = 10;
        let mut local_app = false;
        let mut local_port = 18181;
        let mut reenroll = false;
        let enrollment_token = env::var("HIMIND_AGENT_ENROLLMENT_TOKEN").unwrap_or_default();
        let mut mode_override = None;

        let args: Vec<String> = env::args().collect();
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--api" if i + 1 < args.len() => {
                    api_base = args[i + 1].clone();
                    i += 1;
                }
                "--state" if i + 1 < args.len() => {
                    state_path = PathBuf::from(&args[i + 1]);
                    i += 1;
                }
                "--once" => once = true,
                "--local-app" => local_app = true,
                "--mode" if i + 1 < args.len() => {
                    // This is a process-local debugging override. The panel
                    // remains the product setting and is persisted separately.
                    mode_override = app::runtime_mode::AgentMode::parse(&args[i + 1]);
                    i += 1;
                }
                "--reenroll" => reenroll = true,
                "--interval" if i + 1 < args.len() => {
                    interval_seconds = args[i + 1].parse().unwrap_or(10);
                    i += 1;
                }
                "--local-port" if i + 1 < args.len() => {
                    local_port = args[i + 1].parse().unwrap_or(18181);
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }

        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let effective_mode = mode_override.unwrap_or_else(|| app::runtime_mode::load(&state_path));
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            state_path,
            effective_mode,
            once,
            interval_seconds,
            local_app,
            local_port,
            reenroll,
            enrollment_token,
            agent_credential: Arc::new(RwLock::new(String::new())),
            identity_generation: Arc::new(AtomicU64::new(0)),
            platform_access: Arc::new(RwLock::new(None)),
            task_execution: Arc::new(RwLock::new(None)),
        }
    }
}

fn default_state_path() -> PathBuf {
    store::paths::agent_home()
        .join("data")
        .join("agent-state.json")
}

struct LeaseRenewal {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LeaseRenewal {
    fn start(stop: Arc<AtomicBool>, handle: thread::JoinHandle<()>) -> Self {
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for LeaseRenewal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TaskExecutionGuard {
    options: Options,
}

impl TaskExecutionGuard {
    fn start(
        options: &Options,
        task_id: &str,
        task_type: &str,
        execution_id: &str,
        lease_id: &str,
    ) -> Self {
        options.set_task_execution(task_id, task_type, execution_id, lease_id);
        Self {
            options: options.clone(),
        }
    }
}

impl Drop for TaskExecutionGuard {
    fn drop(&mut self) {
        self.options.clear_task_execution();
    }
}

fn parse_plugin_view_launch(args: &[String]) -> Option<PluginViewLaunch> {
    let mut plugin_id = None;
    let mut view_id = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin-id" if index + 1 < args.len() => {
                plugin_id = Some(args[index + 1].clone());
                index += 1;
            }
            "--view-id" if index + 1 < args.len() => {
                view_id = Some(args[index + 1].clone());
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    match (plugin_id, view_id) {
        (Some(plugin_id), Some(view_id)) if !plugin_id.is_empty() && !view_id.is_empty() => {
            Some(PluginViewLaunch { plugin_id, view_id })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_plugin_view_launch, protocol_open_requested, PluginViewLaunch};

    #[test]
    fn parses_plugin_view_shortcut_arguments() {
        let args = vec![
            "agent.exe".to_string(),
            "--local-app".to_string(),
            "--plugin-id".to_string(),
            "demo.multi-cap".to_string(),
            "--view-id".to_string(),
            "demo.multi-cap.overview".to_string(),
        ];
        assert_eq!(
            parse_plugin_view_launch(&args),
            Some(PluginViewLaunch {
                plugin_id: "demo.multi-cap".to_string(),
                view_id: "demo.multi-cap.overview".to_string(),
            })
        );
    }

    #[test]
    fn rejects_incomplete_plugin_view_shortcut_arguments() {
        let args = vec![
            "agent.exe".to_string(),
            "--plugin-id".to_string(),
            "demo.multi-cap".to_string(),
        ];
        assert_eq!(parse_plugin_view_launch(&args), None);
    }

    #[test]
    fn accepts_only_the_safe_agent_open_protocol_url() {
        let accepted = vec![
            "agent.exe".to_string(),
            "--protocol-url".to_string(),
            "himind-agent://open".to_string(),
        ];
        assert!(protocol_open_requested(&accepted));

        for value in [
            "himind-agent://open?command=exec",
            "himind-agent://open/project",
            "himind-agent://user@open",
            "himind-agent://plugin/open",
            "https://open",
            "not-a-url",
        ] {
            let rejected = vec![
                "agent.exe".to_string(),
                "--protocol-url".to_string(),
                value.to_string(),
            ];
            assert!(!protocol_open_requested(&rejected), "accepted {value}");
        }
    }
}

impl Options {
    pub(crate) fn set_agent_credential(&self, credential: &str) {
        if let Ok(mut current) = self.agent_credential.write() {
            if current.as_str() != credential {
                *current = credential.to_string();
                self.identity_generation.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    pub(crate) fn identity_generation(&self) -> u64 {
        self.identity_generation.load(Ordering::SeqCst)
    }

    pub(crate) fn agent_credential(&self) -> String {
        self.agent_credential
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub(crate) fn set_task_execution(
        &self,
        task_id: &str,
        task_type: &str,
        execution_id: &str,
        lease_id: &str,
    ) {
        if let Ok(mut current) = self.task_execution.write() {
            *current = Some((
                task_id.to_string(),
                task_type.to_string(),
                execution_id.to_string(),
                lease_id.to_string(),
            ));
        }
    }

    pub(crate) fn clear_task_execution(&self) {
        if let Ok(mut current) = self.task_execution.write() {
            *current = None;
        }
    }

    pub(crate) fn task_execution(&self) -> Option<(String, String, String, String)> {
        self.task_execution
            .read()
            .ok()
            .and_then(|value| value.clone())
    }
}

fn execute_task(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: Task,
    approval_mgr: Option<&ApprovalManager>,
) -> Result<(), Box<dyn Error>> {
    println!("executing task {} ({})", task.id, task.task_type);
    let _execution = TaskExecutionGuard::start(
        options,
        &task.id,
        &task.task_type,
        &task.execution_id,
        &task.lease_id,
    );
    let _svn_diagnostic_context = SvnDiagnosticContextGuard::enter(&task.id, &task.execution_id);
    let _lease_renewal = if !task.execution_id.is_empty() && !task.lease_id.is_empty() {
        let lease_stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&lease_stop);
        let renew_client = client.clone();
        let renew_options = options.clone();
        let renew_agent_id = agent_id.to_string();
        let renew_task_id = task.id.clone();
        let renew_execution_id = task.execution_id.clone();
        let renew_lease_id = task.lease_id.clone();
        Some(LeaseRenewal::start(
            lease_stop,
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..30 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Err(error) = api::client::renew_task_lease(
                        &renew_client,
                        &renew_options.api_base,
                        &renew_agent_id,
                        &renew_task_id,
                        &renew_execution_id,
                        &renew_lease_id,
                        &renew_options.agent_credential(),
                    ) {
                        eprintln!("task {} lease renew failed: {}", renew_task_id, error);
                    }
                }
            }),
        ))
    } else {
        None
    };
    if let Some(manager) = approval_mgr {
        manager.add_log(
            "info",
            &format!("开始执行任务: {} ({})", task.id, task.task_type),
        );
    }
    let initial_detail = task
        .detail
        .as_deref()
        .filter(|value| value.contains("中断") || value.contains("重新领取"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("开始执行 {}", task.task_type));
    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        10,
        &initial_detail,
        None,
        None,
    )?;
    let mut last_task_detail = initial_detail;

    if task.task_type == "upload_code"
        || task.task_type == "upload_placeholder"
        || task.task_type == "smb_upload"
    {
        if let Some(mgr) = approval_mgr {
            let approved = mgr.request_approval(
                RequestType::UploadCode,
                format!("上传代码: {}", task.id),
                format!("任务类型: {}", task.task_type),
            )?;
            if !approved {
                return Err("用户拒绝了上传审批".into());
            }
        }
    }
    let result = match task.task_type.as_str() {
        "svn_user_provision" => {
            #[derive(serde::Deserialize)]
            struct SvnUserProvisionRequest {
                user_id: String,
                svn_username: String,
            }
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "正在创建并验证 SVN 用户账号",
                None,
                None,
            )?;
            let request = serde_json::from_value::<SvnUserProvisionRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            let mut result =
                svn::service::provision_default_svn_user_account(&request.svn_username)?;
            result["user_id"] = json!(request.user_id);
            Ok(result)
        }
        "sync_exhibits" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                20,
                "登录内网并读取未上传展项",
                None,
                None,
            )?;
            execute_sync_exhibits(client, options, agent_id, &task)
        }
        "scan_projects" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                25,
                "读取扫描目标并检查目录索引缓存",
                None,
                None,
            )?;
            execute_scan(task.payload.as_ref())
        }
        "upload_code" => {
            execute_upload_code(client, options, agent_id, &task, task.payload.as_ref())
        }
        "upload_placeholder" => {
            execute_upload_placeholder(client, options, agent_id, &task, task.payload.as_ref())
        }
        "smb_upload" => execute_smb_upload(client, options, agent_id, &task, task.payload.as_ref()),
        "project_repository_create" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在连接内网 SvnAdmin 并创建项目仓库",
                None,
                None,
            )?;
            let request = serde_json::from_value::<CreateRepositoryRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            let project_id = request.project_id.clone();
            let repository_access = request.repository_access.clone();
            let repository = create_repository_with_post_commit_hook(request)?;
            let access = ensure_project_exhibits_access(EnsureProjectExhibitsAccessRequest {
                project_id,
                repository_access,
            })?;
            Ok(json!({ "repository": repository, "exhibits_access": access }))
        }
        "project_repository_exhibits_access_ensure" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在配置 TortoiseSVN 兼容且按展项隔离的访问权限",
                None,
                None,
            )?;
            let request = serde_json::from_value::<EnsureProjectExhibitsAccessRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            ensure_project_exhibits_access(request)
        }
        "exhibit_repository_path_create" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在项目仓库中创建展项目录",
                None,
                None,
            )?;
            let request = serde_json::from_value::<CreateExhibitRepositoryPathRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            create_exhibit_repository_path(request)
        }
        "exhibit_repository_initialize" => {
            if let Some(manager) = approval_mgr {
                manager.add_log("info", &format!("{}: 正在创建展项目录", task.id));
            }
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                35,
                "Agent 正在创建展项目录并初始化工程模板",
                None,
                None,
            )?;
            let request = serde_json::from_value::<InitializeExhibitRepositoryRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            create_exhibit_repository_path(CreateExhibitRepositoryPathRequest {
                project_id: request.project_id.clone(),
                exhibit_id: request.exhibit_id.clone(),
            })?;
            if let Some(manager) = approval_mgr {
                manager.add_log("info", &format!("{}: 正在读取并应用工程模板", task.id));
            }
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                55,
                "展项目录已就绪，正在应用模板和 SVN 忽略属性",
                None,
                None,
            )?;
            let mut cancel_guard = TaskCancelGuard::new();
            let mut check_cancel = || cancel_guard.check(client, options, agent_id, &task.id);
            initialize_exhibit_repository_with_cancel(request, &mut check_cancel)
        }
        "exhibit_repository_clone" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                45,
                "Agent 正在从源展项复制 SVN 仓库",
                None,
                None,
            )?;
            let request = serde_json::from_value::<CloneExhibitRepositoryRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            clone_exhibit_repository(request)
        }
        "exhibit_repository_import_local" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                8,
                "Agent 正在预检本地展项工程",
                None,
                None,
            )?;
            let request = serde_json::from_value::<ImportLocalExhibitRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            let mut cancel_guard = TaskCancelGuard::new();
            let mut check_cancel = || cancel_guard.check(client, options, agent_id, &task.id);
            let mut report_progress = |progress: i32, detail: &str| {
                last_task_detail = detail.to_string();
                if let Some(manager) = approval_mgr {
                    manager.add_log("info", &format!("{}: {}", task.id, detail));
                }
                if let Err(error) = report_task(
                    client, options, agent_id, &task.id, "running", progress, detail, None, None,
                ) {
                    if let Some(manager) = approval_mgr {
                        manager.add_log(
                            "warn",
                            &format!(
                                "{}: 进度上报暂时失败，SVN 操作继续执行 - {}",
                                task.id, error
                            ),
                        );
                    }
                }
                Ok(())
            };
            import_local_exhibit_with_cancel_and_progress(
                request,
                &mut check_cancel,
                &mut report_progress,
            )
        }
        "agent_run" => runtime::execute(client, options, agent_id, &task),
        "project_acl_preview" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在读取并比对项目 SVN 权限",
                None,
                None,
            )?;
            let request = serde_json::from_value::<PreviewProjectAclRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            preview_project_acl(request)
        }
        "project_acl_apply" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在校验并应用已批准的项目 SVN 权限",
                None,
                None,
            )?;
            let request = serde_json::from_value::<ApplyProjectAclRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            apply_project_acl(request)
        }
        "project_acl_reconcile" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                40,
                "Agent 正在自动收敛项目 SVN 权限",
                None,
                None,
            )?;
            let request = serde_json::from_value::<ReconcileProjectAclRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            reconcile_project_acl(request)
        }
        _ => Ok(json!({ "message": "unsupported task type", "task_type": task.task_type })),
    };

    match result {
        Ok(value) => {
            if let Some(manager) = approval_mgr {
                manager.add_log(
                    "info",
                    &format!("任务完成: {} ({})", task.id, task.task_type),
                );
            }
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "finished",
                100,
                "任务完成",
                Some(value),
                None,
            )?
        }
        Err(error) => {
            let failure_result = task_failure_result(error.as_ref());
            let error_text = error.to_string();
            if let Some(manager) = approval_mgr {
                manager.add_log(
                    if is_task_canceled_error(&error_text) {
                        "warn"
                    } else {
                        "error"
                    },
                    &format!(
                        "任务失败: {} ({}) - {}",
                        task.id, task.task_type, error_text
                    ),
                );
            }
            if is_task_canceled_error(&error_text) {
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "canceled",
                    100,
                    "任务已取消",
                    None,
                    Some(error_text),
                )?
            } else {
                let failure_detail = if task.task_type == "exhibit_repository_import_local"
                    && !last_task_detail.trim().is_empty()
                {
                    format!("失败于：{}", last_task_detail)
                } else {
                    "任务失败".to_string()
                };
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "failed",
                    100,
                    &failure_detail,
                    failure_result,
                    Some(error_text),
                )?
            }
        }
    }
    Ok(())
}

pub(crate) fn report_task(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task_id: &str,
    status: &str,
    progress: i32,
    detail: &str,
    result: Option<Value>,
    error: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let (_, _, execution_id, lease_id) = options
        .task_execution()
        .unwrap_or_else(|| (String::new(), String::new(), String::new(), String::new()));
    let report = TaskReportRecord {
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        execution_id: execution_id.clone(),
        lease_id: lease_id.clone(),
        status: status.to_string(),
        progress,
        detail: detail.to_string(),
        result: result.clone().unwrap_or_else(|| json!({})),
        error: error.clone().unwrap_or_default(),
    };
    let response = api::client::report_task(
        client,
        &options.api_base,
        agent_id,
        task_id,
        status,
        progress,
        detail,
        result,
        error,
        &execution_id,
        &lease_id,
        &options.agent_credential(),
    );
    if let Err(report_error) = response {
        match store_report(&options.state_path, &report) {
            Ok(path) => {
                if let Err(error) = remove_reports_for_execution(
                    &options.state_path,
                    task_id,
                    &execution_id,
                    Some(&path),
                ) {
                    eprintln!("task report outbox prune failed: {error}");
                }
                eprintln!("task report deferred to outbox: {report_error}");
                return Ok(());
            }
            Err(outbox_error) => {
                eprintln!("task report failed and outbox write failed: {outbox_error}");
                return Err(report_error);
            }
        }
    }
    if let Err(error) =
        remove_reports_for_execution(&options.state_path, task_id, &execution_id, None)
    {
        eprintln!("task report outbox cleanup failed: {error}");
    }
    Ok(())
}

fn flush_report_outbox(client: &Client, options: &Options, agent_id: &str) {
    let reports = match list_reports(&options.state_path) {
        Ok(reports) => reports,
        Err(error) => {
            eprintln!("task report outbox read failed: {error}");
            return;
        }
    };
    for (path, report) in reports {
        if report.agent_id != agent_id {
            continue;
        }
        match api::client::report_task(
            client,
            &options.api_base,
            &report.agent_id,
            &report.task_id,
            &report.status,
            report.progress,
            &report.detail,
            Some(report.result),
            if report.error.is_empty() {
                None
            } else {
                Some(report.error)
            },
            &report.execution_id,
            &report.lease_id,
            &options.agent_credential(),
        ) {
            Ok(()) => {
                if let Err(error) = remove_report(&path) {
                    eprintln!("task report outbox cleanup failed: {error}");
                }
            }
            Err(error) => {
                if error.to_string().contains("409 Conflict") {
                    if let Err(remove_error) = remove_report(&path) {
                        eprintln!("stale task report outbox cleanup failed: {remove_error}");
                    }
                    continue;
                }
                eprintln!("task report outbox replay failed: {error}");
                break;
            }
        }
    }
}
