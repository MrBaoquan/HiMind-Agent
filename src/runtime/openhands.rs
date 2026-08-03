use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::client::{
    claim_agent_run, is_task_canceled_error, renew_agent_run_lease, update_agent_run_status,
    TaskCancelGuard,
};
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::normalize_execution_result;
use crate::runtime::process;
use crate::Options;

const RUNTIME_PROVIDER: &str = "himind.openhands";
const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const OUTPUT_CAPTURE_LIMIT: usize = 64 * 1024;
const ERROR_DETAIL_LIMIT: usize = 4_000;

pub(crate) fn probe() -> RuntimeInstallationReport {
    let executable = env::var_os("HIMIND_OPENHANDS_EXECUTABLE")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("openhands"));
    match crate::runtime::process::verify_command(&executable, &["--version"]) {
        Ok(version) => RuntimeInstallationReport {
            provider: RUNTIME_PROVIDER.to_string(),
            version: openhands_version_line(&version),
            status: "ready".to_string(),
            capabilities: json!({"managed_execution":true,"billing_owner":"himind","ai_proxy":true}),
        },
        Err(_) => RuntimeInstallationReport {
            provider: RUNTIME_PROVIDER.to_string(),
            version: String::new(),
            status: "unavailable".to_string(),
            capabilities: json!({"managed_execution":true,"billing_owner":"himind","ai_proxy":true}),
        },
    }
}

fn openhands_version_line(output: &str) -> String {
    let value = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.to_ascii_lowercase().contains("openhands"))
        .or_else(|| output.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or_default();
    crate::runtime::process::summarize_output(value, 200)
}

#[cfg(windows)]
const WINDOWS_PYTHON_COMPAT: &str = r#"# HiMind compatibility for Rich in CREATE_NO_WINDOW child processes.
try:
    import rich.console as _rich_console
    _rich_console.detect_legacy_windows = lambda: False
except Exception:
    pass
"#;

#[derive(Debug, Deserialize)]
struct AgentRunEnvelope {
    run_id: String,
    runtime_provider: String,
}

#[derive(Debug)]
struct OpenHandsInvocation {
    executable: OsString,
    args: Vec<OsString>,
    workspace: PathBuf,
    api_key: String,
    model: String,
    base_url: String,
    run_id: String,
}

struct RunLeaseRenewal {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for RunLeaseRenewal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
) -> Result<Value, Box<dyn Error>> {
    let envelope = serde_json::from_value::<AgentRunEnvelope>(
        task.payload.clone().ok_or("Agent Run Task 缺少执行信封")?,
    )?;
    if envelope.run_id.trim().is_empty() || envelope.runtime_provider != RUNTIME_PROVIDER {
        return Err("Agent Run Task 只允许 himind.openhands Runtime".into());
    }
    let credential = options.agent_credential();
    let claim = claim_agent_run(
        client,
        &options.api_base,
        agent_id,
        &task.id,
        &envelope.run_id,
        &credential,
    )?;
    if claim.run.id != envelope.run_id || claim.run.runtime_provider != RUNTIME_PROVIDER {
        return Err("Dashboard 返回的 Agent Run 身份或 Runtime 无效".into());
    }
    if claim.run.status != "claimed" || claim.run.created_by_user_id.trim().is_empty() {
        return Err("Dashboard 返回的 Agent Run 状态或用户身份无效".into());
    }

    let result = execute_claimed(client, options, agent_id, task, &claim);
    match result {
        Ok(value) => {
            let value = normalize_execution_result(value, RUNTIME_PROVIDER);
            update_agent_run_status(
                client,
                &options.api_base,
                agent_id,
                &claim.run.id,
                &claim.claim_token,
                "succeeded",
                Some(&value),
                "",
                &credential,
            )?;
            Ok(value)
        }
        Err(error) => {
            let message = redact_error(&error.to_string(), &claim, &credential);
            let status = if is_task_canceled_error(&message) {
                "canceled"
            } else {
                "failed"
            };
            if let Err(report_error) = update_agent_run_status(
                client,
                &options.api_base,
                agent_id,
                &claim.run.id,
                &claim.claim_token,
                status,
                None,
                &message,
                &credential,
            ) {
                eprintln!(
                    "Agent Run {} failure report failed: {report_error}",
                    claim.run.id
                );
            }
            Err(message.into())
        }
    }
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
) -> Result<Value, Box<dyn Error>> {
    let invocation = build_invocation(options, claim)?;
    verify_openhands_available(&invocation.executable)?;
    update_agent_run_status(
        client,
        &options.api_base,
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = start_run_lease_renewal(client, options, agent_id, claim);
    let mut child = spawn_openhands(&invocation)?;
    let stdout = child.stdout.take().map(capture_output);
    let stderr = child.stderr.take().map(capture_output);
    let status = wait_for_openhands(client, options, agent_id, &task.id, &mut child)?;
    let stdout = join_output(stdout);
    let stderr = join_output(stderr);
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let detail = redact_error(&detail, claim, &options.agent_credential());
        return Err(format!(
            "OpenHands 执行失败（exit={}）：{}",
            status.code().unwrap_or(-1),
            detail
        )
        .into());
    }
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": RUNTIME_PROVIDER,
        "completed": true,
        "final_message": process::summarize_output(stdout.trim(), OUTPUT_CAPTURE_LIMIT),
        "billing_owner": "himind"
    }))
}

