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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
#[allow(dead_code)]
mod ecc_import;
mod engineering_project;
mod expert;
mod expert_authoring;
mod extension_authoring;
mod extension_category;
mod extension_contracts;
mod extension_projects;
mod extension_workspace;
mod install_layout;
mod instruction_pack;
#[allow(dead_code)]
mod instruction_projection;
#[allow(dead_code)]
mod instruction_targets;
mod local_activity;
mod mcp;
mod path_guard;
mod plugin_authoring;
mod remote;
mod runtime;
mod scan;
mod scheduler;
mod skill;
mod skill_run;
mod store;
mod svn;
mod upload;
mod worker;
#[allow(dead_code)]
mod workflow;
mod workflow_handoff;
mod workspace_instructions;
mod workspace_lease;
mod worktree_identity;

use api::client::{is_task_canceled_error, TaskCancelGuard};
use api::types::Task;
use approval::manager::ApprovalManager;
use approval::types::RequestType;
use capability::execution::CapabilityExecutionContext;
use capability::service::CapabilityGateway;
use capability::types::{InvocationContext, InvocationSource};
use store::outbox::{
    list_reports, remove_report, remove_reports_for_execution, store_report, TaskReportRecord,
};
use svn::service::{
    initialize_exhibit_repository_with_cancel, task_failure_result, SvnDiagnosticContextGuard,
};
use svn::types::{
    ApplyProjectAclRequest, CloneExhibitRepositoryRequest, CreateExhibitRepositoryPathRequest,
    CreateRepositoryRequest, EnsureProjectExhibitsAccessRequest,
    InitializeExhibitRepositoryRequest, PreviewProjectAclRequest, ReconcileProjectAclRequest,
    SvnCheckoutRequest,
};
use upload::tasks::execute_backup_run;

// Keep the runtime health version aligned with the version stamped into the
// updater package. Cargo is the source of truth for both binaries.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginViewLaunch {
    pub plugin_id: String,
    pub view_id: String,
}

const AGENT_PROTOCOL_SCHEME: &str = "himind-agent";

/// 深链 `himind-agent://open?...` 携带的落点。写通道②（工作台在 Agent 不可达
/// 时经 `himind-agent://` 唤起工具中心）只使用下方「精确枚举」的值，见 ADR 0118。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentOpenTarget {
    /// 纯唤起：只把主窗口带到前台（历史行为）。
    Main,
    /// 唤起并把设置窗口开到「AI 连接」面板，供用户在本机完成注册。
    SettingsAi,
}

impl AgentOpenTarget {
    /// `open` 查询参数的枚举值到落点的映射；未知名返回 `None` 由调用方拒绝。
    fn from_query(value: &str) -> Option<Self> {
        match value {
            "ai" => Some(AgentOpenTarget::SettingsAi),
            _ => None,
        }
    }
}

