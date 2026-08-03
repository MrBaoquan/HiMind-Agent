use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::api::client::update_agent_run_status;
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope, PROVIDER_GITHUB_COPILOT};
use crate::Options;

const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const FINAL_MESSAGE_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct CopilotInvocation {
    executable: OsString,
    args: Vec<OsString>,
    workspace: PathBuf,
    version: String,
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
        PROVIDER_GITHUB_COPILOT,
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
        return Err("personal GitHub Copilot runs must not receive a HiMind AI model".into());
    }
    let (executable, version) = resolve_copilot_executable()?;
    let invocation = build_invocation(executable, version, claim)?;
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
    run_copilot(client, options, agent_id, task, claim, &invocation)
}

fn run_copilot(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
    invocation: &CopilotInvocation,
) -> Result<Value, Box<dyn Error>> {
    let mut child = spawn_copilot(invocation)?;
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child(
        client,
        options,
        agent_id,
        &task.id,
        &mut child,
        "HIMIND_GITHUB_COPILOT_TIMEOUT_SECONDS",
        DEFAULT_TIMEOUT_SECONDS,
        "GitHub Copilot CLI",
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
            "GitHub Copilot CLI execution failed (exit={}): {}",
            status.code().unwrap_or(-1),
            process::redact_error(&detail, claim, &options.agent_credential())
        )
        .into());
    }
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": PROVIDER_GITHUB_COPILOT,
        "completed": true,
        "version": invocation.version,
        "final_message": process::summarize_output(stdout.trim(), FINAL_MESSAGE_LIMIT),
        "billing_owner": "user"
    }))
}

fn build_invocation(
    executable: OsString,
    version: String,
    claim: &AgentRunClaim,
) -> Result<CopilotInvocation, Box<dyn Error>> {
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let prompt = build_prompt(claim)?;
    Ok(CopilotInvocation {
        executable,
        args: vec![
            OsString::from("-p"),
            OsString::from(prompt),
            OsString::from("-s"),
            OsString::from("--no-ask-user"),
            OsString::from("--allow-all-tools"),
        ],
        workspace,
        version,
    })
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

fn spawn_copilot(invocation: &CopilotInvocation) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(&invocation.executable);
    command
        .args(&invocation.args)
        .current_dir(&invocation.workspace)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    process::configure_hidden_process(&mut command);
    Ok(command.spawn()?)
}

