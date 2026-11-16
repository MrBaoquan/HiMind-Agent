//! Persistent host-side Mission supervisor.
//!
//! The desktop Agent owns this loop. Client transports (Tauri UI, MCP, CLI)
//! can request a dispatch, but their process lifetime is not the execution
//! owner. Each scan is best-effort: one broken Mission must not stop others.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::capability::service::CapabilityGateway;
use crate::mission::{MissionEvent, MissionStatus, MissionStore, MissionView, TaskStatus};
use crate::Options;

const SCAN_LIMIT: usize = 100;

/// True when the host should ask the existing dispatcher to advance this
/// Mission. Assignment, confirmation and terminal states are deliberately
/// excluded: a supervisor is not a consent bypass.
pub(crate) fn is_dispatchable(mission: &MissionView) -> bool {
    if mission.status != MissionStatus::Active || mission.confirmation.is_none() {
        return false;
    }
    mission.tasks.iter().any(|task| matches!(
        task.status,
        TaskStatus::Ready | TaskStatus::Running | TaskStatus::AwaitingVerification
    ))
}

/// Plan a scan without touching the dispatcher. Keeping this pure makes the
/// safety rule testable without creating process-global LocalRuns.
pub(crate) fn dispatchable_ids(missions: &[MissionView]) -> Vec<String> {
    missions.iter().filter(|mission| is_dispatchable(mission))
        .map(|mission| mission.mission_id.clone()).collect()
}

/// Identify the exact failed state that needs one host-owned repair. A
/// fingerprint prevents a deterministic generation failure from being retried
/// on every supervisor tick; a later task run changes the fingerprint.
fn failed_follow_up_fingerprint(mission: &MissionView) -> Option<String> {
    if mission.status != MissionStatus::Active || mission.confirmation.is_none() {
        return None;
    }
    if mission.tasks.iter().any(|task| matches!(
        task.status,
        TaskStatus::Ready | TaskStatus::Running | TaskStatus::AwaitingVerification
    )) {
        return None;
    }
    let budget = mission.budget.as_ref()?;
    let repair_count = mission.tasks.iter().filter(|task| !task.spec.repair_of.is_empty()).count();
    if repair_count >= budget.max_repair_attempts as usize {
        return None;
    }
    let parent_ids = mission.tasks.iter()
        .filter(|task| !task.spec.repair_of.is_empty())
        .map(|task| task.spec.repair_of.as_str())
        .collect::<std::collections::HashSet<_>>();
    let failed_repair_ids = mission.tasks.iter()
        .filter(|task| !task.spec.repair_of.is_empty() && task.status == TaskStatus::Failed)
        .map(|task| task.spec.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut failed = mission.tasks.iter()
        .filter(|task| task.status == TaskStatus::Failed)
        .filter(|task| failed_repair_ids.contains(task.spec.id.as_str()) || !parent_ids.contains(task.spec.id.as_str()))
        .collect::<Vec<_>>();
    failed.sort_by(|left, right| left.spec.id.cmp(&right.spec.id));
    if failed.is_empty() {
        return None;
    }
    let mut parts = vec![
        format!("plan={}", mission.plan_sha256),
        format!("repair_count={repair_count}"),
        format!("max_repair_attempts={}", budget.max_repair_attempts),
    ];
    for task in failed {
        parts.push(format!("{}:{}:{}:{:?}", task.spec.id, task.attempt_run_id, task.verification_run_id, task.verification_passed));
    }
    Some(parts.join("|"))
}

/// True when the host should generate a bounded follow-up now. The source
/// client may disappear; this decision must not depend on a UI being open.
pub(crate) fn needs_host_follow_up(mission: &MissionView, events: &[MissionEvent]) -> bool {
    let Some(fingerprint) = failed_follow_up_fingerprint(mission) else {
        return false;
    };
    let retry_after = events.iter().filter(|event| matches!(event.event_type.as_str(), "mission_resume" | "budget_extended"))
        .map(|event| event.sequence).max().unwrap_or(0);
    !events.iter().any(|event| {
        event.event_type == "mission_follow_up_failed"
            && event.sequence > retry_after
            && event.payload.get("fingerprint").and_then(serde_json::Value::as_str) == Some(fingerprint.as_str())
    })
}

/// One best-effort scan. Errors are isolated per Mission and returned only as
/// observational text so the supervisor can keep serving other Missions.
pub(crate) fn scan_once(
    gateway: &CapabilityGateway,
    options: &Options,
    store: &MissionStore,
) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    scan_with(
        store,
        |mission_id| {
            crate::app::mission_dispatch::dispatch_next(gateway, options, mission_id)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
        |mission_id| {
            crate::app::mission_commands::generate_mission_follow_up_sync(options, mission_id)
                .map(|_| ())
        },
    )
}

/// Scan with injected dispatch and follow-up functions. Production uses the
/// real host dispatcher and the coordination Agent; tests can prove isolation
/// and retry suppression without creating external model calls.
fn scan_with<F, G>(
    store: &MissionStore,
    mut dispatch: F,
    mut follow_up: G,
) -> Result<Vec<(String, String)>, Box<dyn Error>>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let missions = store.list(SCAN_LIMIT)?;
    let mut errors = Vec::new();
    for mission in &missions {
        // Admission includes independent verification of the implementation.
        // Paid usage may exhaust the next task's budget while this task is
        // finishing; waiting now would hide it from next_verification_task.
        if mission.status == MissionStatus::Active && !mission.tasks.iter().any(|task|
            matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            if let Some(reason) = store.follow_up_budget_exhaustion_reason(mission)? {
                store.record_budget_exhausted(&mission.mission_id, &reason)?;
            }
        }
    }
    for mission_id in dispatchable_ids(&missions) {
        if let Err(message) = dispatch(&mission_id) {
            let _ = store.append_event(&mission_id, "mission_supervisor_error", "",
                &error_summary("系统暂时无法继续执行：", &message), serde_json::json!({"error": message}));
            errors.push((mission_id, message));
        }
    }
    for mission in &missions {
        let (mission, events) = store.follow_up_control_context(
            &mission.mission_id, failed_follow_up_fingerprint)?;
        let Some(fingerprint) = failed_follow_up_fingerprint(&mission) else { continue; };
        if !needs_host_follow_up(&mission, &events) {
            store.wait_for_follow_up(&mission.mission_id, "先前返工生成失败", &fingerprint)?;
            continue;
        }
        if let Err(message) = follow_up(&mission.mission_id) {
            let _ = store.append_event(
                &mission.mission_id,
                "mission_follow_up_failed",
                "",
                &error_summary("系统暂时无法生成跟进任务：", &message),
                serde_json::json!({"error": message, "fingerprint": fingerprint}),
            );
            store.wait_for_follow_up(&mission.mission_id, &message, &fingerprint)?;
            errors.push((mission.mission_id.clone(), message));
        }
    }
    Ok(errors)
}

