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
    let mut command = Command::new(executable);
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
    let timeout = env::var(timeout_environment)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 60)
        .unwrap_or(default_timeout_seconds);
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
        if started.elapsed() >= Duration::from_secs(timeout) {
            terminate_process_tree(child);
            return Err(format!(
                "{runtime_name} execution exceeded {timeout} seconds and was terminated"
            )
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
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
        let mut command = Command::new("taskkill");
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
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
pub(crate) fn configure_hidden_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::summarize_output;

    #[test]
    fn output_summary_keeps_context_and_tail() {
        let value = format!("{}ROOT_CAUSE", "header".repeat(1_000));
        let summary = summarize_output(&value, 200);
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.starts_with("header"));
        assert!(summary.ends_with("ROOT_CAUSE"));
    }
}