fn resolve_copilot_executable() -> Result<(OsString, String), Box<dyn Error>> {
    if let Some(executable) =
        env::var_os("HIMIND_GITHUB_COPILOT_EXECUTABLE").filter(|value| !value.is_empty())
    {
        let version = verify_copilot_available(&executable)?;
        return Ok((executable, version));
    }
    let mut candidates = Vec::new();
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("GitHub Copilot")
                .join("copilot.exe")
                .into_os_string(),
        );
    }
    candidates.push(OsString::from("copilot"));
    let mut failures = Vec::new();
    for candidate in candidates {
        match verify_copilot_available(&candidate) {
            Ok(version) => return Ok((candidate, version)),
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(format!(
        "GitHub Copilot CLI with programmatic execution support is not installed. Install and authenticate the standalone CLI, or set HIMIND_GITHUB_COPILOT_EXECUTABLE. {}",
        process::summarize_output(&failures.join("; "), 1_000)
    )
    .into())
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    let configured =
        env::var_os("HIMIND_GITHUB_COPILOT_EXECUTABLE").filter(|value| !value.is_empty());
    if configured
        .as_deref()
        .is_some_and(|value| reject_editor_wrapper(Path::new(value)).is_err())
    {
        return RuntimeInstallationReport {
            provider: PROVIDER_GITHUB_COPILOT.to_string(),
            version: String::new(),
            status: "unsupported".to_string(),
            capabilities: copilot_capabilities(),
        };
    }
    match resolve_copilot_executable() {
        Ok((_, version)) => RuntimeInstallationReport {
            provider: PROVIDER_GITHUB_COPILOT.to_string(),
            version,
            status: "ready".to_string(),
            capabilities: copilot_capabilities(),
        },
        Err(_) => RuntimeInstallationReport {
            provider: PROVIDER_GITHUB_COPILOT.to_string(),
            version: String::new(),
            status: "unavailable".to_string(),
            capabilities: copilot_capabilities(),
        },
    }
}

fn copilot_capabilities() -> Value {
    json!({"managed_execution":true,"billing_owner":"user","non_interactive":true,"tool_access":"allow_all"})
}

fn verify_copilot_available(executable: &OsStr) -> Result<String, Box<dyn Error>> {
    reject_editor_wrapper(Path::new(executable))?;
    let help = process::verify_command(executable, &["help"])
        .or_else(|_| process::verify_command(executable, &["--help"]))?;
    validate_copilot_help(&help)?;
    let version = process::verify_command(executable, &["--version"])
        .unwrap_or_else(|_| "GitHub Copilot CLI".to_string());
    Ok(process::summarize_output(version.trim(), 200))
}

fn reject_editor_wrapper(path: &Path) -> Result<(), Box<dyn Error>> {
    let value = path.to_string_lossy().to_ascii_lowercase();
    if value.ends_with(".ps1") || value.contains("github.copilot-chat") {
        return Err("VS Code Copilot wrappers are not managed executors; configure the standalone GitHub Copilot CLI executable".into());
    }
    Ok(())
}

fn validate_copilot_help(help: &str) -> Result<(), Box<dyn Error>> {
    let normalized = help.to_ascii_lowercase();
    if normalized.contains("cannot find github copilot cli") {
        return Err("the configured Copilot wrapper cannot find GitHub Copilot CLI".into());
    }
    for required in ["--no-ask-user", "--allow-all-tools"] {
        if !normalized.contains(required) {
            return Err(
                format!("GitHub Copilot CLI does not expose required option {required}").into(),
            );
        }
    }
    if !normalized.contains("-p") && !normalized.contains("--prompt") {
        return Err("GitHub Copilot CLI does not expose non-interactive prompt mode".into());
    }
    if !normalized.contains("-s") && !normalized.contains("--silent") {
        return Err("GitHub Copilot CLI does not expose silent output mode".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_invocation, reject_editor_wrapper, validate_copilot_help};
    use crate::api::types::{AgentRun, AgentRunClaim};
    use crate::runtime::PROVIDER_GITHUB_COPILOT;
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;

    fn claim(workspace: &Path) -> AgentRunClaim {
        AgentRunClaim {
            run: AgentRun {
                id: "run-1".to_string(),
                instruction: "Fix the failing tests".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "user-1".to_string(),
                runtime_provider: PROVIDER_GITHUB_COPILOT.to_string(),
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
    fn invocation_uses_verified_programmatic_mode() {
        let root =
            std::env::temp_dir().join(format!("himind-copilot-invocation-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let invocation = build_invocation(
            OsString::from("copilot"),
            "copilot 1.0".to_string(),
            &claim(&root),
        )
        .unwrap();
        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(args.contains("-p Fix the failing tests"));
        assert!(args.contains("-s --no-ask-user --allow-all-tools"));
        assert!(!args.contains("claim-secret"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn requires_stable_programmatic_flags() {
        validate_copilot_help("-p --prompt -s --silent --no-ask-user --allow-all-tools").unwrap();
        assert!(validate_copilot_help("interactive only").is_err());
        assert!(validate_copilot_help("Cannot find GitHub Copilot CLI").is_err());
    }

    #[test]
    fn rejects_vscode_copilot_wrapper() {
        assert!(reject_editor_wrapper(Path::new(
            r"C:\Users\user\AppData\Roaming\Code\User\globalStorage\github.copilot-chat\copilotCli\copilot.ps1"
        ))
        .is_err());
    }
}
