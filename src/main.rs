#![cfg_attr(
    all(not(debug_assertions), not(feature = "mcp-console")),
    windows_subsystem = "windows"
)]

use rand::rngs::OsRng;
use reqwest::blocking::Client;
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pss, RsaPrivateKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

mod acp;
#[allow(dead_code)]
mod agent_core_contracts;
#[allow(dead_code)]
mod agent_core_projection;
#[allow(dead_code)]
mod agent_core_service;
mod api;
mod app;
mod approval;
mod business_integration;
mod capability;
mod development_checkpoint;
mod engineering_project;
mod extension_authoring;
mod extension_category;
mod extension_contracts;
mod extension_projects;
mod extension_workspace;
mod install_layout;
mod mcp;
mod plugin_authoring;
mod remote;
mod runtime;
mod scan;
mod skill;
mod skill_run;
mod scheduler;
mod store;
mod svn;
mod upload;
mod worker;
#[allow(dead_code)]
mod workflow;
mod workflow_handoff;
mod workspace_lease;
mod worktree_identity;

use api::client::{is_task_canceled_error, TaskCancelGuard};
use api::types::Task;
use approval::manager::ApprovalManager;
use approval::types::RequestType;
use capability::service::CapabilityGateway;
use remote::sync::execute_sync_exhibits;
use scan::service::execute_scan;
use store::outbox::{
    list_reports, remove_report, remove_reports_for_execution, store_report, TaskReportRecord,
};
use svn::service::{
    apply_project_acl, checkout_workspace, clone_exhibit_repository,
    create_exhibit_repository_path, create_repository_with_post_commit_hook,
    ensure_project_exhibits_access, import_local_exhibit_with_cancel_and_progress,
    initialize_exhibit_repository_with_cancel, preview_project_acl, reconcile_project_acl,
    task_failure_result, SvnDiagnosticContextGuard,
};
use svn::types::{
    ApplyProjectAclRequest, CloneExhibitRepositoryRequest, CreateExhibitRepositoryPathRequest,
    CreateRepositoryRequest, EnsureProjectExhibitsAccessRequest, ImportLocalExhibitRequest,
    InitializeExhibitRepositoryRequest, PreviewProjectAclRequest, ReconcileProjectAclRequest,
    SvnCheckoutRequest,
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
    configure_process_stacks();
    let options = Options::from_env();
    let arguments = env::args().collect::<Vec<_>>();
    let mcp_mode = should_run_mcp(env!("CARGO_BIN_NAME"), &arguments);
    let acp_mode = should_run_acp(&arguments);
    if let Err(error) = app::extension_lock::recover() {
        eprintln!("extension transaction recovery failed: {error}");
    }
    if !mcp_mode && !acp_mode {
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
    if acp_mode {
        if let Err(error) = acp::run(&options) {
            eprintln!("ACP session failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = auth_cli_arguments() {
        if let Err(error) = run_auth_cli(&options, &arguments) {
            eprintln!("auth command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = trust_cli_arguments() {
        if let Err(error) = run_trust_cli(&options, &arguments) {
            eprintln!("trust command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = engineering_cli_arguments() {
        if let Err(error) = run_engineering_cli(&arguments) {
            eprintln!("engineering command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = extension_cli_arguments() {
        if let Err(error) = run_extension_cli(&arguments) {
            eprintln!("extension command failed: {error}");
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
    } else if let Some(arguments) = credential_cli_arguments() {
        if let Err(error) = run_credential_cli(&arguments) {
            eprintln!("credential command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = connector_cli_arguments() {
        if let Err(error) = run_connector_cli(&options, &arguments) {
            eprintln!("connector command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = approval_cli_arguments() {
        if let Err(error) = run_approval_cli(&arguments) {
            eprintln!("approval command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = workflow_cli_arguments() {
        if let Err(error) = run_workflow_cli(&options, &arguments) {
            eprintln!("workflow command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = schedule_cli_arguments() {
        if let Err(error) = run_schedule_cli(&options, &arguments) {
            eprintln!("schedule command failed: {error}");
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
        app::crash::record_event("info", "Agent 正常退出");
    } else if let Err(error) = worker::run_loop(options, None, None) {
        eprintln!("agent failed: {error}");
        std::process::exit(1);
    }
}

/// Give every thread of this process a generous stack.
///
/// The Windows Agent died twice with STATUS_STACK_OVERFLOW while executing
/// `__chkstk`, which means a thread exhausted its stack instead of failing
/// gracefully.  A larger reserve does not fix an unbounded recursion, but it
/// keeps a deep-but-legitimate call chain from killing the whole process, and
/// it makes the eventual crash dump point at the real recursion instead of the
/// guard page.  `RUST_MIN_STACK` is read by `std` for each spawned thread, so
/// setting it before any spawn covers the plain `thread::spawn` call sites.
fn configure_process_stacks() {
    const STACK_BYTES: &str = "8388608";
    if env::var_os("RUST_MIN_STACK").is_none() {
        env::set_var("RUST_MIN_STACK", STACK_BYTES);
    }
    app::crash::install_panic_hook();
    app::crash::install_exception_dump_filter();
    app::crash::run_selftest_if_requested();
}

fn should_run_mcp(binary_name: &str, arguments: &[String]) -> bool {
    binary_name.eq_ignore_ascii_case("himind-agent-mcp")
        || arguments.iter().any(|argument| argument == "--mcp")
}

fn should_run_acp(arguments: &[String]) -> bool {
    arguments.get(1).is_some_and(|argument| argument == "acp")
}

fn trust_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "trust")?;
    Some(arguments[index + 1..].to_vec())
}

fn engineering_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "engineering")?;
    Some(arguments[index + 1..].to_vec())
}

fn run_engineering_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [project, action, workspace]
            if project == "project" && (action == "validate" || action == "resolve") =>
        {
            let (manifest, workspace_root) =
                engineering_project::load_from_workspace(Path::new(workspace))?;
            if action == "validate" {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "project_id": manifest.project_id,
                        "workspace_root": workspace_root,
                        "targets": manifest.targets.iter().map(|target| &target.id).collect::<Vec<_>>(),
                    }))?
                );
                return Ok(());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &manifest.resolved_snapshot(&workspace_root, "", "")?
                )?
            );
        }
        [project, action, workspace, target] if project == "project" && action == "resolve" => {
            let (manifest, workspace_root) =
                engineering_project::load_from_workspace(Path::new(workspace))?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &manifest.resolved_snapshot(&workspace_root, target, "")?
                )?
            );
        }
        [project, action, workspace, target, environment]
            if project == "project" && action == "resolve" =>
        {
            let (manifest, workspace_root) =
                engineering_project::load_from_workspace(Path::new(workspace))?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &manifest.resolved_snapshot(&workspace_root, target, environment)?
                )?
            );
        }
        [checkpoint, action, workspace, project_id]
            if checkpoint == "checkpoint" && action == "create" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&development_checkpoint::create(&json!({
                    "workspace_root": workspace,
                    "project_id": project_id,
                }))?)?
            );
        }
        [checkpoint, action, workspace, project_id, target]
            if checkpoint == "checkpoint" && action == "create" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&development_checkpoint::create(&json!({
                    "workspace_root": workspace,
                    "project_id": project_id,
                    "target_id": target,
                }))?)?
            );
        }
        [checkpoint, action, workspace, project_id, target, environment]
            if checkpoint == "checkpoint" && action == "create" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&development_checkpoint::create(&json!({
                    "workspace_root": workspace,
                    "project_id": project_id,
                    "target_id": target,
                    "environment": environment,
                }))?)?
            );
        }
        [workspace, lease, action, workspace_root, owner_client]
            if workspace == "workspace" && lease == "lease" && action == "acquire" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::acquire(&json!({
                    "workspace_root": workspace_root,
                    "owner_client": owner_client,
                }))?)?
            );
        }
        [workspace, lease, action, workspace_root, owner_client, owner_session]
            if workspace == "workspace" && lease == "lease" && action == "acquire" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::acquire(&json!({
                    "workspace_root": workspace_root,
                    "owner_client": owner_client,
                    "owner_session": owner_session,
                }))?)?
            );
        }
        [workspace, lease, action, workspace_root, owner_client, owner_session, mode]
            if workspace == "workspace" && lease == "lease" && action == "acquire" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::acquire(&json!({
                    "workspace_root": workspace_root,
                    "owner_client": owner_client,
                    "owner_session": owner_session,
                    "mode": mode,
                }))?)?
            );
        }
        [workspace, lease, action, lease_id]
            if workspace == "workspace" && lease == "lease" && action == "release" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::release(&json!({
                    "lease_id": lease_id,
                }))?)?
            );
        }
        [workspace, lease, action]
            if workspace == "workspace" && lease == "lease" && action == "list" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::list(&json!({}))?)?
            );
        }
        [workspace, lease, action, workspace_root]
            if workspace == "workspace" && lease == "lease" && action == "list" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_lease::list(&json!({
                    "workspace_root": workspace_root,
                }))?)?
            );
        }
        _ => {
            return Err(
                "usage: himind-agent engineering project <validate|resolve> <workspace> [target] [environment] | engineering checkpoint create <workspace> <project_id> [target] [environment] | engineering workspace lease <acquire workspace owner [session] [mode]|release lease-id|list [workspace]>"
                    .into(),
            )
        }
    }
    Ok(())
}

