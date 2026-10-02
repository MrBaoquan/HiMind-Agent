use reqwest::blocking::Client;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::client::{is_task_canceled_error, renew_agent_run_lease, TaskCancelGuard};
use crate::api::types::AgentRunClaim;
use crate::Options;

const OUTPUT_CAPTURE_LIMIT: usize = 256 * 1024;
const OUTPUT_HEAD_LIMIT: usize = 64 * 1024;
const ERROR_DETAIL_LIMIT: usize = 4_000;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) struct RunLeaseRenewal {
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

pub(crate) fn canonical_workspace(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = if value.trim().is_empty() {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir()?)
    } else {
        PathBuf::from(value.trim())
    };
    let workspace = path
        .canonicalize()
        .map_err(|error| format!("Agent Run workspace is unavailable: {error}"))?;
    if !workspace.is_dir() {
        return Err("Agent Run workspace is not a directory".into());
    }
    Ok(workspace)
}

pub(crate) fn verify_command(
    executable: &OsStr,
    arguments: &[&str],
) -> Result<String, Box<dyn Error>> {
    let mut command = hidden_command(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    remove_himind_secret_environment(&mut command);
    configure_hidden_process(&mut command);
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}").trim().to_string();
    if !output.status.success() {
        return Err(format!(
            "command preflight failed (exit={}): {}",
            output.status.code().unwrap_or(-1),
            summarize_output(&combined, ERROR_DETAIL_LIMIT)
        )
        .into());
    }
    Ok(combined)
}

pub(crate) fn remove_himind_secret_environment(command: &mut Command) {
    for key in [
        "HIMIND_AGENT_ENROLLMENT_TOKEN",
        "HIMIND_CHANNEL_ADAPTER_HMAC_KEY",
        "HIMIND_AI_INFERENCE_SERVICE_KEY",
        "HIMIND_MODEL_GATEWAY_KEY",
        "AI_GATEWAY_API_KEY",
        "LITELLM_MASTER_KEY",
        "LLM_API_KEY",
        "DASHBOARD_COOKIE",
        "DASHBOARD_ACCESS_TOKEN",
        "DASHBOARD_REFRESH_TOKEN",
    ] {
        command.env_remove(key);
    }
}

pub(crate) fn wait_for_child(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task_id: &str,
    child: &mut Child,
    timeout_environment: &str,
    default_timeout_seconds: u64,
    runtime_name: &str,
) -> Result<ExitStatus, Box<dyn Error>> {
    let timeout = configured_timeout(timeout_environment, default_timeout_seconds, 60);
    let started = Instant::now();
    let mut cancel_guard = TaskCancelGuard::new();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if let Err(error) = cancel_guard.check(client, options, agent_id, task_id) {
            if is_task_canceled_error(&error.to_string()) {
                terminate_process_tree(child);
                return Err(error);
            }
            eprintln!("Agent Run task {task_id} cancellation check failed: {error}");
        }
        if started.elapsed() >= timeout {
            terminate_process_tree(child);
            return Err(format!(
                "{runtime_name} execution exceeded {} seconds and was terminated",
                timeout.as_secs()
            )
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn wait_for_child_with_timeout_and_cancel<F>(
    child: &mut Child,
    timeout_environment: &str,
    default_timeout_seconds: u64,
    runtime_name: &str,
    mut is_canceled: F,
) -> Result<ExitStatus, Box<dyn Error>>
where
    F: FnMut() -> Result<bool, Box<dyn Error>>,
{
    let timeout = configured_timeout(timeout_environment, default_timeout_seconds, 1);
    wait_for_child_until_with_cancel(child, timeout, runtime_name, &mut is_canceled)
}

#[cfg(test)]
fn wait_for_child_until(
    child: &mut Child,
    timeout: Duration,
    runtime_name: &str,
) -> Result<ExitStatus, Box<dyn Error>> {
    wait_for_child_until_with_cancel(child, timeout, runtime_name, &mut || Ok(false))
}

fn wait_for_child_until_with_cancel<F>(
    child: &mut Child,
    timeout: Duration,
    runtime_name: &str,
    is_canceled: &mut F,
) -> Result<ExitStatus, Box<dyn Error>>
where
    F: FnMut() -> Result<bool, Box<dyn Error>>,
{
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if is_canceled()? {
            terminate_process_tree(child);
            return Err(format!("{runtime_name} execution was canceled").into());
        }
        if started.elapsed() >= timeout {
            terminate_process_tree(child);
            return Err(format!(
                "{runtime_name} execution exceeded {} seconds and was terminated",
                timeout.as_secs()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn configured_timeout(
    timeout_environment: &str,
    default_timeout_seconds: u64,
    minimum_seconds: u64,
) -> Duration {
    let seconds = env::var(timeout_environment)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= minimum_seconds)
        .unwrap_or(default_timeout_seconds);
    Duration::from_secs(seconds)
}

pub(crate) fn start_run_lease_renewal(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
) -> RunLeaseRenewal {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let client = client.clone();
    let api_base = options.api_base().clone();
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

pub(crate) fn capture_output<R: Read + Send + 'static>(
    mut reader: R,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut head = Vec::new();
        let mut tail = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let chunk = &buffer[..size];
                    if head.len() < OUTPUT_HEAD_LIMIT {
                        let remaining = OUTPUT_HEAD_LIMIT - head.len();
                        let copied = remaining.min(chunk.len());
                        head.extend_from_slice(&chunk[..copied]);
                        if copied == chunk.len() {
                            continue;
                        }
                        tail.extend_from_slice(&chunk[copied..]);
                    } else {
                        tail.extend_from_slice(chunk);
                    }
                    let tail_limit = OUTPUT_CAPTURE_LIMIT - OUTPUT_HEAD_LIMIT;
                    if tail.len() > tail_limit {
                        tail.drain(..tail.len() - tail_limit);
                    }
                }
                Err(_) => break,
            }
        }
        let mut captured = head;
        if !tail.is_empty() {
            captured.extend_from_slice(b"\n...[output truncated]...\n");
            captured.extend_from_slice(&tail);
        }
        String::from_utf8_lossy(&captured).trim().to_string()
    })
}

pub(crate) fn join_output(handle: Option<thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|value| value.join().ok())
        .unwrap_or_default()
}

