// Included in store::tests to reuse its isolated package fixtures.
fn runtime_change_fixture(
    name: &str,
    confirm_changes: bool,
) -> (
    MissionStore,
    MissionView,
    super::super::MissionAssignmentOptions,
    ExpertTestHome,
) {
    let (store, workflows, pin, mut assignment, home) = assignment_fixture(name);
    let mut input = imported_handoff(&pin);
    if confirm_changes {
        input.workspace.adjustment_mode = crate::mission::MissionAdjustmentMode::ConfirmChanges;
    }
    let mut side = task("design");
    side.write_paths = vec!["frontend/src/design".into()];
    input.imported_plan.as_mut().unwrap().tasks.push(side);
    assignment
        .task_roles
        .push(crate::mission::MissionTaskRoleMapping {
            task_id: "design".into(),
            role_id: "ui.implementer.shared".into(),
        });
    let mission = store.create_handoff_with_store(&input, &workflows).unwrap();
    let assigned = store
        .assign_roles_with_store(&mission.mission_id, &assignment, &workflows)
        .unwrap();
    let active = store
        .confirm_plan(
            &mission.mission_id,
            &assigned.plan_sha256,
            &["fix".into(), "verify".into(), "design".into()],
            &["frontend/src".into()],
            &[],
            &[],
            "test-user",
        )
        .unwrap();
    let catalog = serde_json::from_value(serde_json::json!({
        "workflow_id":pin.workflow_id,"workflow_version":pin.workflow_version,"workflow_sha256":pin.workflow_sha256,"source":"workflow",
        "roles":[],"experts":crate::expert::list_versions().unwrap(),
        "executors":[{"executor":"himind.builtin","label":"HiMind AI","available":true,"reason":"",
            "models":[{"alias":"himind:service:model","label":"model","service_id":"service","service_label":"Service"}]}]
    })).unwrap();
    (store, active, catalog, home)
}

fn runtime_change(
    store: &MissionStore,
    mission: &MissionView,
    name: &str,
) -> MissionPlanChangeProposal {
    let input = MissionMessageInput {
        request_id: format!("instruction-{name}"),
        expected_plan_revision: mission.plan_revision,
        expected_plan_sha256: mission.plan_sha256.clone(),
        kind: MissionMessageKind::Instruction,
        message: "增加离线存储和离线验收，界面继续".into(),
        target_task_ids: vec!["verify".into()],
    };
    store.submit_message(&mission.mission_id, &input).unwrap();
    store
        .record_instruction_status(
            &mission.mission_id,
            &input.request_id,
            "analyzing",
            "正在分析",
        )
        .unwrap();
    let mut offline = mission.tasks[0].spec.clone();
    offline.id = format!("offline-{name}");
    offline.title = "离线存储".into();
    offline.depends_on = vec!["fix".into()];
    MissionPlanChangeProposal {
        mission_id: mission.mission_id.clone(),
        request_id: format!("change-{name}"),
        instruction_request_id: input.request_id,
        expected_plan_revision: mission.plan_revision,
        expected_plan_sha256: mission.plan_sha256.clone(),
        summary: "新增离线存储与验收".into(),
        rationale: "用户明确追加要求".into(),
        added_roles: vec![],
        updates: vec![MissionPlanTaskUpdate {
            priority: None,
            task_id: "verify".into(),
            title: None,
            objective: None,
            depends_on: Some(vec![offline.id.clone()]),
            acceptance_criteria: None,
            required_checks: None,
            role_id: None,
        }],
        added_tasks: vec![offline],
        cancel_task_ids: vec![],
        required_acceptance: vec!["离线运行检查通过".into()],
    }
}

fn change_apply(change: &MissionPlanChangeProposal) -> MissionPlanChangeApply {
    MissionPlanChangeApply {
        mission_id: change.mission_id.clone(),
        request_id: change.request_id.clone(),
        expected_plan_revision: change.expected_plan_revision,
        expected_plan_sha256: change.expected_plan_sha256.clone(),
        authorization: None,
    }
}