/// 解析 `--protocol-url himind-agent://open[?open=<枚举>]`。
///
/// 只认「精确枚举」：主机必须是 `open`、路径为空、无用户名/端口/片段，查询串
/// 里也只允许一个受白名单约束的 `open` 键。其余一律拒绝，避免深链退化成能从
/// 浏览器触发任意动作的命令通道（ADR 0118）。
fn parse_protocol_open(args: &[String]) -> Option<AgentOpenTarget> {
    let index = args.iter().position(|value| value == "--protocol-url")?;
    let value = args.get(index + 1)?;
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != AGENT_PROTOCOL_SCHEME
        || url.host_str() != Some("open")
        || !(url.path().is_empty() || url.path() == "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut target = AgentOpenTarget::Main;
    for (key, raw) in url.query_pairs() {
        if key != "open" {
            return None;
        }
        target = AgentOpenTarget::from_query(raw.as_ref())?;
    }
    Some(target)
}

fn main() {
    configure_process_stacks();
    let arguments = env::args().collect::<Vec<_>>();
    let mcp_mode = should_run_mcp(env!("CARGO_BIN_NAME"), &arguments);
    let acp_mode = should_run_acp(&arguments);
    // Must run before `Options::from_env()`: that is where the state path — and
    // with it every other path of this process — is derived from the profile.
    apply_startup_profile(
        &arguments,
        !mcp_mode && !acp_mode && cli_subcommand(&arguments).is_none(),
    );
    let options = Options::from_env();
    if let Err(error) = app::extension_lock::recover() {
        eprintln!("extension transaction recovery failed: {error}");
    }
    if !mcp_mode && !acp_mode {
        if let Err(error) = svn::service::bootstrap_svn_credentials() {
            eprintln!("SVN credential initialization failed: {error}");
            std::process::exit(1);
        }
        // Central SVN administration is owned by himind-edge-worker. Remove
        // any legacy desktop copy once, then keep this process limited to the
        // signed-in user's own SVN connection, which the user picks in the app.
        if let Err(error) = svn::service::remove_svn_admin_credentials() {
            eprintln!("legacy desktop SVN admin credential cleanup deferred: {error}");
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
        if let Err(error) = run_extension_cli(&options, &arguments) {
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
    } else if let Some(arguments) = market_cli_arguments() {
        if let Err(error) = run_market_cli(&options, &arguments) {
            eprintln!("market command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = instruction_pack_cli_arguments() {
        if let Err(error) = run_instruction_pack_cli(&arguments) {
            eprintln!("instruction pack command failed: {error}");
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

/// The value that follows `name`, if the flag appears exactly once.
fn flag_value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    let mut indexes = arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.as_str() == name)
        .map(|(index, _)| index);
    let index = indexes.next()?;
    if indexes.next().is_some() {
        return None;
    }
    arguments.get(index + 1).map(String::as_str)
}

/// Resolve the Agent profile before anything reads `agent_home()`.
///
/// Two things have to happen before `Options::from_env()` builds the state
/// path, because that is the first `agent_home()` call of the process:
///
/// 1. Pick the profile. An explicit selector wins; without one the executable
///    decides, so a build output runs as `development` instead of opening the
///    installed Agent's data root.
/// 2. Refuse the one combination that is never intentional: an installed
///    `production` profile coming from a binary that is not an installation.
///    That is exactly how a development session erased a production identity,
///    and `--profile`/`HIMIND_AGENT_HOME` are the deliberate ways to ask for it
///    instead.
///
/// The resolved name is written into `HIMIND_AGENT_PROFILE`, so every existing
/// reader — including the MCP companion this process spawns — agrees on it.
fn apply_startup_profile(arguments: &[String], interactive: bool) {
    let executable = env::current_exe().unwrap_or_default();
    let requested = flag_value(arguments, "--profile");
    if let Some(requested) = requested {
        if store::paths::normalize_profile(requested).is_none() {
            let message = format!("--profile 的名称不合法：{requested}");
            eprintln!("HiMind Agent 拒绝启动：{message}");
            if interactive {
                app::crash::show_startup_notice("HiMind Agent 无法启动", &message);
            }
            std::process::exit(2);
        }
    }
    let (profile, source) = store::paths::resolve_profile(requested, &executable);
    let installed = install_layout::executable_is_installed(&executable);
    if !installed
        && store::paths::is_production_profile(&profile)
        && store::paths::explicit_agent_home().is_none()
    {
        let message = format!(
            "这个 himind-agent.exe 不是安装版（{}），不能运行 production profile：\n\
             继续启动会打开并改写本机已安装 Agent 的数据目录。\n\n\
             开发请使用：--profile development\n\
             或运行 scripts\\development\\start-agent.ps1。\n\
             确实要指定数据目录时，请同时设置 HIMIND_AGENT_HOME。",
            executable.display()
        );
        eprintln!("HiMind Agent 拒绝启动：{message}");
        if interactive {
            app::crash::show_startup_notice("HiMind Agent 无法启动", &message);
        }
        std::process::exit(2);
    }
    store::paths::apply_profile(&profile);
    if source.inferred() && !installed {
        eprintln!(
            "HiMind Agent: 非安装版可执行文件，使用 {profile} profile（数据目录 {}）。",
            store::paths::agent_home().display()
        );
    }
}

fn should_run_acp(arguments: &[String]) -> bool {
    arguments.get(1).is_some_and(|argument| argument == "acp")
}

/// 取第一个位置参数（子命令）及其下标，跳过选项名和它们的取值。
///
/// 直接在全量参数里 `position(|v| v == "skill")` 会误判：`market search --kind
/// skill` 里的 `--kind skill` 取值 `skill` 会被当成 `skill` 子命令，于是市场命令
/// 永远走不到。这里按“选项 + 取值”成对跳过，只认第一个裸参数。
fn cli_subcommand(arguments: &[String]) -> Option<(usize, &str)> {
    // 会吃掉下一个参数的选项；不在表里的选项按布尔开关处理。
    const VALUE_FLAGS: &[&str] = &[
        "--api",
        "--profile",
        "--state",
        "--interval",
        "--local-port",
        "--mode",
        "--protocol-url",
        "--workspace",
        "--kind",
        "--id",
        "--version",
        "--source",
        "--query",
        "--category",
        "--limit",
        "--cursor",
        "--client",
        "--artifact-id",
        "--sha256",
        "--env",
        "--manifest",
        "--name",
        "--plugin-id",
        "--view-id",
        "--permission",
        "--reason",
    ];
    let mut index = 1;
    while index < arguments.len() {
        let token = arguments[index].as_str();
        if token.starts_with('-') {
            index += if VALUE_FLAGS.contains(&token) { 2 } else { 1 };
            continue;
        }
        return Some((index, token));
    }
    None
}

/// 命令行入口的参数：只有第一个位置参数等于 `command` 时才返回它后面的参数。
fn cli_command_arguments(command: &str) -> Option<Vec<String>> {
    let arguments = env::args().collect::<Vec<_>>();
    let (index, token) = cli_subcommand(&arguments)?;
    if token != command {
        return None;
    }
    Some(arguments[index + 1..].to_vec())
}

fn trust_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("trust")
}

fn engineering_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("engineering")
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
    cli_command_arguments("auth")
}

fn run_auth_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("login") => {
            let mut authorization = api::oauth::begin_device_authorization(options)?;
            // 与界面授权同一套规则：确认页必须落在 --api 指定的 Dashboard 上。
            if let Some(message) =
                api::oauth::align_authorization_urls(&options.api_base(), &mut authorization)
            {
                println!("Warning: {message}");
            }
            println!("Open {}", authorization.verification_uri_complete);
            println!("Verification page: {}", authorization.verification_uri);
            println!("Authorization code: {}", authorization.user_code);
            let _ = app::system::open_url(&authorization.verification_uri_complete);
            let access = api::oauth::wait_for_device_authorization(options, &authorization)?;
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
                &options.api_base(),
                &options.state_path,
                VERSION,
                &options.enrollment_token,
            )?;
            let rotated = api::client::rotate_agent_credential(
                &client,
                &options.api_base(),
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
    cli_command_arguments("plugin")
}

fn skill_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("skill")
}

fn market_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("market")
}

fn instruction_pack_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("instruction-pack")
}

fn run_instruction_pack_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let value = match arguments {
        [action] if action == "list" => serde_json::to_value(crate::instruction_pack::list()?)?,
          [action, path] if action == "import-file" => serde_json::to_value(
              crate::instruction_pack::import_file(std::path::Path::new(path))?,
          )?,
          [action, path] if action == "import-package" => serde_json::to_value(
              crate::instruction_pack::import_package(crate::instruction_pack::InstructionPackImportInput {
                  package_path: std::path::PathBuf::from(path),
                  source: "local_package".to_string(),
              })?,
          )?,
        [action, id, version] if action == "test" => serde_json::to_value(
            crate::instruction_pack::test(id, version)?,
        )?,
        [action, id, version] if action == "confirm" => serde_json::to_value(
            crate::instruction_pack::confirm(id, version)?,
        )?,
        [action, id, version] if action == "publish-local" => serde_json::to_value(
            crate::instruction_pack::publish_local(id, version)?,
        )?,
        _ => {
              return Err("usage: himind-agent instruction-pack <list|import-file path|import-package path|test id version|confirm id version|publish-local id version>".into())
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// 市场命令行入口：搜索、盘点、计划、安装。
///
/// 与 MCP 的 `market.*` 共用同一份 [`app::market`] 实现，CLI 不引入第二套安装语义。
/// `--workspace` / `--global` 只对技能的计划与安装有意义，语义与 `skill` 子命令一致。
fn run_market_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let mut action = None;
    let mut input = serde_json::Map::new();
    let mut location = None;
    let mut global = false;
    let mut targets: Vec<String> = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments[index].as_str();
        let next = |offset: usize| -> Result<String, Box<dyn Error>> {
            arguments
                .get(index + offset)
                .cloned()
                .ok_or_else(|| format!("{value} 缺少参数值").into())
        };
        match value {
            "search" | "installed" | "plan" | "install" if action.is_none() => {
                action = Some(value.to_string());
                index += 1;
            }
            "--kind" => {
                input.insert("kind".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--id" => {
                input.insert("id".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--version" => {
                input.insert("version".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--source" => {
                input.insert("source".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--query" => {
                input.insert("query".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--category" => {
                input.insert("category".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--limit" => {
                input.insert(
                    "limit".to_string(),
                    serde_json::Value::from(next(1)?.parse::<u64>()?),
                );
                index += 2;
            }
            "--cursor" => {
                input.insert(
                    "cursor".to_string(),
                    serde_json::Value::from(next(1)?.parse::<u64>()?),
                );
                index += 2;
            }
            "--artifact-id" => {
                input.insert("artifact_id".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--sha256" => {
                input.insert("sha256".to_string(), serde_json::Value::from(next(1)?));
                index += 2;
            }
            "--client" => {
                targets.push(next(1)?.to_ascii_lowercase());
                index += 2;
            }
            "--dry-run" => {
                input.insert("dry_run".to_string(), serde_json::Value::Bool(true));
                index += 1;
            }
            "--workspace" => {
                let path = next(1)?;
                if global {
                    return Err("--workspace 与 --global 不能同时使用".into());
                }
                location = Some(crate::skill::target::canonical_workspace_root(
                    std::path::Path::new(&path),
                )?);
                index += 2;
            }
            "--global" => {
                if location.is_some() {
                    return Err("--workspace 与 --global 不能同时使用".into());
                }
                global = true;
                index += 1;
            }
            // 全局选项已经由 `Options::from_env` 解析过，这里只是把它们的取值也
            // 跳过，好让 `market search --kind skill --state <path>` 这种把全局
            // 选项写在子命令之后的自然写法不被当成未知参数拒绝。
            "--api" | "--state" | "--mode" | "--interval" | "--local-port" => {
                let _ = next(1)?;
                index += 2;
            }
            "--once" | "--local-app" | "--reenroll" => {
                index += 1;
            }
            other => {
                // 位置参数：`plan <kind> <id>` / `install <kind> <id>` 的自然写法。
                if input.get("kind").is_none() {
                    input.insert("kind".to_string(), serde_json::Value::from(other));
                } else if input.get("id").is_none() {
                    input.insert("id".to_string(), serde_json::Value::from(other));
                } else {
                    return Err(format!("无法识别的参数: {other}").into());
                }
                index += 1;
            }
        }
    }
    if let Some(root) = location {
        input.insert(
            "workspace_root".to_string(),
            serde_json::Value::from(root.to_string_lossy().to_string()),
        );
    }
    if !targets.is_empty() {
        input.insert(
            "target_clients".to_string(),
            serde_json::Value::Array(
                targets
                    .into_iter()
                    .map(serde_json::Value::from)
                    .collect::<Vec<_>>(),
            ),
        );
    }
    let input = serde_json::Value::Object(input);
    let agent_id = paired_agent_id_or_empty(options);
    let value = match action.as_deref() {
        Some("search") => app::market::search(options, &agent_id, &input)?,
        Some("installed") => app::market::installed(&input)?,
        Some("plan") => app::market::plan(options, &agent_id, &input)?,
        Some("install") => app::market::install(
            options,
            &agent_id,
            &input,
            crate::capability::types::InvocationSource::Cli,
        )?,
        _ => {
            return Err(
                "usage: himind-agent market <search|installed|plan|install> [--kind skill|plugin|workflow|instruction_pack] [--id <id>] [--version <version>] [--source <source>] [--query <text>] [--category <name>] [--limit <n>] [--cursor <n>] [--client <client-id>] [--workspace <project-root>|--global] [--dry-run]".into(),
            )
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// 市场命令在未授权时仍然可用：本地与 GitHub 扩展源不依赖工作台。
fn paired_agent_id_or_empty(options: &Options) -> String {
    match crate::api::client::load_agent_state(&options.state_path) {
        Ok(state) => {
            options.set_agent_credential(&state.credential);
            state.agent_id
        }
        Err(_) => String::new(),
    }
}

fn extension_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("extension")
}

fn workflow_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("workflow")
}

fn schedule_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("schedule")
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
            println!("{}", serde_json::to_string_pretty(&scheduler::delete(id)?)?);
        }
        [action] if action == "tick" => {
            let gateway = capability::service::CapabilityGateway::new(
                options.clone(),
                Arc::new(std::sync::Mutex::new(
                    store::types::LocalWorkerStatus::default(),
                )),
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler::run_due(
                    gateway,
                    scheduler::now_epoch()
                )?)?
            );
        }
        _ => {
            return Err("usage: himind-agent schedule <list|set json|delete id|tick>".into());
        }
    }
    Ok(())
}

fn credential_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("credential")
}

fn connector_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("connector")
}

fn approval_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("approval")
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
                &options.api_base(),
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
        [action, rest @ ..] if action == "projection-requeue" => {
            // 死信以前只能靠内部循环里的一个 401 片段自动重投，契约类故障没有恢复入口。
            let (mut all, mut drain, mut reason) = (false, false, None::<String>);
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--all" => all = true,
                    "--drain" => drain = true,
                    "--reason" => {
                        let value = rest.get(index + 1).ok_or("--reason 需要一个错误片段参数")?;
                        reason = Some(value.clone());
                        index += 1;
                    }
                    other => {
                        return Err(format!("unknown projection-requeue option: {other}").into())
                    }
                }
                index += 1;
            }
            if all && reason.is_some() {
                return Err("projection-requeue: --all 与 --reason 只能选一个".into());
            }
            let mut payload = if all {
                serde_json::to_value(agent_core_projection::requeue_dead_letter_projections(
                    None,
                )?)?
            } else if let Some(fragment) = reason.as_deref() {
                serde_json::to_value(agent_core_projection::requeue_dead_letter_projections(
                    Some(fragment),
                )?)?
            } else {
                // 不带范围时只做体检：先把当前死信按原因列清楚，再由调用方决定重投范围。
                let status = agent_core_projection::projection_sync_status(options)?;
                let groups = store::local_runs::LocalRunLedger::open_default()?
                    .dead_letter_projection_groups(5)?;
                json!({
                    "requeued": 0,
                    "dead_letter_before": status.dead_letter,
                    "dead_letter_after": status.dead_letter,
                    "pending_after": status.pending,
                    "remaining_reasons": groups,
                    "hint": "用 --all 重投全部死信，或用 --reason <片段> 只重投命中该片段的记录；加 --drain 可在重投后立即排空队列",
                })
            };
            if drain {
                let report = agent_core_projection::drain_pending_projections(options, 200)?;
                let status = agent_core_projection::projection_sync_status(options)?;
                let groups = store::local_runs::LocalRunLedger::open_default()?
                    .dead_letter_projection_groups(5)?;
                if let Some(object) = payload.as_object_mut() {
                    object.insert("drain".to_string(), serde_json::to_value(report)?);
                    // 顶层字段一律描述「这条命令结束后」的状态：加了 --drain 就别再让人去分辨
                    // 哪一层数字才是重投后的最终值。
                    object.insert("dead_letter_after".to_string(), json!(status.dead_letter));
                    object.insert("pending_after".to_string(), json!(status.pending));
                    object.insert(
                        "remaining_reasons".to_string(),
                        serde_json::to_value(groups)?,
                    );
                }
            }
            println!("{}", serde_json::to_string_pretty(&payload)?);
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
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "abandoned": count }))?
            );
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
                "usage: himind-agent workflow <author-save <dir>|author-test <id> <version>|author-confirm <id> <version>|validate <dir>|doctor <dir|id> [input-json|@file]|package <dir> [output.hmwf]|sign <dir> [output.hmwf]|install-archive <path> [--require-signature]|install <dir> [--require-signature]|remote-list|remote-install <id> [version]|list|run <dir|id> [input-json|@file]|resume <run-id> [input-json|@file] [feedback]|dispatch [limit]|runs|metrics|projection-status|projection-requeue [--all|--reason <fragment>] [--drain]|recover [--force]|show <run-id>|verify-run <run-id>|approve <run-id> <step-id>|reject <run-id> <step-id>|cancel <run-id>|enable <id>|disable <id>|rollback <id>|remove <id>>"
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
            // 产物必须能被本机 Agent 读回：先用读取侧同一套校验自验一次，
            // 否则私钥能签、公钥不受信时照样会产出一个装完就读取失败的坏包。
            app::system::verify_extension_artifact_signature(
                &staging.join("checksums.sha256"),
                metadata
                    .get("signature")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                key_id,
                "rsa-pss-sha256",
                true,
            )
            .map_err(|error| {
                format!(
                    "workflow signing key {key_id} 产出的签名无法被本机校验通过：{error}。\
                     请把对应公钥放入 HIMIND_TRUSTED_SIGNING_KEYS_DIR（文件名 {key_id}.pem），\
                     或改用本机已有受信公钥的 key id 重新打包。"
                )
            })?;
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

