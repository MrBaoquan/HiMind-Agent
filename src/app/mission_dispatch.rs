//! Mission task dispatch. A Mission task is executed as a one-step
//! WorkflowPackage through the existing WorkflowRunner; the Mission row only
//! records the business task and its LocalRun attempt id.

use serde_json::{json, Value};
use fs4::fs_std::FileExt;
use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use crate::capability::service::CapabilityGateway;
use crate::mission::{MissionStore, MissionTaskSpec};
use crate::workflow::{
    execute_runtime_step, WorkflowPackage, WorkflowRunner, WorkflowStep, WorkflowStepExecution,
    WorkflowStepExecutor,
};
use crate::Options;

struct RuntimeStepExecutor<'a> {
    options: &'a Options,
    canceled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl WorkflowStepExecutor for RuntimeStepExecutor<'_> {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        execute_runtime_step(
            package,
            step,
            input,
            Some(self.options),
            &|| Ok((self.canceled)()),
        )
    }
}

#[tauri::command]
pub(crate) async fn dispatch_next_mission_task(
    mission_id: String,
    options: tauri::State<'_, crate::app::commands::AgentState>,
) -> Result<serde_json::Value, String> {
    let gateway = options.capability_gateway.clone();
    let options = options.options.clone();
    let thread_mission_id = mission_id.clone();
    std::thread::Builder::new()
        .name(format!("mission-dispatch-{mission_id}"))
        .spawn(move || {
            if let Err(error) = dispatch_next(&gateway, &options, &thread_mission_id) {
                let message = error.to_string();
                let _ = MissionStore::open_default().and_then(|store| {
                    store.append_event(&thread_mission_id, "mission_failed", "", &message, json!({"error": message}))
                });
                eprintln!("mission dispatch {thread_mission_id} failed: {error}");
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(json!({"accepted": true, "mission_id": mission_id}))
}

pub(crate) fn control_mission(mission_id: &str, action: &str) -> Result<crate::mission::MissionView, Box<dyn Error>> {
    let store = MissionStore::open_default()?;
    let mission = store.control(mission_id, action)?;
    if action == "cancel" {
        let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
        let runner = WorkflowRunner::open_default()?;
        for task in &mission.tasks {
            for id in [&task.attempt_run_id, &task.verification_run_id] {
                if !id.is_empty() {
                    if let Some(run) = ledger.get_run(id)? { let _ = runner.cancel(run, "Mission canceled by user")?; }
                }
            }
        }
        let root = crate::store::paths::agent_home().join("mission-dispatch-locks");
        std::fs::create_dir_all(&root)?;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(root.join(format!("{mission_id}.lock")))?;
        if file.try_lock_exclusive()? { store.release_canceled_claims(mission_id)?; }
    }
    Ok(mission)
}

struct CanceledClaimCleanup { store: MissionStore, mission_id: String }
impl Drop for CanceledClaimCleanup {
    fn drop(&mut self) { let _ = self.store.release_canceled_claims(&self.mission_id); }
}

pub(crate) fn dispatch_next(
    _gateway: &CapabilityGateway,
    options: &Options,
    mission_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let store = MissionStore::open_default()?;
    let mission = store.get(mission_id)?;
    if mission.status != crate::mission::MissionStatus::Active {
        return Ok(json!({"dispatched": false, "reason": "mission is not active"}));
    }
    crate::mission::validate_mission_workflow_binding(&mission)?;
    // OS locks are released after process exit; two threads or clients cannot
    // advance the same queued LocalRun at once, even with the same lease owner.
    let lock_root = crate::store::paths::agent_home().join("mission-dispatch-locks");
    std::fs::create_dir_all(&lock_root)?;
    let dispatch_lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(lock_root.join(format!("{mission_id}.lock")))?;
    if !dispatch_lock.try_lock_exclusive()? {
        return Ok(json!({"dispatched": false, "reason": "mission dispatch is already active"}));
    }
    // Drops before dispatch_lock: a canceled worker releases its claim only after Runtime exits.
    let _cancel_cleanup = CanceledClaimCleanup { store: store.clone(), mission_id: mission_id.to_string() };
    if let Some(pending) = store.next_verification_task(mission_id)? {
        return run_verification(&store, options, &mission, &pending);
    }
    if let Some(running) = store.running_task(mission_id)? {
        return resume_or_reconcile_attempt(&store, options, &mission, &running);
    }
    let Some(task) = store.next_ready_task(mission_id)? else {
        return Ok(json!({"dispatched": false, "reason": "no ready task"}));
    };
    if let Some((owner_mission, owner_task, _)) = store.workspace_claim(&mission.workspace.root)? {
        let reason = format!("workspace has an active mission attempt: {owner_mission}/{owner_task}");
        return defer_dispatch(&store, mission_id, &task.id, &reason, json!({
            "owner_mission_id": owner_mission,
            "owner_task_id": owner_task,
        }));
    }
    validate_execution_binding(&task)?;
    let package = task_package(&mission.mission_id, &mission.title, &task, &mission.workspace.root)?;
    let runner = WorkflowRunner::open_default()?;
    let request_id = format!("mission_{}_{}", mission.mission_id, task.id);
    let expected_run_id = format!("workflow_run_{request_id}");
    let input = implementation_input(&mission, &task);
    if let Err(error) = store.begin_task_attempt(mission_id, &task.id, &expected_run_id) {
        return defer_dispatch(&store, mission_id, &task.id, &error.to_string(), json!({}));
    }
    let run = match runner.start("local-agent", &package, &request_id, &input) {
        Ok(run) => run,
        Err(error) => {
            let reason = format!("LocalRun could not be created: {error}");
            let _ = store.complete_task_attempt(mission_id, &task.id, &expected_run_id, false, &[], &reason)?;
            return Err(error);
        }
    };
    if run.run_id != expected_run_id {
        let reason = format!("Runner returned unexpected run id {}", run.run_id);
        let _ = store.complete_task_attempt(mission_id, &task.id, &expected_run_id, false, &[], &reason)?;
        let _ = crate::store::local_runs::LocalRunLedger::open_default()?.mark_run_failed(&run.run_id, &reason);
        return defer_dispatch(&store, mission_id, &task.id, &reason, json!({"run_id": run.run_id}));
    }
    execute_implementation_attempt(&store, options, &mission, &task, &package, &input, &runner, run)
}

fn defer_dispatch(
    store: &MissionStore,
    mission_id: &str,
    task_id: &str,
    reason: &str,
    details: Value,
) -> Result<Value, Box<dyn Error>> {
    store.append_event(
        mission_id,
        "task_dispatch_deferred",
        task_id,
        reason,
        details,
    )?;
    Ok(json!({"dispatched": false, "reason": reason, "task_id": task_id}))
}

fn mission_time_remaining(mission: &crate::mission::MissionView) -> std::time::Duration {
    let Some(budget) = &mission.budget else { return std::time::Duration::ZERO; };
    let Some(confirmation) = &mission.confirmation else { return std::time::Duration::ZERO; };
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(&confirmation.confirmed_at) else { return std::time::Duration::ZERO; };
    let elapsed = chrono::Utc::now().signed_duration_since(started).num_seconds().max(0) as u64;
    std::time::Duration::from_secs((u64::from(budget.max_minutes) * 60).saturating_sub(elapsed))
}

fn mission_should_stop(id: &str, original: &crate::mission::MissionView) -> bool {
    mission_time_remaining(original).is_zero() || MissionStore::open_default().and_then(|store| store.get(id))
        .map(|mission| matches!(mission.status, crate::mission::MissionStatus::Canceled | crate::mission::MissionStatus::Failed | crate::mission::MissionStatus::Completed)).unwrap_or(true)
}

fn mission_executor<'a>(options: &'a Options, mission: &crate::mission::MissionView) -> RuntimeStepExecutor<'a> {
    let mission = mission.clone();
    RuntimeStepExecutor { options, canceled: Arc::new(move || mission_should_stop(&mission.mission_id, &mission)) }
}

fn implementation_input(mission: &crate::mission::MissionView, task: &MissionTaskSpec) -> Value {
    json!({
        "workspace_root": mission.workspace.root,
        "mission_id": mission.mission_id,
        "task_id": task.id,
        "task": task,
        "approved_paths": mission.confirmation.as_ref().map(|value| value.approved_paths.clone()).unwrap_or_default(),
        "approved_absolute_read_paths": mission.confirmation.as_ref().map(|value| value.approved_absolute_read_paths.clone()).unwrap_or_default(),
        "approved_absolute_write_paths": mission.confirmation.as_ref().map(|value| value.approved_absolute_write_paths.clone()).unwrap_or_default(),
        "candidate_acceptance": mission.acceptance_criteria,
    })
}

fn execute_implementation_attempt(
    store: &MissionStore,
    options: &Options,
    mission: &crate::mission::MissionView,
    task: &MissionTaskSpec,
    package: &WorkflowPackage,
    input: &Value,
    runner: &WorkflowRunner,
    run: crate::agent_core_contracts::LocalRun,
) -> Result<Value, Box<dyn Error>> {
    let task_id = task.id.clone();
    if matches!(run.status, crate::agent_core_contracts::LocalRunStatus::Running | crate::agent_core_contracts::LocalRunStatus::Waiting) {
        let reason = "the bound LocalRun is already active or waiting; it was not started again";
        return defer_dispatch(store, &mission.mission_id, &task_id, reason, json!({
            "run_id": run.run_id,
            "run_status": run.status,
        }));
    }
    let executor = mission_executor(options, mission);
    let run_id = run.run_id.clone();
    let outcome = match runner.run_ready(package, run, input, &executor) {
        Ok(outcome) => outcome,
        Err(error) => {
            if error.to_string().contains("leased by another process") {
                return defer_dispatch(store, &mission.mission_id, &task.id, "LocalRun is owned by another process", json!({"run_id": run_id}));
            }
            let _ = crate::store::local_runs::LocalRunLedger::open_default()?
                .mark_run_failed(&run_id, &error.to_string());
            return Err(error);
        }
    };
    let usage_result = record_or_require_usage(
        store,
        mission,
        &task.id,
        &outcome.run.run_id,
        outcome.run.usage.as_ref(),
    );
    if let Err(error) = usage_result {
        let _ = store.complete_task_attempt(
            &mission.mission_id,
            &task.id,
            &outcome.run.run_id,
            false,
            &[format!("local-run:{}", outcome.run.run_id)],
            &error.to_string(),
        );
        return Err(error);
    }
    if !outcome.run.status.is_terminal() {
        return defer_dispatch(store, &mission.mission_id, &task.id, "LocalRun is waiting; resume it before continuing",
            json!({"run_id": outcome.run.run_id, "run_status": outcome.run.status}));
    }
    let run_succeeded = outcome.run.status == crate::agent_core_contracts::LocalRunStatus::Succeeded;
    let has_required_checks = !task.required_checks.is_empty();
    let has_acceptance = !task.acceptance_criteria.is_empty();
    let updated = if run_succeeded && has_required_checks && has_acceptance {
        store.await_task_verification(
            &mission.mission_id,
            &task_id,
            &outcome.run.run_id,
            "执行器已完成，等待独立验证 Agent",
        )?
    } else {
        let reason = if !run_succeeded {
            outcome.run.error.clone()
        } else if !has_required_checks {
            "任务没有声明必需检查，不能进入验收".to_string()
        } else {
            "任务没有声明验收标准，不能进入验收".to_string()
        };
        store.complete_task_attempt(&mission.mission_id, &task_id, &outcome.run.run_id, false,
            &[format!("local-run:{}", outcome.run.run_id)], &reason)?
    };
    Ok(json!({
        "dispatched": true,
        "task_id": task_id,
        "run_id": outcome.run.run_id,
        "run_status": outcome.run.status,
        "verification_pending": run_succeeded && has_required_checks && has_acceptance,
        "mission": updated,
    }))
}

fn resume_or_reconcile_attempt(
    store: &MissionStore,
    options: &Options,
    mission: &crate::mission::MissionView,
    pending: &crate::mission::MissionTaskView,
) -> Result<Value, Box<dyn Error>> {
    let task = &pending.spec;
    let run_id = &pending.attempt_run_id;
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let Some(run) = ledger.get_run(run_id)? else {
        let updated = store.complete_task_attempt(
            &mission.mission_id, &task.id, run_id, false, &[],
            "Mission attempt references a missing LocalRun after restart",
        )?;
        return Ok(json!({"dispatched": false, "recovered": true, "reason": "bound LocalRun is missing", "mission": updated}));
    };
    match run.status {
        crate::agent_core_contracts::LocalRunStatus::Queued => {
            validate_execution_binding(task)?;
            let package = task_package(&mission.mission_id, &mission.title, task, &mission.workspace.root)?;
            let input = implementation_input(mission, task);
            let runner = WorkflowRunner::open_default()?;
            execute_implementation_attempt(store, options, mission, task, &package, &input, &runner, run)
        }
        crate::agent_core_contracts::LocalRunStatus::Running
        | crate::agent_core_contracts::LocalRunStatus::Waiting => defer_dispatch(store, &mission.mission_id, &task.id,
            "the bound LocalRun is already active or waiting; it was not started again",
            json!({"run_id": run.run_id, "run_status": run.status})),
        crate::agent_core_contracts::LocalRunStatus::Succeeded => {
            let has_checks = !task.required_checks.is_empty() && !task.acceptance_criteria.is_empty();
            let updated = if has_checks {
                store.await_task_verification(&mission.mission_id, &task.id, run_id, "从持久化 LocalRun 恢复完成态")?
            } else {
                store.complete_task_attempt(&mission.mission_id, &task.id, run_id, false,
                    &[format!("local-run:{run_id}")], "任务缺少必需检查或验收标准")?
            };
            Ok(json!({"dispatched": false, "recovered": true, "run_status": run.status, "mission": updated}))
        }
        crate::agent_core_contracts::LocalRunStatus::Failed
        | crate::agent_core_contracts::LocalRunStatus::Canceled => {
            let reason = if run.error.trim().is_empty() {
                "LocalRun ended before Mission state was finalized".to_string()
            } else {
                run.error.clone()
            };
            let updated = store.complete_task_attempt(&mission.mission_id, &task.id, run_id, false,
                &[format!("local-run:{run_id}")], &reason)?;
            Ok(json!({"dispatched": false, "recovered": true, "run_status": run.status, "mission": updated}))
        }
    }
}

fn candidate_identity(mission: &crate::mission::MissionView, task: &MissionTaskSpec) -> Result<Value, Box<dyn Error>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut identity = crate::workflow::current_candidate_identity(&json!({
        "workspace_root": mission.workspace.root, "allow_dirty": true,
    }))?;
    let repository = identity["candidate"]["repository_root"].as_str().ok_or("repository root missing")?;
    let path_key = |path: &str| path.replace('\\', "/").trim_start_matches("//?/").trim_end_matches('/').to_ascii_lowercase();
    let repository = path_key(repository);
    let mut paths = task.read_paths.iter().chain(&task.write_paths)
        .filter(|path| Path::new(path).is_absolute())
        .filter(|path| {
            let key = path_key(path);
            key != repository && !key.starts_with(&format!("{repository}/"))
        }).cloned().collect::<Vec<_>>();
    paths.sort(); paths.dedup();
    let mut hash = Sha256::new();
    for raw in paths {
        hash.update(raw.as_bytes()); hash.update([0]);
        if !Path::new(&raw).exists() { hash.update(b"missing"); continue; }
        let mut entries = walkdir::WalkDir::new(&raw).follow_links(false).into_iter()
            .take(10_001).collect::<Result<Vec<_>, _>>()?;
        if entries.len() > 10_000 { return Err("external candidate exceeds 10000 entries".into()); }
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        let mut total_bytes = 0u64;
        for entry in entries {
            if entry.file_type().is_symlink() { return Err("external candidate symlinks require a resolved explicit path".into()); }
            hash.update(entry.path().to_string_lossy().as_bytes()); hash.update([0]);
            if entry.file_type().is_file() {
                let size = entry.metadata()?.len();
                total_bytes = total_bytes.saturating_add(size);
                if total_bytes > 256 * 1024 * 1024 { return Err("external candidate exceeds 256 MiB".into()); }
                hash.update(size.to_le_bytes());
                let mut file = std::fs::File::open(entry.path())?;
                let mut buffer = [0u8; 65536];
                loop { let count = file.read(&mut buffer)?; if count == 0 { break; } hash.update(&buffer[..count]); }
            }
        }
    }
    identity["candidate"]["external_paths_sha256"] = json!(format!("{:x}", hash.finalize()));
    Ok(identity)
}

