//! B05/B08 real-store regression fixtures (backlog F01-F10).
//! No model/runtime is started. SQL writes below seed legacy/corrupt fixtures or
//! hold the actual SQLite transaction barrier; they are not claimed as host APIs.
//! F05 exception: explicitly authorized proactive increases remain supported,
//! overriding the original fixture's manual_pause_not_exhausted rejection.
use super::*;
use crate::mission::{ExecutionBinding, MissionBudget, MissionWorkspace};
use crate::agent_core_contracts::LocalRunUsage;

fn isolated_store(label: &str) -> MissionStore {
    let root = std::env::temp_dir()
        .join(format!("mission-remediation-{label}-{:016x}", rand::random::<u64>()));
    MissionStore::new(root.join("missions.sqlite"))
}

fn confirmed(store: &MissionStore) -> MissionView {
    let created = store.create(&MissionCreateInput {
        title: "Budget remediation".into(), objective: "Exercise real store gates".into(),
        workspace: MissionWorkspace {
            permission_mode: String::new(), root: "F:/budget-remediation".into(),
            read_scope: vec!["frontend".into()], write_scope: vec!["frontend".into()],
        },
        acceptance_criteria: vec!["independent verification required".into()],
    }).unwrap();
    let planned = store.propose_plan(&created.mission_id, &MissionPlanProposal {
        summary: "Budget test".into(),
        tasks: vec![MissionTaskSpec {
            id: "fix".into(), title: "Fix".into(), objective: String::new(), depends_on: vec![],
            read_paths: vec!["frontend".into()], write_paths: vec!["frontend/src".into()],
            acceptance_criteria: vec!["verified".into()], required_checks: vec!["npm test".into()],
            execution: ExecutionBinding { expert: None, executor: "himind.builtin".into(),
                model: String::new(), role_id: "ui.implementer".into(), service_id: String::new() },
            repair_of: String::new(),
        }],
        required_acceptance: vec!["verified".into()],
        budget: MissionBudget { max_rounds: 10, max_minutes: 60, max_repair_attempts: 2,
            max_total_tokens: Some(100), max_cost: None, currency: Some("USD".into()) },
    }).unwrap();
    store.confirm_plan(&created.mission_id, &planned.plan_sha256, &["fix".into()],
        &["frontend/src".into()], &[], &[], "budget-test-user").unwrap()
}

fn usage(tokens: u64, cost: f64, currency: &str) -> LocalRunUsage {
    LocalRunUsage { input_tokens: tokens, output_tokens: 0, estimated_cost: cost,
        currency: currency.into(), billing_owner: "local".into() }
}

fn exhausted(store: &MissionStore) -> MissionView {
    let mission = confirmed(store);
    store.begin_task_attempt(&mission.mission_id, "fix", "run-base").unwrap();
    store.record_run_usage(&mission.mission_id, "fix", "run-base", &usage(100, 1.0, "USD")).unwrap();
    store.complete_task_attempt(&mission.mission_id, "fix", "run-base", false, &[], "fixture run ended").unwrap();
    assert!(store.next_ready_task(&mission.mission_id).unwrap().is_none());
    store.append_event(&mission.mission_id, "task_progress", "fix", "late progress", serde_json::json!({})).unwrap();
    let waiting = store.get(&mission.mission_id).unwrap();
    assert_eq!(waiting.status, MissionStatus::Waiting);
    waiting
}

fn increase(mission: &MissionView) -> MissionBudgetIncrease {
    MissionBudgetIncrease { request_id: "K".into(), expected_plan_sha256: mission.plan_sha256.clone(),
        additional_rounds: 0, additional_minutes: 0, additional_repair_attempts: 0,
        additional_tokens: Some(50), unlimited_tokens: false, additional_cost: None }
}

fn events_of(store: &MissionStore, id: &str, kind: &str) -> usize {
    store.snapshot(id, 0).unwrap().events.iter().filter(|event| event.event_type == kind).count()
}

