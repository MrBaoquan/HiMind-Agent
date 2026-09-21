use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::api::client::update_agent_run_status;
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope};
use crate::Options;

const ACP_PROTOCOL_VERSION: i64 = 1;
const ACP_OUTPUT_LIMIT: usize = 64 * 1024;
const ACP_PROFILE_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_PROFILES_JSON";
const ACP_EXECUTABLE_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_EXECUTABLE";
const ACP_ARGS_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_ARGS_JSON";
const ACP_PERMISSION_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_PERMISSION_POLICY";
const ACP_MESSAGE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const ACP_CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcpProfile {
    executable: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    permission_policy: String,
    #[serde(skip)]
    source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionPolicy {
    Deny,
    AllowOnce,
    Prompt,
}

impl PermissionPolicy {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value.trim() {
            "" | "deny" => Ok(Self::Deny),
            "allow_once" => Ok(Self::AllowOnce),
            "prompt" => Ok(Self::Prompt),
            value => Err(format!(
                "ACP permission_policy must be deny, allow_once or prompt, got {value}"
            )
            .into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowOnce => "allow_once",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug)]
struct AcpExecution {
    session_id: String,
    stop_reason: String,
    final_text: String,
    update_count: usize,
    tool_call_count: usize,
    permission_requests: Vec<Value>,
    denied_client_methods: Vec<String>,
    stderr: String,
}

#[derive(Clone)]
struct AcpAttribution {
    provider: String,
    run_id: String,
}

#[derive(Default)]
struct AcpSessionState {
    final_text: String,
    update_count: usize,
    tool_call_count: usize,
    permission_requests: Vec<Value>,
    denied_client_methods: Vec<String>,
}

struct AcpProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    receiver: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<String>>,
    next_request_id: i64,
}

pub(crate) fn is_provider(provider: &str) -> bool {
    provider == "acp.stdio" || provider.starts_with("acp.")
}

pub(crate) fn probe_installations() -> Vec<RuntimeInstallationReport> {
    let profiles = match configured_profiles() {
        Ok(profiles) => profiles,
        Err(error) => {
            return vec![RuntimeInstallationReport {
                provider: "acp.stdio".to_string(),
                version: String::new(),
                status: "unsupported".to_string(),
                capabilities: json!({
                    "protocol_version": ACP_PROTOCOL_VERSION,
                    "error": error.to_string(),
                }),
            }]
        }
    };
    profiles
        .into_iter()
        .map(|(provider, profile)| {
            let policy = profile_permission_policy(&profile);
            let executable = resolve_executable(&profile.executable);
            RuntimeInstallationReport {
                provider,
                version: profile.version.trim().to_string(),
                status: if executable.is_ok() && policy.is_ok() {
                    "ready".to_string()
                } else {
                    "unavailable".to_string()
                },
                capabilities: json!({
                    "protocol_version": ACP_PROTOCOL_VERSION,
                    "streaming": true,
                    "cancel": true,
                    "permission_policy": policy
                        .map(PermissionPolicy::as_str)
                        .unwrap_or("invalid"),
                    "permission_ownership": "runtime_step_or_agent_run",
                    "source": profile.source,
                    "client_file_system": false,
                    "client_terminal": false,
                    "network_isolated": false,
                    "tool_access": "provider_defined",
                }),
            }
        })
        .collect()
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    let provider = envelope.runtime_provider.clone();
    execute_managed(
        client,
        options,
        agent_id,
        task,
        envelope,
        &provider,
        |claim| execute_claimed(client, options, agent_id, claim, &provider),
    )
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    provider: &str,
) -> Result<Value, Box<dyn Error>> {
    if !claim.ai_model.trim().is_empty() {
        return Err("ACP Runtime runs must not receive a HiMind AI model".into());
    }
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let prompt = build_prompt(claim)?;
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
    let execution = run_session(
        provider,
        &workspace,
        &prompt,
        2 * 60 * 60,
        &claim.run.id,
        &mut || Ok(false),
    )?;
    let attribution = AcpAttribution {
        provider: provider.to_string(),
        run_id: claim.run.id.clone(),
    };
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": provider,
        "completed": true,
        "session_id": execution.session_id,
        "final_message": execution.final_text,
        "billing_owner": "user",
        "acp": execution_metadata(&execution, &attribution),
    }))
}

pub(crate) fn execute_workflow(
    provider: &str,
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    run_id: &str,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let workspace = process::canonical_workspace(workspace)?;
    let execution = run_session(
        provider,
        &workspace,
        prompt,
        timeout_seconds,
        run_id,
        &mut || is_canceled(),
    )?;
    Ok(execution.final_text)
}