fn run_verification(
    store: &MissionStore,
    options: &Options,
    mission: &crate::mission::MissionView,
    pending: &crate::mission::MissionTaskView,
) -> Result<Value, Box<dyn Error>> {
    let task = &pending.spec;
    let evidence_refs = vec![format!("local-run:{}", pending.attempt_run_id)];
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let implementation_run = ledger.get_run(&pending.attempt_run_id)?
        .ok_or_else(|| format!("implementation run not found: {}", pending.attempt_run_id))?;
    let implementation_events = ledger.list_events(&pending.attempt_run_id)?;
    let implementation_evidence = implementation_events.iter()
        .filter(|event| matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted))
        .filter_map(|event| event.payload.get("evidence").cloned())
        .collect::<Vec<_>>();
    let implementation_output = implementation_events.iter().rev()
        .find(|event| matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted))
        .map(|event| event.payload.clone())
        .unwrap_or(Value::Null);
    let expected_verification_id = format!("workflow_run_mission_{}_{}_verify", mission.mission_id, task.id);
    if let Some(current) = ledger.get_run(&expected_verification_id)? {
        if matches!(current.status, crate::agent_core_contracts::LocalRunStatus::Running | crate::agent_core_contracts::LocalRunStatus::Waiting) {
            return defer_dispatch(store, &mission.mission_id, &task.id, "Independent verification is active or waiting", json!({"run_id": current.run_id}));
        }
    }
    if store.get(&mission.mission_id)?.status != crate::mission::MissionStatus::Active {
        return Ok(json!({"dispatched": false, "reason": "Mission paused or canceled before host checks"}));
    }
    let host_checks = crate::mission::run_required_checks(
        Path::new(&mission.workspace.root), &task.required_checks,
        mission_time_remaining(mission).min(std::time::Duration::from_secs(300)),
        &|| Ok(mission_should_stop(&mission.mission_id, mission)),
    );
    let candidate_before = candidate_identity(mission, task)?;
    let package = verification_package(mission, task)?;
    let runner = WorkflowRunner::open_default()?;
    let request_id = format!("mission_{}_{}_verify", mission.mission_id, task.id);
    let proposed_input = json!({
        "workspace_root": mission.workspace.root,
        "mission_id": mission.mission_id,
        "task_id": task.id,
        "implementation_run_id": pending.attempt_run_id,
        "acceptance_criteria": task.acceptance_criteria,
        "required_checks": task.required_checks,
        "approved_paths": task.write_paths,
        "approved_absolute_read_paths": mission.confirmation.as_ref().map(|value| value.approved_absolute_read_paths.clone()).unwrap_or_default(),
        "approved_absolute_write_paths": mission.confirmation.as_ref().map(|value| value.approved_absolute_write_paths.clone()).unwrap_or_default(),
        "evidence_refs": evidence_refs,
        "implementation_status": implementation_run.status,
        "implementation_output": implementation_output,
        "implementation_evidence": implementation_evidence,
        "expected_plan_sha256": mission.plan_sha256,
        "candidate_before": candidate_before.get("candidate").cloned().unwrap_or(Value::Null),
        "host_checks": host_checks,
    });
    let expected_run_id = format!("workflow_run_{request_id}");
    let (run, input) = if let Some(run) = ledger.get_run(&expected_run_id)? {
        let interaction = ledger.get_interaction(&run.interaction_id)?.ok_or("verification interaction is missing")?;
        let saved_input = interaction.business_context.get("input").cloned().ok_or("verification input is missing")?;
        (run, saved_input)
    } else {
        (runner.start("local-verifier", &package, &request_id, &proposed_input)?, proposed_input)
    };
    let candidate_before = json!({"candidate": input.get("candidate_before")});
    if input.get("expected_plan_sha256").and_then(Value::as_str) != Some(mission.plan_sha256.as_str()) {
        return Err("saved verification input is bound to an obsolete plan".into());
    }
    if run.run_id == pending.attempt_run_id {
        return Err("verification run must differ from implementation run".into());
    }
    store.begin_verification_attempt(&mission.mission_id, &task.id, &run.run_id)?;
    if matches!(run.status, crate::agent_core_contracts::LocalRunStatus::Running | crate::agent_core_contracts::LocalRunStatus::Waiting) {
        return defer_dispatch(
            store,
            &mission.mission_id,
            &task.id,
            "the bound verification LocalRun is already active or waiting; it was not started again",
            json!({"run_id": run.run_id, "run_status": run.status}),
        );
    }
    let executor = mission_executor(options, mission);
    let run_id = run.run_id.clone();
    let outcome = match runner.run_ready(&package, run, &input, &executor) {
        Ok(outcome) => outcome,
        Err(error) => {
            if error.to_string().contains("leased by another process") {
                return defer_dispatch(store, &mission.mission_id, &task.id, "LocalRun is owned by another process", json!({"run_id": run_id}));
            }
            let _ = crate::store::local_runs::LocalRunLedger::open_default()?
                .mark_run_failed(&run_id, &error.to_string());
            return Err(error);
        }
    };
    let usage_result = record_or_require_usage(
        store,
        mission,
        &task.id,
        &outcome.run.run_id,
        outcome.run.usage.as_ref(),
    );
    if let Err(error) = usage_result {
        let _ = store.record_verification_result(
            &mission.mission_id,
            &task.id,
            &pending.attempt_run_id,
            &outcome.run.run_id,
            false,
            &evidence_refs,
            &error.to_string(),
        );
        return Err(error);
    }
    if !outcome.run.status.is_terminal() {
        return defer_dispatch(store, &mission.mission_id, &task.id, "LocalRun is waiting; resume it before continuing",
            json!({"run_id": outcome.run.run_id, "run_status": outcome.run.status}));
    }
    let run_succeeded = outcome.run.status == crate::agent_core_contracts::LocalRunStatus::Succeeded;
    let verification_output = verification_events_output(&outcome.run.run_id)?;
    let candidate_after = candidate_identity(mission, task);
    let validation = verification_output.as_ref().map(|value| {
        let current_candidate = candidate_after.as_ref()
            .ok()
            .and_then(|identity| identity.get("candidate"))
            .unwrap_or(&Value::Null);
        validate_verification_output(
            value,
            &task.id,
            &mission.plan_sha256,
            candidate_before.get("candidate").unwrap_or(&Value::Null),
            current_candidate,
            &host_checks,
        )
    });
    let passed = run_succeeded
        && candidate_after.is_ok()
        && validation.as_ref().is_some_and(|result| result.is_ok());
    let mut final_evidence = evidence_refs;
    final_evidence.push(format!("local-run:{}", outcome.run.run_id));
    let reason = if passed {
        String::new()
    } else if let Err(error) = candidate_after {
        format!("无法在独立验收后冻结 candidate identity: {error}")
    } else if let Some(Err(error)) = validation {
        error
    } else if outcome.run.error.trim().is_empty() {
        "验证 Agent 未返回可验证的独立证据".to_string()
    } else {
        outcome.run.error.clone()
    };
    let host_event = store.append_event(
        &mission.mission_id, "host_verification_checked", &task.id,
        if passed { "宿主检查与独立验收证据一致" } else { "宿主验收未通过" },
        json!({
            "plan_sha256": mission.plan_sha256,
            "implementation_run_id": pending.attempt_run_id,
            "verification_run_id": outcome.run.run_id,
            "passed": passed,
            "checks": host_checks,
            "candidate_before": candidate_before.get("candidate"),
            "reason": reason,
        }),
    )?;
    final_evidence.push(format!("mission-event:{}", host_event.event_id));
    let updated = store.record_verification_result(
        &mission.mission_id, &task.id, &pending.attempt_run_id,
        &outcome.run.run_id, passed, &final_evidence, &reason,
    )?;
    Ok(json!({
        "dispatched": true,
        "verification": true,
        "task_id": task.id,
        "run_id": outcome.run.run_id,
        "run_status": outcome.run.status,
        "passed": passed,
        "mission": updated,
    }))
}