fn persisted(store: &MissionStore, id: &str) -> serde_json::Value {
    // Includes tasks, budget, status, confirmation, usage, event log and cursor.
    let value = serde_json::to_value(store.snapshot(id, 0).unwrap()).unwrap();
    println!("STORE_SNAPSHOT={value}");
    value
}

fn reject_unchanged(store: &MissionStore, id: &str, request: &MissionBudgetIncrease) {
    let before = persisted(store, id);
    let result = store.increase_budget(id, request, "budget-test-user");
    assert!(result.is_err(), "unexpected acceptance: {request:?}");
    println!("REJECT_EVIDENCE={}", serde_json::json!({"request":request,"response":result.unwrap_err().to_string(),"before":before,"after":persisted(store,id)}));
    assert_eq!(persisted(store, id), before, "rejection mutated persisted state");
    let reopened = MissionStore::new(store.path.clone());
    assert_eq!(persisted(&reopened, id), before, "rejection changed state after restart");
}

fn seed_budget(store: &MissionStore, id: &str, budget: &MissionBudget) {
    store.connection().unwrap().execute("UPDATE mission_runs SET budget_json=?2 WHERE mission_id=?1",
        params![id, serde_json::to_string(budget).unwrap()]).unwrap();
}

#[test]
fn f01_progress_does_not_hide_exhaustion_and_extension_never_dispatches() {
    let store = isolated_store("f01");
    let before = exhausted(&store);
    let count = events_of(&store, &before.mission_id, "task_dispatched");
    let wait = before.effective_wait.as_ref().unwrap();
    assert_eq!(wait.reason, "budget_exhausted");
    assert_eq!(wait.actor, "user");
    assert!(wait.can_increase_budget);
    assert!(!wait.can_resume);
    let listed = store.list(10).unwrap().into_iter().find(|v| v.mission_id == before.mission_id).unwrap();
    let detail = store.snapshot(&before.mission_id, 0).unwrap();
    assert_eq!(listed.effective_wait, detail.mission.effective_wait);
    assert_eq!(before.effective_wait, detail.mission.effective_wait);
    println!("B08_DTO_SAMPLE={}", serde_json::to_string(&detail.mission).unwrap());
    let after = store.increase_budget(&before.mission_id, &increase(&before), "budget-test-user").unwrap();
    assert_eq!(after.status, MissionStatus::Active);
    assert_eq!(after.budget.as_ref().unwrap().max_total_tokens, Some(150));
    assert_eq!(after.plan_sha256, before.plan_sha256);
    assert_eq!(after.confirmation, before.confirmation);
    assert!(store.budget_exhaustion_reason(&after).unwrap().is_none());
    assert_eq!(events_of(&store, &before.mission_id, "budget_extended"), 1);
    assert_eq!(events_of(&store, &before.mission_id, "task_dispatched"), count);
}

#[test]
fn f02_extension_preserves_environment_block_and_existing_authorized_resolution() {
    let store = isolated_store("f02");
    let mission = confirmed(&store);
    store.suspend_runtime(&mission.mission_id, "fix", &serde_json::json!({"code":"runtime_workspace_blocked"})).unwrap();
    store.record_run_usage(&mission.mission_id, "fix", "run-base", &usage(100, 1.0, "USD")).unwrap();
    store.append_event(&mission.mission_id, "task_progress", "fix", "late diagnostic", serde_json::json!({})).unwrap();
    let after = store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap();
    assert_eq!(after.status, MissionStatus::Waiting);
    assert_eq!(after.effective_wait.as_ref().unwrap().reason, "environment_blocked");
    assert!(!after.effective_wait.as_ref().unwrap().can_resume);
    assert!(store.runtime_blocked(&mission.mission_id).unwrap());
    assert!(store.budget_exhaustion_reason(&after).unwrap().is_none());
    assert_eq!(after.confirmation, mission.confirmation);
    assert_eq!(after.workspace, mission.workspace);
    assert!(store.control(&mission.mission_id, "resume").is_err());
    let resolved = store.resolve_runtime_block(&mission.mission_id, &mission.plan_sha256, "default",
        &serde_json::json!({"source":"explicit-test-recheck"})).unwrap();
    assert_eq!(resolved.status, MissionStatus::Active);
    assert_eq!(resolved.workspace, mission.workspace);
    assert_eq!(resolved.confirmation, mission.confirmation);
    assert_eq!(events_of(&store, &mission.mission_id, "task_dispatched"), 0);
}