fn run_trust_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action] if action == "sync" => {
            let report = app::trust::sync(options)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        [action] if action == "status" => {
            let report = app::trust::status()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        [action] if action == "verify" => {
            let report = app::trust::verify()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        _ => return Err("usage: himind-agent trust <sync|status|verify>".into()),
    }
    Ok(())
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

fn extension_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "extension")?;
    Some(arguments[index + 1..].to_vec())
}

fn workflow_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "workflow")?;
    Some(arguments[index + 1..].to_vec())
}

fn schedule_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "schedule")?;
    Some(arguments[index + 1..].to_vec())
}

/// 平台级定时任务的命令行入口，便于验收与排障。
///
/// 与能力 `schedule.*` 共用同一实现，CLI 不引入第二套调度语义。
fn run_schedule_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action] if action == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler::list(scheduler::now_epoch())?)?
            );
        }
        [action, input] if action == "set" => {
            let payload = workflow_cli_input(Some(input))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler::set(&payload, scheduler::now_epoch())?)?
            );
        }
        [action, id] if action == "delete" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler::delete(id)?)?
            );
        }
        [action] if action == "tick" => {
            let gateway = capability::service::CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(store::types::LocalWorkerStatus::default())),
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler::run_due(gateway, scheduler::now_epoch())?)?
            );
        }
        _ => {
            return Err("usage: himind-agent schedule <list|set json|delete id|tick>".into());
        }
    }
    Ok(())
}

fn credential_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "credential")?;
    Some(arguments[index + 1..].to_vec())
}

fn connector_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "connector")?;
    Some(arguments[index + 1..].to_vec())
}

fn approval_cli_arguments() -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let index = arguments.iter().position(|value| value == "approval")?;
    Some(arguments[index + 1..].to_vec())
}

fn run_approval_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action] if action == "ownership" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&approval::ownership::matrix())?
            );
            Ok(())
        }
        _ => Err("usage: himind-agent approval ownership".into()),
    }
}