fn build_invocation(
    options: &Options,
    claim: &AgentRunClaim,
) -> Result<OpenHandsInvocation, Box<dyn Error>> {
    if claim.claim_token.trim().is_empty() {
        return Err("Dashboard 未返回 Agent Run AI 代理凭据".into());
    }
    if claim.ai_model.trim().is_empty() {
        return Err("Dashboard 未配置 OpenHands 使用的 HiMind AI 模型".into());
    }
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let mut instruction = claim.run.instruction.trim().to_string();
    if instruction.is_empty() {
        return Err("Agent Run 指令为空".into());
    }
    if !claim.run.input.is_null()
        && claim
            .run
            .input
            .as_object()
            .is_none_or(|value| !value.is_empty())
    {
        instruction.push_str("\n\n结构化输入（JSON）：\n");
        instruction.push_str(&serde_json::to_string(&claim.run.input)?);
    }
    let executable = env::var_os("HIMIND_OPENHANDS_EXECUTABLE")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("openhands"));
    Ok(OpenHandsInvocation {
        executable,
        args: vec![
            OsString::from("--headless"),
            OsString::from("--json"),
            OsString::from("--override-with-envs"),
            OsString::from("--task"),
            OsString::from(instruction),
        ],
        workspace,
        api_key: claim.claim_token.clone(),
        model: openai_compatible_model(&claim.ai_model),
        base_url: format!(
            "{}/api/agent/runs/{}/ai/v1",
            options.api_base.trim_end_matches('/'),
            claim.run.id
        ),
        run_id: claim.run.id.clone(),
    })
}

fn verify_openhands_available(executable: &OsStr) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_hidden_process(&mut command);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "OpenHands CLI 不可用（version exit={}）。请安装 OpenHands V1 CLI，或设置 HIMIND_OPENHANDS_EXECUTABLE",
            status.code().unwrap_or(-1)
        )
        .into()),
        Err(error) => Err(format!(
            "OpenHands CLI 未安装或不可执行：{error}。请使用 `uv tool install openhands --python 3.12` 安装，或设置 HIMIND_OPENHANDS_EXECUTABLE"
        )
        .into()),
    }
}

fn spawn_openhands(invocation: &OpenHandsInvocation) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(&invocation.executable);
    command
        .args(&invocation.args)
        .current_dir(&invocation.workspace)
        .env("LLM_API_KEY", &invocation.api_key)
        .env("LLM_MODEL", &invocation.model)
        .env("LLM_BASE_URL", &invocation.base_url)
        .env("HIMIND_AGENT_RUN_ID", &invocation.run_id)
        .env("OPENHANDS_SUPPRESS_BANNER", "1")
        .env("PYTHONWARNINGS", "ignore::DeprecationWarning")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .env("TTY_COMPATIBLE", "0")
        .env("NO_COLOR", "1")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("AZURE_OPENAI_API_KEY")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.env("PYTHONPATH", windows_python_compat_path()?);
    configure_hidden_process(&mut command);
    Ok(command.spawn()?)
}

#[cfg(windows)]
fn windows_python_compat_path() -> Result<OsString, Box<dyn Error>> {
    let directory = env::temp_dir()
        .join("himind-agent")
        .join("openhands-python-compat-v1");
    fs::create_dir_all(&directory)?;
    let hook = directory.join("sitecustomize.py");
    let current = fs::read_to_string(&hook).unwrap_or_default();
    if current != WINDOWS_PYTHON_COMPAT {
        fs::write(&hook, WINDOWS_PYTHON_COMPAT)?;
    }
    let mut paths = vec![directory];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    Ok(env::join_paths(paths)?)
}

fn wait_for_openhands(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task_id: &str,
    child: &mut Child,
) -> Result<ExitStatus, Box<dyn Error>> {
    let timeout = env::var("HIMIND_OPENHANDS_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 60)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let started = Instant::now();
    let mut cancel_guard = TaskCancelGuard::new();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if let Err(error) = cancel_guard.check(client, options, agent_id, task_id) {
            if is_task_canceled_error(&error.to_string()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            eprintln!("Agent Run task {task_id} cancellation check failed: {error}");
        }
        if started.elapsed() >= Duration::from_secs(timeout) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("OpenHands 执行超过 {} 秒，已终止", timeout).into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn start_run_lease_renewal(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
) -> RunLeaseRenewal {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let client = client.clone();
    let api_base = options.api_base.clone();
    let credential = options.agent_credential();
    let agent_id = agent_id.to_string();
    let run_id = claim.run.id.clone();
    let claim_token = claim.claim_token.clone();
    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            for _ in 0..120 {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
            if let Err(error) = renew_agent_run_lease(
                &client,
                &api_base,
                &agent_id,
                &run_id,
                &claim_token,
                &credential,
            ) {
                eprintln!("Agent Run {run_id} lease renew failed: {error}");
            }
        }
    });
    RunLeaseRenewal {
        stop,
        handle: Some(handle),
    }
}

fn capture_output<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    captured.extend_from_slice(&buffer[..size]);
                    if captured.len() > OUTPUT_CAPTURE_LIMIT {
                        let excess = captured.len() - OUTPUT_CAPTURE_LIMIT;
                        captured.drain(..excess);
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).trim().to_string()
    })
}