#[test]
fn f03_dropped_response_concurrent_same_key_restart_and_pause_return_current_state() {
    let store = isolated_store("f03");
    let mission = exhausted(&store);
    let request = increase(&mission);
    let dispatches = events_of(&store, &mission.mission_id, "task_dispatched");
    // Deliberately discard the successful response, not the persisted transaction.
    drop(store.increase_budget(&mission.mission_id, &request, "budget-test-user").unwrap());
    let handles: Vec<_> = (0..2).map(|_| {
        let path = store.path.clone(); let id = mission.mission_id.clone(); let request = request.clone();
        std::thread::spawn(move || MissionStore::new(path).increase_budget(&id, &request, "budget-test-user")
            .map(|view| view.budget.unwrap().max_total_tokens).map_err(|error| error.to_string()))
    }).collect();
    for handle in handles { assert_eq!(handle.join().unwrap().unwrap(), Some(150)); }
    let reopened = MissionStore::new(store.path.clone());
    assert_eq!(reopened.increase_budget(&mission.mission_id, &request, "budget-test-user").unwrap().status, MissionStatus::Active);
    let paused = reopened.control(&mission.mission_id, "pause").unwrap();
    assert_eq!(reopened.increase_budget(&mission.mission_id, &request, "budget-test-user").unwrap().status, paused.status);
    let mut conflict = request; conflict.additional_tokens = Some(60);
    reject_unchanged(&reopened, &mission.mission_id, &conflict);
    assert_eq!(events_of(&reopened, &mission.mission_id, "budget_extended"), 1);
    assert_eq!(events_of(&reopened, &mission.mission_id, "task_dispatched"), dispatches);
}