#[test]
fn incremental_change_preserves_running_branch_budget_and_restart_idempotency() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-running", false);
    let increase =
        serde_json::from_value(serde_json::json!({"request_id":"increase-before-change",
        "expected_plan_sha256":active.plan_sha256,"additional_rounds":3}))
        .unwrap();
    store.control(&active.mission_id, "pause").unwrap();
    store
        .increase_budget(&active.mission_id, &increase, "test-user")
        .unwrap();
    let active = store.control(&active.mission_id, "resume").unwrap();
    let running = store
        .begin_task_attempt(&active.mission_id, "fix", "delta-running-worker")
        .unwrap();
    let claim = store.workspace_claim(&active.workspace.root).unwrap();
    let change = runtime_change(&store, &running, "offline");
    assert_eq!(
        store
            .record_plan_change_proposal(&change, &catalog)
            .unwrap()
            .status,
        "ready_to_apply"
    );
    let request = change_apply(&change);
    let result = store.apply_plan_change(&request, &catalog, None).unwrap();
    let reopened = MissionStore::new(store.path.clone());
    let current = reopened.get(&active.mission_id).unwrap();
    assert_eq!(current.plan_revision, active.plan_revision + 1);
    assert_eq!(current.budget, active.budget);
    assert_eq!(
        reopened
            .proposed_plan(&active.mission_id)
            .unwrap()
            .unwrap()
            .budget,
        active.budget.clone().unwrap()
    );
    assert_eq!(current.tasks[0], running.tasks[0]);
    assert_eq!(current.tasks[2], running.tasks[2]);
    assert_eq!(
        reopened.workspace_claim(&active.workspace.root).unwrap(),
        claim
    );
    let before = reopened.snapshot(&active.mission_id, 0).unwrap();
    assert_eq!(
        reopened
            .apply_plan_change(&request, &catalog, None)
            .unwrap(),
        result
    );
    assert_eq!(
        reopened
            .record_plan_change_proposal(&change, &catalog)
            .unwrap(),
        result
    );
    assert_eq!(
        reopened.snapshot(&active.mission_id, 0).unwrap().events,
        before.events
    );
}

#[test]
fn incremental_change_confirmation_holds_only_affected_work_and_withdrawal_releases_it() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-confirm", true);
    let mut change = runtime_change(&store, &active, "confirm");
    change.updates[0].task_id = "design".into();
    change.updates[0].depends_on = None;
    change.updates[0].objective = Some("新的界面要求".into());
    let result = store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    assert_eq!(result.status, "awaiting_confirmation");
    let waiting = store.get(&active.mission_id).unwrap();
    assert_eq!(waiting.tasks[0].status, TaskStatus::Ready);
    assert_eq!(waiting.tasks[2].status, TaskStatus::Blocked);
    assert!(store
        .begin_task_attempt(&active.mission_id, "design", "cannot-start")
        .is_err());
    assert!(store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .is_err());
    store
        .record_instruction_status(
            &active.mission_id,
            &change.instruction_request_id,
            "not_applied",
            "用户撤回待确认要求",
        )
        .unwrap();
    assert_eq!(
        store.get(&active.mission_id).unwrap().tasks[2].status,
        TaskStatus::Ready
    );
    assert!(store
        .apply_plan_change(&change_apply(&change), &catalog, Some("test-user"))
        .is_err());
    assert_eq!(
        store.get(&active.mission_id).unwrap().plan_revision,
        active.plan_revision
    );
}

#[test]
fn incremental_change_conflict_and_invalid_catalog_are_atomic() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-conflict", false);
    let first = runtime_change(&store, &active, "first");
    let second = runtime_change(&store, &active, "second");
    store.record_plan_change_proposal(&first, &catalog).unwrap();
    store
        .record_plan_change_proposal(&second, &catalog)
        .unwrap();
    let mut unavailable = catalog.clone();
    unavailable.executors[0].available = false;
    let before = store.snapshot(&active.mission_id, 0).unwrap();
    assert!(store
        .apply_plan_change(&change_apply(&first), &unavailable, None)
        .is_err());
    assert_eq!(
        store.snapshot(&active.mission_id, 0).unwrap().events,
        before.events
    );
    store
        .apply_plan_change(&change_apply(&first), &catalog, None)
        .unwrap();
    let before = store.snapshot(&active.mission_id, 0).unwrap();
    assert!(store
        .apply_plan_change(&change_apply(&second), &catalog, None)
        .unwrap_err()
        .to_string()
        .contains("conflicts"));
    assert_eq!(
        store.snapshot(&active.mission_id, 0).unwrap().events,
        before.events
    );
    assert_eq!(store.get(&active.mission_id).unwrap().tasks.len(), 4);
}