fn verification_events_output(run_id: &str) -> Result<Option<Value>, Box<dyn Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    Ok(ledger.list_events(run_id)?.into_iter().rev().find_map(|event| {
        if matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted) {
            event.payload.get("output").cloned().map(|output| {
                if output.get("passed").is_some() { return output; }
                if let Some(text) = output.get("summary").and_then(Value::as_str) {
                    let content = text.find("```json").map(|i| &text[i + 7..]).unwrap_or(text);
                    for (index, _) in content.match_indices('{') {
                        if let Some(Ok(value)) = serde_json::Deserializer::from_str(&content[index..]).into_iter::<Value>().next() {
                            if value.get("passed").is_some() { return value; }
                        }
                    }
                }
                output
            })
        } else {
            None
        }
    }))
}

fn validate_verification_output(
    output: &Value,
    task_id: &str,
    expected_plan_sha256: &str,
    candidate_before: &Value,
    candidate_after: &Value,
    host_checks: &[crate::mission::HostCheck],
) -> Result<(), String> {
    if output.get("passed").and_then(Value::as_bool) != Some(true) {
        return Err("验证 Agent 未返回 passed=true".to_string());
    }
    if output.get("plan_sha256").and_then(Value::as_str) != Some(expected_plan_sha256) {
        return Err("验证证据未绑定当前 plan_sha256".to_string());
    }
    if !output.get("verified_task_ids").and_then(Value::as_array).is_some_and(|tasks| {
        tasks.iter().any(|value| value.as_str() == Some(task_id))
    }) {
        return Err(format!("验证证据未覆盖当前任务: {task_id}"));
    }
    let checks = output.get("checks").and_then(Value::as_array).ok_or("验证证据缺少 checks")?;
    if checks.is_empty() {
        return Err("验证证据 checks 不能为空".to_string());
    }
    if checks.iter().any(|check| {
        check.get("command").and_then(Value::as_str).is_none_or(|command| command.trim().is_empty())
            || check.get("exit_code").and_then(Value::as_i64) != Some(0)
    }) {
        return Err("验证证据包含失败、缺失退出码或空命令".to_string());
    }
    if host_checks.is_empty() {
        return Err("宿主没有执行任何必需检查".to_string());
    }
    if host_checks.iter().any(|check| check.exit_code != 0) {
        return Err("宿主必需检查存在失败或超时".to_string());
    }
    for host_check in host_checks {
        if !checks.iter().any(|check| {
            check.get("command").and_then(Value::as_str) == Some(host_check.command.as_str())
                && check.get("exit_code").and_then(Value::as_i64) == Some(0)
                && check.get("detail").and_then(Value::as_str).is_some_and(|detail| {
                    detail.contains("source=host")
                        && (!host_check.output_sha256.is_empty()
                            && detail.contains(&host_check.output_sha256))
                })
        }) {
            return Err(format!(
                "验证证据未覆盖宿主检查或缺少 source=host/output_sha256: {}",
                host_check.command
            ));
        }
    }
    if candidate_before.get("external_paths_sha256") != candidate_after.get("external_paths_sha256") {
        return Err("验证期间已授权外部路径发生变化".to_string());
    }
    let candidate = output.get("candidate").ok_or("验证证据缺少 candidate identity")?;
    for field in ["candidate_id", "commit_sha", "tree_digest", "status_digest"] {
        if candidate.get(field).and_then(Value::as_str).is_none_or(|value| value.is_empty())
            || candidate.get(field) != candidate_before.get(field)
            || candidate.get(field) != candidate_after.get(field)
        {
            return Err(format!("验证证据 candidate {field} 与当前工作区不一致"));
        }
    }
    Ok(())
}

