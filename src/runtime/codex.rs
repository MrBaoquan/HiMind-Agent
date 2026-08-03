use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::api::client::update_agent_run_status;
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope, PROVIDER_CODEX};
use crate::Options;

const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const FINAL_MESSAGE_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct CodexInvocation {
    executable: OsString,
    args: Vec<OsString>,
    workspace: PathBuf,
    prompt: String,
    result_path: PathBuf,
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    execute_managed(
        client,
        options,
        agent_id,
        task,
        envelope,
        PROVIDER_CODEX,
        |claim| execute_claimed(client, options, agent_id, task, claim),
    )
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
) -> Result<Value, Box<dyn Error>> {
    if !claim.ai_model.trim().is_empty() {
        return Err("personal Codex runs must not receive a HiMind AI model".into());
    }
    let (executable, _) = resolve_codex_executable()?;
    let invocation = build_invocation(executable, claim)?;
    process::remove_file_if_present(&invocation.result_path);
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
    let _renewal = process::start_run_lease_renewal(client, options, agent_id, claim);
    let result = run_codex(client, options, agent_id, task, claim, &invocation);
    process::remove_file_if_present(&invocation.result_path);
    result
}

fn run_codex(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
    invocation: &CodexInvocation,
) -> Result<Value, Box<dyn Error>> {
    let mut child = spawn_codex(invocation)?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(invocation.prompt.as_bytes()) {
            process::terminate_process_tree(&mut child);
            return Err(
                format!("failed to send the Agent Run instruction to Codex: {error}").into(),
            );
        }
    } else {
        process::terminate_process_tree(&mut child);
        return Err("Codex stdin was not available".into());
    }
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child(
        client,
        options,
        agent_id,
        &task.id,
        &mut child,
        "HIMIND_CODEX_TIMEOUT_SECONDS",
        DEFAULT_TIMEOUT_SECONDS,
        "Codex",
    )?;
    let stdout = process::join_output(stdout);
    let stderr = process::join_output(stderr);
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!(
            "Codex execution failed (exit={}): {}",
            status.code().unwrap_or(-1),
            process::redact_error(&detail, claim, &options.agent_credential())
        )
        .into());
    }
    let final_message = read_final_message(&invocation.result_path)?;
    let session_id = parse_codex_session_id(&stdout).unwrap_or_default();
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": PROVIDER_CODEX,
        "completed": true,
        "session_id": session_id,
        "final_message": final_message,
        "billing_owner": "user"
    }))
}

fn build_invocation(
    executable: OsString,
    claim: &AgentRunClaim,
) -> Result<CodexInvocation, Box<dyn Error>> {
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let prompt = build_prompt(claim)?;
    let sandbox = codex_sandbox_mode_for_claim(claim)?;
    let result_path = process::safe_temp_path(&claim.run.id, "codex-final.txt")?;
    Ok(CodexInvocation {
        executable,
        args: vec![
            OsString::from("-C"),
            workspace.as_os_str().to_os_string(),
            OsString::from("-s"),
            OsString::from(sandbox),
            OsString::from("-a"),
            OsString::from("never"),
            OsString::from("exec"),
            OsString::from("--json"),
            OsString::from("--color"),
            OsString::from("never"),
            OsString::from("--skip-git-repo-check"),
            OsString::from("-o"),
            result_path.as_os_str().to_os_string(),
            OsString::from("-"),
        ],
        workspace,
        prompt,
        result_path,
    })
}

fn codex_sandbox_mode_for_claim(claim: &AgentRunClaim) -> Result<String, Box<dyn Error>> {
    let access_mode = if claim.run.access_mode.trim().is_empty() {
        claim.access_mode.trim()
    } else {
        claim.run.access_mode.trim()
    };
    match access_mode {
        crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED => {
            Ok("workspace-write".to_string())
        }
        crate::app::remote_execution::ACCESS_MODE_FULL_ACCESS => {
            Ok("danger-full-access".to_string())
        }
        _ => Err("unsupported Agent Run access mode for Codex".into()),
    }
}

fn build_prompt(claim: &AgentRunClaim) -> Result<String, Box<dyn Error>> {
    let mut prompt = claim.run.instruction.trim().to_string();
    if prompt.is_empty() {
        return Err("Agent Run instruction is empty".into());
    }
    if !claim.run.input.is_null()
        && claim
            .run
            .input
            .as_object()
            .is_none_or(|value| !value.is_empty())
    {
        prompt.push_str("\n\nStructured input (JSON):\n");
        prompt.push_str(&serde_json::to_string(&claim.run.input)?);
    }
    Ok(prompt)
}