fn run_credential_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action] if action == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_credentials::list()?)?
            );
        }
        [action, handle, connector_id, path] if action == "set-file" => {
            let summary = store::connector_credentials::set_file_path(
                handle,
                connector_id,
                PathBuf::from(path).as_path(),
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        [action, handle, connector_id, secret] if action == "set-secret" => {
            let summary =
                store::connector_credentials::set_secret(handle, connector_id, secret)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        [action, handle] if action == "remove" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "removed": store::connector_credentials::remove(handle)?,
                    "handle": handle,
                }))?
            );
        }
        _ => {
            return Err(
                "usage: himind-agent credential <list|set-file <handle> <connector-id> <path>|set-secret <handle> <connector-id> <secret>|remove <handle>>"
                    .into(),
            )
        }
    }
    Ok(())
}

fn run_connector_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action] if action == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::list()?)?
            );
        }
        [action, connector_id] if action == "status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::status(connector_id)?)?
            );
        }
        [action, connector_id] if action == "enable" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::set_enabled(
                    connector_id,
                    true,
                )?)?
            );
        }
        [action, connector_id] if action == "disable" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::set_enabled(
                    connector_id,
                    false,
                )?)?
            );
        }
        [action, connector_id] if action == "restore" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::restore(connector_id)?)?
            );
        }
        [action, connector_id] if action == "revoke" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::revoke(
                    connector_id,
                    "",
                )?)?
            );
        }
        [action, connector_id, reason] if action == "revoke" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::connector_state::revoke(
                    connector_id,
                    reason,
                )?)?
            );
        }
        [action] if action == "sync" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::app::connector_policy::sync(options)?)?
            );
        }
        _ => {
            return Err(
                "usage: himind-agent connector <list|status <connector-id>|enable <connector-id>|disable <connector-id>|revoke <connector-id> [reason]|restore <connector-id>|sync>"
                    .into(),
            )
        }
    }
    Ok(())
}