fn validate_execution_binding(task: &MissionTaskSpec) -> Result<(), Box<dyn Error>> {
    crate::mission::validate_execution_binding(task)?;
    let executor = task.execution.executor.trim();
    if executor == "himind.fixture"
        && std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() != Ok("1")
    {
        return Err("mission task executor himind.fixture is available only under the explicit workflow fixture switch".into());
    }
    if executor == "personal.codex" && crate::runtime::codex::probe().status != "ready" {
        return Err("mission task executor personal.codex is unavailable; binding was not replaced".into());
    }
    if executor == "personal.github-copilot" && crate::runtime::copilot::probe().status != "ready" {
        return Err("mission task executor personal.github-copilot is unavailable; binding was not replaced".into());
    }
    if crate::runtime::acp::is_provider(executor) {
        let ready = crate::runtime::acp::probe_installations()
            .into_iter()
            .find(|report| report.provider == executor)
            .is_some_and(|report| report.status == "ready");
        if !ready {
            return Err(format!("mission task executor {executor} failed ACP capability negotiation; binding was not replaced").into());
        }
    }
    Ok(())
}

fn record_or_require_usage(
    store: &MissionStore,
    mission: &crate::mission::MissionView,
    task_id: &str,
    run_id: &str,
    usage: Option<&crate::agent_core_contracts::LocalRunUsage>,
) -> Result<crate::agent_core_contracts::LocalRunUsage, Box<dyn Error>> {
    if let Some(usage) = usage {
        return store.record_run_usage(&mission.mission_id, task_id, run_id, usage);
    }
    let budget = mission.budget.as_ref().ok_or("confirmed mission has no budget")?;
    let requires_usage = budget.max_total_tokens.is_some() || budget.max_cost.is_some();
    if requires_usage {
        return Err(format!(
            "run {run_id} did not report usage; token/cost budget is declared and dispatch must fail closed"
        ).into());
    }
    Ok(store.record_run_usage(
        &mission.mission_id,
        task_id,
        run_id,
        &crate::agent_core_contracts::LocalRunUsage {
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost: 0.0,
            currency: String::new(),
            billing_owner: "unreported".to_string(),
        },
    )?)
}