#[test]
fn f04_restart_and_repeated_out_of_order_cursor_reads_do_not_reapply_events() {
    let store = isolated_store("f04"); let mission = exhausted(&store);
    store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap();
    let reopened = MissionStore::new(store.path.clone());
    let before = persisted(&reopened, &mission.mission_id);
    let full = reopened.snapshot(&mission.mission_id, 0).unwrap();
    assert!(full.events.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    for cursor in [3, 4, 3, 0, full.cursor] {
        let snapshot = reopened.snapshot(&mission.mission_id, cursor).unwrap();
        assert!(snapshot.cursor >= cursor);
        assert!(snapshot.events.iter().all(|event| event.sequence > cursor));
    }
    assert_eq!(persisted(&reopened, &mission.mission_id), before);
    assert_eq!(full.mission.budget.as_ref().unwrap().max_total_tokens, Some(150));
    assert!(reopened.budget_exhaustion_reason(&full.mission).unwrap().is_none());
    assert_eq!(full.mission.effective_wait.as_ref().unwrap().cursor, full.cursor);
    assert!(reopened.snapshot(&mission.mission_id, full.cursor + 100).unwrap().requires_snapshot);
    // Deliberately seed a damaged delta sequence; no event is re-applied.
    reopened.connection().unwrap().execute("DELETE FROM mission_events WHERE mission_id=?1 AND sequence=2", params![mission.mission_id]).unwrap();
    assert!(reopened.snapshot(&mission.mission_id, 1).unwrap().requires_snapshot);
    // Replay is read-only; callers must discard duplicate/out-of-order deltas and
    // replace state with mission when requires_snapshot is true.
}

#[test]
fn f05_stale_missing_mismatched_confirmation_and_active_reject_but_manual_pause_accepts() {
    for variant in ["plan_h2", "confirmation_mismatch", "legacy_unknown", "active"] {
        let store = isolated_store(variant); let mission = exhausted(&store); let request = increase(&mission);
        let connection = store.connection().unwrap();
        match variant {
            "plan_h2" => { connection.execute("UPDATE mission_runs SET plan_sha256=?2 WHERE mission_id=?1", params![mission.mission_id, "b".repeat(64)]).unwrap(); }
            "confirmation_mismatch" => {
                let mut confirmation = mission.confirmation.clone().unwrap(); confirmation.plan_sha256 = "b".repeat(64);
                connection.execute("UPDATE mission_runs SET confirmation_json=?2 WHERE mission_id=?1", params![mission.mission_id, serde_json::to_string(&confirmation).unwrap()]).unwrap();
            }
            "legacy_unknown" => { connection.execute("UPDATE mission_runs SET confirmation_json=NULL WHERE mission_id=?1", params![mission.mission_id]).unwrap(); }
            "active" => { connection.execute("UPDATE mission_runs SET status='active' WHERE mission_id=?1", params![mission.mission_id]).unwrap(); }
            _ => unreachable!(),
        }
        reject_unchanged(&store, &mission.mission_id, &request);
    }
    // Explicit product authorization overrides F05's original rejection expectation.
    let store = isolated_store("f05-proactive"); let mission = confirmed(&store);
    let paused = store.control(&mission.mission_id, "pause").unwrap();
    assert_eq!(paused.effective_wait.as_ref().unwrap().reason, "manual_pause");
    assert!(paused.effective_wait.as_ref().unwrap().can_increase_budget);
    let after = store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap();
    assert_eq!(after.status, MissionStatus::Waiting);
    assert_eq!(after.effective_wait.as_ref().unwrap().reason, "manual_pause");
    assert!(store.next_ready_task(&mission.mission_id).unwrap().is_none());
    assert_eq!(after.confirmation, mission.confirmation);
    assert_eq!(after.budget.as_ref().unwrap().max_total_tokens, Some(150));
    assert_eq!(events_of(&store, &mission.mission_id, "task_dispatched"), 0);
}

#[test]
fn f06_running_present_rejects_then_explicit_retry_rechecks_all_gates() {
    let store = isolated_store("f06-running"); let mission = confirmed(&store);
    store.begin_task_attempt(&mission.mission_id, "fix", "run-live").unwrap();
    store.control(&mission.mission_id, "pause").unwrap();
    reject_unchanged(&store, &mission.mission_id, &increase(&mission));
    store.record_run_usage(&mission.mission_id, "fix", "run-live", &usage(100, 1.0, "USD")).unwrap();
    store.complete_task_attempt(&mission.mission_id, "fix", "run-live", false, &[], "finished").unwrap();
    assert_eq!(store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap().status, MissionStatus::Waiting);
    assert!(store.next_ready_task(&mission.mission_id).unwrap().is_none());
    assert_eq!(store.control(&mission.mission_id, "resume").unwrap().status, MissionStatus::Active);
}

#[test]
fn f06_sqlite_transaction_barrier_rechecks_running_before_any_budget_write() {
    let store = isolated_store("f06-barrier"); let mission = exhausted(&store);
    let mut connection = store.connection().unwrap();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    // A competing writer owns the real database lock. Seed its committed Running
    // transition while increase is starting; no sleeps or fake store are used.
    transaction.execute("UPDATE mission_tasks SET status='running',attempt_run_id='run-race' WHERE mission_id=?1 AND task_id='fix'", params![mission.mission_id]).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let path = store.path.clone(); let id = mission.mission_id.clone(); let request = increase(&mission);
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        MissionStore::new(path).increase_budget(&id, &request, "budget-test-user").map(|_| ()).map_err(|error| error.to_string())
    });
    started_rx.recv().unwrap();
    transaction.commit().unwrap();
    let before = persisted(&store, &mission.mission_id);
    assert!(worker.join().unwrap().unwrap_err().contains("running"));
    assert_eq!(persisted(&store, &mission.mission_id), before);
    assert_eq!(events_of(&store, &mission.mission_id, "budget_extended"), 0);
    // The Running row is a transaction fixture, not a dispatched run/claim.
    store.connection().unwrap().execute("UPDATE mission_tasks SET status='failed',attempt_run_id='run-base' WHERE mission_id=?1", params![mission.mission_id]).unwrap();
    assert!(store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").is_ok());
}