fn run_workflow_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [action, source] if action == "author-save" => {
            let draft = workflow::save_authoring_candidate(PathBuf::from(source).as_path())?;
            println!("{}", serde_json::to_string_pretty(&draft)?);
        }
        [action, package_id, version] if action == "author-test" => {
            let gateway = CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(
                    store::types::LocalWorkerStatus::default(),
                )),
            );
            let capabilities =
                gateway.list_capabilities(&capability::types::InvocationContext::new(
                    capability::types::InvocationSource::Cli,
                    "workflow-author-test",
                ))?;
            let draft = workflow::test_authoring_candidate_with_capabilities(
                package_id,
                version,
                &capabilities,
            )?;
            println!("{}", serde_json::to_string_pretty(&draft)?);
        }
        [action, package_id, version] if action == "author-confirm" => {
            let gateway = CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(
                    store::types::LocalWorkerStatus::default(),
                )),
            );
            let capabilities =
                gateway.list_capabilities(&capability::types::InvocationContext::new(
                    capability::types::InvocationSource::Cli,
                    "workflow-author-confirm",
                ))?;
            let draft = workflow::confirm_authoring_candidate_with_capabilities(
                package_id,
                version,
                &capabilities,
            )?;
            println!("{}", serde_json::to_string_pretty(&draft)?);
        }
        [action, path] if action == "validate" => {
            let package = workflow::load_from_directory(PathBuf::from(path).as_path())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "valid": true,
                    "id": package.id,
                    "version": package.version,
                    "name": package.name,
                    "steps": package.steps.len(),
                    "artifacts": package.artifacts.len(),
                    "capabilities": package.capabilities,
                    "ui": package.ui,
                }))?
            );
        }
        [action, reference] | [action, reference, _] if action == "doctor" => {
            let store = workflow::WorkflowStore::open_default()?;
            let package = workflow_package_from_reference(&store, reference)?;
            let gateway = CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(
                    store::types::LocalWorkerStatus::default(),
                )),
            );
            let context = capability::types::InvocationContext::new(
                capability::types::InvocationSource::Cli,
                "workflow-doctor",
            );
            let capabilities = gateway.list_capabilities(&context)?;
            let report = if arguments.len() > 2 {
                let input = workflow_cli_input(arguments.get(2))?;
                let probe_context = context.clone().without_agent_core_run();
                workflow::preflight_with_connector_probes(
                    &package,
                    VERSION,
                    &capabilities,
                    &input,
                    |capability_id, input| {
                        let mut context = probe_context.clone();
                        context.request_id =
                            format!("{}:health:{capability_id}", context.request_id);
                        gateway.invoke(&context, capability_id, input)
                    },
                )
            } else {
                workflow::preflight(&package, VERSION, &capabilities, &serde_json::json!({}))
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.ready {
                return Err("workflow preflight failed".into());
            }
        }
        [action, reference] | [action, reference, _] if action == "run" => {
            let store = workflow::WorkflowStore::open_default()?;
            let package = if PathBuf::from(reference).join("workflow.json").is_file() {
                store
                    .install_from_directory(PathBuf::from(reference).as_path())?
                    .package
            } else {
                workflow_package_from_reference(&store, reference)?
            };
            let input = workflow_cli_input(arguments.get(2))?;
            run_workflow_package(options, &package, input, "", None)?;
        }
        [action] if action == "remote-list" => {
            let state = api::client::load_agent_state(&options.state_path)?;
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            let items = api::distribution::workflow_catalog(
                &client,
                &options.api_base,
                &state.agent_id,
                &state.credential,
            )?;
            println!("{}", serde_json::to_string_pretty(&items)?);
        }
        [action, workflow_id] | [action, workflow_id, _] if action == "remote-install" => {
            let version = arguments.get(2).map(String::as_str);
            let installed = app::workflow_manager::install_dashboard_catalog_workflow(
                options,
                workflow_id,
                version,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&installed))?
            );
        }
        [action] if action == "runs" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let runs = ledger.list_runs(100)?;
            println!("{}", serde_json::to_string_pretty(&runs)?);
        }
        [action] if action == "metrics" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let items = workflow::workflow_metrics_by_package(&ledger)?
                .into_iter()
                .map(|(workflow_id, metrics)| {
                    json!({
                        "workflow_id": workflow_id,
                        "metrics": metrics,
                    })
                })
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string_pretty(&items)?);
        }
        [action] if action == "projection-status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&agent_core_projection::projection_sync_status(
                    options
                )?)?
            );
        }
        [action] if action == "recover" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let recovered = ledger.recover_running_runs(false, 100)?;
            println!("{}", serde_json::to_string_pretty(&recovered)?);
        }
        [action, force] if action == "recover" && force == "--force" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let recovered = ledger.recover_running_runs(true, 100)?;
            println!("{}", serde_json::to_string_pretty(&recovered)?);
        }
        [action] if action == "abandon-stale" => {
            // 与 Agent 启动时的自动收尾同一实现：租约过期的运行直接给终态。
            let count = scheduler::abandon_stale_runs()?;
            println!("{}", serde_json::to_string_pretty(&json!({ "abandoned": count }))?);
        }
        [action] | [action, _] if action == "dispatch" => {
            let limit = workflow_dispatch_limit(arguments);
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let recovered = ledger.recover_running_runs(false, 100)?;
            let dispatched = dispatch_pending_workflows(options, limit)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "recovered": recovered,
                    "dispatched": dispatched,
                }))?
            );
        }
        [action, run_id] if action == "show" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let run = ledger
                .get_run(run_id)?
                .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
            let interaction = ledger
                .get_interaction(&run.interaction_id)?
                .ok_or("workflow run interaction is missing")?;
            let events = ledger.list_events(run_id)?;
            let projections = ledger.projections_for_aggregate(run_id, 100)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "interaction": interaction,
                    "run": run,
                    "events": events,
                    "projections": projections,
                }))?
            );
        }
        [action, run_id] if action == "verify-run" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let run = ledger
                .get_run(run_id)?
                .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
            if run.status != agent_core_contracts::LocalRunStatus::Succeeded {
                return Err(format!(
                    "workflow run is not succeeded: {} ({:?})",
                    run.run_id, run.status
                )
                .into());
            }
            let interaction = ledger
                .get_interaction(&run.interaction_id)?
                .ok_or("workflow run interaction is missing")?;
            let store = workflow::WorkflowStore::open_default()?;
            let package = store.load_for_run_interaction(&interaction)?;
            let verification = workflow::verify_run(&package, &run)?;
            println!("{}", serde_json::to_string_pretty(&verification)?);
        }
        [action, run_id, step_id] if action == "approve" || action == "reject" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let approval_id = workflow::workflow_approval_id(run_id, step_id);
            let run = if let Err(direct_error) =
                ApprovalManager::global().respond(&approval_id, action == "approve")
            {
                // A short-lived CLI run can exit before the asynchronous
                // Approval Bridge has published its pending fact. In that
                // case the durable Run already contains the authoritative
                // pending approval, so fall back to the Run state transition.
                let run = ledger
                    .get_run(run_id)?
                    .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
                let runner = workflow::WorkflowRunner::open_default()?;
                let transition = if action == "approve" {
                    runner.approve_step(run, step_id)
                } else {
                    runner.reject_step(run, step_id)
                };
                match transition {
                    Ok(run) => run,
                    Err(fallback_error) => {
                        return Err(format!(
                            "approval command failed: {direct_error}; run fallback failed: {fallback_error}"
                        )
                        .into())
                    }
                }
            } else {
                ledger
                    .get_run(run_id)?
                    .ok_or_else(|| format!("workflow run not found: {run_id}"))?
            };
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        [action, run_id] if action == "cancel" => {
            let runner = workflow::WorkflowRunner::open_default()?;
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let run = ledger
                .get_run(run_id)?
                .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
            let approval_id = if run.current_step_id.trim().is_empty() {
                String::new()
            } else {
                workflow::workflow_approval_id(&run.run_id, &run.current_step_id)
            };
            let run = runner.cancel(run, "workflow canceled by user")?;
            if !approval_id.is_empty() {
                let _ = ApprovalManager::global().interrupt(&approval_id, "workflow_canceled");
            }
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        [action, run_id] | [action, run_id, _] | [action, run_id, _, _] if action == "resume" => {
            let ledger = store::local_runs::LocalRunLedger::open_default()?;
            let run = ledger
                .get_run(run_id)?
                .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
            let interaction = ledger
                .get_interaction(&run.interaction_id)?
                .ok_or("workflow run interaction is missing")?;
            let mut input = if let Some(value) = arguments.get(2) {
                workflow_cli_input(Some(value))?
            } else {
                interaction
                    .business_context
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            };
            let feedback = arguments
                .get(3)
                .map(String::as_str)
                .map(str::trim)
                .filter(|feedback| !feedback.is_empty());
            if let (Some(object), Some(feedback)) = (input.as_object_mut(), feedback) {
                object.insert("feedback".to_string(), json!(feedback));
            }
            let store = workflow::WorkflowStore::open_default()?;
            let package = store.load_for_run_interaction(&interaction)?;
            run_workflow_package(options, &package, input, run_id, feedback)?;
        }
        [action, source] | [action, source, _] if action == "package" => {
            let output = arguments.get(2).map(String::as_str);
            println!(
                "{}",
                serde_json::to_string_pretty(&package_workflow_archive(source, output, false)?)?
            );
        }
        [action, source] | [action, source, _] if action == "sign" => {
            let output = arguments.get(2).map(String::as_str);
            println!(
                "{}",
                serde_json::to_string_pretty(&package_workflow_archive(source, output, true)?)?
            );
        }
        [action, path] if action == "install-archive" => {
            let installed =
                app::workflow_manager::install_local_archive(PathBuf::from(path).as_path(), false)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&installed))?
            );
        }
        [action, path, policy]
            if action == "install-archive" && policy == "--require-signature" =>
        {
            let installed =
                app::workflow_manager::install_local_archive(PathBuf::from(path).as_path(), true)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&installed))?
            );
        }
        [action, path] if action == "install" => {
            let store = workflow::WorkflowStore::open_default()?;
            let item = store.install_from_directory(PathBuf::from(path).as_path())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&item))?
            );
        }
        [action, path, policy] if action == "install" && policy == "--require-signature" => {
            let store = workflow::WorkflowStore::open_default()?;
            let item =
                store.install_from_directory_with_policy(PathBuf::from(path).as_path(), true)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&item))?
            );
        }
        [action] if action == "list" => {
            let store = workflow::WorkflowStore::open_default()?;
            let items = store
                .list()?
                .iter()
                .map(workflow_installation_json)
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string_pretty(&items)?);
        }
        [action, package_id] if action == "enable" || action == "disable" => {
            let store = workflow::WorkflowStore::open_default()?;
            let item = store.set_enabled(package_id, action == "enable")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&item))?
            );
        }
        [action, package_id] if action == "rollback" => {
            let store = workflow::WorkflowStore::open_default()?;
            let item = store.rollback(package_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_installation_json(&item))?
            );
        }
        [action, package_id] if action == "remove" => {
            let store = workflow::WorkflowStore::open_default()?;
            let removed = store.remove(package_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "removed": removed,
                    "package_id": package_id,
                }))?
            );
        }
        _ => {
            return Err(
                "usage: himind-agent workflow <author-save <dir>|author-test <id> <version>|author-confirm <id> <version>|validate <dir>|doctor <dir|id> [input-json|@file]|package <dir> [output.hmwf]|sign <dir> [output.hmwf]|install-archive <path> [--require-signature]|install <dir> [--require-signature]|remote-list|remote-install <id> [version]|list|run <dir|id> [input-json|@file]|resume <run-id> [input-json|@file] [feedback]|dispatch [limit]|runs|metrics|projection-status|recover [--force]|show <run-id>|verify-run <run-id>|approve <run-id> <step-id>|reject <run-id> <step-id>|cancel <run-id>|enable <id>|disable <id>|rollback <id>|remove <id>>"
                    .into(),
            );
        }
    }
    Ok(())
}