fn role_entry(task: &MissionTaskSpec) -> Value {
    json!({
        "label": task.execution.role_id,
        "brief": format!("你是专家团队中的 {}。只负责当前任务，不扩大范围，不自行宣布最终通过。", task.execution.role_id),
        "runtime_provider": task.execution.executor,
        "model": task.execution.model,
        "tool_policy": "default",
    })
}

fn task_package(
    mission_id: &str,
    mission_title: &str,
    task: &MissionTaskSpec,
    workspace_root: &str,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let prompt = format!(
        "你是专家团队的{}。只执行下面这一项任务，不扩大范围。\n任务：{}\n目标：{}\n允许读取：{}\n允许写入：{}\n验收：{}\n必须检查：{}\n绝对路径必须严格限于宿主确认记录中的授权范围；Mission 授权是业务策略，不代表操作系统沙箱。\n完成后输出 JSON：{{\"passed\":true|false,\"summary\":\"...\",\"evidence\":[\"...\"]}}。自报 passed 只是送验建议，最终由宿主检查。",
        task.execution.role_id,
        task.title,
        task.objective,
        task.read_paths.join(", "),
        task.write_paths.join(", "),
        task.acceptance_criteria.join("；"),
        task.required_checks.join("；"),
    );
    let value = json!({
        "schema_version": "workflow_package.v1",
        "id": format!("com.himind.mission.{}", mission_id),
        "version": "1.0.0",
        "name": mission_title,
        "description": "Mission task attempt",
        "min_agent_version": "0.3.90",
        "supported_runtimes": [task.execution.executor.clone()],
        "roles": { task.execution.role_id.clone(): role_entry(task) },
        "steps": [{
            "id": task.id,
            "title": task.title,
            "kind": "runtime",
            "role_id": task.execution.role_id,
            "risk_level": if task.write_paths.is_empty() { "read_only" } else { "local_write" },
            "execution_mode": "long_running",
            "runtime": {
                "provider": task.execution.executor,
                "prompt": prompt,
                "workspace_path": workspace_root,
                "model": task.execution.model,
                "tool_policy": "default",
                "timeout_seconds": 900
            }
        }],
        "artifacts": [],
        "ui": { "mode": "standard" }
    });
    Ok(serde_json::from_value(value)?)
}

