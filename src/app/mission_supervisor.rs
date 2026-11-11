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
    !events.iter().any(|event| {
        event.event_type == "mission_follow_up_failed"
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
    for mission_id in dispatchable_ids(&missions) {
        if let Err(message) = dispatch(&mission_id) {
            let _ = store.append_event(&mission_id, "mission_supervisor_error", "",
                &format!("宿主调度未能推进任务：{message}"), serde_json::json!({"error": message}));
            errors.push((mission_id, message));
        }
    }
    for mission in &missions {
        let Some(fingerprint) = failed_follow_up_fingerprint(mission) else { continue; };
        let events = store.snapshot(&mission.mission_id, 0)?.events;
        if !needs_host_follow_up(mission, &events) {
            continue;
        }
        if let Err(message) = follow_up(&mission.mission_id) {
            let _ = store.append_event(
                &mission.mission_id,
                "mission_follow_up_failed",
                "",
                &format!("宿主未能生成有界跟进任务：{message}"),
                serde_json::json!({"error": message, "fingerprint": fingerprint}),
            );
            errors.push((mission.mission_id.clone(), message));
        }
    }
    Ok(errors)
}

/// Spawn the process-lifetime supervisor. The stop flag is used by tests or a
/// future controlled shutdown; normal Agent shutdown terminates the process.
pub(crate) fn start(gateway: CapabilityGateway, options: Options, stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("himind-mission-supervisor".to_string())
        .spawn(move || {
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
            execution: ExecutionBinding {
                executor: "himind.builtin".to_string(), model: String::new(),
                role_id: "implementer".to_string(), service_id: String::new(),
            }, repair_of: String::new(),
        };
        MissionView {
            schema_version: "mission.v1".to_string(), mission_id: id.to_string(), title: "Task".to_string(),
            objective: String::new(), status,
            workspace: MissionWorkspace { root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()], plan_revision: 1, plan_sha256: "a".repeat(64),
            plan_summary: String::new(), required_acceptance: vec![], proposed_tasks: vec![],
            budget: Some(MissionBudget { max_rounds: 1, max_minutes: 1, max_repair_attempts: 0, max_total_tokens: None, max_cost: None }),
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
            workspace: MissionWorkspace { root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()],
        }).unwrap();
        let proposal = MissionPlanProposal {
            summary: "test".to_string(),
            tasks: vec![mission("unused", MissionStatus::Active, TaskStatus::Ready, true).tasks[0].spec.clone()],
            required_acceptance: vec!["ok".to_string()],
            budget: MissionBudget { max_rounds: 1, max_minutes: 1, max_repair_attempts: 0, max_total_tokens: None, max_cost: None },
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
            if mission_id == broken { Err("isolated failure".to_string()) } else { Ok(()) }
        }, |_| Ok(())).unwrap();
        assert_eq!(visited.len(), 2);
        assert!(visited.contains(&broken));
        assert!(visited.contains(&healthy));
        assert_eq!(errors, vec![(broken.clone(), "isolated failure".to_string())]);
        let snapshot = store.snapshot(&broken, 0).unwrap();
        assert!(snapshot.events.iter().any(|event| event.event_type == "mission_supervisor_error"));
        let healthy_snapshot = store.snapshot(&healthy, 0).unwrap();
        assert!(!healthy_snapshot.events.iter().any(|event| event.event_type == "mission_supervisor_error"));
    }

    fn failed_task_mission_to_repair(store: &MissionStore) -> String {
        let created = store.create(&crate::mission::MissionCreateInput {
            title: "needs repair".to_string(),
            objective: String::new(),
            workspace: MissionWorkspace { root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![] },
            acceptance_criteria: vec!["ok".to_string()],
        }).unwrap();
        let proposal = MissionPlanProposal {
            summary: "repairable".to_string(),
            tasks: vec![mission("seed", MissionStatus::Active, TaskStatus::Ready, true).tasks[0].spec.clone()],
            required_acceptance: vec!["ok".to_string()],
            budget: MissionBudget { max_rounds: 3, max_minutes: 10, max_repair_attempts: 1, max_total_tokens: None, max_cost: None },
        };
        let planned = store.propose_plan(&created.mission_id, &proposal).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["task".to_string()], &[], &[], &[], "test-user").unwrap();
        store.begin_task_attempt(&created.mission_id, "task", "run-failed").unwrap();
        store.complete_task_attempt(&created.mission_id, "task", "run-failed", false, &[], "failed").unwrap();
        created.mission_id
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
        let _ = fs::remove_dir_all(root);
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