#[test]
fn incremental_change_scope_requires_exact_host_confirmation() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-scope", false);
    let mut change = runtime_change(&store, &active, "scope");
    change.added_tasks[0].write_paths = vec!["offline".into()];
    let result = store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    assert_eq!(result.status, "awaiting_authorization");
    assert_eq!(result.authorization_write_paths, vec!["offline"]);
    let mut request = change_apply(&change);
    assert!(store
        .apply_plan_change(&request, &catalog, Some("test-user"))
        .is_err());
    request.authorization = Some(MissionPlanChangeAuthorization {
        read_paths: vec![],
        write_paths: vec!["offline".into()],
        authorized_by: "test-user".into(),
        reason: "批准新增离线目录".into(),
    });
    assert!(store.apply_plan_change(&request, &catalog, None).is_err());
    let mut extra = request.clone();
    extra
        .authorization
        .as_mut()
        .unwrap()
        .write_paths
        .push("private".into());
    assert!(store
        .apply_plan_change(&extra, &catalog, Some("test-user"))
        .is_err());
    store
        .apply_plan_change(&request, &catalog, Some("test-user"))
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    assert!(current
        .confirmation
        .unwrap()
        .approved_paths
        .contains(&"offline".into()));
}

#[test]
fn incremental_change_rejects_unselected_dependencies_and_finalized_cancellation() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-invalid", false);
    let mut change = runtime_change(&store, &active, "invalid");
    change.updates[0].depends_on = Some(vec!["missing".into()]);
    let before = store.snapshot(&active.mission_id, 0).unwrap();
    assert!(store
        .record_plan_change_proposal(&change, &catalog)
        .is_err());
    assert_eq!(
        store.snapshot(&active.mission_id, 0).unwrap().events,
        before.events
    );
    change.updates[0].depends_on = Some(vec!["fix".into()]);
    change.cancel_task_ids = vec!["design".into()];
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    // Simulate a racing terminal transition; applying must revalidate, not trust the proposal.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mission_tasks SET status='completed' WHERE mission_id=?1 AND task_id='design'",
            [&active.mission_id],
        )
        .unwrap();
    assert!(store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .is_err());
    assert_eq!(
        store.get(&active.mission_id).unwrap().plan_revision,
        active.plan_revision
    );
}

#[test]
fn incremental_change_coordination_inbox_survives_restart_without_double_admission() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-inbox", false);
    let change = runtime_change(&store, &active, "inbox");
    let queued = store
        .pending_coordination_messages(&active.mission_id)
        .unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].request_id, change.instruction_request_id);
    assert!(store
        .admit_instruction_analysis(&active.mission_id, &change.instruction_request_id)
        .unwrap());
    let reopened = MissionStore::new(store.path.clone());
    assert!(!reopened
        .admit_instruction_analysis(&active.mission_id, &change.instruction_request_id)
        .unwrap());
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    assert_eq!(
        reopened
            .pending_coordination_messages(&active.mission_id)
            .unwrap()
            .len(),
        1
    );
    store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .unwrap();
    assert!(reopened
        .pending_coordination_messages(&active.mission_id)
        .unwrap()
        .is_empty());
    let snapshot = store.snapshot(&active.mission_id, 0).unwrap();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| event.event_type == "mission_coordination_started")
            .count(),
        1
    );
}