fn run_session(
    provider: &str,
    workspace: &Path,
    prompt: &str,
    timeout_seconds: u64,
    run_id: &str,
    is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<AcpExecution, Box<dyn Error>> {
    if prompt.trim().is_empty() {
        return Err("ACP prompt is empty".into());
    }
    let profile = profile_for(provider)?;
    let policy = profile_permission_policy(&profile)?;
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let attribution = AcpAttribution {
        provider: provider.to_string(),
        run_id: run_id.to_string(),
    };
    let mut process = AcpProcess::spawn(&profile, workspace)?;
    let mut state = AcpSessionState::default();

    let initialize = process.request(
        "initialize",
        json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false,
                "auth": {"terminal": false}
            },
            "clientInfo": {
                "name": "HiMind Agent",
                "version": crate::VERSION
            }
        }),
        timeout,
        policy,
        &mut state,
        &attribution,
        is_canceled,
    )?;
    let negotiated = initialize
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .ok_or("ACP initialize response is missing protocolVersion")?;
    if negotiated != ACP_PROTOCOL_VERSION {
        return Err(
            format!("ACP agent negotiated unsupported protocol version {negotiated}").into(),
        );
    }

    let session = process.request(
        "session/new",
        json!({
            "cwd": workspace.to_string_lossy(),
            "mcpServers": []
        }),
        timeout,
        policy,
        &mut state,
        &attribution,
        is_canceled,
    )?;
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("ACP session/new response is missing sessionId")?
        .to_string();

    let prompt_response = process.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": prompt}]
        }),
        timeout,
        policy,
        &mut state,
        &attribution,
        is_canceled,
    )?;
    let stop_reason = prompt_response
        .get("stopReason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("ACP session/prompt response is missing stopReason")?
        .to_string();
    if stop_reason == "cancelled" {
        return Err("ACP prompt was canceled".into());
    }
    let stderr = process.finish()?;
    Ok(AcpExecution {
        session_id,
        stop_reason,
        final_text: process::summarize_output(&state.final_text, ACP_OUTPUT_LIMIT),
        update_count: state.update_count,
        tool_call_count: state.tool_call_count,
        permission_requests: state.permission_requests,
        denied_client_methods: state.denied_client_methods,
        stderr: process::summarize_output(stderr.trim(), 8 * 1024),
    })
}