fn workflow_package_from_reference(
    store: &workflow::WorkflowStore,
    reference: &str,
) -> Result<workflow::WorkflowPackage, Box<dyn Error>> {
    let path = PathBuf::from(reference);
    if path.join("workflow.json").is_file() {
        return workflow::load_from_directory(&path);
    }
    Ok(store.load_enabled_for_run(reference)?.package)
}

fn package_workflow_archive(
    source: &str,
    output: Option<&str>,
    sign: bool,
) -> Result<Value, Box<dyn Error>> {
    let source = PathBuf::from(source).canonicalize()?;
    let package = workflow::load_from_directory(&source)?;
    let output = output
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!(
                "{}-{}{}.hmwf",
                package.id.replace('.', "-"),
                package.version,
                if sign { "-signed" } else { "" }
            ))
        });
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "hmwf" | "zip") {
        return Err("Workflow 制品必须使用 .hmwf 或 .zip 扩展名".into());
    }
    let output = if output.is_absolute() {
        output
    } else {
        env::current_dir()?.join(output)
    };
    if output.exists() {
        return Err(format!(
            "workflow package output already exists: {}",
            output.display()
        )
        .into());
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default()
    );
    let staging = env::temp_dir().join(format!("himind-workflow-package-stage-{suffix}"));
    let temporary_output = output.with_file_name(format!(
        ".{}.{}.staging",
        output
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workflow.hmwf"),
        suffix
    ));
    let result = (|| {
        app::local_package::stage_local_package(
            &source,
            &staging,
            &app::local_package::PackageLimits {
                max_files: 100_000,
                max_bytes: 512 * 1024 * 1024,
                label: "Workflow 制品",
            },
            |_| true,
        )?;
        let signature_metadata = if sign {
            let key_path = env::var("HIMIND_SIGNING_PRIVATE_KEY_PATH")
                .or_else(|_| env::var("HIMIND_WORKFLOW_SIGNING_KEY_PATH"))
                .map_err(|_| "workflow signing requires HIMIND_SIGNING_PRIVATE_KEY_PATH")?;
            let key_id = env::var("HIMIND_SIGNING_KEY_ID")
                .or_else(|_| env::var("HIMIND_WORKFLOW_SIGNING_KEY_ID"))
                .map_err(|_| "workflow signing requires HIMIND_SIGNING_KEY_ID")?;
            let key_id = key_id.trim();
            if key_id.is_empty()
                || key_id.len() > 200
                || !key_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(format!("invalid signing key id: {key_id}").into());
            }
            let key = RsaPrivateKey::from_pkcs8_pem(&std::fs::read_to_string(key_path)?).map_err(
                |error| format!("workflow signing key is not valid PKCS#8 PEM: {error}"),
            )?;
            let checksums = std::fs::read(staging.join("checksums.sha256"))?;
            let signature = key
                .sign_with_rng(
                    &mut OsRng,
                    Pss::new::<Sha256>(),
                    &Sha256::digest(&checksums),
                )
                .map_err(|error| format!("workflow signing failed: {error}"))?;
            let metadata = json!({
                "algorithm": "rsa-pss-sha256",
                "key_id": key_id,
                "signature": base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    signature,
                ),
            });
            std::fs::write(
                staging.join("manifest.sig"),
                serde_json::to_vec_pretty(&metadata)?,
            )?;
            Some(metadata)
        } else {
            None
        };
        app::local_package::archive_directory(&staging, &temporary_output)?;
        std::fs::rename(&temporary_output, &output)?;
        let bytes = std::fs::read(&output)?;
        Ok(json!({
            "workflow_id": package.id,
            "version": package.version,
            "output": output,
            "file_size": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(&bytes)),
            "signed": signature_metadata.is_some(),
            "signature": signature_metadata,
        }))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&temporary_output);
    result
}