fn join_output(handle: Option<thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|value| value.join().ok())
        .unwrap_or_default()
}

fn redact_error(value: &str, claim: &AgentRunClaim, agent_credential: &str) -> String {
    let mut redacted = value.to_string();
    if !claim.claim_token.is_empty() {
        redacted = redacted.replace(&claim.claim_token, "[redacted]");
    }
    if !agent_credential.is_empty() {
        redacted = redacted.replace(agent_credential, "[redacted]");
    }
    summarize_output(&redacted, ERROR_DETAIL_LIMIT)
}

fn openai_compatible_model(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("openai/") {
        model.to_string()
    } else {
        format!("openai/{model}")
    }
}

fn summarize_output(value: &str, limit: usize) -> String {
    let length = value.chars().count();
    if length <= limit {
        return value.to_string();
    }
    let marker = "\n...[truncated; preserving stderr tail]...\n";
    let marker_length = marker.chars().count();
    if limit <= marker_length {
        return value.chars().skip(length - limit).collect();
    }
    let head_limit = (limit - marker_length) / 4;
    let tail_limit = limit - marker_length - head_limit;
    let head = value.chars().take(head_limit).collect::<String>();
    let tail = value.chars().skip(length - tail_limit).collect::<String>();
    format!("{head}{marker}{tail}")
}

#[cfg(windows)]
fn configure_hidden_process(command: &mut Command) {
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn configure_hidden_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::{
        build_invocation, openai_compatible_model, openhands_version_line, summarize_output,
        AgentRunClaim, AgentRunEnvelope, RUNTIME_PROVIDER,
    };

    #[test]
    fn probe_keeps_only_the_openhands_version_line() {
        assert_eq!(
            openhands_version_line("OpenHands CLI 1.16.0\n\n+------ banner ------+"),
            "OpenHands CLI 1.16.0"
        );
    }
    #[cfg(windows)]
    use super::{windows_python_compat_path, WINDOWS_PYTHON_COMPAT};
    use crate::api::types::AgentRun;
    use crate::Options;
    use serde_json::json;
    use std::fs;

    #[test]
    fn parses_only_openhands_delivery_envelope() {
        let envelope: AgentRunEnvelope = serde_json::from_value(json!({
            "run_id": "run-1",
            "runtime_provider": "himind.openhands"
        }))
        .unwrap();
        assert_eq!(envelope.run_id, "run-1");
        assert_eq!(envelope.runtime_provider, RUNTIME_PROVIDER);
    }

    #[test]
    fn invocation_keeps_proxy_token_out_of_arguments() {
        let root = std::env::temp_dir().join(format!(
            "himind-openhands-invocation-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut options = Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        let claim = AgentRunClaim {
            run: AgentRun {
                id: "run-1".to_string(),
                instruction: "检查并修复测试".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "user-1".to_string(),
                runtime_provider: RUNTIME_PROVIDER.to_string(),
                access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
                input: json!({"suite":"unit"}),
            },
            claim_token: "run-secret-token".to_string(),
            workspace_path: root.to_string_lossy().to_string(),
            ai_model: "himind-coding".to_string(),
            access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
        };
        let invocation = build_invocation(&options, &claim).unwrap();
        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(args.contains("--headless"));
        assert!(args.contains("--override-with-envs"));
        assert!(args.contains("结构化输入"));
        assert!(!args.contains("run-secret-token"));
        assert_eq!(invocation.api_key, "run-secret-token");
        assert_eq!(invocation.model, "openai/himind-coding");
        assert_eq!(
            invocation.base_url,
            "https://dashboard.example/api/agent/runs/run-1/ai/v1"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn openhands_model_uses_openai_compatible_provider() {
        assert_eq!(openai_compatible_model("glm-5.1"), "openai/glm-5.1");
        assert_eq!(openai_compatible_model("openai/glm-5.1"), "openai/glm-5.1");
    }

    #[test]
    fn long_error_summary_preserves_the_traceback_tail() {
        let value = format!("{}ROOT_CAUSE", "banner".repeat(1_000));
        let summary = summarize_output(&value, 200);
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.starts_with("banner"));
        assert!(summary.contains("[truncated; preserving stderr tail]"));
        assert!(summary.ends_with("ROOT_CAUSE"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_python_compat_disables_rich_legacy_renderer() {
        let python_path = windows_python_compat_path().unwrap();
        let directory = std::env::split_paths(&python_path).next().unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("sitecustomize.py")).unwrap(),
            WINDOWS_PYTHON_COMPAT
        );
    }
}