/// 把确认这一步也做成可脚本化动作：提审要求候选制品先被确认，
/// GUI 与 CLI 走同一实现，避免两套语义。
fn draft_confirm_value(
    kind: &str,
    id: &str,
    version: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    match kind {
        "plugin" => Ok(serde_json::to_value(crate::plugin_authoring::confirm(
            id, version,
        )?)?),
        "skill" => Ok(serde_json::to_value(crate::skill::authoring::confirm(
            id, version,
        )?)?),
        "workflow" => Ok(serde_json::to_value(
            crate::workflow::confirm_authoring_candidate(id, version)?,
        )?),
        _ => Err("扩展类型必须是 plugin、skill 或 workflow".into()),
    }
}

/// Local, scriptable view of the extension sources.
///
/// The development workspace is driven from the UI, but source management is
/// also needed from shells and verification scripts: listing and refreshing the
/// snapshot, adding or removing a source, planning an install and reading
/// provenance.  Every action returns the same JSON the UI consumes.
fn run_extension_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        // 提审链路：列出草稿与提交状态，便于验收与排障，不必依赖 GUI。
        [group, action] if group == "draft" && action == "list" => {
            let drafts = serde_json::json!({
                "plugins": crate::plugin_authoring::list()?,
                "skills": crate::skill::authoring::list()?,
                "workflows": crate::workflow::list_authoring_drafts()?,
            });
            println!("{}", serde_json::to_string_pretty(&drafts)?);
        }
        // submit 只对有候选制品且已测试通过的草稿生效；CLI 默认拒绝执行，
        // 必须显式加 --yes（与 GUI 的确认框同一条同意门）。
        [group, action, kind, id, version] if group == "draft" && action == "confirm" => {
            let value = draft_confirm_value(kind, id, version)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        [group, action, kind, id, version] | [group, action, kind, id, version, _]
            if group == "draft" && action == "submit" =>
        {
            if !arguments.iter().any(|value| value == "--yes")
                && env::var("HIMIND_AGENT_SUBMIT_CONFIRM").as_deref() != Ok("1")
            {
                return Err(
                    "提交候选制品需要显式确认：加 --yes 或设置 HIMIND_AGENT_SUBMIT_CONFIRM=1"
                        .into(),
                );
            }
            let state = api::client::load_agent_state(&options.state_path)
                .map_err(|error| format!("读取 HiMind 账号授权状态失败：{error}"))?;
            options.set_agent_credential(&state.credential);
            let value = match kind.as_str() {
                "plugin" => serde_json::to_value(crate::plugin_authoring::submit(
                    options,
                    &state.agent_id,
                    id,
                    version,
                )?)?,
                "skill" => serde_json::to_value(crate::skill::authoring::submit(
                    options,
                    &state.agent_id,
                    id,
                    version,
                )?)?,
                "workflow" => serde_json::to_value(crate::workflow::submit_authoring_candidate(
                    options,
                    &state.agent_id,
                    id,
                    version,
                )?)?,
                _ => return Err("扩展类型必须是 plugin、skill 或 workflow".into()),
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
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
        [source, action, unit_key, acquisition]
            if source == "source" && action == "acquisition" =>
        {
            let acquisition = match acquisition.as_str() {
                "local" => app::extension_source::ExtensionSourceAcquisition::Local,
                "remote" => app::extension_source::ExtensionSourceAcquisition::Remote,
                other => return Err(format!("取用侧必须是 local 或 remote，收到: {other}").into()),
            };
            let settings = app::extension_source::set_unit_acquisition(unit_key, acquisition)?;
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
        // 分发目标：项目级覆盖与分发单元级默认。`inherit` 表示清除覆盖。
        [group, action, verb] if group == "project" && action == "target" && verb == "list" => {
            let projects = crate::extension_projects::list()?
                .into_iter()
                .map(|project| {
                    let unit_targets = crate::extension_projects::unit_distribution_targets_for(
                        project.kind,
                        &project.extension_id,
                    );
                    serde_json::json!({
                        "project_id": project.id,
                        "kind": project.kind.as_str(),
                        "id": project.extension_id,
                        "name": project.name,
                        "unit_key": project.source_unit_key,
                        "targets": project
                            .distribution_targets
                            .iter()
                            .map(|target| target.as_str())
                            .collect::<Vec<_>>(),
                        "source": project.distribution_targets_source,
                        "declared_targets": project
                            .distribution_targets_declared
                            .iter()
                            .map(|target| target.as_str())
                            .collect::<Vec<_>>(),
                        "unit_targets": unit_targets
                            .iter()
                            .map(|target| target.as_str())
                            .collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "projects": projects,
                    "available_targets": ["workbench", "github"],
                    "default_targets": ["workbench"],
                }))?
            );
        }
        [group, action, verb, kind, id, targets]
            if group == "project" && action == "target" && verb == "set" =>
        {
            let kind = crate::extension_projects::ExtensionProjectKind::parse(kind)?;
            let targets = cli_distribution_targets(targets)?;
            let project =
                crate::extension_projects::set_distribution_targets(kind, id, targets.as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "project_id": project.id,
                    "kind": project.kind.as_str(),
                    "id": project.extension_id,
                    "targets": project
                        .distribution_targets
                        .iter()
                        .map(|target| target.as_str())
                        .collect::<Vec<_>>(),
                    "source": project.distribution_targets_source,
                }))?
            );
        }
        // GitHub 分发凭据：token 只经本机校验后加密落盘，不接受未验证的登录名。
        [group, action] if group == "github" && action == "status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::store::github_credentials::status()?)?
            );
        }
        [group, action, token] if group == "github" && action == "set" => {
            let identity = app::github_publisher::verify_token(token)?;
            let account =
                crate::store::github_credentials::set_account(&identity.login, token, "", &[])?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "account": account,
                    "identity": { "login": identity.login, "id": identity.id },
                }))?
            );
        }
        [group, action, token, repositories] if group == "github" && action == "set" => {
            let identity = app::github_publisher::verify_token(token)?;
            let repositories = repositories
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            let account = crate::store::github_credentials::set_account(
                &identity.login,
                token,
                "",
                &repositories,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "account": account,
                    "identity": { "login": identity.login, "id": identity.id },
                }))?
            );
        }
        [group, action, word] if group == "github" && action == "remove" && word == "--yes" => {
            let removed = crate::store::github_credentials::remove()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "removed": removed,
                    "account": crate::store::github_credentials::status()?,
                }))?
            );
        }
        // GitHub App 授权：设备流需要一个已注册的 App client_id。
        [group, action] if group == "github" && action == "app-start" => {
            let client_id = app::github_app::configured_client_id();
            let authorization = app::github_app::start_device_flow(&client_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "authorization": authorization,
                    "client_id": client_id,
                    "next_steps": [
                        "在浏览器打开 verification_uri 并输入 user_code",
                        "随后用 extension github app-poll <device_code> 完成授权"
                    ],
                }))?
            );
        }
        [group, action, device_code] if group == "github" && action == "app-poll" => {
            let client_id = app::github_app::configured_client_id();
            let outcome = app::github_app::poll_device_flow(&client_id, device_code)?;
            match outcome {
                app::github_app::DevicePollOutcome::Authorized(token) => {
                    let record = crate::store::github_credentials::GithubAppRecord {
                        login: String::new(),
                        client_id: client_id.clone(),
                        installation_id: String::new(),
                        installation_account: String::new(),
                        user_token: token.access_token.clone(),
                        refresh_token: token.refresh_token.clone(),
                        user_token_expires_at: (app::github_app::now_epoch() + token.expires_in)
                            .to_string(),
                    };
                    crate::store::github_credentials::save_app_state(&record)?;
                    // 授权成功后立刻列出可用安装，用户只要再选一次就能发布。
                    let installations = app::github_app::list_installations(&record.user_token)?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "state": "authorized",
                            "installations": installations,
                            "next_steps": [
                                "用 extension github app-select <installation_id> 绑定发布用的安装"
                            ],
                        }))?
                    );
                }
                other => println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "state": format!("{other:?}"),
                    }))?
                ),
            }
        }
        [group, action] if group == "github" && action == "app-installations" => {
            let state = crate::store::github_credentials::app_state()?
                .ok_or("GitHub App 尚未授权，请先执行 extension github app-start")?;
            let installations = app::github_app::list_installations(&state.user_token)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "installations": installations,
                }))?
            );
        }
        [group, action, installation_id] if group == "github" && action == "app-select" => {
            let state = crate::store::github_credentials::app_state()?
                .ok_or("GitHub App 尚未授权，请先执行 extension github app-start")?;
            let installations = app::github_app::list_installations(&state.user_token)?;
            let selected = installations
                .into_iter()
                .find(|item| item.id == installation_id.trim())
                .ok_or_else(|| {
                    format!(
                        "未找到安装 {installation_id}，请先执行 extension github app-installations"
                    )
                })?;
            app::github_app::select_installation(&selected)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "state": "ready",
                    "installation": selected,
                    "account": crate::store::github_credentials::status()?,
                }))?
            );
        }
        // 导入 App 私钥（PKCS#1 / PKCS#8 PEM）：有私钥才走 App 身份签安装令牌。
        [group, action, path] if group == "github" && action == "app-key" => {
            let pem = std::fs::read_to_string(path)
                .map_err(|error| format!("读取 App 私钥失败：{error}"))?;
            crate::store::github_credentials::save_app_private_key(&pem)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "state": "ready",
                    "private_key_configured": true,
                    "account": crate::store::github_credentials::status()?,
                    "next_steps": [
                        "extension github verify owner/repo 确认写权限",
                        "extension github status 查看当前凭据形态"
                    ]
                }))?
            );
        }
        // 只读自检：确认当前凭据（PAT 或 App 安装令牌）对该仓库真的可写，
        // 发布前先跑一次可以避免把失败留到建 tag 的阶段。
        [group, action, repository] if group == "github" && action == "verify" => {
            let token = crate::store::github_credentials::resolve_token()?
                .ok_or("尚未授权 GitHub 账号，请先执行 extension github set 或 github app-start")?;
            let info = app::github_publisher::repository_info(&token, repository)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "repository": info.full_name,
                    "default_branch": info.default_branch,
                    "private": info.private,
                    "can_push": info.can_push,
                    "account": crate::store::github_credentials::status()?,
                }))?
            );
            if !info.can_push {
                return Err(format!(
                    "凭据对 {} 没有写权限：App 安装需要勾选该仓库，且 Contents 权限为 Read and write",
                    info.full_name
                )
                .into());
            }
        }
        // 分发：preview 无副作用，publish 需要 --yes 与可用的 GitHub 凭据。
        [group, action, kind, id, version] if group == "distribution" && action == "preview" => {
            let kind = crate::extension_projects::ExtensionProjectKind::parse(kind)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&app::distribution_publish::preview(
                    kind, id, version,
                )?)?
            );
        }
        [group, action, kind, id, version] | [group, action, kind, id, version, _]
            if group == "distribution" && action == "publish" =>
        {
            if !arguments.iter().any(|value| value == "--yes")
                && env::var("HIMIND_AGENT_SUBMIT_CONFIRM").as_deref() != Ok("1")
            {
                return Err(
                    "发布扩展需要显式确认：加 --yes 或设置 HIMIND_AGENT_SUBMIT_CONFIRM=1".into(),
                );
            }
            let kind = crate::extension_projects::ExtensionProjectKind::parse(kind)?;
            let state = api::client::load_agent_state(&options.state_path)
                .map_err(|error| format!("读取 HiMind 账号授权状态失败：{error}"))?;
            options.set_agent_credential(&state.credential);
            let report =
                app::distribution_publish::publish(options, &state.agent_id, kind, id, version)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            // 部分完成或失败要以非零退出码返回，脚本与 CI 才能据此停下来人工处理。
            if report.get("status").and_then(serde_json::Value::as_str) != Some("released") {
                let failed = report
                    .get("outcomes")
                    .and_then(serde_json::Value::as_array)
                    .map(|outcomes| {
                        outcomes
                            .iter()
                            .filter(|outcome| {
                                outcome.get("status").and_then(serde_json::Value::as_str)
                                    == Some("failed")
                            })
                            .map(|outcome| {
                                format!(
                                    "{}: {}",
                                    outcome
                                        .get("target")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("unknown"),
                                    outcome
                                        .get("error")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("发布失败")
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return Err(format!("发布未全部完成：{}", failed.join("；")).into());
            }
        }
        [group, action] if group == "distribution" && action == "state" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "items": app::distribution_state::load()?.all(),
                }))?
            );
        }
        [group, action, kind, id] if group == "distribution" && action == "state" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "items": app::distribution_state::load()?.for_asset(kind, id),
                }))?
            );
        }
        // 从 GitHub Release 安装：plan 只读，install 需要 --yes（dry-run 除外）。
        [group, action, repository, tag, id, version]
            if group == "distribution" && action == "plan" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::release_install::plan(
                    repository, tag, id, version,
                )?)?
            );
        }
        [group, action, repository, tag, id, version]
            if group == "distribution" && action == "install" =>
        {
            if !arguments.iter().any(|value| value == "--yes")
                && env::var("HIMIND_AGENT_SUBMIT_CONFIRM").as_deref() != Ok("1")
            {
                return Err(
                    "从 Release 安装扩展需要显式确认：加 --yes（或先加 --dry-run 只校验）".into(),
                );
            }
            let plan = app::release_install::plan(repository, tag, id, version)?;
            let report = app::release_install::install(&plan, false)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "plan": plan,
                    "report": report,
                }))?
            );
            if report.state != "ready" {
                return Err(format!("安装未完成：{}", report.errors.join("；")).into());
            }
        }
        [group, action, repository, tag, id, version, word]
            if group == "distribution"
                && action == "install"
                && (word == "--dry-run" || word == "--yes") =>
        {
            let plan = app::release_install::plan(repository, tag, id, version)?;
            let report = app::release_install::install(&plan, word == "--dry-run")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "plan": plan,
                    "report": report,
                }))?
            );
            // dry-run 也会真实下载与校验，失败必须让脚本看见。
            if report.state != "ready" {
                return Err(format!(
                    "{}未通过：{}",
                    if word == "--dry-run" {
                        "安装预检"
                    } else {
                        "安装"
                    },
                    report.errors.join("；")
                )
                .into());
            }
        }
        [group, action, verb, unit_key, targets]
            if group == "project" && action == "target" && verb == "unit" =>
        {
            let targets = cli_distribution_targets(targets)?;
            let settings =
                app::extension_source::set_unit_distribution_targets(unit_key, targets.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        [source, action] if source == "source" && action == "update" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::extension_source::reconcile_auto_updates()?)?
            );
        }
        _ => {
            return Err("usage: himind-agent extension <source ...|draft list|draft submit plugin|skill|workflow id version --yes|project target list|project target set kind id workbench,github|inherit|project target unit <unit-key> workbench,github|inherit|github status|github set <token> [owner/repo,...]|github remove --yes|github verify owner/repo|github app-start|github app-poll <device-code>|github app-installations|github app-select <installation-id>|github app-key <pem-path>|distribution preview kind id version|distribution publish kind id version --yes|distribution state [kind id]|distribution plan repository tag id version|distribution install repository tag id version [--dry-run|--yes]>".into());
        }
    }
    Ok(())
}