#[test]
fn f07_every_remaining_limit_and_untrusted_cost_reject_without_mutation() {
    for variant in ["rounds", "minutes", "tokens", "cost", "cost_missing", "currency_empty", "cost_untrusted", "budget_currency_missing", "budget_currency_empty", "currency_mismatch", "no_cost_reports"] {
        let store = isolated_store(variant); let mission = exhausted(&store);
        let mut budget = mission.budget.clone().unwrap(); let mut request = increase(&mission);
        match variant {
            "rounds" => budget.max_rounds = 1,
            // Set the declared limit to zero: deterministic elapsed-time exhaustion,
            // independent of scheduler speed or wall-clock sleeps.
            "minutes" => budget.max_minutes = 0,
            "tokens" => request.additional_tokens = Some(1),
            _ => {
                budget.max_cost = Some(if variant == "cost" { 1.0 } else { 10.0 });
                if variant == "budget_currency_missing" { budget.currency = None; }
                if variant == "budget_currency_empty" { budget.currency = Some("  ".into()); }
                if variant == "no_cost_reports" {
                    store.connection().unwrap().execute("UPDATE mission_events SET event_type='legacy_usage_unavailable' WHERE mission_id=?1 AND event_type='run_usage_recorded'", params![mission.mission_id]).unwrap();
                }
                if variant == "currency_mismatch" { budget.currency = Some("CNY".into()); }
                if matches!(variant, "cost_missing" | "currency_empty" | "cost_untrusted") {
                    let mut payload = serde_json::json!({"run_id":"run-base", "input_tokens":100, "output_tokens":0,
                        "estimated_cost":1.0,"currency":"USD","billing_owner":"local", "trusted":true,"complete":true});
                    if variant == "cost_missing" { payload.as_object_mut().unwrap().remove("estimated_cost"); }
                    if variant == "currency_empty" { payload["currency"] = serde_json::json!("  "); }
                    if variant == "cost_untrusted" { payload["billing_owner"] = serde_json::json!("unreported"); }
                    store.connection().unwrap().execute("UPDATE mission_events SET payload_json=?2 WHERE mission_id=?1 AND event_type='run_usage_recorded'", params![mission.mission_id, payload.to_string()]).unwrap();
                }
            }
        }
        if variant == "tokens" {
            // One additional token is still insufficient against actual use 150.
            store.record_run_usage(&mission.mission_id, "fix", "run-more", &usage(50, 0.0, "USD")).unwrap();
        }
        seed_budget(&store, &mission.mission_id, &budget);
        reject_unchanged(&store, &mission.mission_id, &request);
    }
}