fn verification_package(
    mission: &crate::mission::MissionView,
    task: &MissionTaskSpec,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let prompt = format!(
        "你是独立验证 Agent，不得修改代码，不得相信开发 Agent 自报通过。检查任务 {} 的实际改动、验收标准和必需检查。只读审查。输入 host_checks 是宿主在当前候选上实际执行的独立检查记录，含退出码、输出哈希和经脱敏的测试摘要；它是宿主证据，不是 worker 自报。你必须在输出 checks 中逐条引用这些宿主记录：command 与 exit_code 原样复制，detail 注明 source=host 并引用对应输出哈希。若客户端沙箱能实际执行检查，也可以额外提供执行结果；若不能，不得伪造 worker 执行，也不得因此拒绝宿主已核验且与当前 candidate 绑定的全绿检查。仍须独立检查代码、授权范围和完成标准；宿主检查全绿不自动代表全部标准满足。只输出 JSON：{{\"passed\":true|false,\"verified_task_ids\":[\"{}\"],\"plan_sha256\":\"复制输入中的 expected_plan_sha256\",\"candidate\":{{\"candidate_id\":\"复制输入 candidate_before.candidate_id\",\"commit_sha\":\"复制输入 candidate_before.commit_sha\",\"tree_digest\":\"复制输入 candidate_before.tree_digest\",\"status_digest\":\"复制输入 candidate_before.status_digest\"}},\"checks\":[{{\"command\":\"宿主检查命令原样\",\"exit_code\":0,\"detail\":\"source=host; output_sha256=...\"}}],\"reasons\":[]}}。每一条 checks 必须能追溯到宿主记录或真实执行结果且 exit_code 为 0；没有任务覆盖、当前计划 hash 或 candidate identity 时 passed 必须为 false。",
        task.id, task.id
    );
    let value = json!({
        "schema_version": "workflow_package.v1",
        "id": format!("com.himind.mission.verify.{}", mission.mission_id),
        "version": "1.0.0",
        "name": "Mission verification",
        "description": "Independent verification attempt",
        "min_agent_version": "0.3.90",
        "supported_runtimes": [task.execution.executor.clone()],
        "steps": [{
            "id": "VERIFY",
            "title": "独立验证",
            "kind": "runtime",
            "role_id": "verifier",
            "risk_level": "read_only",
            "execution_mode": "long_running",
            "runtime": {
                "provider": task.execution.executor,
                "prompt": prompt,
                "workspace_path": mission.workspace.root,
                "model": task.execution.model,
                "tool_policy": "default",
                "timeout_seconds": 900
            }
        }],
        "artifacts": [],
        "ui": { "mode": "standard" }
    });
    Ok(serde_json::from_value(value)?)
}