fn workflow_cli_input(value: Option<&String>) -> Result<Value, Box<dyn Error>> {
    let Some(value) = value else {
        return Ok(json!({}));
    };
    let path = value.strip_prefix('@').unwrap_or(value);
    let raw = if std::path::Path::new(path).is_file() {
        std::fs::read_to_string(path)?
    } else {
        value.clone()
    };
    let input: Value = serde_json::from_str(&raw)?;
    if !input.is_object() {
        return Err("workflow input must be a JSON object".into());
    }
    Ok(input)
}

fn run_workflow_package(
    options: &Options,
    package: &workflow::WorkflowPackage,
    input: Value,
    resume_run_id: &str,
    resume_feedback: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let gateway = CapabilityGateway::new(
        options.clone(),
        Arc::new(std::sync::Mutex::new(
            store::types::LocalWorkerStatus::default(),
        )),
    );
    let context = capability::types::InvocationContext::new(
        capability::types::InvocationSource::Workflow,
        "workflow-runner",
    );
    let capabilities = gateway.list_capabilities(&context)?;
    let probe_context = context.clone().without_agent_core_run();
    let report = workflow::preflight_with_connector_probes(
        package,
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
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Err("workflow preflight failed".into());
    }
    let runner = workflow::WorkflowRunner::open_default()?;
    let ledger = store::local_runs::LocalRunLedger::open_default()?;
    let run = if resume_run_id.is_empty() {
        runner.start("local-agent", package, &workflow_request_id(), &input)?
    } else {
        ledger
            .get_run(resume_run_id)?
            .ok_or_else(|| format!("workflow run not found: {resume_run_id}"))?
    };
    let run = if let Some(feedback) = resume_feedback {
        runner.record_loop_feedback(package, run, feedback)?
    } else {
        run
    };
    let executor = workflow::WorkflowGatewayExecutor::new(
        gateway,
        context,
        ledger.clone(),
        run.run_id.clone(),
    );
    let outcome = runner.run_ready(package, run, &input, &executor)?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}

fn dispatch_pending_workflows(
    options: &Options,
    limit: usize,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let ledger = store::local_runs::LocalRunLedger::open_default()?;
    let store = workflow::WorkflowStore::open_default()?;
    let runs = ledger.list_runs(limit)?;
    let mut dispatched = Vec::new();
    for run in runs
        .into_iter()
        .filter(|run| run.status == agent_core_contracts::LocalRunStatus::Queued)
    {
        let interaction = match ledger.get_interaction(&run.interaction_id)? {
            Some(interaction) => interaction,
            None => {
                dispatched.push(json!({
                    "run_id": run.run_id,
                    "status": "blocked",
                    "error": "workflow run interaction is missing",
                }));
                continue;
            }
        };
        let package = match store.load_for_run_interaction(&interaction) {
            Ok(package) => package,
            Err(error) => {
                dispatched.push(json!({
                    "run_id": run.run_id,
                    "status": "blocked",
                    "error": error.to_string(),
                }));
                continue;
            }
        };
        let input = interaction
            .business_context
            .get("input")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match run_workflow_package(options, &package, input, &run.run_id, None) {
            Ok(()) => dispatched.push(json!({"run_id": run.run_id, "status": "executed"})),
            Err(error) if error.to_string().contains("leased by another process") => {
                dispatched.push(json!({
                    "run_id": run.run_id,
                    "status": "skipped",
                    "reason": "leased by another process",
                }));
            }
            Err(error) => dispatched.push(json!({
                "run_id": run.run_id,
                "status": "failed",
                "error": error.to_string(),
            })),
        }
    }
    Ok(dispatched)
}

fn workflow_dispatch_limit(arguments: &[String]) -> usize {
    arguments
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100)
}

fn workflow_request_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    format!("{millis}_{}", std::process::id())
}

fn workflow_installation_json(item: &workflow::InstalledWorkflow) -> Value {
    json!({
        "id": item.package.id,
        "name": item.package.name,
        "version": item.package.version,
        "previous_version": item.previous_version,
        "enabled": item.enabled,
        "package_digest": item.package_digest,
        "source": item.source,
        "installed_at": item.installed_at,
        "updated_at": item.updated_at,
        "steps": item.package.steps.len(),
        "artifacts": item.package.artifacts.len(),
    })
}