#[test]
fn b05_currency_buckets_unknown_cost_and_run_usage_are_restart_idempotent() {
    let store = isolated_store("b05-buckets"); let mission = confirmed(&store);
    store.record_run_usage(&mission.mission_id, "fix", "run-usd", &usage(7, 1.25, "USD")).unwrap();
    store.record_run_usage(&mission.mission_id, "fix", "run-cny", &usage(3, 2.5, "CNY")).unwrap();
    let reopened = MissionStore::new(store.path.clone());
    reopened.record_run_usage(&mission.mission_id, "fix", "run-usd", &usage(7, 1.25, "USD")).unwrap();
    let view = reopened.get(&mission.mission_id).unwrap();
    assert_eq!(view.usage.as_ref().unwrap().input_tokens, 10);
    assert_eq!(view.cost_usage.buckets.get("USD"), Some(&1.25));
    assert_eq!(view.cost_usage.buckets.get("CNY"), Some(&2.5));
    assert!(!view.cost_usage.comparable);
    assert_eq!(events_of(&reopened, &mission.mission_id, "run_usage_recorded"), 2);
    let mut budget = view.budget.clone().unwrap(); budget.max_cost = Some(100.0);
    seed_budget(&reopened, &mission.mission_id, &budget);
    assert!(reopened.budget_exhaustion_reason(&reopened.get(&mission.mission_id).unwrap()).unwrap().is_some());
    reopened.record_run_usage(&mission.mission_id, "fix", "run-unknown", &LocalRunUsage {
        billing_owner: "unreported".into(), ..usage(0, 0.0, "")
    }).unwrap();
    let unknown = reopened.get(&mission.mission_id).unwrap();
    assert_eq!(unknown.cost_usage.unknown_runs, 1);
    assert!(!unknown.cost_usage.comparable);
    assert_eq!(unknown.cost_usage.buckets.get("USD"), Some(&1.25));
    let before = persisted(&reopened, &mission.mission_id);
    MissionStore::new(reopened.path.clone()).record_run_usage(&mission.mission_id, "fix", "run-unknown",
        &LocalRunUsage { billing_owner: "unreported".into(), ..usage(0, 0.0, "") }).unwrap();
    assert_eq!(persisted(&reopened, &mission.mission_id), before);
}

#[test]
fn f08_parameter_validation_and_overflow_leave_all_persisted_state_unchanged() {
    for variant in ["zero", "range", "overflow", "both", "empty_id", "old_hash", "no_change", "nan", "negative_cost"] {
        let store = isolated_store(variant); let mission = exhausted(&store); let mut request = increase(&mission);
        match variant {
            "zero" => request.additional_tokens = Some(0),
            "range" => request.additional_minutes = 525_601,
            "overflow" => {
                let mut budget = mission.budget.clone().unwrap(); budget.max_rounds = u32::MAX;
                seed_budget(&store, &mission.mission_id, &budget); request.additional_rounds = 1;
            }
            "both" => request.unlimited_tokens = true,
            "empty_id" => request.request_id.clear(),
            "old_hash" => request.expected_plan_sha256 = "b".repeat(64),
            "no_change" => request.additional_tokens = None,
            "nan" => request.additional_cost = Some(f64::NAN),
            "negative_cost" => request.additional_cost = Some(-1.0),
            _ => unreachable!(),
        }
        reject_unchanged(&store, &mission.mission_id, &request);
    }
}

#[test]
fn f09_legacy_explicit_resume_diagnostic_then_explicit_extension_no_automatic_action() {
    let store = isolated_store("f09-safe"); let mission = exhausted(&store);
    let before = persisted(&store, &mission.mission_id);
    // Diagnosis/reload is read-only. The only permitted mutation is explicit resume.
    assert!(store.budget_exhaustion_reason(&mission).unwrap().is_some());
    assert_eq!(persisted(&store, &mission.mission_id), before);
    assert!(store.control(&mission.mission_id, "resume").is_err());
    let waiting = store.get(&mission.mission_id).unwrap();
    assert_eq!(waiting.status, MissionStatus::Waiting);
    assert_eq!(waiting.budget, mission.budget);
    assert_eq!(waiting.confirmation, mission.confirmation);
    assert_eq!(store.snapshot(&mission.mission_id, 0).unwrap().events.last().unwrap().event_type, "budget_exhausted");
    store.append_event(&mission.mission_id, "task_progress", "fix", "progress after diagnostic", serde_json::json!({})).unwrap();
    // New-contract Store rechecks real state rather than using the last event.
    assert_eq!(store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap().status, MissionStatus::Active);
    assert_eq!(events_of(&store, &mission.mission_id, "budget_extended"), 1);
    // Legacy capability negotiation/UI stop-and-handoff is not exposed by Store.
}

