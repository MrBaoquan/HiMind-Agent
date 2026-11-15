use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::api::client::update_agent_run_status;
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope, PROVIDER_CODEX};
use crate::Options;

const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;

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
    let mut cancel_guard = crate::api::client::TaskCancelGuard::new();
    let cancel = std::cell::RefCell::new(&mut cancel_guard);
    let (executable, version) = resolve_codex_executable_with_cancel(&|| {
        cancel.borrow_mut().check(client, options, agent_id, &task.id)?; Ok(false)
    })?;
    let invocation = build_invocation(executable, claim)?;
    process::remove_file_if_present(&invocation.result_path);
    update_agent_run_status(
        client,
        &options.api_base(),
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = process::start_run_lease_renewal(client, options, agent_id, claim);
    let result = run_codex(client, options, agent_id, task, claim, &invocation).map(|mut result| {
        result["version"] = Value::String(version.trim().to_string());
        result
    });
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
    let mut cancel_guard = crate::api::client::TaskCancelGuard::new();
    let cancel = std::cell::RefCell::new(&mut cancel_guard);
    let is_canceled = || {
        cancel.borrow_mut().check(client, options, agent_id, &task.id)?;
        Ok(false)
    };
    execute_invocation(invocation, claim, &is_canceled, DEFAULT_TIMEOUT_SECONDS)
}

fn execute_invocation(invocation: &CodexInvocation, claim: &AgentRunClaim,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>, timeout: u64) -> Result<Value, Box<dyn Error>> {
    let mut command = codex_command(invocation);
    let outcome = crate::runtime::codex_transport::execute(&mut command, &invocation.prompt,
        &invocation.result_path, timeout, is_canceled, std::time::Duration::from_secs(15),
        &claim.run.id, &claim.resume_provider_session_id)?;
    let report = crate::workflow::parse_runtime_payload(&outcome.text);
    if report.as_ref().is_some_and(|value| value.get("passed").and_then(Value::as_bool) == Some(false))
        || ((outcome.text.trim().starts_with('{') || outcome.text.trim().starts_with("```")) && report.is_none()) {
        return Err(Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
            provenance:Some(json!({"provider":"personal.codex","session_id":outcome.session_id,"exit_code":0,"model_source":"personal_cli_config","usage_receipt":outcome.usage_receipt})),
            message: "Codex AgentRun returned passed=false".into(), report, usage: outcome.usage,
        }));
    }
    Ok(json!({"run_id":claim.run.id,"runtime_provider":PROVIDER_CODEX,"completed":true,
        "session_id":outcome.session_id,"final_message":outcome.text,"usage":outcome.usage,"usage_receipt":outcome.usage_receipt,
        "billing_owner":"user","cost":null,"cost_status":"unknown",
        "workspace":invocation.workspace,"model_source":"personal_cli_config"}))
}

fn build_invocation(
    executable: OsString,
    claim: &AgentRunClaim,
) -> Result<CodexInvocation, Box<dyn Error>> {
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let prompt = build_prompt(claim)?;
    let sandbox = codex_sandbox_mode_for_claim(claim)?;
    let result_path = process::safe_temp_path(&claim.run.id, "codex-final.txt")?;
    let resume_session_id = claim.resume_provider_session_id.trim();
    let args = if resume_session_id.is_empty() {
        vec![
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
        ]
    } else {
        validate_resume_session_id(resume_session_id)?;
        vec![
            OsString::from("-C"), workspace.as_os_str().to_os_string(),
            OsString::from("-s"), OsString::from(sandbox),
            OsString::from("-a"), OsString::from("never"),
            OsString::from("exec"),
            OsString::from("resume"),
            OsString::from("--json"),
            OsString::from("--skip-git-repo-check"),
            OsString::from("-o"),
            result_path.as_os_str().to_os_string(),
            OsString::from(resume_session_id),
            OsString::from("-"),
        ]
    };
    Ok(CodexInvocation {
        executable,
        args,
        workspace,
        prompt,
        result_path,
    })
}