/// Local, scriptable view of the extension sources.
///
/// The development workspace is driven from the UI, but source management is
/// also needed from shells and verification scripts: listing and refreshing the
/// snapshot, adding or removing a source, planning an install and reading
/// provenance.  Every action returns the same JSON the UI consumes.
fn run_extension_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [source, action] if source == "source" && action == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::settings()?)?
            );
        }
        [source, action] if source == "source" && action == "refresh" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::refresh_snapshot()?)?
            );
        }
        [source, action, name, repository, reference] if source == "source" && action == "add" => {
            let settings =
                app::extension_source::add_github_source(name, repository, reference, None, None)?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, name, root] if source == "source" && action == "add-local" => {
            let settings = app::extension_source::add_local_source(name, root, None)?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, name, root, catalog_path]
            if source == "source" && action == "add-local" =>
        {
            let settings = app::extension_source::add_local_source(name, root, Some(catalog_path))?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, name, repository, reference, catalog_path]
            if source == "source" && action == "add" =>
        {
            let settings = app::extension_source::add_github_source(
                name,
                repository,
                reference,
                Some(catalog_path),
                None,
            )?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, name, repository, reference, catalog_path, verification]
            if source == "source" && action == "add" =>
        {
            let settings = app::extension_source::add_github_source(
                name,
                repository,
                reference,
                Some(catalog_path),
                Some(verification),
            )?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, source_id] if source == "source" && action == "remove" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::remove_source(source_id)?)?
            );
        }
        [source, action, source_id] if source == "source" && action == "enable" => {
            let settings = app::extension_source::set_source_enabled(source_id, true)?;
            let _ = app::extension_source::reconcile_dsh_presets_now();
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, source_id] if source == "source" && action == "disable" => {
            let settings = app::extension_source::set_source_enabled(source_id, false)?;
            let _ = app::extension_source::reconcile_dsh_presets_now();
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        // 取用侧决定单元按哪一侧安装。默认 local，显式写 remote 才会落盘。
        [source, action, unit_key, acquisition] if source == "source" && action == "acquisition" => {
            let acquisition = match acquisition.as_str() {
                "local" => app::extension_source::ExtensionSourceAcquisition::Local,
                "remote" => app::extension_source::ExtensionSourceAcquisition::Remote,
                other => return Err(format!("取用侧必须是 local 或 remote，收到: {other}").into()),
            };
            let settings =
                app::extension_source::set_unit_acquisition(unit_key, acquisition)?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action, kind, id] if source == "source" && action == "plan" => {
            let value = match kind.as_str() {
                "plugin" => serde_json::to_value(app::extension_source::plan_plugin(id, None)?)?,
                "skill" => serde_json::to_value(app::extension_source::plan_skill(id, None)?)?,
                "workflow" => {
                    serde_json::to_value(app::extension_source::plan_workflow(id, None)?)?
                }
                _ => return Err("扩展类型必须是 plugin、skill 或 workflow".into()),
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        [source, action, kind, id] | [source, action, kind, id, _]
            if source == "source" && action == "install" =>
        {
            let version = arguments.get(4).map(String::as_str);
            let value = match kind.as_str() {
                "plugin" => {
                    serde_json::to_value(app::extension_source::install_plugin(id, version)?)?
                }
                "skill" => {
                    let (catalog_item, record) = app::extension_source::install_skill(id, version)?;
                    serde_json::json!({ "catalog_item": catalog_item, "record": record })
                }
                "workflow" => {
                    let (catalog_item, installed) =
                        app::extension_source::install_workflow(id, version)?;
                    serde_json::json!({
                        "catalog_item": catalog_item,
                        "installation": workflow_installation_json(&installed),
                    })
                }
                _ => return Err("扩展类型必须是 plugin、skill 或 workflow".into()),
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        [source, action] if source == "source" && action == "provenance" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::list_provenance()?)?
            );
        }
        [source, action] if source == "source" && action == "update" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::reconcile_auto_updates()?)?
            );
        }
        _ => {
            return Err("usage: himind-agent extension source <list|refresh|add name github-url [ref] [catalog-path] [required|optional]|add-local name path [catalog-path]|remove source-id|enable source-id|disable source-id|acquisition unit-key local|remote|plan plugin|skill|workflow id|install plugin|skill|workflow id [version]|provenance|update>".into());
        }
    }
    Ok(())
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
        Some("acp-profile") => {
            run_acp_profile_cli(&arguments[1..])?;
        }
        Some("providers") => {
            if arguments.len() != 1 {
                return Err("usage: himind-agent runtime providers".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime::probe_installations())?
            );
        }
        Some("status") => {
            if arguments.len() != 1 {
                return Err("usage: himind-agent runtime status".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime::builtin::status())?
            );
        }
        Some("install") => {
            apply_runtime_manifest_argument(&arguments[1..])?;
            let client_instance_id =
                format!("himind-agent-runtime-{}", store::paths::profile_name());
            let status = runtime::builtin::install(options, &client_instance_id)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Some("check-update") => {
            apply_runtime_manifest_argument(&arguments[1..])?;
            let client_instance_id =
                format!("himind-agent-runtime-{}", store::paths::profile_name());
            let status = runtime::builtin::check_update(options, &client_instance_id)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Some("update") => {
            apply_runtime_manifest_argument(&arguments[1..])?;
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
            if arguments.len() != 1 {
                return Err("usage: himind-agent runtime uninstall".into());
            }
            let mut progress = |stage: &str, percent: u8, message: &str| {
                eprintln!("[{percent:>3}%] {stage}: {message}");
            };
            let status = runtime::builtin::uninstall_with_progress(&mut progress)
                .map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        _ => {
            return Err(
                "usage: himind-agent runtime <acp-profile <list|set|enable|disable|remove>|providers|status|install|check-update|update|uninstall> [--manifest <runtime-release.json>]".into(),
            )
        }
    }
    Ok(())
}

fn run_acp_profile_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("list") if arguments.len() == 1 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&store::acp_profiles::list()?)?
            );
        }
        Some("set") if arguments.len() >= 3 => {
            let mut provider_id = arguments[1].clone();
            let executable = arguments[2].clone();
            let mut args_json = None;
            let mut display_name = String::new();
            let mut version = String::new();
            let mut permission_policy = "deny".to_string();
            let mut enabled = true;
            let mut index = 3;
            if arguments
                .get(index)
                .is_some_and(|value| value.trim_start().starts_with('['))
            {
                args_json = Some(arguments[index].clone());
                index += 1;
            }
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--name" if index + 1 < arguments.len() => {
                        display_name = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--version" if index + 1 < arguments.len() => {
                        version = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--permission" if index + 1 < arguments.len() => {
                        permission_policy = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--disabled" => {
                        enabled = false;
                        index += 1;
                    }
                    value => return Err(format!("unknown ACP profile option: {value}").into()),
                }
            }
            provider_id = store::acp_profiles::normalize_provider_id(&provider_id);
            if display_name.trim().is_empty() {
                display_name = provider_id.clone();
            }
            let args = args_json
                .map(|value| serde_json::from_str::<Vec<String>>(&value))
                .transpose()?
                .unwrap_or_default();
            let profile = store::acp_profiles::upsert(
                store::acp_profiles::AcpRuntimeProfileRecord {
                    provider_id,
                    display_name,
                    executable,
                    args,
                    version,
                    permission_policy,
                    enabled,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&profile)?);
        }
        Some("enable") | Some("disable") if arguments.len() == 2 => {
            let enabled = arguments[0] == "enable";
            println!(
                "{}",
                serde_json::to_string_pretty(&store::acp_profiles::set_enabled(
                    &arguments[1],
                    enabled,
                )?)?
            );
        }
        Some("remove") if arguments.len() == 2 => {
            let removed = store::acp_profiles::remove(&arguments[1])?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "provider_id": store::acp_profiles::normalize_provider_id(&arguments[1]),
                    "removed": removed,
                }))?
            );
        }
        _ => {
            return Err(
                "usage: himind-agent runtime acp-profile <list|set provider executable [args-json] [--name name] [--version version] [--permission deny|allow_once|prompt] [--disabled]|enable provider|disable provider|remove provider>".into(),
            )
        }
    }
    Ok(())
}