#[cfg(test)]
mod tests {
    use super::validate_verification_output;
    use serde_json::json;

    fn candidate() -> serde_json::Value {
        json!({
            "candidate_id": "candidate-1",
            "commit_sha": "commit-1",
            "tree_digest": "tree-1",
            "status_digest": "status-1"
        })
    }

    fn output() -> serde_json::Value {
        json!({
            "passed": true,
            "verified_task_ids": ["fix"],
            "plan_sha256": "plan-1",
            "candidate": candidate(),
            "checks": [{
                "command": "cargo test",
                "exit_code": 0,
                "detail": "source=host; output_sha256=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
            }]
        })
    }

    #[test]
    fn verification_evidence_requires_host_bound_identity_and_successful_checks() {
        let candidate = candidate();
        let host_checks = vec![crate::mission::HostCheck {
            command: "cargo test".to_string(),
            exit_code: 0,
            output_sha256: "x".repeat(64),
            test_summary: String::new(),
            elapsed_ms: 1,
            error: String::new(),
        }];
        assert!(validate_verification_output(&output(), "fix", "plan-1", &candidate, &candidate, &host_checks).is_ok());

        let mut failed_host = host_checks.clone(); failed_host[0].exit_code = 7;
        assert!(validate_verification_output(&output(), "fix", "plan-1", &candidate, &candidate, &failed_host)
            .unwrap_err().contains("宿主必需检查"));
        assert!(validate_verification_output(&output(), "fix", "plan-1", &candidate, &candidate, &[]).is_err());
        let mut missing_candidate = output(); missing_candidate["candidate"] = json!({});
        assert!(validate_verification_output(&missing_candidate, "fix", "plan-1", &json!({}), &json!({}), &host_checks).is_err());
        let mut uncovered = host_checks.clone(); uncovered[0].command = "npm test".to_string();
        assert!(validate_verification_output(&output(), "fix", "plan-1", &candidate, &candidate, &uncovered).is_err());

        let mut forged = output();
        forged["checks"][0]["exit_code"] = json!(1);
        assert!(validate_verification_output(&forged, "fix", "plan-1", &candidate, &candidate, &host_checks)
            .unwrap_err().contains("退出码"));

        let mut unbound = output();
        unbound["checks"][0]["detail"] = json!("ran locally");
        assert!(validate_verification_output(&unbound, "fix", "plan-1", &candidate, &candidate, &host_checks)
            .unwrap_err().contains("source=host"));
        let mut wrong_digest = output();
        wrong_digest["checks"][0]["detail"] = json!(format!("source=host; output_sha256={}", "y".repeat(64)));
        assert!(validate_verification_output(&wrong_digest, "fix", "plan-1", &candidate, &candidate, &host_checks)
            .unwrap_err().contains("output_sha256"));

        let mut stale_plan = output();
        stale_plan["plan_sha256"] = json!("old-plan");
        assert!(validate_verification_output(&stale_plan, "fix", "plan-1", &candidate, &candidate, &host_checks)
            .unwrap_err().contains("plan_sha256"));

        let mut changed_candidate = candidate.clone();
        changed_candidate["tree_digest"] = json!("tree-2");
        assert!(validate_verification_output(&output(), "fix", "plan-1", &candidate, &changed_candidate, &host_checks)
            .unwrap_err().contains("tree_digest"));

        assert!(validate_verification_output(&output(), "other-task", "plan-1", &candidate, &candidate, &host_checks)
            .unwrap_err().contains("未覆盖当前任务"));
    }
}