fn error_summary(prefix: &str, message: &str) -> String {
    // Keep full diagnostics in the payload; an oversized summary must never
    // prevent the user from seeing that dispatch failed.
    format!("{prefix}{message}").chars().take(1900).collect()
}

/// Spawn the process-lifetime supervisor. The stop flag is used by tests or a
/// future controlled shutdown; normal Agent shutdown terminates the process.
pub(crate) fn start(gateway: CapabilityGateway, options: Options, stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("himind-mission-supervisor".to_string())
        .spawn(move || {
            if let Err(error)=crate::runtime::codex_usage_delivery::recover_bound(&crate::runtime::codex_usage::default_root()) {
                eprintln!("Codex receipt startup recovery failed: {error}");
            }
            // The Agent host is the durable execution owner. At startup there
            // cannot be a live workflow process owned by this same host, so
            // running attempts are reconciled back to `queued` and resumed via
            // their persisted run id. Never create a replacement LocalRun.
            if let Ok(ledger) = crate::store::local_runs::LocalRunLedger::open_default() {
                match ledger.recover_running_runs(true, 200) {
                    Ok(recovered) if !recovered.is_empty() => eprintln!(
                        "mission supervisor recovered {} interrupted workflow runs",
                        recovered.len()
                    ),
                    Ok(_) => {}
                    Err(error) => eprintln!("mission supervisor some run recovery failed: {error}"),
                }
            }
            while !stop.load(Ordering::Relaxed) {
                let result = MissionStore::open_default()
                    .and_then(|store| scan_once(&gateway, &options, &store));
                match result {
                    Ok(errors) => {
                        for (mission_id, error) in errors {
                            eprintln!("mission supervisor {mission_id} deferred: {error}");
                        }
                    }
                    Err(error) => eprintln!("mission supervisor scan failed: {error}"),
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) { return; }
                    thread::sleep(Duration::from_millis(500));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::{
        ExecutionBinding, MissionBudget, MissionConfirmation, MissionPlanProposal, MissionTaskSpec,
        MissionTaskView, MissionWorkspace,
    };
    use std::fs;

    fn mission(id: &str, status: MissionStatus, task_status: TaskStatus, confirmed: bool) -> MissionView {
        let spec = MissionTaskSpec {
            id: "task".to_string(), title: "Task".to_string(), objective: String::new(),
            depends_on: vec![], read_paths: vec![], write_paths: vec![],
            acceptance_criteria: vec!["ok".to_string()], required_checks: vec!["node --test".to_string()],
            execution: ExecutionBinding { expert: None,
                executor: "himind.builtin".to_string(), model: String::new(),
                role_id: "implementer".to_string(), service_id: String::new(),
            }, repair_of: String::new(),
        };
        MissionView {
            scheduling: None, schema_version: "mission.v1".to_string(), mission_id: id.to_string(), title: "Task".to_string(),
            objective: String::new(), status,
            workspace: MissionWorkspace { permission_mode: String::new(), root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()], plan_revision: 1, plan_sha256: "a".repeat(64),
            plan_summary: String::new(), required_acceptance: vec![], proposed_tasks: vec![],
            budget: Some(MissionBudget { max_rounds: 1, max_minutes: 1, max_repair_attempts: 0, max_total_tokens: None, max_cost: None, currency: None }),
            cost_usage: Default::default(), effective_wait: None,
            usage: None, handoff: None, workflow: None, assignment: None,
            plan_mode: crate::mission::MissionPlanMode::Generated,
            confirmation: confirmed.then(|| MissionConfirmation {
                plan_sha256: "a".repeat(64), selected_task_ids: vec!["task".to_string()],
                approved_absolute_read_paths: vec![], approved_absolute_write_paths: vec![],
                approved_paths: vec![], confirmed_by: "local".to_string(), confirmed_at: "2026-11-10T00:00:00Z".to_string(),
            }),
            tasks: vec![MissionTaskView { spec, status: task_status, attempt_run_id: String::new(),
                verification_run_id: String::new(), verification_passed: None, evidence_refs: vec![] }],
            created_at: "2026-11-10T00:00:00Z".to_string(), updated_at: "2026-11-10T00:00:00Z".to_string(),
        }
    }

    fn persist_confirmed_mission(store: &MissionStore, title: &str) -> String {
        let created = store.create(&crate::mission::MissionCreateInput {
            title: title.to_string(),
            objective: String::new(),
            workspace: MissionWorkspace { permission_mode: String::new(), root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()],
        }).unwrap();
        let proposal = MissionPlanProposal {
            scheduling: None, summary: "test".to_string(),
            tasks: vec![mission("unused", MissionStatus::Active, TaskStatus::Ready, true).tasks[0].spec.clone()],
            required_acceptance: vec!["ok".to_string()],
            budget: MissionBudget { max_rounds: 1, max_minutes: 1, max_repair_attempts: 0, max_total_tokens: None, max_cost: None, currency: None },
        };
        let planned = store.propose_plan(&created.mission_id, &proposal).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["task".to_string()], &[], &[], &[], "test-user").unwrap();
        created.mission_id
    }

    #[test]
    fn one_broken_mission_does_not_stop_the_scan_and_is_recorded() {
        let root = std::env::temp_dir().join(format!(
            "himind-supervisor-isolation-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = MissionStore::new(root.join("missions.sqlite3"));
        let broken = persist_confirmed_mission(&store, "broken");
        let healthy = persist_confirmed_mission(&store, "healthy");
        let mut visited = Vec::new();
        let errors = scan_with(&store, |mission_id| {
            visited.push(mission_id.to_string());
            if mission_id == broken { Err("中文错误".repeat(1000)) } else { Ok(()) }
        }, |_| Ok(())).unwrap();
        assert_eq!(visited.len(), 2);
        assert!(visited.contains(&broken));
        assert!(visited.contains(&healthy));
        assert_eq!(errors, vec![(broken.clone(), "中文错误".repeat(1000))]);
        let snapshot = store.snapshot(&broken, 0).unwrap();
        assert!(snapshot.events.iter().any(|event| event.event_type == "mission_supervisor_error"));
        let event = snapshot.events.iter().find(|event| event.event_type == "mission_supervisor_error").unwrap();
        assert!(event.summary.chars().count() <= 1900);
        assert_eq!(event.payload["error"], "中文错误".repeat(1000));
        let healthy_snapshot = store.snapshot(&healthy, 0).unwrap();
        assert!(!healthy_snapshot.events.iter().any(|event| event.event_type == "mission_supervisor_error"));
    }

    fn failed_task_mission_to_repair(store: &MissionStore) -> String {
        let created = store.create(&crate::mission::MissionCreateInput {
            title: "needs repair".to_string(),
            objective: String::new(),
            workspace: MissionWorkspace { permission_mode: String::new(), root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()],
        }).unwrap();
        let proposal = MissionPlanProposal {
            scheduling: None, summary: "repairable".to_string(),
            tasks: vec![mission("seed", MissionStatus::Active, TaskStatus::Ready, true).tasks[0].spec.clone()],
            required_acceptance: vec!["ok".to_string()],
            budget: MissionBudget { max_rounds: 3, max_minutes: 10, max_repair_attempts: 1, max_total_tokens: None, max_cost: None, currency: None },
        };
        let planned = store.propose_plan(&created.mission_id, &proposal).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["task".to_string()], &[], &[], &[], "test-user").unwrap();
        store.begin_task_attempt(&created.mission_id, "task", "run-failed").unwrap();
        store.complete_task_attempt(&created.mission_id, "task", "run-failed", false, &[], "failed").unwrap();
        created.mission_id
    }

    #[test]
    fn supervisor_control_events_survive_pages_and_store_reopen() {
        for recovery in ["resume", "budget"] {
            let root = std::env::temp_dir().join(format!("supervisor-pages-{}", rand::random::<u64>()));
            let path = root.join("missions.sqlite3");
            let store = MissionStore::new(&path);
            let id = failed_task_mission_to_repair(&store);
            let fingerprint = failed_follow_up_fingerprint(&store.get(&id).unwrap()).unwrap();
            assert!(store.seed_test_progress(&id, 10_001).unwrap() > 10_000);
            // Simulate a restart after the failure marker was persisted but
            // before the subsequent wait transition. The Mission is Active.
            store.append_event(&id, "mission_follow_up_failed", "", "failed",
                serde_json::json!({"fingerprint":fingerprint})).unwrap();
            store.append_event(&id, "mission_follow_up_failed", "", "other fingerprint",
                serde_json::json!({"fingerprint":"unrelated"})).unwrap();
            let other_input = serde_json::from_value(serde_json::json!({"title":"other mission",
                "workspace":{"root":root.to_string_lossy()}})).unwrap();
            let other = store.create(&other_input).unwrap();
            store.append_event(&other.mission_id, "mission_resume", "", "wrong mission recovery",
                serde_json::json!({})).unwrap();
            let store = MissionStore::new(&path);
            let mut calls = 0;
            scan_with(&store, |_| Ok(()), |_| { calls += 1; Ok(()) }).unwrap();
            assert_eq!(calls, 0);
            assert_eq!(store.get(&id).unwrap().status, MissionStatus::Waiting);
            assert!(store.seed_test_progress(&id, 10_001).unwrap() > 20_000);
            if recovery == "resume" {
                store.control(&id, "resume").unwrap();
            } else {
                let current = store.get(&id).unwrap();
                let input = serde_json::from_value(serde_json::json!({"request_id":"extend",
                    "expected_plan_sha256":current.plan_sha256,"additional_minutes":1})).unwrap();
                store.increase_budget(&id, &input, "test").unwrap();
            }
            let store = MissionStore::new(&path);
            // Recovery does not change this failed task's identity/fingerprint.
            assert_eq!(failed_follow_up_fingerprint(&store.get(&id).unwrap()), Some(fingerprint.clone()));
            scan_with(&store, |_| Ok(()), |_| { calls += 1; Err("still unavailable".into()) }).unwrap();
            assert_eq!(calls, 1);
            assert_eq!(store.get(&id).unwrap().status, MissionStatus::Waiting);
            let store = MissionStore::new(&path);
            scan_with(&store, |_| Ok(()), |_| { calls += 1; Ok(()) }).unwrap();
            assert_eq!(calls, 1);
            // Check the bounded R/F view itself, including the later failure.
            let (_, events) = store.follow_up_control_context(&id, |_| Some(fingerprint.clone())).unwrap();
            assert_eq!(events.len(), 2);
            assert!(events[0].sequence < events[1].sequence);
            assert_eq!(events[1].payload["fingerprint"], fingerprint);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn host_follow_up_is_bounded_and_not_retried_after_failure() {
        let root = std::env::temp_dir().join(format!(
            "himind-supervisor-follow-up-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = MissionStore::new(root.join("missions.sqlite3"));
        let mission_id = failed_task_mission_to_repair(&store);
        let mut calls = Vec::new();
        let errors = scan_with(&store, |_| Ok(()), |id| {
            calls.push(id.to_string());
            Err("coordination model unavailable".to_string())
        }).unwrap();
        assert_eq!(calls, vec![mission_id.clone()]);
        assert_eq!(errors.len(), 1);
        let events = store.snapshot(&mission_id, 0).unwrap().events;
        assert!(events.iter().any(|event| event.event_type == "mission_follow_up_failed"));

        let mut second_calls = Vec::new();
        scan_with(&store, |_| Ok(()), |id| {
            second_calls.push(id.to_string());
            Ok(())
        }).unwrap();
        assert!(second_calls.is_empty(), "the same failed fingerprint must not retry every tick");
        assert!(!needs_host_follow_up(&store.get(&mission_id).unwrap(), &events));
        assert_eq!(store.get(&mission_id).unwrap().status, MissionStatus::Waiting);
        store.control(&mission_id, "resume").unwrap();
        let resumed_events = store.snapshot(&mission_id, 0).unwrap().events;
        assert!(needs_host_follow_up(&store.get(&mission_id).unwrap(), &resumed_events));
        scan_with(&store, |_| Ok(()), |id| { second_calls.push(id.to_string()); Ok(()) }).unwrap();
        assert_eq!(second_calls, vec![mission_id]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn executor_budget_awaiting_verification_preserves_admitted_boundary() {
        // Real store transitions and supervisor scan; independent review is a
        // deterministic fixture, never a claim about this Mission's host VERIFY.
        for passed in [false, true] {
            let root = std::env::temp_dir().join(format!("budget-boundary-{}", rand::random::<u64>()));
            fs::create_dir_all(&root).unwrap();
            let path = root.join("missions.sqlite3");
            let store = MissionStore::new(&path);
            let created = store.create(&crate::mission::MissionCreateInput {
                title: "admitted verification boundary".into(), objective: String::new(),
                workspace: MissionWorkspace { permission_mode: String::new(), root: root.to_string_lossy().into(), read_scope: vec![], write_scope: vec![] },
                acceptance_criteria: vec!["independent check".into()],
            }).unwrap();
            let first = mission("seed", MissionStatus::Active, TaskStatus::Ready, true).tasks.remove(0).spec;
            let mut next = first.clone(); next.id = "next".into(); next.depends_on = vec!["task".into()];
            let proposal = MissionPlanProposal { scheduling: None, summary: "boundary fixture".into(), tasks: vec![first, next],
                required_acceptance: vec!["independent check".into()],
                budget: MissionBudget { max_rounds: 10, max_minutes: 60, max_repair_attempts: 2, max_total_tokens: Some(100), max_cost: None, currency: None } };
            let planned = store.propose_plan(&created.mission_id, &proposal).unwrap();
            let confirmed = store.confirm_plan(&created.mission_id, &planned.plan_sha256,
                &["task".into(), "next".into()], &[], &[], &[], "boundary-owner").unwrap();
            let id = &created.mission_id;
            store.begin_task_attempt(id, "task", "implementation").unwrap();
            let usage = crate::agent_core_contracts::LocalRunUsage { input_tokens: 90, output_tokens: 20,
                estimated_cost: 0.0, currency: String::new(), billing_owner: "local".into() };
            store.record_run_usage(id, "task", "implementation", &usage).unwrap();
            store.record_budget_exhausted(id, "stale scan during implementation").unwrap();
            assert_eq!(store.get(id).unwrap().status, MissionStatus::Active);
            store.await_task_verification(id, "task", "implementation", "successful paid implementation").unwrap();
            store.record_run_usage(id, "task", "implementation", &usage).unwrap();
            store.record_budget_exhausted(id, "stale scan during verification").unwrap();
            let claim = store.workspace_claim(&confirmed.workspace.root).unwrap();
            assert!(claim.is_some());
            let pending = store.get(id).unwrap();
            assert!(store.budget_exhaustion_reason(&pending).unwrap().unwrap().contains("Token"));
            assert_eq!(pending.usage.as_ref().unwrap().input_tokens, 90);
            assert_eq!(pending.tasks[0].status, TaskStatus::AwaitingVerification);
            let mut dispatches = 0;
            let errors = scan_with(&store, |mission_id| {
                dispatches += 1;
                let task = store.next_verification_task(mission_id).map_err(|e|e.to_string())?
                    .ok_or("budget wait incorrectly hid admitted verification")?;
                assert_eq!(task.spec.id, "task");
                assert!(store.begin_verification_attempt(id, "task", "implementation").is_err());
                store.begin_verification_attempt(id, "task", "independent").unwrap();
                store.begin_verification_attempt(id, "task", "independent").unwrap();
                assert!(store.begin_verification_attempt(id, "task", "different-worker").is_err());
                assert!(store.record_verification_result(id, "task", "implementation", "independent", true,
                    &["fixture:review".into()], "").unwrap_err().to_string().contains("missing host"));
                assert_eq!(store.workspace_claim(&confirmed.workspace.root).unwrap(), claim);
                Ok(())
            }, |_| panic!("pending verification cannot generate a repair")).unwrap();
            assert!(errors.is_empty(), "{errors:?}");
            assert!(dispatches == 1);
            let pending = store.get(id).unwrap();
            assert_eq!(pending.status, MissionStatus::Active);
            assert_eq!(pending.confirmation, confirmed.confirmation);
            let input = crate::mission::MissionBudgetIncrease { request_id: "boundary-extension".into(),
                expected_plan_sha256: planned.plan_sha256.clone(), additional_rounds: 0, additional_minutes: 0,
                additional_repair_attempts: 0, additional_tokens: Some(100), unlimited_tokens: false, additional_cost: None,
                legacy_verification_override: None };
            // Explicit pause remains paused; it does not lose the admitted
            // verifier or allow inflight budget extension. Only user resume
            // with this confirmed claim may finish the same verification.
            let paused = store.control(id, "pause").unwrap();
            let wait = paused.effective_wait.as_ref().unwrap();
            assert!(wait.can_resume && !wait.can_increase_budget);
            assert_eq!(wait.reason, "manual_pause");
            assert!(wait.budget_reason.is_some());
            let paused_events = store.snapshot(id, 0).unwrap().events.len();
            scan_with(&store, |_| panic!("manual pause"), |_| panic!("manual pause")).unwrap();
            store.record_budget_exhausted(id, "stale snapshot must not replace pause").unwrap();
            assert_eq!(store.snapshot(id, 0).unwrap().events.len(), paused_events);
            assert!(store.next_verification_task(id).unwrap().is_none());
            assert!(store.begin_verification_attempt(id, "task", "new-verifier-while-paused").is_err());
            assert!(store.increase_budget(id, &input, "boundary-owner").is_err());
            assert_eq!(store.control(id, "resume").unwrap().status, MissionStatus::Active);
            store.suspend_runtime(id, "task", &serde_json::json!({"code":"fixture_environment"})).unwrap();
            let blocked = store.get(id).unwrap();
            let wait = blocked.effective_wait.as_ref().unwrap();
            assert!(!wait.can_resume);
            assert_eq!(wait.reason, "environment_blocked");
            assert!(store.control(id, "resume").is_err());
            store.record_budget_exhausted(id, "stale snapshot must not replace environment").unwrap();
            assert!(store.runtime_blocked(id).unwrap());
            store.resolve_runtime_block(id, &planned.plan_sha256, "default", &serde_json::json!({"fixture":true})).unwrap();
            assert_eq!(store.control(id, "resume").unwrap().status, MissionStatus::Active);
            assert_eq!(store.get(id).unwrap().tasks[0].verification_run_id, "independent");
            assert_eq!(store.workspace_claim(&confirmed.workspace.root).unwrap(), claim);
            // A second implementation selector must not strand verification.
            assert!(store.next_ready_task(id).unwrap().is_none());
            assert_eq!(store.get(id).unwrap().status, MissionStatus::Active);
            if passed {
                store.append_event(id, "host_verification_checked", "task", "deterministic test fixture only",
                    serde_json::json!({"passed":true,"plan_sha256":planned.plan_sha256,
                        "implementation_run_id":"implementation","verification_run_id":"independent"})).unwrap();
            }
            let evidence = vec!["fixture:independent-review".into()];
            store.record_verification_result(id, "task", "implementation", "independent", passed, &evidence, "fixture").unwrap();
            let reopened = MissionStore::new(&path);
            let before_replay = reopened.snapshot(id, 0).unwrap().events.len();
            reopened.record_verification_result(id, "task", "implementation", "independent", passed, &evidence, "fixture").unwrap();
            assert_eq!(reopened.snapshot(id, 0).unwrap().events.len(), before_replay);
            assert!(reopened.workspace_claim(&confirmed.workspace.root).unwrap().is_none());
            scan_with(&reopened, |_| {
                assert!(reopened.next_ready_task(id).unwrap().is_none()); Ok(())
            }, |_| panic!("exhausted budget must block a new repair")).unwrap();
            let waiting = reopened.get(id).unwrap();
            assert_eq!(waiting.status, MissionStatus::Waiting);
            assert_eq!(waiting.tasks[0].status, if passed {TaskStatus::Completed} else {TaskStatus::Failed});
            assert_eq!(waiting.tasks[1].status, if passed {TaskStatus::Ready} else {TaskStatus::Blocked});
            let extended = reopened.increase_budget(id, &input, "boundary-owner").unwrap();
            assert_eq!(extended.status, MissionStatus::Active);
            assert_eq!(extended.confirmation, confirmed.confirmation);
            assert_eq!(reopened.increase_budget(id, &input, "boundary-owner").unwrap().budget, extended.budget);
            assert_eq!(reopened.snapshot(id, 0).unwrap().events.iter().filter(|e|e.event_type=="budget_extended").count(), 1);
            assert_eq!(reopened.get(id).unwrap().usage.as_ref().unwrap().output_tokens, 20);
            let events = reopened.snapshot(id, 0).unwrap().events.len();
            reopened.record_budget_exhausted(id, "old exhausted snapshot after extension").unwrap();
            assert_eq!(reopened.get(id).unwrap().status, MissionStatus::Active);
            assert_eq!(reopened.snapshot(id, 0).unwrap().events.len(), events);
            reopened.control(id, "cancel").unwrap();
            let events = reopened.snapshot(id, 0).unwrap().events.len();
            reopened.record_budget_exhausted(id, "old exhausted snapshot after cancel").unwrap();
            scan_with(&reopened, |_| panic!("canceled"), |_| panic!("canceled")).unwrap();
            assert_eq!(reopened.get(id).unwrap().status, MissionStatus::Canceled);
            assert_eq!(reopened.snapshot(id, 0).unwrap().events.len(), events);
        }
    }

    #[test]
    fn executor_budget_pending_resume_requires_current_confirmation_and_claim() {
        for case in ["missing-claim", "foreign-claim", "stale-confirmation"] {
            let root = std::env::temp_dir().join(format!("budget-resume-{}", rand::random::<u64>()));
            fs::create_dir_all(&root).unwrap();
            let path = root.join("missions.sqlite3");
            let store = MissionStore::new(&path);
            let id = persist_confirmed_mission(&store, case);
            store.begin_task_attempt(&id, "task", "implementation").unwrap();
            store.record_run_usage(&id, "task", "implementation", &crate::agent_core_contracts::LocalRunUsage {
                input_tokens: 110, output_tokens: 1, estimated_cost: 0.0, currency: String::new(), billing_owner: "local".into() }).unwrap();
            store.await_task_verification(&id, "task", "implementation", "fixture").unwrap();
            store.control(&id, "pause").unwrap();
            let original_claim = store.workspace_claim("F:/repo").unwrap();
            // Malformed ownership/confirmation is an isolated SQL fixture.
            let db = rusqlite::Connection::open(&path).unwrap();
            db.pragma_update(None, "foreign_keys", "ON").unwrap();
            if case == "missing-claim" {
                db.execute("DELETE FROM mission_workspace_claims WHERE mission_id=?1", [&id]).unwrap();
            } else if case == "foreign-claim" {
                let foreign = persist_confirmed_mission(&store, "foreign");
                db.execute("UPDATE mission_workspace_claims SET mission_id=?2 WHERE mission_id=?1", rusqlite::params![id,foreign]).unwrap();
            } else {
                db.execute("UPDATE mission_runs SET confirmation_json=NULL WHERE mission_id=?1", [&id]).unwrap();
            }
            if case == "missing-claim" {
                // The existing legacy migration legitimately reconstructs the
                // same still-current attempt's claim before control admission.
                // Do not mistake this recovery for an unowned admission.
                assert_eq!(store.control(&id, "resume").unwrap().status, MissionStatus::Active);
                assert_eq!(store.workspace_claim("F:/repo").unwrap(), original_claim);
                assert_eq!(store.get(&id).unwrap().tasks[0].attempt_run_id, "implementation");
                continue;
            }
            assert!(!store.get(&id).unwrap().effective_wait.as_ref().unwrap().can_resume);
            assert!(store.control(&id, "resume").is_err(), "{case}");
            assert_eq!(store.get(&id).unwrap().status, MissionStatus::Waiting);
            assert!(store.next_verification_task(&id).unwrap().is_none());
            assert!(store.workspace_claim("F:/repo").unwrap().is_some() || case == "missing-claim");
        }
    }

    #[test]
    fn executor_recovery_projection_store_supervisor_and_negative_boundaries() {
        // Real Store/control/projection and supervisor entrypoints. Only the
        // legacy/corrupt persisted states and independent reviewer are fixtures.
        // No formal reviewer or installed host acceptance is claimed here.
        for case in ["pending", "pending-budget", "manual-pause", "environment",
            "foreign-claim", "wrong-task", "wrong-run", "wrong-workspace",
            "missing-implementation", "missing-confirmation", "stale-confirmation",
            "empty-plan", "running-worker", "canceled", "active-verification"] {
            let root = std::env::temp_dir().join(format!("recovery-projection-{:016x}", rand::random::<u64>()));
            fs::create_dir_all(&root).unwrap();
            let path = root.join("missions.sqlite3");
            let store = MissionStore::new(&path);
            let created = store.create(&crate::mission::MissionCreateInput {
                title: case.into(), objective: String::new(),
                workspace: MissionWorkspace { permission_mode: String::new(), root: root.to_string_lossy().into(), read_scope: vec![], write_scope: vec![] },
                acceptance_criteria: vec!["separate review".into()],
            }).unwrap();
            let first = mission("seed", MissionStatus::Active, TaskStatus::Ready, true).tasks.remove(0).spec;
            let mut next = first.clone(); next.id = "next".into(); next.depends_on = vec!["task".into()];
            let planned = store.propose_plan(&created.mission_id, &MissionPlanProposal {
                scheduling: None, summary: "recovery boundary".into(), tasks: vec![first, next], required_acceptance: vec!["separate review".into()],
                budget: MissionBudget { max_rounds: 100, max_minutes: 60, max_repair_attempts: 2,
                    max_total_tokens: Some(100), max_cost: None, currency: None },
            }).unwrap();
            let id = &created.mission_id;
            let confirmed = store.confirm_plan(id, &planned.plan_sha256, &["task".into(),"next".into()], &[], &[], &[], "fixture-owner").unwrap();
            store.begin_task_attempt(id, "task", "implementation").unwrap();
            store.await_task_verification(id, "task", "implementation", "submitted implementation").unwrap();
            if case == "pending-budget" {
                store.record_run_usage(id, "task", "implementation", &crate::agent_core_contracts::LocalRunUsage {
                    input_tokens: 110, output_tokens: 1, estimated_cost: 0.0, currency: String::new(), billing_owner: "local".into() }).unwrap();
            }
            if case == "active-verification" {
                store.begin_verification_attempt(id, "task", "independent").unwrap();
            } else if case == "environment" {
                store.suspend_runtime(id, "task", &serde_json::json!({"code":"fixture_environment"})).unwrap();
            } else if case == "manual-pause" {
                // Submitted reviewer IDs survive a pause, but are not liveness
                // proof. Explicit pause stays a user choice, not a pending CTA.
                store.begin_verification_attempt(id, "task", "independent").unwrap();
                store.control(id, "pause").unwrap();
            } else if case == "canceled" {
                store.control(id, "cancel").unwrap();
            } else {
                // Seed exactly the historical Waiting state, without pretending
                // a user pause caused it. Normal APIs produced the task/claim.
                rusqlite::Connection::open(&path).unwrap().execute(
                    "UPDATE mission_runs SET status='waiting' WHERE mission_id=?1", [id]).unwrap();
            }
            let db = rusqlite::Connection::open(&path).unwrap();
            match case {
                "foreign-claim" => {
                    let foreign = persist_confirmed_mission(&store, "foreign");
                    store.control(&foreign,"pause").unwrap();
                    db.execute("UPDATE mission_workspace_claims SET mission_id=?2 WHERE mission_id=?1", rusqlite::params![id,foreign]).unwrap();
                },
                "wrong-task" => { db.execute("UPDATE mission_workspace_claims SET task_id='next' WHERE mission_id=?1", [id]).unwrap(); },
                "wrong-run" => { db.execute("UPDATE mission_workspace_claims SET run_id='obsolete' WHERE mission_id=?1", [id]).unwrap(); },
                "wrong-workspace" => { let key: String = db.query_row("SELECT workspace_key FROM mission_workspace_claims WHERE mission_id=?1", [id], |r|r.get(0)).unwrap();
                    db.execute("UPDATE mission_workspace_claims SET workspace_key='wrong' WHERE mission_id=?1", [id]).unwrap();
                    // Keep a foreign occupant at the current key so the legacy
                    // migration cannot legitimately restore this owned claim.
                    let foreign = persist_confirmed_mission(&store, "occupant");
                    store.control(&foreign,"pause").unwrap();
                    db.execute("INSERT INTO mission_workspace_claims SELECT ?1,?2,'task','foreign-run','fixture'", rusqlite::params![key,foreign]).unwrap();
                },
                "missing-implementation" => { db.execute("UPDATE mission_tasks SET attempt_run_id='' WHERE mission_id=?1 AND task_id='task'", [id]).unwrap(); },
                "missing-confirmation" => { db.execute("UPDATE mission_runs SET confirmation_json=NULL WHERE mission_id=?1", [id]).unwrap(); },
                "stale-confirmation" => { db.execute("UPDATE mission_runs SET confirmation_json=json_set(confirmation_json,'$.plan_sha256',?2) WHERE mission_id=?1", rusqlite::params![id,"f".repeat(64)]).unwrap(); },
                "empty-plan" => { db.execute("UPDATE mission_runs SET plan_sha256='',confirmation_json=json_set(confirmation_json,'$.plan_sha256','') WHERE mission_id=?1", [id]).unwrap(); },
                "running-worker" => { db.execute("UPDATE mission_tasks SET status='running',attempt_run_id='other-worker' WHERE mission_id=?1 AND task_id='next'", [id]).unwrap(); },
                _ => {},
            }
            let before = store.snapshot(id, 0).unwrap();
            let wait = before.mission.effective_wait.as_ref().unwrap();
            assert_eq!(wait.cursor, before.cursor, "{case}");
            assert_eq!(wait.plan_sha256, before.mission.plan_sha256, "{case}");
            let listed = store.list(100).unwrap().into_iter().find(|m| &m.mission_id==id).unwrap();
            assert_eq!(listed.effective_wait, before.mission.effective_wait, "{case}");
            let actionable = matches!(case, "pending" | "pending-budget" | "manual-pause");
            assert_eq!(wait.can_resume, actionable, "{case}");
            assert!(!wait.can_increase_budget, "{case}");
            match case {
                "pending" | "pending-budget" => { assert_eq!((&*wait.reason,&*wait.actor), ("verification_resume_required","user")); },
                "manual-pause" => { assert_eq!((&*wait.reason,&*wait.actor), ("manual_pause","user")); },
                "environment" => { assert_eq!((&*wait.reason,&*wait.actor), ("environment_blocked","user")); assert!(wait.environment_blocked); },
                "running-worker" | "active-verification" => { assert_eq!((&*wait.reason,&*wait.actor), ("system_verification","system")); },
                "canceled" => { assert_eq!((&*wait.reason,&*wait.actor), ("not_waiting","none")); },
                _ => { assert_eq!((&*wait.reason,&*wait.actor), ("unknown","unknown")); },
            }
            let mut dispatched = 0usize;
            scan_with(&store, |selected| { assert_eq!(selected,id); dispatched+=1; Ok(()) }, |_| panic!("no repair for {case}")).unwrap();
            assert_eq!(dispatched, if case=="active-verification" { 1 } else { 0 }, "{case}");
            assert_eq!(store.snapshot(id,0).unwrap().cursor, before.cursor, "scan must not mutate {case}");
            if actionable {
                let resumed = store.control(id,"resume").unwrap();
                assert_eq!(resumed.status,MissionStatus::Active);
                assert_eq!(resumed.tasks[0].attempt_run_id,"implementation");
                assert_eq!(resumed.confirmation,confirmed.confirmation);
                assert_eq!(resumed.budget,confirmed.budget);
                let cursor = resumed.effective_wait.as_ref().unwrap().cursor;
                assert_eq!(store.control(id,"resume").unwrap().effective_wait.as_ref().unwrap().cursor,cursor);
                scan_with(&store, |selected| {
                    assert_eq!(selected,id);
                    assert!(store.next_ready_task(id).unwrap().is_none());
                    assert_eq!(store.next_verification_task(id).unwrap().unwrap().spec.id,"task");
                    assert!(store.begin_verification_attempt(id,"task","implementation").is_err());
                    store.begin_verification_attempt(id,"task","independent").unwrap();
                    assert!(store.begin_verification_attempt(id,"task","foreign-verifier").is_err());
                    Ok(())
                }, |_| panic!("no repair")).unwrap();
                let active = store.get(id).unwrap();
                let active_wait = active.effective_wait.as_ref().unwrap();
                assert_eq!((&*active_wait.reason,&*active_wait.actor),("system_verification","system"));
                assert!(!active_wait.can_resume);
                if case=="pending-budget" { assert!(active_wait.budget_reason.is_some()); }
            } else if case!="active-verification" {
                assert!(store.control(id,"resume").is_err(),"{case}");
                assert_eq!(store.snapshot(id,0).unwrap().cursor,before.cursor,"rejected control changed {case}");
            }
            println!("RECOVERY_CASE={case}; reason={}; actor={}; can_resume={}; assertions=passed",wait.reason,wait.actor,wait.can_resume);
        }
    }

    #[test]
    fn only_confirmed_active_missions_with_work_are_dispatchable() {
        let missions = vec![
            mission("ready", MissionStatus::Active, TaskStatus::Ready, true),
            mission("unconfirmed", MissionStatus::Active, TaskStatus::Ready, false),
            mission("assignment", MissionStatus::AwaitingAssignment, TaskStatus::Ready, false),
            mission("waiting", MissionStatus::Waiting, TaskStatus::Ready, true),
            mission("completed", MissionStatus::Completed, TaskStatus::Completed, true),
        ];
        assert_eq!(dispatchable_ids(&missions), vec!["ready"]);
    }
}