/// 解析 CLI 传入的分发目标：`workbench,github` 形式，或 `inherit` 表示继承。
fn cli_distribution_targets(
    value: &str,
) -> Result<Option<Vec<crate::extension_contracts::DistributionTarget>>, Box<dyn std::error::Error>>
{
    use crate::extension_contracts::DistributionTarget;
    if value.trim() == "inherit" {
        return Ok(None);
    }
    let mut targets = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        targets.push(match item {
            "workbench" => DistributionTarget::Workbench,
            "github" => DistributionTarget::Github,
            other => {
                return Err(
                    format!("分发目标必须是 workbench、github 或 inherit，收到: {other}").into(),
                )
            }
        });
    }
    if targets.is_empty() {
        return Err("分发目标不能为空，至少需要一个 workbench 或 github".into());
    }
    Ok(Some(targets))
}

fn runtime_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("runtime")
}

fn mcp_cli_arguments() -> Option<Vec<String>> {
    cli_command_arguments("mcp")
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
        // 目录（server.json）三件事：看现状、联网刷新、按条目安装。界面走的是同样
        // 三个函数，这条 CLI 只是让链路可以无头验证（内网自建源、CI、排障）。
        Some("catalog") if arguments.len() == 1 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_catalog::view(&options.state_path))?
            );
        }
        Some("catalog-refresh") if arguments.len() == 1 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_catalog::refresh(&options.state_path))?
            );
        }
        Some("catalog-install") if arguments.len() >= 3 => {
            let mut request = app::mcp_catalog::InstallRequest {
                source_id: arguments[1].clone(),
                entry_id: arguments[2].clone(),
                ..Default::default()
            };
            let mut index = 3;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--name" if index + 1 < arguments.len() => {
                        request.server_name = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--label" if index + 1 < arguments.len() => {
                        request.display_name = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--set" if index + 1 < arguments.len() => {
                        let pair = &arguments[index + 1];
                        let (key, value) = pair.split_once('=').ok_or_else(|| {
                            format!("--set 需要 key=value 形式，收到: {pair}")
                        })?;
                        request.values.insert(key.to_string(), value.to_string());
                        index += 2;
                    }
                    "--acknowledge" => {
                        request.acknowledge = true;
                        index += 1;
                    }
                    other => {
                        return Err(format!(
                            "usage: himind-agent mcp catalog-install <source-id> <entry-id> [--name name] [--label label] [--set key=value]... [--acknowledge]，收到未知参数: {other}"
                        )
                        .into())
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&app::mcp_catalog::install(
                    &options.state_path,
                    &request
                )?)?
            );
        }
        _ => {
            return Err("usage: himind-agent mcp <list|targets|inspect server-id|plan target-id|apply target-id [--reset-invalid]|apply-all [--include-undetected] [--reset-invalid]|remove target-id|remove-all [--include-undetected]|test server-id|catalog|catalog-refresh|catalog-install source-id entry-id [--name name] [--label label] [--set key=value]... [--acknowledge]>".into())
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
            let mut environment = std::collections::BTreeMap::new();
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
                    "--env" if index + 1 < arguments.len() => {
                        let entry = arguments[index + 1].trim();
                        let (key, value) = entry
                            .split_once('=')
                            .ok_or("--env expects KEY=VALUE")?;
                        let key = key.trim();
                        if key.is_empty() {
                            return Err("--env expects KEY=VALUE".into());
                        }
                        environment.insert(key.to_string(), value.to_string());
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
                    env: environment,
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
                "usage: himind-agent runtime acp-profile <list|set provider executable [args-json] [--name name] [--version version] [--permission deny|allow_once|prompt] [--env KEY=VALUE] [--disabled]|enable provider|disable provider|remove provider>".into(),
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
    /// Live Dashboard/API base of the **active workbench connection**.
    ///
    /// Deliberately not a process constant: per ADR 0008 the Agent can be
    /// pointed at another workbench while it keeps running, so the value is
    /// shared mutable state and every call site reads it through
    /// [`Options::api_base`].
    api_base: Arc<RwLock<String>>,
    state_path: PathBuf,
    /// Control-plane binding, shared by every clone of this process. The
    /// settings switch (AI 工作台) flips it in place, so workers, projection
    /// and capability visibility follow within one poll interval instead of
    /// requiring an Agent restart.
    workbench_mode: Arc<AtomicU8>,
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
    /// The Dashboard/API base of the active workbench connection.
    pub(crate) fn api_base(&self) -> String {
        self.api_base
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    /// Point this process at another workbench address.
    ///
    /// Never call this on its own: the address and the workbench identity must
    /// move together, so switching is done by
    /// [`crate::store::workbenches::switch`] plus
    /// [`Options::adopt_connection`].
    pub(crate) fn set_api_base(&self, api_base: &str) {
        if let Ok(mut current) = self.api_base.write() {
            *current = api_base.trim().trim_end_matches('/').to_string();
        }
    }

    /// Follow a completed workbench switch: new address, and a worker restart
    /// so the next connection attempt uses the identity that was just
    /// materialised on disk.
    pub(crate) fn adopt_connection(&self) {
        if let Ok(mut credential) = self.agent_credential.write() {
            credential.clear();
        }
        self.identity_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Returns the mode used by every service in this process.
    pub(crate) fn mode(&self) -> app::runtime_mode::AgentMode {
        app::runtime_mode::AgentMode::from_code(
            self.workbench_mode
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Applies a control-plane mode change to the live service graph.
    pub(crate) fn set_mode(&self, mode: app::runtime_mode::AgentMode) {
        self.workbench_mode
            .store(mode.as_code(), std::sync::atomic::Ordering::Relaxed);
    }

    /// 持久化值已经与运行时同步，保留该入口只为兼容既有调用点。
    pub(crate) fn pending_mode(&self) -> app::runtime_mode::AgentMode {
        self.mode()
    }

    pub(crate) fn plugin_view_launch(&self) -> Option<PluginViewLaunch> {
        parse_plugin_view_launch(&env::args().collect::<Vec<_>>())
    }

    pub(crate) fn protocol_open_target(&self) -> Option<AgentOpenTarget> {
        parse_protocol_open(&env::args().collect::<Vec<_>>())
    }

    fn from_env() -> Self {
        // An explicit address is a *selector*, not the answer: it picks which
        // workbench connection this process starts on (ADR 0008). Without one
        // the store decides, and the shipped default only seeds a fresh
        // install.
        let mut explicit_api_base = env::var("DASHBOARD_API_BASE")
            .or_else(|_| env::var("HIMIND_DEVELOPMENT_AGENT_API_BASE"))
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
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
                    explicit_api_base = Some(args[i + 1].trim().trim_end_matches('/').to_string());
                    i += 1;
                }
                "--state" if i + 1 < args.len() => {
                    let requested = PathBuf::from(&args[i + 1]);
                    // MCP registrations outlive the build that wrote them, and
                    // an old one still names the production state file. Taking
                    // it at face value would point this profile at the
                    // installed Agent's identity, so the profile decides.
                    if store::paths::explicit_state_crosses_profiles(&requested) {
                        eprintln!(
                            "HiMind Agent: 忽略 --state {}（属于 production 数据根），改用 {}。",
                            requested.display(),
                            state_path.display()
                        );
                    } else {
                        state_path = requested;
                    }
                    i += 1;
                }
                // Already applied by `apply_startup_profile`; consumed here so
                // the value can never be mistaken for a subcommand.
                "--profile" if i + 1 < args.len() => {
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
        // ADR 0008: the workbench address belongs to a stored connection, not
        // to the build. Failures here are not fatal — the Agent still starts
        // against the address it was told to use.
        let api_base = match explicit_api_base {
            Some(value) => store::workbenches::activate_for(&state_path, &value)
                .map(|connection| connection.api_base)
                .unwrap_or(value),
            None => store::workbenches::sync_active(&state_path, SHIPPED_DASHBOARD_API_BASE)
                .ok()
                .and_then(|store| {
                    store
                        .active_connection()
                        .map(|connection| connection.api_base.clone())
                })
                .unwrap_or_else(|| SHIPPED_DASHBOARD_API_BASE.to_string()),
        };
        Self {
            api_base: api_base_cell(api_base),
            state_path,
            workbench_mode: Arc::new(AtomicU8::new(effective_mode.as_code())),
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

/// The development Agent is expected to serve on 18082 (local Dashboard/API
/// typically sits on 18083).  Anything that is not the installed Agent follows
/// that topology by default, so a plain `cargo run -- --local-app` or a
/// `target/release` build cannot collide with the installed Agent on 18181.
/// The decision follows the resolved profile, not `cfg!(debug_assertions)`:
/// a release build that is not an installation used to take 18181 and fight the
/// installed Agent for the port.
fn default_local_port() -> u16 {
    const DEVELOPMENT_PORT: u16 = 18082;
    const PRODUCTION_PORT: u16 = 18181;
    // Only the installed Agent owns the shipped port. A debug build never
    // takes it, even when it was told to use the production profile.
    let production =
        !cfg!(debug_assertions) && store::paths::profile_name() == store::paths::PRODUCTION_PROFILE;
    env::var("HIMIND_AGENT_LOCAL_PORT")
        .or_else(|_| env::var("HIMIND_DEVELOPMENT_AGENT_PORT"))
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(if production {
            PRODUCTION_PORT
        } else {
            DEVELOPMENT_PORT
        })
}

/// Shared cell for the live API base. Exists so construction sites (including
/// test fixtures) do not have to spell out the lock type.
pub(crate) fn api_base_cell(api_base: impl Into<String>) -> Arc<RwLock<String>> {
    Arc::new(RwLock::new(api_base.into()))
}

/// The address a fresh install is seeded with.
///
/// Nothing infers a workbench from `cfg!(debug_assertions)` or from the
/// profile name any more (ADR 0008): a build flag is not something the user can
/// see or control. Local development passes `--api` explicitly, and every
/// install can add, enroll and switch workbenches from the UI afterwards.
const SHIPPED_DASHBOARD_API_BASE: &str = "http://localhost:8080";

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
        cli_subcommand, default_local_port, parse_plugin_view_launch, parse_protocol_open,
        should_run_acp, should_run_mcp, workflow_dispatch_limit, AgentOpenTarget, PluginViewLaunch,
        SHIPPED_DASHBOARD_API_BASE,
    };
    use crate::api::types::Task;
    use serde_json::json;
    use std::env;

    fn cli_args(values: &[&str]) -> Vec<String> {
        std::iter::once("himind-agent".to_string())
            .chain(values.iter().map(|value| value.to_string()))
            .collect()
    }

    #[test]
    fn cli_subcommand_ignores_flag_values_that_look_like_commands() {
        assert_eq!(
            cli_subcommand(&cli_args(&["market", "search"])).unwrap().1,
            "market"
        );
        // `--kind skill` 的取值是 `skill`，但它不是子命令：
        // 老实现按字面量找 `skill`，市场命令会被误判成技能命令。
        assert_eq!(
            cli_subcommand(&cli_args(&["market", "search", "--kind", "skill"]))
                .unwrap()
                .1,
            "market"
        );
        // 子命令前面的全局选项（含取值）要先跳过。
        assert_eq!(
            cli_subcommand(&cli_args(&[
                "--state",
                "C:/tmp/agent-state.json",
                "skill",
                "plan",
                "demo"
            ]))
            .unwrap()
            .1,
            "skill"
        );
        // `skill market` 是技能命令，不是市场命令。
        assert_eq!(
            cli_subcommand(&cli_args(&["skill", "market"])).unwrap().1,
            "skill"
        );
    }

    #[test]
    fn local_port_defaults_follow_the_build_but_the_workbench_does_not() {
        let overridden = env::var("HIMIND_AGENT_LOCAL_PORT").is_ok()
            || env::var("HIMIND_DEVELOPMENT_AGENT_PORT").is_ok();
        if !overridden {
            assert_eq!(
                default_local_port(),
                if cfg!(debug_assertions) { 18082 } else { 18181 }
            );
        }
        // ADR 0008: the workbench address is a stored connection, and the
        // shipped default is the only value the binary contributes. Local
        // development selects `http://127.0.0.1:18083` via `--api` instead.
        assert_eq!(SHIPPED_DASHBOARD_API_BASE, "http://localhost:8080");
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
    fn explicit_task_capability_accepts_prefixed_and_plain_ids() {
        let mut task: Task = serde_json::from_value(json!({
            "id": "task-1",
            "type": "agent_run"
        }))
        .unwrap();
        assert_eq!(super::explicit_task_capability(&task), None);
        task.capability = "capability:exhibit.workspace.checkout".to_string();
        assert_eq!(
            super::explicit_task_capability(&task),
            Some("exhibit.workspace.checkout")
        );
        task.capability = "remote.connect".to_string();
        assert_eq!(
            super::explicit_task_capability(&task),
            Some("remote.connect")
        );
    }

    #[test]
    fn legacy_task_capability_mapping_covers_migrated_queue_tasks() {
        let expected = [
            ("scan_projects", "scan.projects"),
            ("sync_exhibits", "inner_admin.sync_exhibits"),
            ("upload_code", "upload.code"),
            ("upload_placeholder", "upload.placeholder"),
            ("smb_upload", "storage.smb.upload"),
            (
                "exhibit_repository_import_local",
                "exhibit.repository.import_local",
            ),
            ("project_repository_create", "project.repository.create"),
            (
                "project_repository_exhibits_access_ensure",
                "project.repository.exhibits_access.ensure",
            ),
            (
                "exhibit_repository_path_create",
                "exhibit.repository_path.create",
            ),
            ("exhibit_workspace_checkout", "exhibit.workspace.checkout"),
            ("exhibit_repository_clone", "exhibit.repository.clone"),
            ("project_acl_preview", "project.repository.acl.preview"),
            ("project_acl_apply", "project.repository.acl.apply"),
            ("project_acl_reconcile", "project.repository.acl.reconcile"),
        ];
        for (task_type, capability_id) in expected {
            assert_eq!(
                super::legacy_task_capability(task_type),
                Some(capability_id)
            );
        }
        assert_eq!(
            super::legacy_task_capability("upload_code"),
            Some("upload.code")
        );
    }

    #[test]
    fn central_svn_tasks_are_rejected_by_the_desktop_executor() {
        for task_type in [
            "svn_user_provision",
            "project_repository_create",
            "exhibit_repository_initialize",
            "project_acl_apply",
            "project_repository_archive",
            "project_repository_prune",
        ] {
            let task: Task = serde_json::from_value(json!({
                "id": "task-central",
                "type": task_type,
                "capability": ""
            }))
            .unwrap();
            assert!(super::is_central_svn_management_task(&task), "{task_type}");
        }
        let personal: Task = serde_json::from_value(json!({
            "id": "task-personal",
            "type": "exhibit_workspace_checkout"
        }))
        .unwrap();
        assert!(!super::is_central_svn_management_task(&personal));
        let personal_template: Task = serde_json::from_value(json!({
            "id": "task-personal-template",
            "type": "exhibit_repository_initialize_template"
        }))
        .unwrap();
        assert!(!super::is_central_svn_management_task(&personal_template));
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
        assert_eq!(parse_protocol_open(&accepted), Some(AgentOpenTarget::Main));

        let open_ai = vec![
            "agent.exe".to_string(),
            "--protocol-url".to_string(),
            "himind-agent://open?open=ai".to_string(),
        ];
        assert_eq!(
            parse_protocol_open(&open_ai),
            Some(AgentOpenTarget::SettingsAi)
        );

        for value in [
            "himind-agent://open?command=exec",
            "himind-agent://open?open=unknown",
            "himind-agent://open?open=",
            "himind-agent://open?open=ai&command=exec",
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
            assert_eq!(parse_protocol_open(&rejected), None, "accepted {value}");
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

/// Execute a Dashboard task through the same Capability Gateway used by MCP,
/// Tauri and native Agent Runs. Legacy task types are translated at this
/// boundary so their wire format stays compatible while governance converges.
fn invoke_dashboard_capability(
    options: &Options,
    agent_id: &str,
    task: &Task,
    capability_id: &str,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    invoke_dashboard_capability_inner(options, agent_id, task, capability_id, input, None)
}

fn invoke_dashboard_capability_with_execution(
    options: &Options,
    agent_id: &str,
    task: &Task,
    capability_id: &str,
    input: Value,
    execution: &mut CapabilityExecutionContext<'_>,
) -> Result<Value, Box<dyn Error>> {
    invoke_dashboard_capability_inner(
        options,
        agent_id,
        task,
        capability_id,
        input,
        Some(execution),
    )
}

fn invoke_dashboard_capability_inner(
    options: &Options,
    agent_id: &str,
    task: &Task,
    capability_id: &str,
    input: Value,
    execution: Option<&mut CapabilityExecutionContext<'_>>,
) -> Result<Value, Box<dyn Error>> {
    let workspace_ref = input
        .get("workspace_root")
        .or_else(|| input.get("target_path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let principal = if task.created_by_user_id.trim().is_empty() {
        format!("dashboard-worker:{agent_id}")
    } else {
        format!("dashboard-user:{}", task.created_by_user_id.trim())
    };
    let context = InvocationContext::new(InvocationSource::DashboardWorker, principal)
        .with_device_id(agent_id)
        .with_workspace_ref(workspace_ref)
        .with_business_context(json!({
            "task_id": task.id,
            "task_type": task.task_type,
            "source": task.source,
            "execution_role": task.execution_role,
            "capability": task.capability,
            "dedupe_key": task.dedupe_key,
            "execution_id": task.execution_id,
            "lease_id": task.lease_id,
            "prerequisite_task_id": input
                .get("prerequisite_task_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        }));
    let gateway = CapabilityGateway::new(
        options.clone(),
        Arc::new(Mutex::new(store::types::LocalWorkerStatus::default())),
    );
    match execution {
        Some(execution) => {
            gateway.invoke_with_execution_context(&context, capability_id, input, execution)
        }
        None => gateway.invoke(&context, capability_id, input),
    }
}

fn task_execution_context<'a>(
    client: &'a Client,
    options: &'a Options,
    agent_id: &'a str,
    task: &'a Task,
    capability_id: &str,
    input: &Value,
) -> CapabilityExecutionContext<'a> {
    let mut cancel_guard = TaskCancelGuard::new();
    let workspace_scope = input
        .get("workspace_root")
        .or_else(|| input.get("target_path"))
        .or_else(|| input.get("source_path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    CapabilityExecutionContext::new(
        task.id.clone(),
        capability_id.to_string(),
        workspace_scope,
        move || cancel_guard.check(client, options, agent_id, &task.id),
        move |progress, detail| {
            report_task(
                client, options, agent_id, &task.id, "running", progress, detail, None, None,
            )
        },
    )
}

fn explicit_task_capability(task: &Task) -> Option<&str> {
    let capability = task.capability.trim();
    let capability = capability.strip_prefix("capability:").unwrap_or(capability);
    (!capability.trim().is_empty()).then_some(capability)
}

/// Stable translation for Dashboard's legacy task queue. The queue keeps its
/// old `type` values for compatibility, while migrated tasks use the
/// Capability ID as the policy and execution owner.
fn legacy_task_capability(task_type: &str) -> Option<&'static str> {
    match task_type {
        "scan_projects" => Some("scan.projects"),
        "sync_exhibits" => Some("inner_admin.sync_exhibits"),
        "upload_code" => Some("upload.code"),
        "upload_placeholder" => Some("upload.placeholder"),
        "smb_upload" => Some("storage.smb.upload"),
        "exhibit_repository_import_local" => Some("exhibit.repository.import_local"),
        "project_repository_create" => Some("project.repository.create"),
        "project_repository_exhibits_access_ensure" => {
            Some("project.repository.exhibits_access.ensure")
        }
        "exhibit_repository_path_create" => Some("exhibit.repository_path.create"),
        "exhibit_workspace_checkout" => Some("exhibit.workspace.checkout"),
        "exhibit_repository_clone" => Some("exhibit.repository.clone"),
        "project_acl_preview" => Some("project.repository.acl.preview"),
        "project_acl_apply" => Some("project.repository.acl.apply"),
        "project_acl_reconcile" => Some("project.repository.acl.reconcile"),
        _ => None,
    }
}

/// Central SVN administration is an Edge Worker-only boundary. Keep this
/// guard in the desktop executor as a final defense for stale or forged queue
/// entries; Dashboard task routing remains the primary enforcement point.
fn is_central_svn_management_task(task: &Task) -> bool {
    const CENTRAL_TASK_TYPES: &[&str] = &[
        "svn_user_provision",
        "project_repository_create",
        "project_repository_exhibits_access_ensure",
        "exhibit_repository_path_create",
        "exhibit_repository_initialize",
        "exhibit_repository_clone",
        "project_acl_preview",
        "project_acl_apply",
        "project_acl_reconcile",
        "project_repository_archive",
        "exhibit_repository_restore",
        "project_repository_prune",
        "project_repository_archive_drill",
    ];
    if CENTRAL_TASK_TYPES.contains(&task.task_type.as_str()) {
        return true;
    }
    matches!(
        explicit_task_capability(task),
        Some(
            "project.repository.create"
                | "project.repository.exhibits_access.ensure"
                | "exhibit.repository_path.create"
                | "exhibit.repository.initialize"
                | "exhibit.repository.clone"
                | "project.repository.acl.preview"
                | "project.repository.acl.apply"
                | "project.repository.acl.reconcile"
                | "project.repository.archive"
                | "exhibit.repository.restore"
                | "project.repository.prune"
                | "project.repository.archive_drill"
                | "svn.user.provision"
        )
    )
}

fn validate_released_template_task(
    task: &Task,
    request: &InitializeExhibitRepositoryRequest,
) -> Result<(), Box<dyn Error>> {
    if task.task_type != "exhibit_repository_initialize_template"
        || task.source.trim() != "dashboard"
        || task.created_by_user_id.trim().is_empty()
    {
        return Err("展项模板任务必须来自已登录用户创建的 Dashboard 任务".into());
    }
    if request.svn_username.trim().is_empty() {
        return Err("展项模板任务缺少本机 SVN 账号，请先在 Agent 中配置 SVN 连接".into());
    }
    if request.prerequisite_task_id.trim().is_empty() {
        return Err("展项模板任务缺少 Edge 前置任务凭据".into());
    }
    Ok(())
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
                        &renew_options.api_base(),
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

    if is_central_svn_management_task(&task) {
        let detail = "集中 SVN 管理任务必须由 himind-edge-worker 执行";
        eprintln!(
            "rejecting desktop execution of central SVN task {} ({})",
            task.id, task.task_type
        );
        report_task(
            client,
            options,
            agent_id,
            &task.id,
            "failed",
            100,
            detail,
            None,
            Some(detail.to_string()),
        )?;
        return Ok(());
    }

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
    let result = if let Some(capability_id) = explicit_task_capability(&task) {
        report_task(
            client,
            options,
            agent_id,
            &task.id,
            "running",
            20,
            &format!("通过能力网关执行 {}", capability_id),
            None,
            None,
        )?;
        let input = task.payload.clone().unwrap_or_else(|| json!({}));
        let mut execution =
            task_execution_context(client, options, agent_id, &task, capability_id, &input);
        invoke_dashboard_capability_with_execution(
            options,
            agent_id,
            &task,
            capability_id,
            input,
            &mut execution,
        )
    } else {
        match task.task_type.as_str() {
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
                let input = task.payload.clone().unwrap_or_else(|| json!({}));
                let mut execution = task_execution_context(
                    client,
                    options,
                    agent_id,
                    &task,
                    "inner_admin.sync_exhibits",
                    &input,
                );
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "inner_admin.sync_exhibits",
                    input,
                    &mut execution,
                )
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
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("scan_projects capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
            }
            "upload_code" => {
                let input = task.payload.clone().unwrap_or_else(|| json!({}));
                let mut execution =
                    task_execution_context(client, options, agent_id, &task, "upload.code", &input);
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "upload.code",
                    input,
                    &mut execution,
                )
            }
            "backup_run" => {
                execute_backup_run(client, options, agent_id, &task, task.payload.as_ref())
            }
            "upload_placeholder" => {
                let input = task.payload.clone().unwrap_or_else(|| json!({}));
                let mut execution = task_execution_context(
                    client,
                    options,
                    agent_id,
                    &task,
                    "upload.placeholder",
                    &input,
                );
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "upload.placeholder",
                    input,
                    &mut execution,
                )
            }
            "smb_upload" => {
                let input = task.payload.clone().unwrap_or_else(|| json!({}));
                let mut execution = task_execution_context(
                    client,
                    options,
                    agent_id,
                    &task,
                    "storage.smb.upload",
                    &input,
                );
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "storage.smb.upload",
                    input,
                    &mut execution,
                )
            }
            "project_repository_create" => {
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "running",
                    40,
                    "Edge Worker 正在连接内网 SvnAdmin 并创建项目仓库",
                    None,
                    None,
                )?;
                let request = serde_json::from_value::<CreateRepositoryRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                let project_id = request.project_id.clone();
                let repository_access = request.repository_access.clone();
                let repository = invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    "project.repository.create",
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "running",
                    70,
                    "项目仓库已创建，正在配置展项目录访问权限",
                    None,
                    None,
                )?;
                let access = invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("project repository access capability mapping"),
                    json!({
                        "project_id": project_id,
                        "repository_access": repository_access,
                    }),
                )?;
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
                let _request = serde_json::from_value::<EnsureProjectExhibitsAccessRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    "project.repository.exhibits_access.ensure",
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
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
                let _request = serde_json::from_value::<CreateExhibitRepositoryPathRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("exhibit repository path capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
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
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    "exhibit.repository_path.create",
                    json!({
                        "project_id": request.project_id,
                        "exhibit_id": request.exhibit_id,
                    }),
                )?;
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
                let mut execution_context = CapabilityExecutionContext::new(
                    task.id.clone(),
                    "exhibit.repository.initialize_template",
                    request.exhibit_id.clone(),
                    || cancel_guard.check(client, options, agent_id, &task.id),
                    |progress, detail| {
                        report_task(
                            client, options, agent_id, &task.id, "running", progress, detail, None,
                            None,
                        )
                    },
                );
                execution_context
                    .report_progress(55, "展项目录已就绪，正在应用模板和 SVN 忽略属性")?;
                initialize_exhibit_repository_with_cancel(request, &mut || {
                    execution_context.check_cancelled()
                })
            }
            "exhibit_repository_initialize_template" => {
                if let Some(manager) = approval_mgr {
                    manager.add_log("info", &format!("{}: 正在应用工程模板", task.id));
                }
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "running",
                    35,
                    "正在使用当前 SVN 账号应用工程模板",
                    None,
                    None,
                )?;
                let request = serde_json::from_value::<InitializeExhibitRepositoryRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                validate_released_template_task(&task, &request)?;
                let mut cancel_guard = TaskCancelGuard::new();
                let mut execution_context = CapabilityExecutionContext::new(
                    task.id.clone(),
                    "exhibit.repository.initialize_template".to_string(),
                    request.exhibit_id.clone(),
                    || cancel_guard.check(client, options, agent_id, &task.id),
                    |progress, detail| {
                        report_task(
                            client, options, agent_id, &task.id, "running", progress, detail, None,
                            None,
                        )
                    },
                );
                execution_context.report_progress(45, "目标目录已准备，正在读取工程模板")?;
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "exhibit.repository.initialize_template",
                    serde_json::to_value(request)?,
                    &mut execution_context,
                )
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
                let _request = serde_json::from_value::<CloneExhibitRepositoryRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("exhibit clone capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
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
                let input = task.payload.clone().unwrap_or_else(|| json!({}));
                let mut execution = task_execution_context(
                    client,
                    options,
                    agent_id,
                    &task,
                    "exhibit.repository.import_local",
                    &input,
                );
                invoke_dashboard_capability_with_execution(
                    options,
                    agent_id,
                    &task,
                    "exhibit.repository.import_local",
                    input,
                    &mut execution,
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
                let mut checkout_input = json!({
                    "project_id": request.project_id,
                    "exhibit_id": request.exhibit_id,
                    "target_path": request.target_path,
                });
                if let Some(repository_url) = request.repository_url {
                    checkout_input["repository_url"] = json!(repository_url);
                }
                let mut result = invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("exhibit workspace checkout capability mapping"),
                    checkout_input,
                )?;
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
                let _request = serde_json::from_value::<PreviewProjectAclRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("ACL preview capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
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
                let _request = serde_json::from_value::<ApplyProjectAclRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type).expect("ACL apply capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
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
                let _request = serde_json::from_value::<ReconcileProjectAclRequest>(
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )?;
                invoke_dashboard_capability(
                    options,
                    agent_id,
                    &task,
                    legacy_task_capability(&task.task_type)
                        .expect("ACL reconcile capability mapping"),
                    task.payload.clone().unwrap_or_else(|| json!({})),
                )
            }
            _ => Ok(json!({ "message": "unsupported task type", "task_type": task.task_type })),
        }
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
                report_task(
                    client,
                    options,
                    agent_id,
                    &task.id,
                    "failed",
                    100,
                    "任务失败",
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
        &options.api_base(),
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
            &options.api_base(),
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