#[test]
fn incremental_change_superseded_or_withdrawn_requests_report_not_applied() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-obsolete", false);
    let first = runtime_change(&store, &active, "winner");
    let second = runtime_change(&store, &active, "obsolete");
    store.record_plan_change_proposal(&first, &catalog).unwrap();
    store
        .record_plan_change_proposal(&second, &catalog)
        .unwrap();
    store
        .apply_plan_change(&change_apply(&first), &catalog, None)
        .unwrap();
    assert_eq!(
        store
            .record_plan_change_proposal(&second, &catalog)
            .unwrap()
            .status,
        "not_applied"
    );
    assert!(store
        .pending_coordination_messages(&active.mission_id)
        .unwrap()
        .is_empty());
}
#[test]
fn incremental_change_retains_old_verification_across_multiple_revisions() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-signature", false);
    store
        .begin_task_attempt(&active.mission_id, "fix", "old-worker")
        .unwrap();
    store
        .await_task_verification(&active.mission_id, "fix", "old-worker", "送验")
        .unwrap();
    store
        .begin_verification_attempt(&active.mission_id, "fix", "old-review")
        .unwrap();
    store
        .append_event(
            &active.mission_id,
            "host_verification_checked",
            "fix",
            "旧候选宿主检查",
            serde_json::json!({"passed":true,"plan_sha256":active.plan_sha256,
            "implementation_run_id":"old-worker","verification_run_id":"old-review"}),
        )
        .unwrap();
    for name in ["one", "two"] {
        let current = store.get(&active.mission_id).unwrap();
        let change = runtime_change(&store, &current, name);
        store
            .record_plan_change_proposal(&change, &catalog)
            .unwrap();
        store
            .apply_plan_change(&change_apply(&change), &catalog, None)
            .unwrap();
    }
    let reopened = MissionStore::new(store.path.clone());
    assert!(reopened
        .evidence_plan_compatible(&active.mission_id, "fix", "old-worker", &active.plan_sha256)
        .unwrap());
    assert!(!reopened
        .evidence_plan_compatible(
            &active.mission_id,
            "fix",
            "unknown-worker",
            &active.plan_sha256
        )
        .unwrap());
    assert!(!reopened
        .evidence_plan_compatible(&active.mission_id, "fix", "old-worker", &"f".repeat(64))
        .unwrap());
    let completed = reopened
        .record_verification_result(
            &active.mission_id,
            "fix",
            "old-worker",
            "old-review",
            true,
            &["local-run:old-review".into()],
            "",
        )
        .unwrap();
    assert_eq!(completed.tasks[0].status, TaskStatus::Completed);
    let context = reopened
        .executor_handoff_context(&active.mission_id)
        .unwrap();
    assert_eq!(
        context
            .1
            .iter()
            .filter(|event| event.event_type == "mission_attempt_plan_retained")
            .count(),
        2
    );
}

#[test]
fn incremental_change_rejects_changed_inputs_and_canceled_late_results() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-late", false);
    store
        .begin_task_attempt(&active.mission_id, "fix", "original-worker")
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    let change = runtime_change(&store, &current, "late");
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    let events = store.snapshot(&active.mission_id, 0).unwrap().events;
    let mut changed = current.clone();
    changed.tasks[0].spec.objective.push_str("changed");
    assert!(!super::super::attempt_signature::evidence_plan_compatible(
        &changed,
        &changed.tasks[0],
        &active.plan_sha256,
        &events
    ));
    let mut canceled = current.clone();
    canceled.tasks[0].status = TaskStatus::Canceled;
    assert!(!super::super::attempt_signature::evidence_plan_compatible(
        &canceled,
        &canceled.tasks[0],
        &current.plan_sha256,
        &events
    ));
    store.control(&active.mission_id, "cancel").unwrap();
    assert!(store
        .complete_task_attempt(
            &active.mission_id,
            "fix",
            "original-worker",
            true,
            &["late".into()],
            ""
        )
        .is_err());
}

fn finish_runtime_change_task(
    store: &MissionStore,
    mission_id: &str,
    task_id: &str,
    worker: &str,
    reviewer: &str,
) {
    let current = store.get(mission_id).unwrap();
    if current
        .tasks
        .iter()
        .find(|task| task.spec.id == task_id)
        .unwrap()
        .status
        != TaskStatus::Running
    {
        store
            .begin_task_attempt(mission_id, task_id, worker)
            .unwrap();
    }
    store
        .await_task_verification(mission_id, task_id, worker, "送验")
        .unwrap();
    store
        .begin_verification_attempt(mission_id, task_id, reviewer)
        .unwrap();
    let current = store.get(mission_id).unwrap();
    store
        .append_event(
            mission_id,
            "host_verification_checked",
            task_id,
            "宿主检查",
            serde_json::json!({"passed":true,"plan_sha256":current.plan_sha256,
            "implementation_run_id":worker,"verification_run_id":reviewer}),
        )
        .unwrap();
    store
        .record_verification_result(
            mission_id,
            task_id,
            worker,
            reviewer,
            true,
            &[format!("local-run:{reviewer}")],
            "",
        )
        .unwrap();
}