impl AcpProcess {
    fn spawn(profile: &AcpProfile, workspace: &Path) -> Result<Self, Box<dyn Error>> {
        let executable = resolve_executable(&profile.executable)?;
        let mut command = Command::new(executable);
        command
            .args(&profile.args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::remove_himind_secret_environment(&mut command);
        process::configure_hidden_process(&mut command);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or("ACP agent stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("ACP agent stdout is unavailable")?;
        let stderr = child.stderr.take().map(process::capture_output);
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if line.trim().is_empty() => {}
                    Ok(line) => match serde_json::from_str::<Value>(line.trim()) {
                        Ok(value) => {
                            if sender.send(Ok(value)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(format!("invalid ACP JSON-RPC: {error}")));
                            return;
                        }
                    },
                    Err(error) => {
                        let _ = sender.send(Err(format!("failed to read ACP stdout: {error}")));
                        return;
                    }
                }
            }
        });
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            receiver,
            reader: Some(reader),
            stderr,
            next_request_id: 1,
        })
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        policy: PermissionPolicy,
        state: &mut AcpSessionState,
        attribution: &AcpAttribution,
        is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
    ) -> Result<Value, Box<dyn Error>> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.write(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        }))?;
        let started = Instant::now();
        loop {
            if is_canceled()? {
                if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                    self.cancel(session_id)?;
                }
                return Err(format!("ACP {} was canceled", method).into());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                self.terminate();
                return Err(format!("ACP {} timed out", method).into());
            }
            let message = match self
                .receiver
                .recv_timeout(remaining.min(ACP_MESSAGE_POLL_INTERVAL))
            {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => return Err(error.into()),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("ACP agent closed stdout before responding".into())
                }
            };
            if message.get("id").and_then(Value::as_i64) == Some(request_id)
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    return Err(format!(
                        "ACP {} failed: {}",
                        method,
                        process::summarize_output(&error.to_string(), 2_000)
                    )
                    .into());
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            self.handle_message(message, policy, state, attribution, is_canceled)?;
        }
    }

    fn handle_message(
        &mut self,
        message: Value,
        policy: PermissionPolicy,
        state: &mut AcpSessionState,
        attribution: &AcpAttribution,
        is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(());
        };
        if let Some(request_id) = message.get("id").and_then(Value::as_i64) {
            if method == "session/request_permission" {
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                let outcome = permission_outcome(&params, policy, attribution, is_canceled)?;
                let recorded = json!({
                    "tool_call": params.get("toolCall").cloned().unwrap_or(Value::Null),
                    "options": params.get("options").cloned().unwrap_or_else(|| json!([])),
                    "outcome": outcome,
                });
                state.permission_requests.push(recorded);
                self.write(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"outcome": outcome}
                }))?;
                return Ok(());
            }
            state.denied_client_methods.push(method.to_string());
            self.write(json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {
                    "code": -32601,
                    "message": format!("client method is not supported: {method}")
                }
            }))?;
            return Ok(());
        }
        if method == "session/update" {
            if let Some(update) = message.pointer("/params/update") {
                apply_session_update(update, state);
            }
        }
        Ok(())
    }

    fn cancel(&mut self, session_id: &str) -> Result<(), Box<dyn Error>> {
        self.write(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }))?;
        thread::sleep(ACP_CANCEL_GRACE_PERIOD);
        self.terminate();
        Ok(())
    }

    fn write(&mut self, message: Value) -> Result<(), Box<dyn Error>> {
        let stdin = self.stdin.as_mut().ok_or("ACP agent stdin is closed")?;
        serde_json::to_writer(&mut *stdin, &message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn finish(mut self) -> Result<String, Box<dyn Error>> {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        if let Some(child) = self.child.as_mut() {
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        self.terminate();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(process::join_output(self.stderr.take()))
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            process::terminate_process_tree(&mut child);
        }
    }
}

impl Drop for AcpProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn configured_profiles() -> Result<BTreeMap<String, AcpProfile>, Box<dyn Error>> {
    let mut profiles = BTreeMap::new();
    for profile in crate::store::acp_profiles::list()? {
        if !profile.enabled {
            continue;
        }
        profiles.insert(
            profile.provider_id,
            AcpProfile {
                executable: profile.executable,
                args: profile.args,
                version: profile.version,
                permission_policy: profile.permission_policy,
                source: "agent".to_string(),
            },
        );
    }
    if let Ok(raw) = env::var(ACP_PROFILE_ENVIRONMENT) {
        if !raw.trim().is_empty() {
            let value: Value = serde_json::from_str(&raw)?;
            let object = value
                .as_object()
                .ok_or("ACP runtime profiles must be a JSON object")?;
            for (name, value) in object {
                let profile: AcpProfile = serde_json::from_value(value.clone())?;
                let provider = if name.starts_with("acp.") {
                    name.clone()
                } else {
                    format!("acp.{}", name.trim())
                };
                if provider == "acp." {
                    return Err("ACP runtime profile name is empty".into());
                }
                profiles.insert(provider, profile);
            }
        }
    }
    if let Ok(executable) = env::var(ACP_EXECUTABLE_ENVIRONMENT) {
        if !executable.trim().is_empty() {
            let args = env::var(ACP_ARGS_ENVIRONMENT)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| serde_json::from_str::<Vec<String>>(&value))
                .transpose()?
                .unwrap_or_default();
            profiles.insert(
                "acp.stdio".to_string(),
                AcpProfile {
                    executable,
                    args,
                    version: String::new(),
                    permission_policy: env::var(ACP_PERMISSION_ENVIRONMENT).unwrap_or_default(),
                    source: "environment".to_string(),
                },
            );
        }
    }
    Ok(profiles)
}

fn profile_for(provider: &str) -> Result<AcpProfile, Box<dyn Error>> {
    configured_profiles()?
        .remove(provider)
        .ok_or_else(|| format!("ACP runtime profile is not configured: {provider}").into())
}

fn profile_permission_policy(profile: &AcpProfile) -> Result<PermissionPolicy, Box<dyn Error>> {
    PermissionPolicy::parse(&profile.permission_policy)
}