#[test]
fn f09_unsafe_legacy_diagnosis_is_read_only_and_extension_rechecks_inflight_and_cost() {
    for variant in ["environment", "running", "untrusted"] {
        let store = isolated_store(variant); let mission = confirmed(&store);
        match variant {
            "environment" => {
                store.suspend_runtime(&mission.mission_id, "fix", &serde_json::json!({"code":"runtime_workspace_blocked"})).unwrap();
            }
            "running" => {
                store.begin_task_attempt(&mission.mission_id, "fix", "run-unsafe").unwrap();
                store.control(&mission.mission_id, "pause").unwrap();
            }
            "untrusted" => {
                store.control(&mission.mission_id, "pause").unwrap();
                store.record_run_usage(&mission.mission_id, "fix", "run-unknown",
                    &LocalRunUsage { billing_owner: "unreported".into(), ..usage(0, 0.0, "") }).unwrap();
                let mut budget = mission.budget.clone().unwrap(); budget.max_cost = Some(10.0);
                seed_budget(&store, &mission.mission_id, &budget);
            }
            _ => unreachable!(),
        }
        let before = persisted(&store, &mission.mission_id);
        let current = store.get(&mission.mission_id).unwrap();
        let _ = store.budget_exhaustion_reason(&current).unwrap();
        let _ = store.runtime_blocked(&mission.mission_id).unwrap();
        assert_eq!(persisted(&store, &mission.mission_id), before);
        if variant == "environment" {
            // This is an explicit user probe, not an automatic fallback loop.
            assert!(store.control(&mission.mission_id, "resume").is_err());
            assert_eq!(persisted(&store, &mission.mission_id), before);
        } else {
            reject_unchanged(&store, &mission.mission_id, &increase(&mission));
        }
        assert_eq!(events_of(&store, &mission.mission_id, "budget_extended"), 0);
    }
}

#[test]
fn f10_new_consumption_exhausts_again_and_old_plan_events_do_not_complete_or_activate() {
    let store = isolated_store("f10"); let mission = exhausted(&store);
    let active = store.increase_budget(&mission.mission_id, &increase(&mission), "budget-test-user").unwrap();
    assert_eq!(active.status, MissionStatus::Active);
    assert!(active.tasks.iter().all(|task| task.status != TaskStatus::Completed));
    store.record_run_usage(&mission.mission_id, "fix", "run-after-extension", &usage(50, 0.5, "USD")).unwrap();
    assert!(store.next_ready_task(&mission.mission_id).unwrap().is_none());
    let waiting = store.get(&mission.mission_id).unwrap();
    assert_eq!(waiting.status, MissionStatus::Waiting);
    assert!(store.budget_exhaustion_reason(&waiting).unwrap().is_some());
    let count = events_of(&store, &mission.mission_id, "task_dispatched");
    store.append_event(&mission.mission_id, "budget_extended", "", "stale old-plan event",
        serde_json::json!({"plan_sha256":"b".repeat(64),"budget":{"max_total_tokens":999999}})).unwrap();
    store.append_event(&mission.mission_id, "task_progress", "fix", "old plan progress",
        serde_json::json!({"plan_sha256":"b".repeat(64)})).unwrap();
    let reopened = MissionStore::new(store.path.clone());
    let after = reopened.get(&mission.mission_id).unwrap();
    assert_eq!(after.status, MissionStatus::Waiting);
    assert_eq!(after.budget, waiting.budget);
    assert_eq!(after.confirmation, waiting.confirmation);
    assert!(reopened.budget_exhaustion_reason(&after).unwrap().is_some());
    assert_eq!(events_of(&reopened, &mission.mission_id, "task_dispatched"), count);
}