fn validate_resume_session_id(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty() || value.len() > 512 || value.chars().any(|char| char.is_control()) {
        return Err("Codex resume session id is invalid".into());
    }
    Ok(())
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
        crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED => {
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

fn codex_command(invocation: &CodexInvocation) -> Command {
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
    command
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    let sandbox = match codex_sandbox_mode() {
        Ok(value) => value,
        Err(_) => {
            return RuntimeInstallationReport {
                provider: PROVIDER_CODEX.to_string(),
                version: String::new(),
                status: "unsupported".to_string(),
                capabilities: json!({
                    "managed_execution": true,
                    "billing_owner": "user",
                    "network_isolated": false,
                    "tool_access": "workspace-write"
                }),
            }
        }
    };
    match resolve_codex_executable() {
        Ok((_, version)) => RuntimeInstallationReport {
            provider: PROVIDER_CODEX.to_string(),
            version: process::summarize_output(version.trim(), 200),
            status: "ready".to_string(),
            capabilities: json!({
                "managed_execution": true,
                "billing_owner": "user",
                "sandbox": sandbox,
                "network_isolated": false,
                "tool_access": "workspace-write"
            }),
        },
        Err(_) => RuntimeInstallationReport {
            provider: PROVIDER_CODEX.to_string(),
            version: String::new(),
            status: "unavailable".to_string(),
            capabilities: json!({
                "managed_execution": true,
                "billing_owner": "user",
                "sandbox": sandbox,
                "network_isolated": false,
                "tool_access": "workspace-write"
            }),
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

pub(crate) fn resolve_codex_executable() -> Result<(OsString, String), Box<dyn Error>> {
    resolve_codex_executable_with_cancel(&|| Ok(false))
}

pub(crate) fn resolve_codex_executable_with_cancel(is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>) -> Result<(OsString, String), Box<dyn Error>> {
    if let Some(executable) =
        env::var_os("HIMIND_CODEX_EXECUTABLE").filter(|value| !value.is_empty())
    {
        let version = verify_codex_available(&executable, is_canceled)?;
        remember_codex_selection(&executable,&version);
        return Ok((executable, version));
    }
    let mut candidates = Vec::new();
    // Persist a validated Desktop selection in the Agent cache (never personal
    // Codex config). An explicit host pin always wins; every reuse rechecks it.
    let selection_path = crate::store::paths::agent_home().join("cache/codex-selection.json");
    if let Ok(bytes) = std::fs::read(&selection_path) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(path) = value.get("executable").and_then(Value::as_str).filter(|_|
                value["schema_version"]=="codex_selection.v1" && value["version"].as_str().is_some_and(|version|validate_codex_version(version).is_ok())) {
                if std::path::Path::new(path).is_absolute() { candidates.push(OsString::from(path)); }
            }
        }
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let bin = PathBuf::from(&local_app_data).join("OpenAI/Codex/bin");
        let mut installed = std::fs::read_dir(&bin).into_iter().flatten().flatten()
            .filter_map(|entry|entry.file_type().ok().filter(|kind|kind.is_dir()).map(|_|entry.path().join("codex.exe")))
            .filter(|path|path.is_file()).collect::<Vec<_>>();
        installed.sort(); candidates.extend(installed.into_iter().map(PathBuf::into_os_string));
        candidates.push(
            PathBuf::from(local_app_data)
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join("codex.exe")
                .into_os_string(),
        );
    }
    if let Some(path)=process::resolve_executable("codex") { candidates.push(path.into_os_string()); }
    let mut failures = Vec::new();
    for candidate in candidates {
        match verify_codex_available(&candidate, is_canceled) {
            Ok(version) => {
                remember_codex_selection(&candidate,&version);
                return Ok((candidate, version));
            },
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(format!(
        "Codex CLI is not installed or executable. Install/login to Codex, or set HIMIND_CODEX_EXECUTABLE. {}",
        process::summarize_output(&failures.join("; "), 1_000)
    )
    .into())
}

fn verify_codex_available(executable: &OsStr, is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>) -> Result<String, Box<dyn Error>> {
    let output = process::verify_command_with_cancel(executable, &["--version"], std::time::Duration::from_secs(5), is_canceled)?;
    if !output.to_ascii_lowercase().contains("codex") {
        return Err("configured executable did not identify itself as Codex CLI".into());
    }
    validate_codex_version(&output)?;
    Ok(output)
}

fn remember_codex_selection(executable: &OsStr, version: &str) {
    if !std::path::Path::new(executable).is_absolute() {return;}
    let path=crate::store::paths::agent_home().join("cache/codex-selection.json");
    let Some(executable)=executable.to_str() else {return;};
    let data=json!({"schema_version":"codex_selection.v1","executable":executable,"version":version});
    if crate::store::atomic_file::atomic_write(&path,&serde_json::to_vec(&data).unwrap_or_default()).is_err() {
        eprintln!("Codex executable selection cache could not be persisted");
    }
}

fn validate_codex_version(output: &str) -> Result<(),Box<dyn Error>> {
    let version=output.split_whitespace().find_map(|word|semver::Version::parse(word).ok())
        .ok_or("Codex CLI did not report a semantic version")?;
    // This Windows transport was exercised against 0.167. Older cap_sid path
    // mapping is incompatible with the Desktop shell environment used here.
    if cfg!(windows) && version.major==0 && version.minor<167 {
        return Err("Codex CLI is below the validated Windows compatibility floor (0.167); select a compatible Desktop executable".into());
    }
    Ok(())
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
    use sha2::Digest;
    use super::{build_invocation, codex_sandbox_mode, parse_codex_session_id};
    use crate::api::types::{AgentRun, AgentRunClaim};
    use crate::runtime::process;
    use crate::runtime::PROVIDER_CODEX;
    use serde_json::{json,Value};
    use super::execute_invocation;
    use std::{path::PathBuf,process::Stdio};
    use crate::{Options,api::types::Task,runtime::AgentRunEnvelope};
    use reqwest::blocking::Client;
    use std::ffi::OsString;
    use std::fs;

    #[cfg(windows)]
    #[test]
    fn executor_codex_selection_persists_revalidates_and_honors_pin() {
        use crate::runtime::process::codex_test_support::Environment;
        let _lock = crate::store::paths::test_env_lock();
        let env = Environment::new();
        let (first, version) = super::resolve_codex_executable().unwrap();
        assert!(version.contains("0.167"));
        let cache = crate::store::paths::agent_home().join("cache/codex-selection.json");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(saved["version"], version);
        std::env::remove_var("HIMIND_CODEX_EXECUTABLE");
        let (reused, _) = super::resolve_codex_executable().unwrap();
        assert_eq!(reused, first);
        // An explicit invalid pin cannot silently fall back to a cached binary.
        std::env::set_var("HIMIND_CODEX_EXECUTABLE", env.root.join("missing.exe"));
        assert!(super::resolve_codex_executable().is_err());
        std::env::set_var("HIMIND_CODEX_EXECUTABLE", &first);
        env.case(&json!({"version_hang":true}));
        assert!(super::resolve_codex_executable_with_cancel(&||Ok(true)).is_err());
        assert!(super::validate_codex_version("codex-cli 0.129.0").is_err());
        assert!(super::validate_codex_version("codex-cli fixture").is_err());
        assert!(super::validate_codex_version("codex-cli 0.167.0-alpha.4.1").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn executor_codex_selection_restart_stale_schema_and_incompatible_fallback() {
        use crate::runtime::process::codex_test_support::Environment;
        let _lock=crate::store::paths::test_env_lock();let env=Environment::new();env.case(&json!({}));
        let exe=std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap();
        let old=env.root.join("old.exe");fs::copy(&exe,&old).unwrap();
        let installed=env.root.join("OpenAI/Codex/bin/fixture-version/codex.exe");fs::create_dir_all(installed.parent().unwrap()).unwrap();fs::copy(&exe,&installed).unwrap();
        let saved_local=std::env::var_os("LOCALAPPDATA");
        std::env::set_var("LOCALAPPDATA",&env.root);
        let result=(|| {
            super::resolve_codex_executable().unwrap();std::env::remove_var("HIMIND_CODEX_EXECUTABLE");
            let child=process::hidden_command(std::env::current_exe().unwrap())
                .args(["runtime::codex::tests::executor_selection_restart_worker","--exact","--ignored","--nocapture"])
                .env("CODEX_SELECTION_EXPECTED",&exe).output().unwrap();
            assert!(child.status.success(),"{}",String::from_utf8_lossy(&child.stdout));
            assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed; 0 failed; 0 ignored"));
            let cache=crate::store::paths::agent_home().join("cache/codex-selection.json");
            for (schema,path,version) in [("invalid",exe.as_os_str(),"codex-cli 0.167.0"),("codex_selection.v1",old.as_os_str(),"codex-cli 0.167.0"),
                ("codex_selection.v1",env.root.join("missing.exe").as_os_str(),"codex-cli 0.167.0")] {
                fs::write(&cache,serde_json::to_vec(&json!({"schema_version":schema,"executable":path,"version":version})).unwrap()).unwrap();
                let (chosen,_)=super::resolve_codex_executable().unwrap();assert_eq!(std::path::Path::new(&chosen).canonicalize().unwrap(),installed.canonicalize().unwrap());
            }
            println!("selection restart process, invalid schema, stale cache, incompatible candidate: verified");
        })();
        match saved_local {Some(value)=>std::env::set_var("LOCALAPPDATA",value),None=>std::env::remove_var("LOCALAPPDATA")};result
    }

    #[test]
    #[ignore = "invoked in a separate process by the executable selection contract"]
    fn executor_selection_restart_worker() {
        let expected=std::env::var_os("CODEX_SELECTION_EXPECTED").expect("selection restart fixture");
        assert!(std::env::var_os("HIMIND_CODEX_EXECUTABLE").is_none());
        assert_eq!(super::resolve_codex_executable().unwrap().0,expected);
    }

    #[cfg(windows)]
    #[test]
    fn executor_agent_run_launch_resume_liveness_final_and_usage() {
        use crate::runtime::process::codex_test_support::{Environment, ProcessProbe};
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc};
        struct TerminateHook(Option<Box<dyn FnOnce()>>);
        impl TerminateHook {
            fn install(hook: impl FnOnce() + 'static) -> Self {
                Self(crate::runtime::owned_tree::BEFORE_TERMINATE.with(|slot| slot.replace(Some(Box::new(hook)))))
            }
        }
        impl Drop for TerminateHook {
            fn drop(&mut self) {
                crate::runtime::owned_tree::BEFORE_TERMINATE.with(|slot| { slot.replace(self.0.take()); });
            }
        }
        let _lock = crate::store::paths::test_env_lock();
        for resume in [false,true] {
            for mode in ["positive","missing","nonzero","negative","no-turn","no-thread","timeout","cancel"] {
                let env=Environment::new();
                let case=match mode {
                    "missing"=>json!({"missing":true}),"nonzero"=>json!({"nonzero":true}),
                    "negative"=>json!({"negative":true}),"no-turn"=>json!({"no_turn":true}),
                    "no-thread"=>json!({"no_thread":true}),
                    "timeout"|"cancel"=>json!({"no_reader":true,"output_before_input":true}),
                    _=>json!({"output_before_input":true}),
                };
                env.case(&case);
                let mut claim=claim(&env.root);
                claim.run.access_mode=crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED.into();
                claim.run.instruction="中文😀\"$()\\shell\n".repeat(90000);
                if resume {claim.resume_provider_session_id="prior-thread".into();}
                let invocation=build_invocation(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap(),&claim).unwrap();
                let observed=Arc::new(AtomicBool::new(false));
                let (probes,_terminate_hook)=if matches!(mode,"timeout"|"cancel") {
                    let root=env.root.clone();
                    let signal=observed.clone();
                    let (ready,retained)=mpsc::sync_channel(1);
                    let hook=TerminateHook::install(move || {
                        // Explicit terminate runs before OwnedTree::drop: gate it here,
                        // not in BEFORE_CLOSE, while the observer retains both handles.
                        retained.recv_timeout(std::time::Duration::from_secs(1))
                            .expect("fixture observer did not retain parent and child before termination");
                    });
                    let probes=std::thread::spawn(move || {
                        let parent=ProcessProbe::open(&root.join("root.pid"));
                        let child=ProcessProbe::open(&root.join("child.pid"));
                        assert!(parent.is_alive());assert!(child.is_alive());
                        signal.store(true,Ordering::Release);
                        ready.send(()).expect("fixture termination handshake disconnected");
                        parent.assert_exited();child.assert_exited();
                    });
                    (Some(probes),Some(hook))
                }else{(None,None)};
                let started=std::time::Instant::now();
                let result=super::execute_invocation(&invocation,&claim,
                    &||Ok(mode=="cancel" && observed.load(Ordering::Acquire)),1);
                if let Some(probes)=probes {probes.join().unwrap();}
                assert!(started.elapsed()<std::time::Duration::from_secs(4),"{mode}");
                let args=fs::read_to_string(env.root.join("argv.json")).unwrap_or_else(|_|panic!("fixture did not start: {:?}",result.as_ref().err().map(|e|e.to_string())));
                assert!(args.contains("danger-full-access") && args.contains("-C"));
                assert_eq!(args.contains("resume"),resume);
                assert!(!args.contains("--model"));
                if mode=="positive" {
                    let result=result.unwrap();
                    assert_eq!(result["session_id"],if resume {"prior-thread"}else{"fixture-thread"});
                    if resume { assert!(result["usage"].is_null()); assert_eq!(result["usage_receipt"]["increment_status"],"unknown_baseline"); }
                    else { assert_eq!(result["usage"]["input_tokens"],1); }
                    assert_eq!(result["cost_status"],"unknown");
                    assert_eq!(fs::read_to_string(env.root.join("stdin.txt")).unwrap(),invocation.prompt);
                } else {
                    let error=result.unwrap_err();
                    let failure=error.downcast_ref::<crate::runtime::deepseek_harness::WorkflowRuntimeFailure>().unwrap();
                    if mode=="nonzero" {
                        assert_eq!(failure.provenance.as_ref().unwrap()["exit_code"],7);
                        if resume { assert!(failure.usage.is_none()); } else { assert_eq!(failure.usage.as_ref().unwrap().input_tokens,1); }
                    }
                    if mode=="negative" {assert_eq!(failure.report.as_ref().unwrap()["passed"],false);}
                    assert!(!error.to_string().contains("claim-secret"));
                }
            }
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "explicit real personal Codex model execution in task-generated workspace"]
    fn executor_real_codex_launch_resume_shell_preflight() {
        // SQLite requires the host-provided short spelling of this same
        // authorized resource; canonicalize only for scope comparisons.
        let root=std::path::PathBuf::from(std::env::var_os("EXECUTOR_SHORT_ROOT").expect("task resource short root"))
            .join(format!("live-codex-{:016x}",rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let native=std::env::var("HIMIND_EXECUTOR_LIVE_HIMIND").as_deref()==Ok("1");
        let nonce=format!("{:032x}",rand::random::<u128>());
        let baseline=fs::read("remediation-input/baseline-manifest.json").unwrap();
        let baseline_value:Value=serde_json::from_slice(&baseline).unwrap();assert!(!baseline_value["files"].as_object().unwrap().is_empty());
        let seed=json!({"value":40,"nonce":nonce,"historical_baseline_sha256":format!("{:x}",sha2::Sha256::digest(&baseline)),
            "candidate_id":std::env::var("HIMIND_EXECUTOR_CANDIDATE_ID").unwrap_or_default()});
        fs::write(root.join("baseline.json"),serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
        let mut handoff=if native {Some(super::super::executor_live_handoff::LiveHandoff::new(&root,seed["candidate_id"].as_str().unwrap(),&nonce).unwrap())}else{None};
        let h1=if let Some(helper)=handoff.as_mut() {
            let run=helper.begin(0).unwrap();
            let result=super::super::executor_live::stage(&root,"baseline.json","himind-first.json","himind-first",&run)
                .unwrap_or_else(|e|panic!("native_handoff_first_failed: {}; diagnostics redacted",super::super::executor_live::redacted_code(e.as_ref())));
            helper.complete(0,serde_json::from_value(result["usage"].clone()).unwrap(),&result).unwrap();Some(result)
        }else{None};
        let mut claim=claim(&root);
        if let Some(helper)=handoff.as_mut() {claim.run.id=helper.begin(1).unwrap();}
        claim.run.access_mode=crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED.into();
        claim.run.instruction=if native {String::from("Work ONLY in the current directory. Do not load skills, call MCP, inspect config or other paths. Read only accepted-handoff-codex-launch.json and its listed himind-first.json dependency. Verify the listed artifact SHA256 before consuming it. Use your shell to create codex-launch.json with passed:true, the same nonce and value increased by 1; do not hardcode the input. Also write launch-ok to launch.txt and read it back. Return only JSON {\"passed\":true,\"summary\":\"launch-ok\"} if successful. Do not claim independent verification.")} else {String::from("Work ONLY in the current directory; no skills or MCP. Use shell to write launch-ok to launch.txt then read it back. Return only JSON {\"passed\":true,\"summary\":\"launch-ok\"}.")};
        std::env::var_os("HIMIND_CODEX_EXECUTABLE").expect("host-provided Codex executable");
        let (executable,version)=super::resolve_codex_executable().unwrap();
        let invocation=build_invocation(executable.clone(),&claim).unwrap();
        let first=super::execute_invocation(&invocation,&claim,&||Ok(false),180).unwrap();
        assert!(fs::read_to_string(root.join("launch.txt")).unwrap().contains("launch-ok"));
        let session=first["session_id"].as_str().unwrap();assert!(!session.is_empty());
        if let Some(helper)=handoff.as_mut() {helper.complete(1,serde_json::from_value(first["usage"].clone()).unwrap(),&first).unwrap();}
        let h2=if native {
            let c:Value=serde_json::from_slice(&fs::read(root.join("codex-launch.json")).unwrap()).unwrap();
            assert_eq!(super::super::executor_live_handoff::artifact_value(&c["value"]).unwrap(),42);assert_eq!(c["nonce"],nonce);assert_eq!(c["passed"],true);
            let helper=handoff.as_mut().unwrap();let run=helper.begin(2).unwrap();
            let result=super::super::executor_live::stage(&root,"codex-launch.json","himind-second.json","himind-second",&run)
                .unwrap_or_else(|e|panic!("native_handoff_second_failed: {}; diagnostics redacted",super::super::executor_live::redacted_code(e.as_ref())));
            helper.complete(2,serde_json::from_value(result["usage"].clone()).unwrap(),&result).unwrap();Some(result)
        }else{None};
        claim.resume_provider_session_id=session.into();
        claim.run.id=if let Some(helper)=handoff.as_mut() {helper.begin(3).unwrap()}else{"real-resume-run".into()};
        claim.run.instruction=if native {String::from("Resume in the SAME working directory. Work ONLY here; no skills, MCP or config access. Read only accepted-handoff-codex-resume.json, its listed himind-second.json dependency and launch.txt. Verify the listed artifact SHA256 before consuming it. Confirm launch-ok. Use your shell to create codex-resume.json with passed:true, the same nonce and value increased by 1; do not hardcode input. Write resume-ok to resume.txt. Return only {\"passed\":true,\"summary\":\"resume-ok\"} if successful.")} else {String::from("Resume ONLY in this directory; no skills or MCP. Use shell to read launch.txt, confirm launch-ok and write resume-ok to resume.txt. Return only {\"passed\":true,\"summary\":\"resume-ok\"}.")};
        let resumed=build_invocation(executable,&claim).unwrap();
        let second=super::execute_invocation(&resumed,&claim,&||Ok(false),180).unwrap();
        assert!(fs::read_to_string(root.join("resume.txt")).unwrap().contains("resume-ok"));
        assert_eq!(second["session_id"],first["session_id"]);
        assert_ne!(second["run_id"],first["run_id"]);
        for key in ["input_tokens","output_tokens"] {
            let a=first["usage_receipt"]["provider_raw"][key].as_u64().unwrap();
            let b=second["usage_receipt"]["provider_raw"][key].as_u64().unwrap();
            assert!(b>a,"live counters must grow across distinct provider turns");
            assert_eq!(second["usage"][key],b-a);
        }
        assert_eq!(second["usage_receipt"]["counter_scope"],"provider_session_cumulative");
        if let Some(helper)=handoff.as_mut() {helper.complete(3,serde_json::from_value(second["usage"].clone()).unwrap(),&second).unwrap();}
        if native {
            let c:Value=serde_json::from_slice(&fs::read(root.join("codex-resume.json")).unwrap()).unwrap();
            assert_eq!(super::super::executor_live_handoff::artifact_value(&c["value"]).unwrap(),44);assert_eq!(c["nonce"],nonce);assert_eq!(c["passed"],true);
            let stages=vec![h1.unwrap(),h2.unwrap()];
            assert_eq!(stages[0]["configuration_sha256"],stages[1]["configuration_sha256"]);
            assert_eq!(stages[0]["runtime_sha256"],stages[1]["runtime_sha256"]);
            let artifacts=["baseline.json","himind-first.json","codex-launch.json","himind-second.json","codex-resume.json"].into_iter()
                .map(|name|(name.to_string(),json!(super::super::executor_live::hash(&root.join(name)).unwrap()))).collect::<serde_json::Map<String,Value>>();
            let native_evidence=json!({"classification":"real_himind_current_candidate_execution","passed":true,
                "candidate_id":seed["candidate_id"],"service_id":"lucen","model":"himind:lucen:gpt-6.1-sol",
                "stages":stages,"accepted_envelopes":handoff.as_ref().unwrap().envelopes,"artifact_checks":handoff.as_ref().unwrap().checks,"artifacts":artifacts,"artifact_root":root,"historical_baseline_sha256":seed["historical_baseline_sha256"],
                "codex_launch_run":first["run_id"],"codex_resume_run":second["run_id"],"codex_session":first["session_id"],
                "bidirectional_handoff_executed":true,"handoff_chain":["baseline","himind-first","codex-launch","himind-second","codex-resume"],
                "handoff_classification":"host_delegated_current_source_product_runtime_calls_with_scoped_host_artifact_delivery",
                "helper_only":true,"mission_pipeline_acceptance":false});
            let evidence_root=std::path::PathBuf::from(std::env::var_os("SERVICE_UI_RUNTIME_ROOT").unwrap());
            fs::write(evidence_root.join("executor-lifecycle-repair/real-himind.json"),serde_json::to_vec_pretty(&native_evidence).unwrap()).unwrap();
        }
        let evidence=json!({"classification":"real_personal_codex_model_launch_resume_shell",
            "version":version.trim(),"launch":first,"resume":second,
            "launch_args":invocation.args,"resume_args":resumed.args,
            "permission":"danger-full-access","model_source":"personal_cli_config","host_verify_invoked":false});
        fs::write(root.join("result.json"),serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
        println!("real Codex launch/resume shell result: {}",root.join("result.json").display());
    }

    #[cfg(windows)]
    #[test]
    fn executor_product_transport_exact_labca_rejects_stale_identity_before_provider() {
        use crate::runtime::codex_usage::{Meter,read_receipt,default_root};
        use process::codex_test_support::{Environment,wait_file};
        let _lock=crate::store::paths::test_env_lock();
        let env=Environment::new();
        let execute=|id:&str,resume:&str| {
            let mut claim=claim(&env.root);claim.run.id=id.into();claim.resume_provider_session_id=resume.into();
            let invocation=build_invocation(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap(),&claim).unwrap();
            execute_invocation(&invocation,&claim,&||Ok(false),5)
        };
        env.case(&json!({"session_s":true,"usage_100_10":true}));
        let l=execute("L","").unwrap();assert_eq!(l["usage"]["input_tokens"],100);
        // A enters the real transport and invalidates the baseline, then the
        // parent kills its OS process before any receipt can settle.
        env.case(&json!({"no_reader":true}));
        let mut command=process::hidden_command(std::env::current_exe().unwrap());
        command.args(["runtime::codex::tests::executor_abc_interrupted_worker","--exact","--ignored","--nocapture"])
            .env("ABC_PRODUCT_ROOT",&env.root).stdout(Stdio::null()).stderr(Stdio::inherit());
        let (mut child,tree)=crate::runtime::owned_tree::OwnedTree::spawn(&mut command).unwrap();
        wait_file(&env.root.join("child.pid"));assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();assert!(!child.wait().unwrap().success());drop(tree);
        assert!(read_receipt(&default_root(),"A").unwrap().is_none());
        env.case(&json!({"usage_200_20":true}));
        let b=execute("B","s").unwrap();assert!(b["usage"].is_null());
        assert_eq!(b["usage_receipt"]["increment_status"],"unknown_baseline");
        env.case(&json!({"usage_230_23":true}));
        let c=execute("C","s").unwrap();assert_eq!(c["usage"]["input_tokens"],30);assert_eq!(c["usage"]["output_tokens"],3);
        let before=std::fs::read_to_string(env.root.join("provider.invocations")).unwrap();
        env.case(&json!({"usage_250_25":true}));
        assert!(execute("A","s").unwrap_err().to_string().contains("unsettled run identity"));
        drop(Meter::begin(&default_root(),"D","s").unwrap());
        assert!(execute("A","s").unwrap_err().to_string().contains("unsettled run identity"));
        let db=super::super::codex_usage::connection(&default_root().join("usage.sqlite3")).unwrap();
        let pending:String=db.query_row("SELECT pending FROM sessions WHERE id='s'",[],|r|r.get(0)).unwrap();assert_eq!(pending,"D");
        assert_eq!(std::fs::read_to_string(env.root.join("provider.invocations")).unwrap(),before);
        let known=(l["usage"]["input_tokens"].as_u64().unwrap()+c["usage"]["input_tokens"].as_u64().unwrap(),
            l["usage"]["output_tokens"].as_u64().unwrap()+c["usage"]["output_tokens"].as_u64().unwrap());
        assert_eq!(known,(130,13));assert_ne!(known,(280,28));
        assert_eq!(read_receipt(&default_root(),"C").unwrap().unwrap()["run_increment"]["input_tokens"],30);
        println!("U2-01 actual_Codex_transport: L=100/10; A=OS_process_killed_unsettled; B=200/20_unknown; C=230/23_delta30/3; stale_A250/25_rejected_before_spawn; pending_D_unchanged; known=130/13; cost=unknown");
    }

    #[test]
    #[ignore="only exact L/A/B/C product regression invokes this subprocess"]
    fn executor_abc_interrupted_worker() {
        let root=PathBuf::from(std::env::var_os("ABC_PRODUCT_ROOT").unwrap());
        let mut claim=claim(&root);claim.run.id="A".into();claim.resume_provider_session_id="s".into();
        let invocation=build_invocation(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap(),&claim).unwrap();
        let _=execute_invocation(&invocation,&claim,&||Ok(false),30);
        panic!("parent must kill A before settle");
    }

    #[cfg(windows)]
    #[test]
    fn executor_agent_run_managed_claim_caller_receipt_recovery_preserves_failure() {
        use crate::{agent_core_contracts::{LocalRunStatus,RuntimeEventType},store::local_runs::LocalRunLedger};
        use std::{io::{Read,Write},net::TcpListener,sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}}};
        let _lock=crate::store::paths::test_env_lock();
        let env=process::codex_test_support::Environment::new();env.case(&json!({"usage_40_4":true}));
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let address=listener.local_addr().unwrap();
        let stop=Arc::new(AtomicBool::new(false));let statuses=Arc::new(Mutex::new(Vec::<String>::new()));
        let claim=json!({"run":{"id":"managed-provider","instruction":"isolated complete managed entry fixture",
            "status":"claimed","created_by_user_id":"fixture-user","runtime_provider":"personal.codex","access_mode":"exhibit_linked"},
            "claim_token":"fixture-not-a-credential","workspace_path":env.root,"ai_model":"","access_mode":"exhibit_linked"}).to_string();
        let flag=stop.clone();let observed=statuses.clone();
        let server=std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let (mut stream,_)=match listener.accept() {Ok(value)=>value,Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{
                    std::thread::sleep(std::time::Duration::from_millis(2));continue;},Err(e)=>panic!("{e}")};
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
                let mut request=Vec::new();let mut buffer=[0;4096];
                let (end,length)=loop {
                    let count=stream.read(&mut buffer).unwrap();assert!(count>0);request.extend_from_slice(&buffer[..count]);
                    assert!(request.len()<65536);
                    if let Some(end)=request.windows(4).position(|w|w==b"\r\n\r\n") {
                        let header=String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length=header.lines().find_map(|line|line.strip_prefix("content-length:").map(|s|s.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        break (end+4,length);
                    }
                };
                while request.len()<end+length {let count=stream.read(&mut buffer).unwrap();assert!(count>0);request.extend_from_slice(&buffer[..count]);}
                let header=String::from_utf8_lossy(&request[..end]);let route=header.lines().next().unwrap();
                let body=if route.contains("/claim ") {claim.clone()}else if route.contains("/status ") {
                    let value:Value=serde_json::from_slice(&request[end..end+length]).unwrap();
                    observed.lock().unwrap().push(value["status"].as_str().unwrap().into());"{}".into()
                }else {assert!(route.contains("/cancel "),"unexpected fixture route {route}");r#"{"status":"running","cancel_requested":false}"#.into()};
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        struct Server {stop:Arc<AtomicBool>,thread:Option<std::thread::JoinHandle<()>>}
        impl Drop for Server {fn drop(&mut self) {self.stop.store(true,Ordering::SeqCst);self.thread.take().unwrap().join().unwrap();}}
        let _server=Server{stop,thread:Some(server)};
        let mut options=Options::from_env();options.state_path=env.root.join("agent-state.json");options.set_api_base(&format!("http://{address}"));
        crate::app::remote_execution::save(&options.state_path,&crate::app::remote_execution::RemoteExecutionSettings {
            enabled:true,access_mode:"exhibit_linked".into(),default_provider:"personal.codex".into()}).unwrap();
        let task:Task=serde_json::from_value(json!({"id":"managed-task","type":"agent_run","task_type":"agent_run",
            "payload":{"run_id":"managed-provider","runtime_provider":"personal.codex"}})).unwrap();
        let envelope=AgentRunEnvelope{run_id:"managed-provider".into(),runtime_provider:"personal.codex".into(),work_item_id:String::new(),attempt_no:1};
        let client=Client::builder().no_proxy().timeout(std::time::Duration::from_secs(5)).build().unwrap();
        crate::runtime::codex_usage_delivery::AFTER_COMMIT.with(|v|v.set(true));
        let error=super::execute(&client,&options,"fixture-agent",&task,&envelope).unwrap_err().to_string();assert!(error.contains("injected crash"));
        let ledger=LocalRunLedger::open_default().unwrap();let failed=ledger.list_runs(10).unwrap().remove(0);
        assert_eq!(failed.status,LocalRunStatus::Failed);assert!(failed.usage.is_none());
        std::thread::scope(|scope| {
            let handles=(0..4).map(|_|scope.spawn(|| {
                let error=super::execute(&client,&options,"fixture-agent",&task,&envelope).unwrap_err().to_string();
                assert!(error.contains("business outcome is unknown"));
            })).collect::<Vec<_>>();for h in handles {h.join().unwrap();}
        });
        let replay=ledger.get_run(&failed.run_id).unwrap().unwrap();
        assert_eq!(replay.status,LocalRunStatus::Failed);assert_eq!(replay.error,failed.error);
        assert_eq!((replay.usage.as_ref().unwrap().input_tokens,replay.usage.as_ref().unwrap().output_tokens),(40,4));
        let events=ledger.list_events(&failed.run_id).unwrap();
        assert_eq!(events.iter().filter(|e|e.payload["usage_receipt"]["run_id"]=="managed-provider").count(),1);
        assert!(events.iter().any(|e|e.event_type==RuntimeEventType::Error));assert!(!events.iter().any(|e|e.event_type==RuntimeEventType::ToolCompleted));
        assert_eq!(std::fs::read_to_string(env.root.join("provider.invocations")).unwrap().lines().count(),1);
        assert!(!statuses.lock().unwrap().iter().any(|s|s=="succeeded"));
        assert!(statuses.lock().unwrap().iter().filter(|s|s.as_str()=="failed").count()>=5);
        println!("U2-02 full_execute_managed_claim_execute_claimed_caller; loopback_API_and_CLI_fixtures; AFTER_COMMIT_helper_error_not_process_death; concurrent_recovery=4; receipt_events=1; provider_calls=1; original_failure_preserved=true; business_success=false");
    }

    #[cfg(windows)]
    #[test]
    fn executor_agent_run_receipt_crash_restart_and_concurrent_recovery_are_once() {
        use crate::{agent_core_contracts::{LocalRunStatus,RuntimeEventType},store::local_runs::LocalRunLedger};
        let _lock=crate::store::paths::test_env_lock();
        let env=process::codex_test_support::Environment::new();env.case(&json!({}));
        let ledger=LocalRunLedger::new(env.root.join("agent-caller.sqlite3"));
        let mut context=crate::capability::types::InvocationContext::new(crate::capability::types::InvocationSource::DashboardWorker,"dashboard-user:fixture");
        context.request_id="agent-local".into();context.workspace_ref=env.root.to_string_lossy().into();
        let mut run=crate::agent_core_service::AgentCoreRunRecorder::with_ledger(ledger.clone()).begin("fixture-agent",&context,"personal.codex").unwrap();
        let mut claim=claim(&env.root);claim.run.id="agent-provider".into();
        let invocation=build_invocation(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap(),&claim).unwrap();
        crate::runtime::codex_usage_delivery::AFTER_COMMIT.with(|v|v.set(true));
        let error=crate::runtime::execute_agent_codex_with_receipt(&ledger,&claim.run.id,&mut run,
            ||super::execute_invocation(&invocation,&claim,&||Ok(false),5)).unwrap_err();
        assert!(error.to_string().contains("injected crash"));
        assert!(ledger.get_run(&run.run_id).unwrap().unwrap().usage.is_none());
        let output=process::hidden_command(std::env::current_exe().unwrap()).args([
            "runtime::codex::tests::executor_agent_receipt_restart_worker","--exact","--ignored","--nocapture"])
            .env("AGENT_RECEIPT_RESTART_ROOT",&env.root).output().unwrap();
        assert!(output.status.success(),"{} {}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed; 0 ignored"));
        std::thread::scope(|scope| {
            let handles=(0..4).map(|_| {let ledger=&ledger;scope.spawn(move || {
                let mut run=ledger.get_run("run_agent-local").unwrap().unwrap();
                assert!(crate::runtime::execute_agent_codex_with_receipt(ledger,"agent-provider",&mut run,||panic!("provider replay")).is_err());
                run.usage.unwrap().input_tokens
            })}).collect::<Vec<_>>();
            let values=handles.into_iter().map(|h|h.join().unwrap()).collect::<Vec<_>>();
            assert!(values.iter().all(|v|*v==values[0] && *v>0));
        });
        let recovered=ledger.get_run("run_agent-local").unwrap().unwrap();
        assert_eq!(recovered.status,LocalRunStatus::Failed);
        let events=ledger.list_events("run_agent-local").unwrap();
        assert_eq!(events.iter().filter(|e|e.payload["usage_receipt"]["run_id"]=="agent-provider").count(),1);
        assert!(events.iter().any(|e|e.event_type==RuntimeEventType::Error));
        assert!(!events.iter().any(|e|e.event_type==RuntimeEventType::ToolCompleted));
    }

    #[test]
    #[ignore="invoked by AgentRun receipt crash regression"]
    fn executor_agent_receipt_restart_worker() {
        use crate::{store::local_runs::LocalRunLedger,agent_core_service::AgentCoreRunRecorder};
        let root=std::path::PathBuf::from(std::env::var_os("AGENT_RECEIPT_RESTART_ROOT").unwrap());
        let ledger=LocalRunLedger::new(root.join("agent-caller.sqlite3"));
        assert_eq!(crate::runtime::codex_usage_delivery::recover_bound(&crate::runtime::codex_usage::default_root()).unwrap(),1);
        let mut run=ledger.get_run("run_agent-local").unwrap().unwrap();
        let error=crate::runtime::execute_agent_codex_with_receipt(&ledger,"agent-provider",&mut run,||panic!("provider replay after restart")).unwrap_err();
        assert!(error.to_string().contains("business outcome is unknown"));
        assert!(error.downcast_ref::<crate::runtime::deepseek_harness::WorkflowRuntimeFailure>().unwrap().report.is_none());
        assert!(run.usage.is_some());
        AgentCoreRunRecorder::with_ledger(ledger).fail(run,&error.to_string()).unwrap();
    }

    fn claim(workspace: &std::path::Path) -> AgentRunClaim {
        AgentRunClaim {
            run: AgentRun {
                id: "run-1".to_string(),
                work_item_id: String::new(),
                parent_run_id: String::new(),
                root_run_id: String::new(),
                parent_work_item_id: String::new(),
                root_work_item_id: String::new(),
                attempt_no: 0,
                instruction: "Fix the failing tests".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "user-1".to_string(),
                requested_agent_id: String::new(),
                project_id: String::new(),
                exhibit_pid: String::new(),
                runtime_provider: PROVIDER_CODEX.to_string(),
                access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
                input: json!({"suite":"unit"}),
            },
            claim_token: "claim-secret".to_string(),
            workspace_path: workspace.to_string_lossy().to_string(),
            ai_model: String::new(),
            resume_provider_session_id: String::new(),
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

    #[test]
    fn invocation_resumes_recorded_codex_session() {
        let root = std::env::temp_dir().join(format!("himind-codex-resume-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut claim = claim(&root);
        claim.resume_provider_session_id = "thread-prior".to_string();
        let invocation = build_invocation(OsString::from("codex"), &claim).unwrap();
        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(args.contains("exec resume"));
        assert!(args.contains("thread-prior"));
        assert!(args.ends_with(" -"));
        assert!(args.contains("-C "));
        assert!(args.contains("-s workspace-write -a never exec resume"));
        process::remove_file_if_present(&invocation.result_path);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invocation_rejects_unsafe_resume_session_id() {
        let root = std::env::temp_dir().join(format!(
            "himind-codex-resume-invalid-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut claim = claim(&root);
        claim.resume_provider_session_id = "thread\ninvalid".to_string();
        assert!(build_invocation(OsString::from("codex"), &claim).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