#[test]
fn incremental_change_waits_for_checkpoint_then_reassigns_without_releasing_live_claim() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-checkpoint", false);
    let running = store
        .begin_task_attempt(&active.mission_id, "fix", "checkpoint-worker")
        .unwrap();
    let mut change = runtime_change(&store, &running, "checkpoint");
    let mut update = change.updates[0].clone();
    update.task_id = "fix".into();
    update.depends_on = None;
    update.objective = Some("在安全检查点后按新要求实现".into());
    change.updates.push(update);
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    let request = change_apply(&change);
    assert_eq!(
        store
            .apply_plan_change(&request, &catalog, None)
            .unwrap()
            .status,
        "awaiting_checkpoint"
    );
    assert_eq!(
        store
            .apply_plan_change(&request, &catalog, None)
            .unwrap()
            .status,
        "awaiting_checkpoint"
    );
    assert_eq!(
        store.get(&active.mission_id).unwrap().plan_sha256,
        active.plan_sha256
    );
    assert_eq!(
        store.get(&active.mission_id).unwrap().tasks[0],
        running.tasks[0]
    );
    assert!(store
        .workspace_claim(&active.workspace.root)
        .unwrap()
        .is_some());
    assert_eq!(
        store.get(&active.mission_id).unwrap().tasks[2].status,
        TaskStatus::Ready
    );
    finish_runtime_change_task(
        &store,
        &active.mission_id,
        "fix",
        "checkpoint-worker",
        "checkpoint-review",
    );
    let reopened = MissionStore::new(store.path.clone());
    let (saved, actor) = reopened
        .checkpoint_change_request(&active.mission_id, &change.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved, request);
    assert_eq!(
        reopened
            .apply_plan_change(&saved, &catalog, actor.as_deref())
            .unwrap()
            .status,
        "applied"
    );
    let current = reopened.get(&active.mission_id).unwrap();
    assert_eq!(current.tasks[0].status, TaskStatus::Ready);
    assert!(current.tasks[0].attempt_run_id.is_empty());
    assert!(current.tasks[0].verification_passed.is_none());
    assert!(reopened
        .record_verification_result(
            &active.mission_id,
            "fix",
            "checkpoint-worker",
            "checkpoint-review",
            true,
            &["local-run:checkpoint-review".into()],
            ""
        )
        .is_err());
    assert_eq!(
        reopened
            .snapshot(&active.mission_id, 0)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.event_type == "mission_plan_change_checkpoint")
            .count(),
        1
    );
}

#[test]
fn incremental_change_invalidates_completed_downstream_but_keeps_unrelated_evidence() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-invalidate", false);
    finish_runtime_change_task(
        &store,
        &active.mission_id,
        "fix",
        "producer",
        "producer-review",
    );
    finish_runtime_change_task(
        &store,
        &active.mission_id,
        "verify",
        "consumer",
        "consumer-review",
    );
    let current = store.get(&active.mission_id).unwrap();
    let mut change = runtime_change(&store, &current, "invalidate");
    change.added_tasks.clear();
    change.updates[0].task_id = "fix".into();
    change.updates[0].depends_on = None;
    change.updates[0].objective = Some("增加离线实现要求，重新验收下游".into());
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    assert_eq!(current.tasks[0].status, TaskStatus::Ready);
    assert_eq!(current.tasks[1].status, TaskStatus::Blocked);
    assert!(current.tasks[1].attempt_run_id.is_empty());
    assert!(current.tasks[1].evidence_refs.is_empty());
    assert_eq!(current.tasks[2], active.tasks[2]);
    let events = store.snapshot(&active.mission_id, 0).unwrap().events;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "mission_task_evidence_invalidated")
            .count(),
        2
    );
    assert!(events
        .iter()
        .any(|event| event.event_type == "task_completed" && event.task_id == "verify"));
}