fn apply_runtime_manifest_argument(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [] => Ok(()),
        [flag, path] if flag == "--manifest" && !path.trim().is_empty() => {
            runtime::distribution::use_local_manifest(path)
        }
        _ => Err("usage: himind-agent runtime <install|check-update|update> [--manifest <runtime-release.json>]".into()),
    }
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
            let result = gateway.invoke(
                &capability::types::InvocationContext::new(
                    capability::types::InvocationSource::Cli,
                    "local-cli",
                ),
                &arguments[1],
                input,
            )?;
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout().lock(), "{result}");
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
        let mut api_base = env::var("DASHBOARD_API_BASE")
            .or_else(|_| env::var("HIMIND_DEVELOPMENT_AGENT_API_BASE"))
            .unwrap_or_else(|_| default_dashboard_api_base().to_string());
        let mut state_path = default_state_path();
        let mut once = false;
        let mut interval_seconds = 10;
        let mut local_app = false;
        let mut local_port = default_local_port();
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
                    local_port = args[i + 1].parse().unwrap_or(local_port);
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

/// The local development ports are fixed by the repository tooling:
/// Dashboard/API on 18083 and the development Agent on 18082.  A debug build
/// follows that topology by default so a plain `cargo run -- --local-app`
/// cannot collide with the installed production Agent on 18181.  Release
/// builds keep the shipped 18181 default.
fn default_local_port() -> u16 {
    const DEVELOPMENT_PORT: u16 = 18082;
    const PRODUCTION_PORT: u16 = 18181;
    env::var("HIMIND_AGENT_LOCAL_PORT")
        .or_else(|_| env::var("HIMIND_DEVELOPMENT_AGENT_PORT"))
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            DEVELOPMENT_PORT
        } else {
            PRODUCTION_PORT
        })
}

fn default_dashboard_api_base() -> &'static str {
    if cfg!(debug_assertions) {
        "http://127.0.0.1:18083"
    } else {
        "http://localhost:8080"
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
    use super::{
        default_dashboard_api_base, default_local_port, parse_plugin_view_launch,
        protocol_open_requested, should_run_acp, should_run_mcp, workflow_dispatch_limit,
        PluginViewLaunch,
    };
    use std::env;

    #[test]
    fn development_builds_default_to_the_fixed_local_ports() {
        if cfg!(debug_assertions) {
            assert_eq!(default_dashboard_api_base(), "http://127.0.0.1:18083");
        }
        let overridden = env::var("HIMIND_AGENT_LOCAL_PORT").is_ok()
            || env::var("HIMIND_DEVELOPMENT_AGENT_PORT").is_ok();
        if !overridden {
            assert_eq!(
                default_local_port(),
                if cfg!(debug_assertions) { 18082 } else { 18181 }
            );
        }
    }

    #[test]
    fn workflow_dispatch_limit_is_bounded() {
        assert_eq!(workflow_dispatch_limit(&[]), 20);
        assert_eq!(
            workflow_dispatch_limit(&["dispatch".to_string(), "55".to_string()]),
            55
        );
        assert_eq!(
            workflow_dispatch_limit(&["dispatch".to_string(), "0".to_string()]),
            1
        );
        assert_eq!(
            workflow_dispatch_limit(&["dispatch".to_string(), "999".to_string()]),
            100
        );
    }

    #[test]
    fn selects_mcp_mode_by_binary_target_or_explicit_argument() {
        assert!(should_run_mcp("himind-agent-mcp", &[]));
        assert!(should_run_mcp(
            "himind-agent",
            &["himind-agent.exe".to_string(), "--mcp".to_string()]
        ));
        assert!(!should_run_mcp(
            "himind-agent",
            &["himind-agent.exe".to_string(), "--local-app".to_string()]
        ));
    }

    #[test]
    fn selects_acp_mode_only_from_the_first_command() {
        assert!(should_run_acp(&[
            "himind-agent.exe".to_string(),
            "acp".to_string()
        ]));
        assert!(!should_run_acp(&[
            "himind-agent.exe".to_string(),
            "workflow".to_string(),
            "acp".to_string()
        ]));
    }

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
        "exhibit_workspace_checkout" => {
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                30,
                "正在准备检出工作区",
                None,
                None,
            )?;
            let request = serde_json::from_value::<SvnCheckoutRequest>(
                task.payload.clone().unwrap_or_else(|| json!({})),
            )?;
            let mut cancel_guard = TaskCancelGuard::new();
            cancel_guard.check(client, options, agent_id, &task.id)?;
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                45,
                "正在检出 SVN 工作区",
                None,
                None,
            )?;
            let mut result = checkout_workspace(request)?;
            cancel_guard.check(client, options, agent_id, &task.id)?;
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                85,
                "正在同步工作区外部依赖",
                None,
                None,
            )?;
            if result.get("target_path").is_none() {
                if let Some(target_path) = task
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("target_path"))
                {
                    result["target_path"] = target_path.clone();
                }
            }
            Ok(result)
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