pub(crate) fn redact_error(value: &str, claim: &AgentRunClaim, agent_credential: &str) -> String {
    let mut redacted = value.to_string();
    if !claim.claim_token.is_empty() {
        redacted = redacted.replace(&claim.claim_token, "[redacted]");
    }
    if !agent_credential.is_empty() {
        redacted = redacted.replace(agent_credential, "[redacted]");
    }
    summarize_output(&redacted, ERROR_DETAIL_LIMIT)
}

pub(crate) fn summarize_output(value: &str, limit: usize) -> String {
    let length = value.chars().count();
    if length <= limit {
        return value.to_string();
    }
    let marker = "\n...[truncated]...\n";
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

pub(crate) fn safe_temp_path(run_id: &str, suffix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let safe_run_id = run_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || value == '-' || value == '_' {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let directory = env::temp_dir().join("himind-agent").join("agent-runs");
    std::fs::create_dir_all(&directory)?;
    Ok(directory.join(format!("{safe_run_id}-{}-{suffix}", std::process::id())))
}

pub(crate) fn remove_file_if_present(path: &Path) {
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut command = hidden_command("taskkill");
        command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_hidden_process(&mut command);
        let _ = command.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
pub(crate) fn configure_hidden_process(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn configure_hidden_process(_command: &mut Command) {}

pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    configure_hidden_process(&mut command);
    command
}

/// 把用户填的命令解析成真实可执行文件路径。
///
/// Windows 上 `npx`、`uvx`、`pnpm` 这类命令落地的是 `.cmd` 批处理，而
/// `Command::new("npx")` 只会按 `.exe` 去 PATH 里找，找不到就直接报
/// `program not found`——终端里敲得好好的命令到了这里必然失败。桌面端由启动器
/// 拉起时环境又比终端干净，所以除了 PATHEXT，这里还兜了几个常见安装目录，
/// 免得「装了 Node 但 GUI 里找不到」这种问题留给用户排查。
///
/// 带路径分隔符的输入按原样校验；其余一律走名字查找。
pub(crate) fn resolve_executable(program: &str) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    if program.contains('/') || program.contains('\\') {
        let direct = Path::new(program);
        return direct.is_file().then(|| direct.to_path_buf());
    }
    for directory in executable_search_directories() {
        for name in executable_candidate_names(program) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 待查文件名。Windows 下 PATHEXT 后缀优先，原名最后兜底。
///
/// 顺序很关键：`C:\Program Files\nodejs` 里同时存在无扩展名的 `npx`（Git for Windows
/// 的 shell 脚本）和 `npx.cmd`。按原名优先会先命中（或跳过）错误的那个文件，
/// Windows 无法直接 CreateProcess 无扩展名脚本，会报 os error 193。
fn executable_candidate_names(program: &str) -> Vec<String> {
    #[cfg(not(windows))]
    {
        vec![program.to_string()]
    }
    #[cfg(windows)]
    {
        let extensions = env::var_os("PATHEXT")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut names = Vec::new();
        // 已经写明后缀的（如 `npx.cmd`）优先按原样匹配，避免多走一轮后缀拼接。
        let named_extension = std::path::Path::new(program)
            .extension()
            .map(|value| format!(".{}", value.to_string_lossy()))
            .filter(|extension| {
                extensions
                    .split(';')
                    .any(|known| known.trim().eq_ignore_ascii_case(extension))
            });
        if named_extension.is_some() {
            names.push(program.to_string());
        }
        for extension in extensions.split(';') {
            let extension = extension.trim();
            if !extension.is_empty() {
                names.push(format!("{program}{extension}"));
            }
        }
        names.push(program.to_string());
        names
    }
}

fn executable_search_directories() -> Vec<PathBuf> {
    let mut directories = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect::<Vec<_>>();
    #[cfg(windows)]
    {
        for (key, suffix) in [
            ("ProgramFiles", "nodejs"),
            ("ProgramFiles(x86)", "nodejs"),
            ("LOCALAPPDATA", "Programs\\nodejs"),
            ("APPDATA", "npm"),
            ("LOCALAPPDATA", "pnpm"),
            ("LOCALAPPDATA", "Microsoft\\WinGet\\Links"),
            ("USERPROFILE", ".bun\\bin"),
            ("USERPROFILE", ".local\\bin"),
            ("USERPROFILE", ".cargo\\bin"),
            ("USERPROFILE", "scoop\\shims"),
        ] {
            if let Some(root) = env::var_os(key).filter(|value| !value.is_empty()) {
                directories.push(PathBuf::from(root).join(suffix));
            }
        }
        directories.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));
    }
    #[cfg(not(windows))]
    {
        directories.push(PathBuf::from("/usr/local/bin"));
        directories.push(PathBuf::from("/opt/homebrew/bin"));
    }
    directories
}

#[cfg(test)]
mod tests {
    use super::{
        executable_candidate_names, summarize_output, wait_for_child_until,
        wait_for_child_until_with_cancel,
    };
    use std::error::Error;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn output_summary_keeps_context_and_tail() {
        let value = format!("{}ROOT_CAUSE", "header".repeat(1_000));
        let summary = summarize_output(&value, 200);
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.starts_with("header"));
        assert!(summary.ends_with("ROOT_CAUSE"));
    }

    /// Windows 的 `npx` 目录里同时有无扩展名脚本和 `npx.cmd`，候选顺序必须先看后缀，
    /// 否则会拿到无法直接 CreateProcess 的那个文件（os error 193）。
    #[test]
    fn windows_prefers_pathext_suffixed_candidates() {
        let names = executable_candidate_names("npx");
        #[cfg(windows)]
        {
            let first = names.first().expect("至少有一个候选名");
            assert!(
                first.len() > "npx".len(),
                "第一个候选应是带后缀的可执行文件，实际为 {first}"
            );
            assert_eq!(names.last().map(String::as_str), Some("npx"));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(names, vec!["npx".to_string()]);
        }
    }

    #[test]
    fn process_timeout_terminates_the_child() {
        let mut command;
        #[cfg(windows)]
        {
            command = Command::new("cmd");
            command.args(["/C", "ping 127.0.0.1 -n 6 >NUL"]);
        }
        #[cfg(not(windows))]
        {
            command = Command::new("sh");
            command.args(["-c", "sleep 5"]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let error =
            wait_for_child_until(&mut child, Duration::from_secs(1), "timeout contract test")
                .unwrap_err();
        assert!(error.to_string().contains("exceeded 1 seconds"));
    }

    #[test]
    fn process_cancellation_terminates_the_child() {
        let mut command;
        #[cfg(windows)]
        {
            command = Command::new("cmd");
            command.args(["/C", "ping 127.0.0.1 -n 6 >NUL"]);
        }
        #[cfg(not(windows))]
        {
            command = Command::new("sh");
            command.args(["-c", "sleep 5"]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let mut canceled = false;
        let mut is_canceled = || -> Result<bool, Box<dyn Error>> {
            canceled = true;
            Ok(canceled)
        };
        let error = wait_for_child_until_with_cancel(
            &mut child,
            Duration::from_secs(10),
            "cancel contract test",
            &mut is_canceled,
        )
        .unwrap_err();
        assert!(error.to_string().contains("was canceled"));
    }
}