#[test]
fn incremental_change_canceled_coordination_usage_is_metered_without_reactivation() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, _catalog, _home) = runtime_change_fixture("delta-cancel-usage", false);
    store.control(&active.mission_id, "cancel").unwrap();
    let usage = crate::agent_core_contracts::LocalRunUsage {
        input_tokens: 123,
        output_tokens: 45,
        estimated_cost: 0.0,
        currency: "USD".into(),
        billing_owner: "service".into(),
    };
    store
        .record_run_usage(
            &active.mission_id,
            "coordination",
            "cancel-model-run",
            &usage,
        )
        .unwrap();
    store
        .record_run_usage(
            &active.mission_id,
            "coordination",
            "cancel-model-run",
            &usage,
        )
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    assert_eq!(current.status, MissionStatus::Canceled);
    assert_eq!(current.usage.unwrap().input_tokens, 123);
}
#[test]
fn incremental_change_unified_message_routing_is_conservative_and_idempotent() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, _catalog, _home) = runtime_change_fixture("delta-routing", false);
    let base = MissionMessageInput {
        request_id: "natural-question".into(),
        expected_plan_revision: active.plan_revision,
        expected_plan_sha256: active.plan_sha256.clone(),
        kind: MissionMessageKind::Message,
        target_task_ids: vec![],
        message: "为什么验收还没开始？".into(),
    };
    assert_eq!(
        store
            .submit_message(&active.mission_id, &base)
            .unwrap()
            .status,
        "answered"
    );
    assert_eq!(
        store
            .submit_message(&active.mission_id, &base)
            .unwrap()
            .status,
        "answered"
    );
    let after_question = store.get(&active.mission_id).unwrap();
    assert_eq!(after_question.plan_sha256, active.plan_sha256);
    assert_eq!(after_question.tasks, active.tasks);
    assert_eq!(after_question.confirmation, active.confirmation);
    let mut ambiguous = base.clone();
    ambiguous.request_id = "maybe-offline".into();
    ambiguous.message = "是否可以增加离线支持？".into();
    assert_eq!(
        store
            .submit_message(&active.mission_id, &ambiguous)
            .unwrap()
            .status,
        "needs_clarification"
    );
    assert!(store
        .pending_coordination_messages(&active.mission_id)
        .unwrap()
        .is_empty());
    let mut instruction = base;
    instruction.request_id = "natural-instruction".into();
    instruction.message = "新增离线支持，界面继续".into();
    assert_eq!(
        store
            .submit_message(&active.mission_id, &instruction)
            .unwrap()
            .status,
        "received"
    );
    assert_eq!(
        store
            .pending_coordination_messages(&active.mission_id)
            .unwrap()
            .len(),
        1
    );
    store
        .record_instruction_status(
            &active.mission_id,
            &instruction.request_id,
            "not_applied",
            "用户立即撤回",
        )
        .unwrap();
    assert!(store
        .pending_coordination_messages(&active.mission_id)
        .unwrap()
        .is_empty());
}

#[test]
fn incremental_change_new_model_route_requires_confirmation_even_with_full_access() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, mut catalog, _home) = runtime_change_fixture("delta-route", false);
    let mut change = runtime_change(&store, &active, "route");
    let mut role = active.assignment.as_ref().unwrap().roles[0].clone();
    role.role_id = "new-specialty".into();
    role.model = "himind:service:other".into();
    let mut model = catalog.executors[0].models[0].clone();
    model.alias = role.model.clone();
    catalog.executors[0].models.push(model);
    change.added_roles = vec![role.clone()];
    change.added_tasks[0].execution.model = role.model;
    change.added_tasks[0].execution.role_id = role.role_id;
    assert_eq!(
        store
            .record_plan_change_proposal(&change, &catalog)
            .unwrap()
            .status,
        "awaiting_confirmation"
    );
    assert!(store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .is_err());
    assert_eq!(
        store
            .apply_plan_change(&change_apply(&change), &catalog, Some("test-user"))
            .unwrap()
            .status,
        "applied"
    );
}

#[test]
fn incremental_change_priority_reorders_unstarted_dispatch_without_changing_specs() {
    let _lock = crate::store::paths::test_env_lock();
    let (store, active, catalog, _home) = runtime_change_fixture("delta-priority", false);
    let mut change = runtime_change(&store, &active, "priority");
    change.added_tasks.clear();
    change.updates[0].task_id = "design".into();
    change.updates[0].depends_on = None;
    change.updates[0].priority = Some(90);
    store
        .record_plan_change_proposal(&change, &catalog)
        .unwrap();
    store
        .apply_plan_change(&change_apply(&change), &catalog, None)
        .unwrap();
    let current = store.get(&active.mission_id).unwrap();
    assert_eq!(current.tasks[0].spec, active.tasks[2].spec);
    assert_eq!(current.tasks[0].status, TaskStatus::Ready);
    assert_eq!(
        store
            .next_ready_task(&active.mission_id)
            .unwrap()
            .unwrap()
            .id,
        "design"
    );
}