fn resolve_executable(executable: &str) -> Result<PathBuf, Box<dyn Error>> {
    let executable = executable.trim();
    if executable.is_empty() {
        return Err("ACP runtime executable is empty".into());
    }
    let path = Path::new(executable);
    if path.is_absolute() || executable.contains(std::path::MAIN_SEPARATOR) {
        return path
            .is_file()
            .then(|| path.to_path_buf())
            .ok_or_else(|| format!("ACP runtime executable is unavailable: {executable}").into());
    }
    let mut names = vec![OsString::from(executable)];
    #[cfg(windows)]
    {
        let extension =
            env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for extension in extension.to_string_lossy().split(';') {
            if !extension.trim().is_empty() {
                names.push(OsString::from(format!("{executable}{extension}")));
            }
        }
    }
    let mut directories =
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    if let Ok(current) = env::current_dir() {
        directories.insert(0, current);
    }
    for directory in directories {
        for name in &names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!("ACP runtime executable is unavailable: {executable}").into())
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

fn permission_outcome(
    params: &Value,
    policy: PermissionPolicy,
    attribution: &AcpAttribution,
    is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<Value, Box<dyn Error>> {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if policy == PermissionPolicy::Prompt {
        let tool = params.get("toolCall").cloned().unwrap_or_else(|| json!({}));
        let tool_id = tool
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = tool
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ACP tool call");
        let kind = tool
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let approved = crate::approval::manager::ApprovalManager::global()
            .request_capability_approval_with_cancel(
                "acp.permission",
                "R3",
                process::summarize_output(&format!("{}: {title}", attribution.provider), 300),
                process::summarize_output(
                    &format!(
                        "provider={} run_id={} session_id={} tool_call_id={} kind={kind}",
                        attribution.provider, attribution.run_id, session_id, tool_id,
                    ),
                    1_000,
                ),
                || is_canceled().map_err(|error| error.to_string()),
            )?;
        let Some(approved) = approved else {
            return Err("ACP permission request was canceled".into());
        };
        return Ok(select_permission_option(
            &options,
            if approved {
                "allow_once"
            } else {
                "reject_once"
            },
        ));
    }
    let preferred_kind = match policy {
        PermissionPolicy::AllowOnce => "allow_once",
        PermissionPolicy::Deny => "reject_once",
        PermissionPolicy::Prompt => unreachable!(),
    };
    Ok(select_permission_option(&options, preferred_kind))
}

fn select_permission_option(options: &[Value], preferred_kind: &str) -> Value {
    if let Some(option_id) = options
        .iter()
        .find(|option| option.get("kind").and_then(Value::as_str) == Some(preferred_kind))
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
    {
        return json!({"outcome": "selected", "optionId": option_id});
    }
    if preferred_kind.starts_with("reject") {
        if let Some(option_id) = options
            .iter()
            .find(|option| {
                option
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.starts_with("reject"))
            })
            .and_then(|option| option.get("optionId"))
            .and_then(Value::as_str)
        {
            return json!({"outcome": "selected", "optionId": option_id});
        }
    }
    json!({"outcome": "cancelled"})
}

fn apply_session_update(update: &Value, state: &mut AcpSessionState) {
    state.update_count = state.update_count.saturating_add(1);
    match update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "agent_message_chunk" => {
            if let Some(text) = update
                .get("content")
                .filter(|content| content.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
            {
                state.final_text.push_str(text);
            }
        }
        "tool_call" | "tool_call_update" => {
            state.tool_call_count = state.tool_call_count.saturating_add(1);
        }
        _ => {}
    }
}

fn execution_metadata(execution: &AcpExecution, attribution: &AcpAttribution) -> Value {
    json!({
        "provider": attribution.provider,
        "run_id": attribution.run_id,
        "stop_reason": execution.stop_reason,
        "update_count": execution.update_count,
        "tool_call_count": execution.tool_call_count,
        "permission_requests": execution.permission_requests,
        "denied_client_methods": execution.denied_client_methods,
        "stderr": execution.stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::{permission_outcome, AcpAttribution, PermissionPolicy};
    use serde_json::json;

    #[test]
    fn permission_policy_denies_by_default() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({
                "options": [
                    {"optionId":"allow","name":"Allow","kind":"allow_once"},
                    {"optionId":"deny","name":"Deny","kind":"reject_once"}
                ]
            }),
            PermissionPolicy::Deny,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"selected","optionId":"deny"}));
    }

    #[test]
    fn permission_policy_allows_once_only_when_explicit() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({
                "options": [
                    {"optionId":"allow","name":"Allow","kind":"allow_once"},
                    {"optionId":"deny","name":"Deny","kind":"reject_once"}
                ]
            }),
            PermissionPolicy::AllowOnce,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"selected","optionId":"allow"}));
    }

    #[test]
    fn permission_policy_cancels_without_a_matching_option() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({"options":[]}),
            PermissionPolicy::AllowOnce,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"cancelled"}));
    }
}