fn spawn_codex(invocation: &CodexInvocation) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(&invocation.executable);
    command
        .args(&invocation.args)
        .current_dir(&invocation.workspace)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    process::configure_hidden_process(&mut command);
    Ok(command.spawn()?)
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    let sandbox = match codex_sandbox_mode() {
        Ok(value) => value,
        Err(_) => {
            return RuntimeInstallationReport {
                provider: PROVIDER_CODEX.to_string(),
                version: String::new(),
                status: "unsupported".to_string(),
                capabilities: json!({"managed_execution":true,"billing_owner":"user"}),
            }
        }
    };
    match resolve_codex_executable() {
        Ok((_, version)) => RuntimeInstallationReport {
            provider: PROVIDER_CODEX.to_string(),
            version: process::summarize_output(version.trim(), 200),
            status: "ready".to_string(),
            capabilities: json!({"managed_execution":true,"billing_owner":"user","sandbox":sandbox}),
        },
        Err(_) => RuntimeInstallationReport {
            provider: PROVIDER_CODEX.to_string(),
            version: String::new(),
            status: "unavailable".to_string(),
            capabilities: json!({"managed_execution":true,"billing_owner":"user","sandbox":sandbox}),
        },
    }
}

fn codex_sandbox_mode() -> Result<String, Box<dyn Error>> {
    let value = env::var("HIMIND_CODEX_SANDBOX")
        .unwrap_or_else(|_| "workspace-write".to_string())
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "read-only" | "workspace-write" | "danger-full-access" => Ok(value),
        _ => Err(
            "HIMIND_CODEX_SANDBOX must be read-only, workspace-write, or danger-full-access".into(),
        ),
    }
}

fn resolve_codex_executable() -> Result<(OsString, String), Box<dyn Error>> {
    if let Some(executable) =
        env::var_os("HIMIND_CODEX_EXECUTABLE").filter(|value| !value.is_empty())
    {
        let version = verify_codex_available(&executable)?;
        return Ok((executable, version));
    }
    let mut candidates = Vec::new();
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join("codex.exe")
                .into_os_string(),
        );
    }
    candidates.push(OsString::from("codex"));
    let mut failures = Vec::new();
    for candidate in candidates {
        match verify_codex_available(&candidate) {
            Ok(version) => return Ok((candidate, version)),
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(format!(
        "Codex CLI is not installed or executable. Install/login to Codex, or set HIMIND_CODEX_EXECUTABLE. {}",
        process::summarize_output(&failures.join("; "), 1_000)
    )
    .into())
}

fn verify_codex_available(executable: &OsStr) -> Result<String, Box<dyn Error>> {
    let output = process::verify_command(executable, &["--version"])?;
    if !output.to_ascii_lowercase().contains("codex") {
        return Err("configured executable did not identify itself as Codex CLI".into());
    }
    Ok(output)
}

fn read_final_message(path: &Path) -> Result<String, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(String::new());
    }
    Ok(process::summarize_output(
        fs::read_to_string(path)?.trim(),
        FINAL_MESSAGE_LIMIT,
    ))
}

fn parse_codex_session_id(output: &str) -> Option<String> {
    for line in output.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) == Some("thread.started") {
            if let Some(value) = event.get("thread_id").and_then(Value::as_str) {
                if !value.trim().is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        for key in ["session_id", "thread_id"] {
            if let Some(value) = event.get(key).and_then(Value::as_str) {
                if !value.trim().is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{build_invocation, codex_sandbox_mode, parse_codex_session_id};
    use crate::api::types::{AgentRun, AgentRunClaim};
    use crate::runtime::process;
    use crate::runtime::PROVIDER_CODEX;
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs;

    fn claim(workspace: &std::path::Path) -> AgentRunClaim {
        AgentRunClaim {
            run: AgentRun {
                id: "run-1".to_string(),
                instruction: "Fix the failing tests".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "user-1".to_string(),
                runtime_provider: PROVIDER_CODEX.to_string(),
                access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
                input: json!({"suite":"unit"}),
            },
            claim_token: "claim-secret".to_string(),
            workspace_path: workspace.to_string_lossy().to_string(),
            ai_model: String::new(),
            access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
        }
    }

    #[test]
    fn invocation_uses_workspace_sandbox_and_stdin_prompt() {
        let root =
            std::env::temp_dir().join(format!("himind-codex-invocation-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let invocation = build_invocation(OsString::from("codex"), &claim(&root)).unwrap();
        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(args.contains("workspace-write"));
        assert!(args.contains("-a never exec"));
        assert!(args.ends_with(" -"));
        assert!(!args.contains("Fix the failing tests"));
        assert!(!args.contains("claim-secret"));
        assert!(invocation.prompt.contains("Fix the failing tests"));
        assert!(invocation.prompt.contains("Structured input"));
        process::remove_file_if_present(&invocation.result_path);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_mode_defaults_to_workspace_write() {
        if std::env::var_os("HIMIND_CODEX_SANDBOX").is_none() {
            assert_eq!(codex_sandbox_mode().unwrap(), "workspace-write");
        }
    }

    #[test]
    fn parses_codex_thread_started_event() {
        let output = r#"{"type":"thread.started","thread_id":"thread-123"}
{"type":"item.completed"}"#;
        assert_eq!(
            parse_codex_session_id(output).as_deref(),
            Some("thread-123")
        );
    }
}
