#![cfg_attr(
    all(not(debug_assertions), not(feature = "mcp-console")),
    windows_subsystem = "windows"
)]

use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

mod api;
mod app;
mod approval;
mod capability;
mod mcp;
mod remote;
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
use store::outbox::{list_reports, remove_report, store_report, TaskReportRecord};
use svn::service::{
    apply_project_acl, create_exhibit_repository_path, create_repository,
    initialize_exhibit_repository_with_cancel, preview_project_acl,
};
use svn::types::{
    ApplyProjectAclRequest, CreateExhibitRepositoryPathRequest, CreateRepositoryRequest,
    InitializeExhibitRepositoryRequest, PreviewProjectAclRequest,
};
use upload::smb::execute_smb_upload;
use upload::tasks::{execute_upload_code, execute_upload_placeholder};

pub(crate) const VERSION: &str = "0.2.0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginViewLaunch {
    pub plugin_id: String,
    pub view_id: String,
}

fn main() {
    if let Err(error) = svn::service::bootstrap_svn_admin_credentials() {
        eprintln!("SVN administrator credential initialization failed: {error}");
        std::process::exit(1);
    }
    let options = Options::from_env();
    if let Some(arguments) = skill_cli_arguments() {
        if let Err(error) = run_skill_cli(&options, &arguments) {
            eprintln!("skill command failed: {error}");
            std::process::exit(1);
        }
    } else if let Some(arguments) = plugin_cli_arguments() {
        if let Err(error) = run_plugin_cli(&options, &arguments) {
            eprintln!("plugin command failed: {error}");
            std::process::exit(1);
        }
    } else if cfg!(feature = "mcp-console") || env::args().any(|argument| argument == "--mcp") {
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

fn run_skill_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    skill::cli::run(options, arguments)
}

fn run_plugin_cli(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let state: api::types::AgentState =
        serde_json::from_str(&std::fs::read_to_string(&options.state_path)?)?;
    options.set_agent_credential(&state.credential);
    match arguments.first().map(String::as_str) {
        Some("list") => println!("{}", capability::plugin::registry_json()?),
        Some("install") if arguments.len() == 2 => {
            app::plugin_manager::install(options, &state.agent_id, &arguments[1])?
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
            return Err("usage: himind-agent plugin <list|install|uninstall|enable|disable|rollback|invoke> [plugin-id|capability-id json]".into())
        }
    }
    if let Some(plugin_id) = arguments.get(1).filter(|_| {
        matches!(
            arguments.first().map(String::as_str),
            Some("install" | "uninstall" | "enable" | "disable" | "rollback")
        )
    }) {
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
    once: bool,
    interval_seconds: u64,
    local_app: bool,
    local_port: u16,
    enrollment_token: String,
    agent_credential: Arc<RwLock<String>>,
    task_execution: Arc<RwLock<Option<(String, String)>>>,
}

impl Options {
    pub(crate) fn plugin_view_launch(&self) -> Option<PluginViewLaunch> {
        parse_plugin_view_launch(&env::args().collect::<Vec<_>>())
    }

    fn from_env() -> Self {
        let mut api_base =
            env::var("DASHBOARD_API_BASE").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let mut state_path = PathBuf::from("agent-state.json");
        let mut once = false;
        let mut interval_seconds = 10;
        let mut local_app = false;
        let mut local_port = 18181;
        let enrollment_token = env::var("HIMIND_AGENT_ENROLLMENT_TOKEN").unwrap_or_default();

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

        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            state_path,
            once,
            interval_seconds,
            local_app,
            local_port,
            enrollment_token,
            agent_credential: Arc::new(RwLock::new(String::new())),
            task_execution: Arc::new(RwLock::new(None)),
        }
    }
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
    fn start(options: &Options, execution_id: &str, lease_id: &str) -> Self {
        options.set_task_execution(execution_id, lease_id);
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
    use super::{parse_plugin_view_launch, PluginViewLaunch};

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
}

impl Options {
    pub(crate) fn set_agent_credential(&self, credential: &str) {
        if let Ok(mut current) = self.agent_credential.write() {
            *current = credential.to_string();
        }
    }

    pub(crate) fn agent_credential(&self) -> String {
        self.agent_credential
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub(crate) fn set_task_execution(&self, execution_id: &str, lease_id: &str) {
        if let Ok(mut current) = self.task_execution.write() {
            *current = Some((execution_id.to_string(), lease_id.to_string()));
        }
    }

    pub(crate) fn clear_task_execution(&self) {
        if let Ok(mut current) = self.task_execution.write() {
            *current = None;
        }
    }

    pub(crate) fn task_execution(&self) -> Option<(String, String)> {
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
    let _execution = TaskExecutionGuard::start(options, &task.execution_id, &task.lease_id);
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
            create_repository(request)
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
                    None,
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
    let (execution_id, lease_id) = options
        .task_execution()
        .unwrap_or_else(|| (String::new(), String::new()));
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
        if let Err(outbox_error) = store_report(&options.state_path, &report) {
            eprintln!("task report failed and outbox write failed: {outbox_error}");
        }
        return Err(report_error);
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
                eprintln!("task report outbox replay failed: {error}");
                break;
            }
        }
    }
}
