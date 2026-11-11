use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::contract::{
    absolute_path_approved, is_absolute_path, normalized_paths, path_allowed, plan_hash, require_id, require_sha256, require_text,
    apply_assignment_to_plan, validate_assignment, validate_create, validate_handoff_input,
    validate_proposal, validate_task_paths, validate_selection_with_scopes, validate_task_graph,
    MissionAssignment, MissionAssignmentInput, MissionConfirmation, MissionCreateInput,
    MissionEvent, MissionHandoff, MissionHandoffInput, MissionPlanMode, MissionPlanProposal,
    MissionStatus, MissionTaskSpec, MissionTaskView, MissionView, MissionWorkflowPin, TaskStatus,
};

pub(crate) const MISSION_DB_FILE: &str = "mission-runs.sqlite3";
const MAX_EVENTS: usize = 10_000;
const MAX_SUMMARY_CHARS: usize = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MissionSnapshot {
    pub mission: MissionView,
    pub events: Vec<MissionEvent>,
    pub cursor: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct MissionStore {
    path: PathBuf,
}

impl MissionStore {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        let root = crate::store::paths::agent_home();
        fs::create_dir_all(&root)?;
        Ok(Self::new(root.join(MISSION_DB_FILE)))
    }

    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Create one Mission from a client handoff. The request is idempotent by
    /// `idempotency_key`: replay returns the existing Mission only when the
    /// canonical request digest matches.
    pub(crate) fn create_handoff(&self, input: &MissionHandoffInput) -> Result<MissionView, Box<dyn Error>> {
        let workflows = crate::workflow::WorkflowStore::open_default()?;
        self.create_handoff_with_store(input, &workflows)
    }

    pub(crate) fn create_handoff_with_store(
        &self,
        input: &MissionHandoffInput,
        workflows: &crate::workflow::WorkflowStore,
    ) -> Result<MissionView, Box<dyn Error>> {
        validate_handoff_input(input)?;
        // A handoff must name an exact installed artifact. The host, not the
        // source client, proves the version/digest relationship before storing it.
        workflows.load_pinned_version(
            &input.workflow.workflow_id,
            &input.workflow.workflow_version,
            &input.workflow.workflow_sha256,
        )?;
        let request_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(input)?));
        let mission_id = new_mission_id();
        let now = now_stamp();
        let handoff = MissionHandoff {
            idempotency_key: input.idempotency_key.trim().to_string(),
            source_client: input.source_client.trim().to_string(),
            source_reference: input.source_reference.trim().to_string(),
            source_message_refs: input.source_message_refs.clone(),
            context_summary: input.context_summary.trim().to_string(),
            request_sha256: request_sha256.clone(),
            received_at: now.clone(),
        };
        let proposal = input.imported_plan.clone();
        let plan_sha256 = proposal.as_ref().map(|proposal| plan_hash(
            &mission_id, &input.workspace.root, &input.acceptance_criteria, proposal,
        )).transpose()?.unwrap_or_default();
        let revision = if proposal.is_some() { 1 } else { 0 };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction.query_row(
            "SELECT mission_id, handoff_json FROM mission_runs WHERE idempotency_key = ?1",
            params![handoff.idempotency_key], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional()?;
        if let Some((existing_id, stored)) = existing {
            let stored: MissionHandoff = serde_json::from_str(stored.as_deref().unwrap_or("{}"))?;
            if stored.request_sha256 != request_sha256 {
                return Err("idempotency key was already used by a different handoff request".into());
            }
            transaction.commit()?;
            return self.get(&existing_id);
        }
        transaction.execute(
            "INSERT INTO mission_runs(
                mission_id, title, objective, status, workspace_json, acceptance_json,
                plan_revision, plan_sha256, plan_json, confirmation_json, budget_json,
                created_at, updated_at, handoff_json, workflow_json, assignment_json,
                plan_mode, idempotency_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, ?12, ?13, ?14, NULL, ?15, ?16)",
            params![
                mission_id, input.title.trim(), input.objective.trim(),
                MissionStatus::AwaitingAssignment.as_str(),
                serde_json::to_string(&input.workspace)?, serde_json::to_string(&input.acceptance_criteria)?,
                revision, plan_sha256,
                serde_json::to_string(&proposal)?, proposal.as_ref().map(|p| serde_json::to_string(&p.budget)).transpose()?,
                now, now, serde_json::to_string(&handoff)?, serde_json::to_string(&input.workflow)?,
                input.plan_mode.as_str(), handoff.idempotency_key,
            ],
        )?;
        append_event_tx(&transaction, &mission_id, "mission_handoff_received", "",
            format!("已接收来自 {} 的协作任务交接，等待角色与服务指派", handoff.source_client),
            serde_json::json!({
                "source_client": handoff.source_client,
                "source_reference": handoff.source_reference,
                "idempotency_key": handoff.idempotency_key,
                "workflow": input.workflow,
                "plan_mode": input.plan_mode.as_str(),
            }), &now)?;
        if proposal.is_some() {
            append_event_tx(&transaction, &mission_id, "plan_proposed", "",
                "已导入来源客户端方案，等待指派后确认", serde_json::json!({"plan_sha256": plan_sha256}), &now)?;
        }
        transaction.commit()?;
        self.get(&mission_id)
    }

    /// Persist the user's role-to-binding choices. This is still not consent to
    /// run: imported plans move to confirmation, generated plans remain planned
    /// until the workflow proposes a task graph.
    pub(crate) fn assign_roles(&self, mission_id: &str, input: &MissionAssignmentInput) -> Result<MissionView, Box<dyn Error>> {
        let workflows = crate::workflow::WorkflowStore::open_default()?;
        self.assign_roles_with_store(mission_id, input, &workflows)
    }

    pub(crate) fn assign_roles_with_store(
        &self,
        mission_id: &str,
        input: &MissionAssignmentInput,
        workflows: &crate::workflow::WorkflowStore,
    ) -> Result<MissionView, Box<dyn Error>> {
        validate_assignment(input)?;
        let (mission, mut proposal) = self.get_with_plan(mission_id)?;
        if !matches!(mission.status, MissionStatus::AwaitingAssignment | MissionStatus::Planned | MissionStatus::AwaitingConfirmation) {
            return Err("mission is not awaiting role assignment".into());
        }
        let workflow = mission.workflow.as_ref().ok_or("workflow-pinned mission has no workflow pin")?;
        let (package, _) = workflows.load_pinned_version(
            &workflow.workflow_id,
            &workflow.workflow_version,
            &workflow.workflow_sha256,
        )?;
        let allowed_roles = package.roles.keys().cloned().collect::<BTreeSet<_>>();
        if allowed_roles.is_empty() {
            let roles = package.steps.iter().map(|step| step.role_id.as_str()).filter(|role| !role.is_empty()).map(str::to_string).collect::<BTreeSet<_>>();
            if input.roles.iter().any(|role| !roles.contains(&role.role_id)) {
                return Err("assignment contains a role not declared by the pinned workflow".into());
            }
        } else if input.roles.iter().any(|role| !allowed_roles.contains(&role.role_id)) {
            return Err("assignment contains a role not declared by the pinned workflow".into());
        }
        let assignment = MissionAssignment {
            roles: input.roles.clone(), task_roles: input.task_roles.clone(),
            applied_by: input.applied_by.trim().to_string(),
            applied_at: now_stamp(), recommended: input.recommended,
        };
        let imported = mission.plan_mode == MissionPlanMode::Imported;
        let next_status = if imported { MissionStatus::AwaitingConfirmation } else { MissionStatus::Planned };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if imported {
            if proposal.tasks.is_empty() {
                return Err("imported mission has no plan to assign".into());
            }
            proposal = apply_assignment_to_plan(&proposal, input)?;
            let hash = plan_hash(&mission_id, &mission.workspace.root, &mission.acceptance_criteria, &proposal)?;
            transaction.execute(
                "UPDATE mission_runs SET assignment_json = ?2, status = ?3, plan_sha256 = ?4, plan_json = ?5, updated_at = ?6 WHERE mission_id = ?1",
                params![mission_id, serde_json::to_string(&assignment)?, next_status.as_str(), hash, serde_json::to_string(&proposal)?, assignment.applied_at],
            )?;
        } else {
            transaction.execute(
                "UPDATE mission_runs SET assignment_json = ?2, status = ?3, updated_at = ?4 WHERE mission_id = ?1",
                params![mission_id, serde_json::to_string(&assignment)?, next_status.as_str(), assignment.applied_at],
            )?;
        }
        append_event_tx(&transaction, mission_id, "mission_roles_assigned", "",
            format!("已为 {} 个角色应用执行与服务绑定", input.roles.len()),
            serde_json::json!({"roles": input.roles, "recommended": input.recommended}), &assignment.applied_at)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn create(&self, input: &MissionCreateInput) -> Result<MissionView, Box<dyn Error>> {
        validate_create(input)?;
        let mission_id = new_mission_id();
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO mission_runs(
                mission_id, title, objective, status, workspace_json,
                acceptance_json, plan_revision, plan_sha256, plan_json,
                confirmation_json, budget_json, created_at, updated_at,
                handoff_json, workflow_json, assignment_json, plan_mode, idempotency_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, '', '{}', NULL, NULL, ?7, ?8, NULL, NULL, NULL, 'generated', NULL)",
            params![
                mission_id,
                input.title.trim(),
                input.objective.trim(),
                MissionStatus::Draft.as_str(),
                serde_json::to_string(&input.workspace)?,
                serde_json::to_string(&input.acceptance_criteria)?,
                now,
                now,
            ],
        )?;
        append_event_tx(
            &transaction,
            &mission_id,
            "mission_created",
            "",
            "任务已创建，等待协调 Agent 提出执行计划",
            serde_json::json!({"title": input.title.trim()}),
            &now,
        )?;
        transaction.commit()?;
        self.get(&mission_id)
    }

    pub(crate) fn list(&self, limit: usize) -> Result<Vec<MissionView>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT mission_id FROM mission_runs ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let ids = statement
        .query_map(params![limit.clamp(1, 500) as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| self.get(&id)).collect()
    }

    pub(crate) fn get(&self, mission_id: &str) -> Result<MissionView, Box<dyn Error>> {
        require_id("mission_id", mission_id)?;
        let connection = self.connection()?;
        load_mission(&connection, mission_id)?
            .ok_or_else(|| format!("mission not found: {mission_id}").into())
    }

    pub(crate) fn control(&self, mission_id: &str, action: &str) -> Result<MissionView, Box<dyn Error>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        let next = match (action, mission.status) {
            ("pause", MissionStatus::Active) => MissionStatus::Waiting,
            ("resume", MissionStatus::Waiting) => {
                if let Some(reason) = budget_exhaustion_reason_on(&transaction, &mission)? { return Err(reason.into()); }
                MissionStatus::Active
            }
            ("cancel", status) if !matches!(status, MissionStatus::Completed | MissionStatus::Canceled) => MissionStatus::Canceled,
            ("pause", MissionStatus::Waiting) | ("resume", MissionStatus::Active) | ("cancel", MissionStatus::Canceled) => {
                transaction.commit()?; return self.get(mission_id);
            }
            _ => return Err("invalid mission control transition".into()),
        };
        let now = now_stamp();
        transaction.execute("UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
            params![mission_id, next.as_str(), now])?;
        if next == MissionStatus::Canceled {
            transaction.execute("UPDATE mission_tasks SET status = 'canceled', updated_at = ?2
                WHERE mission_id = ?1 AND status NOT IN ('completed', 'failed', 'canceled')", params![mission_id, now])?;
        }
        append_event_tx(&transaction, mission_id, &format!("mission_{action}"), "",
            match action { "pause" => "用户暂停后续派发，当前步骤可收尾", "resume" => "用户恢复任务", _ => "用户取消任务" },
            serde_json::json!({"action": action}), &now)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    /// Called only while holding the dispatch OS lock after execution has stopped.
    pub(crate) fn release_canceled_claims(&self, mission_id: &str) -> Result<(), Box<dyn Error>> {
        let connection = self.connection()?;
        connection.execute("DELETE FROM mission_workspace_claims WHERE mission_id = ?1
            AND EXISTS (SELECT 1 FROM mission_runs WHERE mission_id = ?1 AND status = 'canceled')", params![mission_id])?;
        Ok(())
    }

    pub(crate) fn workspace_claim(
        &self,
        workspace_root: &str,
    ) -> Result<Option<(String, String, String)>, Box<dyn Error>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT mission_id, task_id, run_id FROM mission_workspace_claims WHERE workspace_key = ?1",
                params![workspace_claim_key(workspace_root)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn propose_plan(
        &self,
        mission_id: &str,
        proposal: &MissionPlanProposal,
    ) -> Result<MissionView, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        validate_proposal(proposal)?;
        let assigned = if mission.workflow.is_some() {
            let assignment = mission.assignment.as_ref().ok_or("mission roles must be assigned before planning")?;
            apply_assignment_to_plan(proposal, &MissionAssignmentInput {
                roles: assignment.roles.clone(), task_roles: assignment.task_roles.clone(),
                applied_by: assignment.applied_by.clone(), recommended: assignment.recommended,
            })?
        } else { proposal.clone() };
        let proposal = &assigned;
        if mission.plan_mode == MissionPlanMode::Imported && !mission.plan_sha256.is_empty() {
            return Err("an imported plan cannot be silently replaced by a generated plan".into());
        }
        validate_task_paths(&proposal.tasks, &mission.workspace.root)?;
        if matches!(mission.status, MissionStatus::Completed | MissionStatus::Canceled) {
            return Err("completed or canceled mission cannot accept a new plan".into());
        }
        if mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            return Err("cannot replace a plan with an active implementation or verification attempt".into());
        }
        let plan_sha256 = plan_hash(
            &mission.mission_id,
            &mission.workspace.root,
            &mission.acceptance_criteria,
            proposal,
        )?;
        let revision = mission.plan_revision.saturating_add(1);
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE mission_runs SET status = ?2, plan_revision = ?3,
                plan_sha256 = ?4, plan_json = ?5, confirmation_json = NULL,
                budget_json = ?6, updated_at = ?7
             WHERE mission_id = ?1",
            params![
                mission_id,
                MissionStatus::AwaitingConfirmation.as_str(),
                revision as i64,
                plan_sha256,
                serde_json::to_string(proposal)?,
                serde_json::to_string(&proposal.budget)?,
                now,
            ],
        )?;
        append_event_tx(
            &transaction,
            mission_id,
            "plan_proposed",
            "",
            format!("计划修订 r{revision} 已生成，等待确认 {} 项任务", proposal.tasks.len()),
            serde_json::json!({
                "plan_revision": revision,
                "plan_sha256": plan_sha256,
                "task_ids": proposal.tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>()
            }),
            &now,
        )?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn confirm_plan(
        &self,
        mission_id: &str,
        expected_plan_sha256: &str,
        selected_task_ids: &[String],
        approved_paths: &[String],
        approved_absolute_read_paths: &[String],
        approved_absolute_write_paths: &[String],
        confirmed_by: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_sha256(expected_plan_sha256)?;
        require_text("confirmed_by", confirmed_by, 160)?;
        if selected_task_ids.is_empty() {
            return Err("plan confirmation must select at least one task".into());
        }
        let (mission, proposal) = self.get_with_plan(mission_id)?;
        if mission.status != MissionStatus::AwaitingConfirmation {
            return Err("mission plan is not awaiting confirmation".into());
        }
        if mission.plan_sha256 != expected_plan_sha256 {
            return Err("mission plan changed after review; show the latest revision before confirming".into());
        }
        let selected = selected_task_ids.iter().cloned().collect::<BTreeSet<_>>();
        validate_selection_with_scopes(
            &proposal,
            &selected,
            approved_paths,
            approved_absolute_read_paths,
            approved_absolute_write_paths,
        )?;
        let now = now_stamp();
        let confirmation = MissionConfirmation {
            plan_sha256: mission.plan_sha256.clone(),
            selected_task_ids: selected.iter().cloned().collect(),
            approved_paths: normalized_paths(approved_paths),
            approved_absolute_read_paths: normalized_paths(approved_absolute_read_paths),
            approved_absolute_write_paths: normalized_paths(approved_absolute_write_paths),
            confirmed_by: confirmed_by.trim().to_string(),
            confirmed_at: now.clone(),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE mission_runs SET status = ?2, confirmation_json = ?3, updated_at = ?4
             WHERE mission_id = ?1 AND status = ?5 AND plan_sha256 = ?6",
            params![
                mission_id,
                MissionStatus::Active.as_str(),
                serde_json::to_string(&confirmation)?,
                now,
                MissionStatus::AwaitingConfirmation.as_str(),
                expected_plan_sha256,
            ],
        )?;
        if changed != 1 {
            return Err("mission plan changed concurrently; reload before confirming".into());
        }
        transaction.execute("DELETE FROM mission_tasks WHERE mission_id = ?1", params![mission_id])?;
        for (position, task) in proposal.tasks.iter().enumerate() {
            let status = if selected.contains(&task.id) {
                TaskStatus::Blocked
            } else {
                TaskStatus::Canceled
            };
            transaction.execute(
                "INSERT INTO mission_tasks(
                    mission_id, task_id, position, status, spec_json, attempt_run_id,
                    verification_passed, evidence_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, '', NULL, '[]', ?6)",
                params![
                    mission_id,
                    task.id,
                    position as i64,
                    status.as_str(),
                    serde_json::to_string(task)?,
                    now,
                ],
            )?;
        }
        append_event_tx(
            &transaction,
            mission_id,
            "plan_confirmed",
            "",
            format!("计划 r{} 已确认，{} 项任务获准执行", mission.plan_revision, selected.len()),
            serde_json::json!({
                "plan_sha256": mission.plan_sha256,
                "selected_task_ids": selected,
                "approved_paths": confirmation.approved_paths,
                "approved_absolute_read_paths": confirmation.approved_absolute_read_paths,
                "approved_absolute_write_paths": confirmation.approved_absolute_write_paths,
                "confirmed_by": confirmation.confirmed_by,
            }),
            &now,
        )?;
        recompute_task_readiness_tx(&transaction, mission_id, &now)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    /// Changes one task's executor/model/role before confirmation. The whole
    /// proposal is re-hashed and the old confirmation is cleared, so a binding
    /// change can never reuse an earlier approval.
    pub(crate) fn update_task_execution(
        &self,
        mission_id: &str,
        task_id: &str,
        expected_plan_sha256: &str,
        execution: &crate::mission::ExecutionBinding,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_sha256(expected_plan_sha256)?;
        let (mission, mut proposal) = self.get_with_plan(mission_id)?;
        if mission.status != MissionStatus::AwaitingConfirmation {
            return Err("task execution can only be changed before plan confirmation".into());
        }
        if mission.plan_sha256 != expected_plan_sha256 {
            return Err("plan changed while editing execution binding; reload first".into());
        }
        let task = proposal
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| format!("task is not in the proposed plan: {task_id}"))?;
        task.execution = execution.clone();
        crate::mission::validate_execution_binding(task)?;
        validate_proposal(&proposal)?;
        validate_task_paths(&proposal.tasks, &mission.workspace.root)?;
        let plan_sha256 = plan_hash(
            &mission.mission_id,
            &mission.workspace.root,
            &mission.acceptance_criteria,
            &proposal,
        )?;
        let revision = mission.plan_revision.saturating_add(1);
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE mission_runs SET plan_revision = ?2, plan_sha256 = ?3,
                plan_json = ?4, confirmation_json = NULL, updated_at = ?5
             WHERE mission_id = ?1 AND status = ?6 AND plan_sha256 = ?7",
            params![
                mission_id,
                revision as i64,
                plan_sha256,
                serde_json::to_string(&proposal)?,
                now,
                MissionStatus::AwaitingConfirmation.as_str(),
                expected_plan_sha256,
            ],
        )?;
        if changed != 1 {
            return Err("plan changed concurrently; reload before editing".into());
        }
        append_event_tx(
            &transaction,
            mission_id,
            "task_execution_updated",
            task_id,
            format!("计划 r{revision} 已更新任务执行绑定，等待重新确认"),
            serde_json::json!({
                "plan_sha256": plan_sha256,
                "executor": execution.executor,
                "model": execution.model,
                "role_id": execution.role_id,
            }),
            &now,
        )?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn add_follow_up_tasks(
        &self,
        mission_id: &str,
        tasks: &[MissionTaskSpec],
        reason: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        if tasks.is_empty() {
            return Err("follow-up plan must contain at least one task".into());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        let confirmation = mission.confirmation.as_ref().ok_or("mission has no confirmed plan")?;
        if !matches!(mission.status, MissionStatus::Active | MissionStatus::Waiting | MissionStatus::Planned) {
            return Err("mission is not active for follow-up work".into());
        }
        let mut all = mission.tasks.iter().map(|task| task.spec.clone()).collect::<Vec<_>>();
        all.extend_from_slice(tasks);
        validate_task_graph(&all)?;
        validate_task_paths(tasks, &mission.workspace.root)?;
        let existing = mission.tasks.iter().map(|task| task.spec.id.as_str()).collect::<BTreeSet<_>>();
        if tasks.iter().any(|task| existing.contains(task.id.as_str())) {
            return Err("follow-up task id already exists in mission".into());
        }
        for task in tasks {
            for path in &task.read_paths {
                if is_absolute_path(path)
                    && !absolute_path_approved(path, &confirmation.approved_absolute_read_paths)
                {
                    return Err(format!(
                        "follow-up task {} reads outside confirmed absolute scope: {path}",
                        task.id
                    ).into());
                }
            }
            for path in &task.write_paths {
                let allowed = if is_absolute_path(path) {
                    absolute_path_approved(path, &confirmation.approved_absolute_write_paths)
                } else {
                    path_allowed(path, &confirmation.approved_paths)
                };
                if !allowed {
                    return Err(format!(
                        "follow-up task {} writes outside confirmed scope: {path}",
                        task.id
                    ).into());
                }
                if is_absolute_path(path)
                    && !absolute_path_approved(path, &confirmation.approved_absolute_write_paths)
                {
                    return Err(format!(
                        "follow-up task {} writes outside confirmed absolute scope: {path}",
                        task.id
                    ).into());
                }
            }
            let parent = mission.tasks.iter().find(|parent| parent.spec.id == task.repair_of)
                .ok_or("follow-up must reference a failed repair target")?;
            if parent.status != TaskStatus::Failed {
                return Err("follow-up repair target must be a failed selected task".into());
            }
            if parent.spec.required_checks.iter().any(|check| !task.required_checks.contains(check))
            { return Err("follow-up must retain the failed task's required checks".into()); }
        }
        let now = now_stamp();
        let offset = mission.tasks.len() as i64;
        let budget = mission.budget.as_ref().ok_or("confirmed mission has no budget")?;
        let repair_count = mission.tasks.iter().filter(|task| !task.spec.repair_of.is_empty()).count();
        if repair_count + tasks.len() > budget.max_repair_attempts as usize {
            return Err("follow-up repair budget exceeded".into());
        }
        if let Some(reason) = budget_exhaustion_reason_on(&transaction, &mission)? {
            return Err(reason.into());
        }
        for (index, task) in tasks.iter().enumerate() {
            transaction.execute(
                "INSERT INTO mission_tasks(
                    mission_id, task_id, position, status, spec_json, attempt_run_id,
                    verification_passed, evidence_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, '', NULL, '[]', ?6)",
                params![
                    mission_id,
                    task.id,
                    offset + index as i64,
                    TaskStatus::Blocked.as_str(),
                    serde_json::to_string(task)?,
                    now,
                ],
            )?;
        }
        append_event_tx(
            &transaction,
            mission_id,
            "follow_up_tasks_added",
            "",
            format!("协调 Agent 在已确认范围内新增 {} 项跟进任务", tasks.len()),
            serde_json::json!({
                "task_ids": tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                "reason": reason.trim(),
                "plan_sha256": confirmation.plan_sha256,
            }),
            &now,
        )?;
        recompute_task_readiness_tx(&transaction, mission_id, &now)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn snapshot(&self, mission_id: &str, after_sequence: u64) -> Result<MissionSnapshot, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        let connection = self.connection()?;
        let events = list_events(&connection, mission_id, after_sequence)?;
        let cursor = events.last().map(|event| event.sequence).unwrap_or(after_sequence);
        Ok(MissionSnapshot { mission, events, cursor })
    }

    pub(crate) fn append_event(
        &self,
        mission_id: &str,
        event_type: &str,
        task_id: &str,
        summary: &str,
        payload: serde_json::Value,
    ) -> Result<MissionEvent, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        if mission.status == MissionStatus::Canceled {
            return Err("canceled mission cannot accept new events".into());
        }
        require_text("event_type", event_type, 160)?;
        require_text("summary", summary, MAX_SUMMARY_CHARS)?;
        if !task_id.is_empty() && !mission.tasks.iter().any(|task| task.spec.id == task_id) {
            return Err(format!("event references unknown task: {task_id}").into());
        }
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = append_event_tx(
            &transaction, mission_id, event_type, task_id, summary, payload.clone(), &now,
        )?;
        transaction.commit()?;
        Ok(MissionEvent {
            event_id: format!("{}:{}", mission_id, sequence),
            mission_id: mission_id.to_string(),
            sequence,
            event_type: event_type.to_string(),
            task_id: task_id.to_string(),
            summary: summary.to_string(),
            payload,
            occurred_at: now,
        })
    }

    /// Record one run's usage exactly once and return the accumulated mission
    /// usage. The event id is the LocalRun id, so retrying dispatch after a
    /// restart cannot double-charge a token or cost budget.
    pub(crate) fn record_run_usage(
        &self,
        mission_id: &str,
        task_id: &str,
        run_id: &str,
        usage: &crate::agent_core_contracts::LocalRunUsage,
    ) -> Result<crate::agent_core_contracts::LocalRunUsage, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("run_id", run_id)?;
        if !usage.estimated_cost.is_finite() || usage.estimated_cost < 0.0 {
            return Err("run usage contains an invalid cost".into());
        }
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status == MissionStatus::Canceled {
            return Err("canceled mission cannot accept usage".into());
        }
        let already: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1 AND event_type = 'run_usage_recorded' AND payload_json LIKE ?2",
            params![mission_id, format!("%\"run_id\":\"{}\"%", run_id)],
            |row| row.get(0),
        )?;
        if already == 0 {
            append_event_tx(
                &transaction,
                mission_id,
                "run_usage_recorded",
                task_id,
                "记录执行器用量",
                serde_json::json!({
                    "run_id": run_id,
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "estimated_cost": usage.estimated_cost,
                    "currency": usage.currency,
                    "billing_owner": usage.billing_owner,
                }),
                &now,
            )?;
        }
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT payload_json FROM mission_events
                 WHERE mission_id = ?1 AND event_type = 'run_usage_recorded'",
            )?;
            let rows = statement
                .query_map(params![mission_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut total = crate::agent_core_contracts::LocalRunUsage {
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost: 0.0,
            currency: usage.currency.clone(),
            billing_owner: usage.billing_owner.clone(),
        };
        for raw in rows {
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            total.input_tokens = total.input_tokens.saturating_add(
                value.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            );
            total.output_tokens = total.output_tokens.saturating_add(
                value.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            );
            total.estimated_cost += value
                .get("estimated_cost")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
        }
        transaction.commit()?;
        Ok(total)
    }

    pub(crate) fn proposed_plan(
        &self,
        mission_id: &str,
    ) -> Result<Option<MissionPlanProposal>, Box<dyn Error>> {
        let connection = self.connection()?;
        let raw: String = connection.query_row(
            "SELECT plan_json FROM mission_runs WHERE mission_id = ?1",
            params![mission_id],
            |row| row.get(0),
        )?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        if value.as_object().is_some_and(|object| object.is_empty()) {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(value)?))
    }

    /// Moves one confirmed ready task into execution. The caller supplies the
    /// LocalRun id and must create the run first; this method atomically checks
    /// that the task is still ready and binds it to that run.
    pub(crate) fn begin_task_attempt(
        &self,
        mission_id: &str,
        task_id: &str,
        run_id: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("run_id", run_id)?;
        let mission = self.get(mission_id)?;
        if mission.status != MissionStatus::Active {
            return Err("mission must be active before dispatching a task".into());
        }
        let now = now_stamp();
        let workspace_key = workspace_claim_key(&mission.workspace.root);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission_status: String = transaction.query_row(
            "SELECT status FROM mission_runs WHERE mission_id = ?1",
            params![mission_id],
            |row| row.get(0),
        )?;
        if mission_status != MissionStatus::Active.as_str() {
            return Err("mission must be active before dispatching a task".into());
        }
        let (status, bound_run): (String, String) = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status == TaskStatus::Running.as_str() && bound_run == run_id {
            let claim: Option<(String, String, String)> = transaction.query_row(
                "SELECT mission_id, task_id, run_id FROM mission_workspace_claims WHERE workspace_key = ?1",
                params![workspace_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?;
            if claim.as_ref().is_some_and(|(claimed_mission, claimed_task, claimed_run)| {
                claimed_mission == mission_id && claimed_task == task_id && claimed_run == run_id
            }) {
                transaction.commit()?;
                return self.get(mission_id);
            }
            return Err(format!("task {task_id} is running but its workspace claim is missing or inconsistent").into());
        }
        if status != TaskStatus::Ready.as_str() {
            return Err(format!("task {task_id} is not ready (status={status})").into());
        }
        let current = load_mission(&transaction, mission_id)?.ok_or("mission missing")?;
        if let Some(reason) = budget_exhaustion_reason_on(&transaction, &current)? { return Err(reason.into()); }
        let active_claim: Option<(String, String)> = transaction.query_row(
            "SELECT mission_id, task_id FROM mission_workspace_claims WHERE workspace_key = ?1",
            params![workspace_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        if let Some((claimed_mission, claimed_task)) = active_claim {
            return Err(format!(
                "workspace already has an active mission attempt: {claimed_mission}/{claimed_task}"
            ).into());
        }
        transaction.execute(
            "INSERT INTO mission_workspace_claims(workspace_key, mission_id, task_id, run_id, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workspace_key, mission_id, task_id, run_id, now],
        )?;
        let changed = transaction.execute(
            "UPDATE mission_tasks SET status = ?3, attempt_run_id = ?4, updated_at = ?5
             WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id, TaskStatus::Running.as_str(), run_id, now],
        )?;
        if changed != 1 {
            return Err(format!("task {task_id} changed while being claimed").into());
        }
        transaction.execute(
            "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
            params![mission_id, MissionStatus::Active.as_str(), now],
        )?;
        append_event_tx(
            &transaction, mission_id, "task_dispatched", task_id,
            format!("任务 {task_id} 已派发到执行器"),
            serde_json::json!({"run_id": run_id}), &now,
        )?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn complete_task_attempt(
        &self,
        mission_id: &str,
        task_id: &str,
        run_id: &str,
        passed: bool,
        evidence_refs: &[String],
        reason: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("run_id", run_id)?;
        let mission = self.get(mission_id)?;
        if !matches!(mission.status, MissionStatus::Active | MissionStatus::Waiting) {
            return Err("mission is not active for task completion".into());
        }
        let now = now_stamp();
        let next = if passed { TaskStatus::Completed } else { TaskStatus::Failed };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, bound_run, stored_passed): (String, String, Option<i64>) = transaction.query_row(
            "SELECT status, attempt_run_id, verification_passed FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if matches!(status.as_str(), "completed" | "failed") && bound_run == run_id {
            if status == next.as_str() && stored_passed == Some(if passed { 1 } else { 0 }) {
                transaction.execute(
                    "DELETE FROM mission_workspace_claims WHERE run_id = ?1",
                    params![run_id],
                )?;
                transaction.commit()?;
                return self.get(mission_id);
            }
            return Err(format!("task {task_id} attempt {run_id} was finalized with a conflicting result").into());
        }
        if status != TaskStatus::Running.as_str() || bound_run != run_id {
            return Err(format!("task {task_id} is not running on attempt {run_id}").into());
        }
        if passed {
            return Err("implementation attempts cannot pass without independent host verification".into());
        }
        transaction.execute(
            "UPDATE mission_tasks SET status = ?3, verification_passed = ?4,
                evidence_json = ?5, updated_at = ?6
             WHERE mission_id = ?1 AND task_id = ?2",
            params![
                mission_id, task_id, next.as_str(), if passed { 1 } else { 0 },
                serde_json::to_string(evidence_refs)?, now,
            ],
        )?;
        append_event_tx(
            &transaction, mission_id,
            if passed { "task_completed" } else { "task_failed" },
            task_id,
            if passed { "任务通过验收".to_string() } else { format!("任务未通过验收：{}", reason.trim()) },
            serde_json::json!({"run_id": run_id, "passed": passed, "evidence_refs": evidence_refs, "reason": reason.trim()}),
            &now,
        )?;
        recompute_task_readiness_tx(&transaction, mission_id, &now)?;
        transaction.execute(
            "DELETE FROM mission_workspace_claims WHERE run_id = ?1",
            params![run_id],
        )?;
        let remaining: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM mission_tasks WHERE mission_id = ?1 AND status NOT IN (?2, ?3)",
            params![mission_id, TaskStatus::Completed.as_str(), TaskStatus::Canceled.as_str()],
            |row| row.get(0),
        )?;
        if passed && remaining == 0 {
            transaction.execute(
                "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
                params![mission_id, MissionStatus::Completed.as_str(), now],
            )?;
            append_event_tx(
                &transaction, mission_id, "mission_completed", "",
                "全部任务已通过验收", serde_json::json!({}), &now,
            )?;
        }
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn await_task_verification(
        &self,
        mission_id: &str,
        task_id: &str,
        run_id: &str,
        reason: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("run_id", run_id)?;
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, bound_run): (String, String) = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status == TaskStatus::AwaitingVerification.as_str() && bound_run == run_id {
            transaction.commit()?;
            return self.get(mission_id);
        }
        if status != TaskStatus::Running.as_str() || bound_run != run_id {
            return Err(format!("task {task_id} is not running on attempt {run_id}").into());
        }
        transaction.execute(
            "UPDATE mission_tasks SET status = ?3, updated_at = ?4 WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id, TaskStatus::AwaitingVerification.as_str(), now],
        )?;
        append_event_tx(
            &transaction, mission_id, "task_awaiting_verification", task_id,
            format!("执行完成，等待独立验证：{}", reason.trim()),
            serde_json::json!({"run_id": run_id}), &now,
        )?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn begin_verification_attempt(
        &self,
        mission_id: &str,
        task_id: &str,
        run_id: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("run_id", run_id)?;
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, bound_run, bound_verification_run): (String, String, String) = transaction.query_row(
            "SELECT status, attempt_run_id, verification_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if status == TaskStatus::AwaitingVerification.as_str() && bound_verification_run == run_id {
            transaction.commit()?;
            return self.get(mission_id);
        }
        if status != TaskStatus::AwaitingVerification.as_str() {
            return Err(format!("task {task_id} is not awaiting verification (status={status})").into());
        }
        if bound_run.trim().is_empty() || bound_run == run_id {
            return Err("verification must use a different run from the implementation attempt".into());
        }
        if !bound_verification_run.trim().is_empty() {
            return Err(format!("task {task_id} already has verification run {bound_verification_run}").into());
        }
        transaction.execute(
            "UPDATE mission_tasks SET verification_run_id = ?3, updated_at = ?4
             WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id, run_id, now],
        )?;
        append_event_tx(
            &transaction, mission_id, "verification_started", task_id,
            "独立验证 Agent 已开始检查",
            serde_json::json!({"implementation_run_id": bound_run, "verification_run_id": run_id}), &now,
        )?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn record_verification_result(
        &self,
        mission_id: &str,
        task_id: &str,
        implementation_run_id: &str,
        verification_run_id: &str,
        passed: bool,
        evidence_refs: &[String],
        reason: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_id("implementation_run_id", implementation_run_id)?;
        require_id("verification_run_id", verification_run_id)?;
        if implementation_run_id == verification_run_id {
            return Err("verification evidence cannot come from the implementation run".into());
        }
        if evidence_refs.is_empty() {
            return Err("verification must provide at least one evidence reference".into());
        }
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, bound_run, bound_verification_run, stored_passed, stored_evidence): (String, String, String, Option<i64>, String) = transaction.query_row(
            "SELECT status, attempt_run_id, verification_run_id, verification_passed, evidence_json
             FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        if matches!(status.as_str(), "completed" | "failed")
            && bound_run == implementation_run_id
            && bound_verification_run == verification_run_id
        {
            let expected_evidence = serde_json::to_string(evidence_refs)?;
            if stored_passed == Some(if passed { 1 } else { 0 }) && stored_evidence == expected_evidence {
                transaction.execute(
                    "DELETE FROM mission_workspace_claims WHERE run_id = ?1",
                    params![implementation_run_id],
                )?;
                transaction.commit()?;
                return self.get(mission_id);
            }
            return Err(format!("task {task_id} verification run {verification_run_id} was finalized with conflicting evidence").into());
        }
        if status != TaskStatus::AwaitingVerification.as_str() {
            return Err(format!("task {task_id} is not awaiting verification").into());
        }
        if bound_run != implementation_run_id {
            return Err(format!(
                "verification evidence is bound to implementation run {bound_run}, not {implementation_run_id}"
            ).into());
        }
        if bound_verification_run.trim().is_empty()
            || bound_verification_run != verification_run_id
        {
            return Err(format!(
                "verification run {verification_run_id} was not admitted by the host for task {task_id}"
            ).into());
        }
        if passed {
            let mission = load_mission(&transaction, mission_id)?.ok_or("mission missing")?;
            if mission.status != MissionStatus::Active || mission.confirmation.as_ref()
                .is_none_or(|confirmation| confirmation.plan_sha256 != mission.plan_sha256)
            { return Err("verification has no active confirmed plan".into()); }
            let evidence: Option<String> = transaction.query_row(
                "SELECT payload_json FROM mission_events WHERE mission_id = ?1 AND task_id = ?2
                 AND event_type = 'host_verification_checked' ORDER BY sequence DESC LIMIT 1",
                params![mission_id, task_id], |row| row.get(0),
            ).optional()?;
            let evidence: serde_json::Value = serde_json::from_str(&evidence.ok_or("missing host verification evidence")?)?;
            if evidence["passed"] != true || evidence["plan_sha256"] != mission.plan_sha256
                || evidence["implementation_run_id"] != implementation_run_id
                || evidence["verification_run_id"] != verification_run_id
            { return Err("stale or failed host verification evidence".into()); }
        }
        transaction.execute(
            "UPDATE mission_tasks SET status = ?3, verification_passed = ?4,
                evidence_json = ?5, updated_at = ?6
             WHERE mission_id = ?1 AND task_id = ?2",
            params![
                mission_id, task_id,
                if passed { TaskStatus::Completed.as_str() } else { TaskStatus::Failed.as_str() },
                if passed { 1 } else { 0 }, serde_json::to_string(evidence_refs)?, now,
            ],
        )?;
        transaction.execute(
            "DELETE FROM mission_workspace_claims WHERE run_id = ?1",
            params![implementation_run_id],
        )?;
        append_event_tx(
            &transaction, mission_id,
            if passed { "task_completed" } else { "verification_failed" },
            task_id,
            if passed { "独立验证通过".to_string() } else { format!("独立验证失败：{}", reason.trim()) },
            serde_json::json!({
                "implementation_run_id": implementation_run_id,
                "verification_run_id": verification_run_id,
                "passed": passed,
                "evidence_refs": evidence_refs,
                "reason": reason.trim(),
            }), &now,
        )?;
        if passed {
            let mut repaired_id: String = transaction.query_row(
                "SELECT json_extract(spec_json, '$.repair_of') FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
                params![mission_id, task_id], |row| row.get(0),
            )?;
            while !repaired_id.is_empty() {
                let (parent_status, parent_spec): (String, String) = transaction.query_row(
                    "SELECT status, spec_json FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
                    params![mission_id, repaired_id], |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if parent_status != TaskStatus::Failed.as_str() { break; }
                transaction.execute(
                    "UPDATE mission_tasks SET status = 'completed', verification_passed = 1, evidence_json = ?3, updated_at = ?4
                     WHERE mission_id = ?1 AND task_id = ?2",
                    params![mission_id, repaired_id, serde_json::to_string(evidence_refs)?, now],
                )?;
                append_event_tx(&transaction, mission_id, "task_repaired", &repaired_id,
                    "修复任务已通过独立验收", serde_json::json!({"repair_task_id": task_id, "verification_run_id": verification_run_id}), &now)?;
                repaired_id = serde_json::from_str::<MissionTaskSpec>(&parent_spec)?.repair_of;
            }
        }
        recompute_task_readiness_tx(&transaction, mission_id, &now)?;
        let remaining: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM mission_tasks WHERE mission_id = ?1 AND status NOT IN (?2, ?3)",
            params![mission_id, TaskStatus::Completed.as_str(), TaskStatus::Canceled.as_str()],
            |row| row.get(0),
        )?;
        if passed && remaining == 0 {
            transaction.execute(
                "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
                params![mission_id, MissionStatus::Completed.as_str(), now],
            )?;
            append_event_tx(
                &transaction, mission_id, "mission_completed", "",
                "全部任务已通过独立验收", serde_json::json!({}), &now,
            )?;
        }
        transaction.commit()?;
        self.get(mission_id)
    }

    fn get_with_plan(&self, mission_id: &str) -> Result<(MissionView, MissionPlanProposal), Box<dyn Error>> {
        let connection = self.connection()?;
        let mission = load_mission(&connection, mission_id)?
            .ok_or_else(|| format!("mission not found: {mission_id}"))?;
        let plan_json: String = connection.query_row(
            "SELECT plan_json FROM mission_runs WHERE mission_id = ?1",
            params![mission_id],
            |row| row.get(0),
        )?;
        let proposal = serde_json::from_str(&plan_json)?;
        Ok((mission, proposal))
    }

    fn connection(&self) -> Result<Connection, Box<dyn Error>> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize_schema(&connection)?;
        Ok(connection)
    }

    pub(crate) fn budget_exhaustion_reason(&self, mission: &MissionView) -> Result<Option<String>, Box<dyn Error>> {
        budget_exhaustion_reason_on(&self.connection()?, mission)
    }

    fn record_budget_exhausted(&self, mission_id: &str, reason: &str) -> Result<(), Box<dyn Error>> {
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1 AND event_type = 'budget_exhausted'",
            params![mission_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
            params![mission_id, MissionStatus::Waiting.as_str(), now],
        )?;
        if already == 0 {
            append_event_tx(&transaction, mission_id, "budget_exhausted", "", reason,
                serde_json::json!({"reason": reason}), &now)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn next_ready_task(&self, mission_id: &str) -> Result<Option<MissionTaskSpec>, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        if mission.status != MissionStatus::Active {
            return Ok(None);
        }
        if let Some(reason) = self.budget_exhaustion_reason(&mission)? {
            self.record_budget_exhausted(mission_id, &reason)?;
            return Ok(None);
        }
        Ok(mission.tasks.into_iter()
            .find(|task| task.status == TaskStatus::Ready)
            .map(|task| task.spec))
    }

    pub(crate) fn running_task(&self, mission_id: &str) -> Result<Option<MissionTaskView>, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        if mission.status != MissionStatus::Active {
            return Ok(None);
        }
        Ok(mission.tasks.into_iter().find(|task| task.status == TaskStatus::Running))
    }

    pub(crate) fn next_verification_task(&self, mission_id: &str) -> Result<Option<MissionTaskView>, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        if mission.status != MissionStatus::Active {
            return Ok(None);
        }
        Ok(mission.tasks.into_iter()
            .find(|task| task.status == TaskStatus::AwaitingVerification))
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), Box<dyn Error>> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS mission_runs(
            mission_id TEXT PRIMARY KEY, title TEXT NOT NULL, objective TEXT NOT NULL,
            status TEXT NOT NULL, workspace_json TEXT NOT NULL, acceptance_json TEXT NOT NULL,
            plan_revision INTEGER NOT NULL DEFAULT 0, plan_sha256 TEXT NOT NULL DEFAULT '',
            plan_json TEXT NOT NULL DEFAULT '{}', confirmation_json TEXT, budget_json TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            handoff_json TEXT, workflow_json TEXT, assignment_json TEXT,
            plan_mode TEXT NOT NULL DEFAULT 'generated', idempotency_key TEXT
         );
         CREATE TABLE IF NOT EXISTS mission_tasks(
            mission_id TEXT NOT NULL REFERENCES mission_runs(mission_id) ON DELETE CASCADE,
            task_id TEXT NOT NULL, position INTEGER NOT NULL, status TEXT NOT NULL,
            spec_json TEXT NOT NULL, attempt_run_id TEXT NOT NULL DEFAULT '',
            verification_run_id TEXT NOT NULL DEFAULT '',
            verification_passed INTEGER, evidence_json TEXT NOT NULL DEFAULT '[]',
            updated_at TEXT NOT NULL, PRIMARY KEY(mission_id, task_id)
         );
         CREATE TABLE IF NOT EXISTS mission_workspace_claims(
            workspace_key TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES mission_runs(mission_id) ON DELETE CASCADE,
            task_id TEXT NOT NULL,
            run_id TEXT NOT NULL UNIQUE,
            acquired_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_mission_tasks_status
            ON mission_tasks(mission_id, status);
         CREATE TABLE IF NOT EXISTS mission_events(
            mission_id TEXT NOT NULL REFERENCES mission_runs(mission_id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL, event_id TEXT NOT NULL, event_type TEXT NOT NULL,
            task_id TEXT NOT NULL DEFAULT '', summary TEXT NOT NULL,
            payload_json TEXT NOT NULL, occurred_at TEXT NOT NULL,
            PRIMARY KEY(mission_id, sequence)
         );
         CREATE TABLE IF NOT EXISTS mission_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT OR IGNORE INTO mission_meta(key, value) VALUES ('event_sequence', '0');",
    )?;
    let has_verification_run = {
        let mut statement = connection.prepare("PRAGMA table_info(mission_tasks)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        columns.iter().any(|column| column == "verification_run_id")
    };
    if !has_verification_run {
        connection.execute(
            "ALTER TABLE mission_tasks
             ADD COLUMN verification_run_id TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    let mut columns = {
        let mut statement = connection.prepare("PRAGMA table_info(mission_runs)")?;
        let mut rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        let values = rows.by_ref().collect::<Result<Vec<_>, _>>()?;
        values
    };
    for (name, declaration) in [
        ("handoff_json", "TEXT"),
        ("workflow_json", "TEXT"),
        ("assignment_json", "TEXT"),
        ("plan_mode", "TEXT NOT NULL DEFAULT 'generated'"),
        ("idempotency_key", "TEXT"),
    ] {
        if !columns.iter().any(|column| column == name) {
            connection.execute(&format!("ALTER TABLE mission_runs ADD COLUMN {name} {declaration}"), [])?;
            columns.push(name.to_string());
        }
    }
    // The idempotency index depends on a column introduced by the migration
    // above. Creating it in the initial batch fails for pre-idempotency DBs.
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mission_runs_idempotency_key
         ON mission_runs(idempotency_key) WHERE idempotency_key IS NOT NULL",
        [],
    )?;
    let legacy_claims = {
        let mut statement = connection.prepare(
            "SELECT runs.mission_id, tasks.task_id, tasks.attempt_run_id, tasks.updated_at, runs.workspace_json
             FROM mission_tasks AS tasks
             JOIN mission_runs AS runs ON runs.mission_id = tasks.mission_id
             WHERE tasks.status IN ('running', 'awaiting_verification')
               AND tasks.attempt_run_id <> ''",
        )?;
        let rows = statement.query_map([], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?,
        )))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (mission_id, task_id, run_id, acquired_at, workspace_json) in legacy_claims {
        let workspace: crate::mission::MissionWorkspace = serde_json::from_str(&workspace_json)?;
        connection.execute(
            "INSERT OR IGNORE INTO mission_workspace_claims(workspace_key, mission_id, task_id, run_id, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workspace_claim_key(&workspace.root), mission_id, task_id, run_id, acquired_at],
        )?;
    }
    Ok(())
}

fn budget_exhaustion_reason_on(connection: &Connection, mission: &MissionView) -> Result<Option<String>, Box<dyn Error>> {
        let Some(budget) = mission.budget.as_ref() else { return Ok(None); };
        let Some(confirmation) = mission.confirmation.as_ref() else { return Ok(None); };
        let rounds: i64 = connection.query_row(
            "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1 AND event_type = 'task_dispatched'",
            params![mission.mission_id],
            |row| row.get(0),
        )?;
        let confirmed_at = chrono::DateTime::parse_from_rfc3339(&confirmation.confirmed_at)
            .map_err(|error| format!("invalid confirmation timestamp: {error}"))?;
        let elapsed_minutes = Utc::now()
            .signed_duration_since(confirmed_at.with_timezone(&Utc))
            .num_minutes()
            .max(0) as u64;
        if rounds >= budget.max_rounds as i64 {
            return Ok(Some(format!("达到轮次上限：已派发 {rounds}/{} 轮", budget.max_rounds)));
        }
        if elapsed_minutes >= budget.max_minutes as u64 {
            return Ok(Some(format!("达到时限上限：已运行 {elapsed_minutes}/{} 分钟", budget.max_minutes)));
        }
        let usage_json: Vec<String> = {
            let mut statement = connection.prepare(
                "SELECT payload_json FROM mission_events WHERE mission_id = ?1 AND event_type = 'run_usage_recorded'",
            )?;
            let rows = statement
                .query_map(params![mission.mission_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut estimated_cost = 0.0f64;
        let mut cost_unreported = false;
        let usage_json_count = usage_json.len();
        for raw in usage_json {
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            input_tokens = input_tokens.saturating_add(
                value.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            );
            output_tokens = output_tokens.saturating_add(
                value.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            );
            let cost = value.get("estimated_cost").and_then(serde_json::Value::as_f64);
            let currency = value.get("currency").and_then(serde_json::Value::as_str).unwrap_or_default().trim();
            // A numeric zero alone is not proof that the Runtime reported cost:
            // older/error paths serialize a default zero. A currency is the
            // explicit marker that this is a billable, measured amount.
            if cost.is_none()
                || cost.is_some_and(|cost| !cost.is_finite() || cost < 0.0)
                || currency.is_empty()
            {
                cost_unreported = true;
            } else {
                estimated_cost += cost.unwrap_or(0.0);
            }
        }
        if let Some(limit) = budget.max_total_tokens {
            let total_tokens = input_tokens.saturating_add(output_tokens);
            if total_tokens >= limit {
                return Ok(Some(format!("达到 Token 上限：已使用 {total_tokens}/{limit}")));
            }
        }
        if let Some(limit) = budget.max_cost {
            if usage_json_count == 0 {
                return Ok(Some("声明了成本上限，但尚无执行器上报可信成本，已停止继续派发".to_string()));
            }
            if cost_unreported {
                return Ok(Some("声明了成本上限，但执行器未上报可信成本，已停止继续派发".to_string()));
            }
            if !estimated_cost.is_finite() || estimated_cost >= limit {
                return Ok(Some(format!("达到成本上限：已使用 {estimated_cost:.6}/{limit:.6}")));
            }
        }
        Ok(None)
 }

fn workspace_claim_key(workspace_root: &str) -> String {
    let path = PathBuf::from(workspace_root.trim());
    let path = fs::canonicalize(&path).unwrap_or(path);
    let normalized = path.to_string_lossy().replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_string()
    }
}

fn load_mission(connection: &Connection, mission_id: &str) -> Result<Option<MissionView>, Box<dyn Error>> {
    let row = connection.query_row(
        "SELECT mission_id, title, objective, status, workspace_json, acceptance_json,
                plan_revision, plan_sha256, plan_json, confirmation_json, budget_json,
                created_at, updated_at, handoff_json, workflow_json, assignment_json, plan_mode
         FROM mission_runs WHERE mission_id = ?1",
        params![mission_id],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?, row.get::<_, Option<String>>(10)?,
            row.get::<_, String>(11)?, row.get::<_, String>(12)?,
            row.get::<_, Option<String>>(13)?, row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<String>>(15)?, row.get::<_, String>(16)?,
        )),
    ).optional()?;
    let Some((id, title, objective, status, workspace, acceptance, revision, hash, plan_json, confirmation, budget, created, updated, handoff, workflow, assignment, plan_mode)) = row else { return Ok(None); };
    let proposal = serde_json::from_str::<MissionPlanProposal>(&plan_json).ok();
    let mut statement = connection.prepare(
        "SELECT status, spec_json, attempt_run_id, verification_run_id, verification_passed, evidence_json
         FROM mission_tasks WHERE mission_id = ?1 ORDER BY position ASC",
    )?;
    let tasks = statement.query_map(params![id], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
        row.get::<_, String>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, String>(5)?,
    )))?
    .map(|row| {
        let (status, spec, run_id, verification_run_id, verified, evidence) = row?;
        Ok(MissionTaskView {
            spec: serde_json::from_str(&spec)?, status: TaskStatus::parse(&status)?,
            attempt_run_id: run_id, verification_run_id, verification_passed: verified.map(|value| value != 0),
            evidence_refs: serde_json::from_str(&evidence)?,
        })
    }).collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let usage = usage_from_events(connection, &id)?;
    Ok(Some(MissionView {
        schema_version: "mission.v1".to_string(), mission_id: id, title, objective,
        status: MissionStatus::parse(&status)?, workspace: serde_json::from_str(&workspace)?,
        acceptance_criteria: serde_json::from_str(&acceptance)?,
        plan_revision: revision.max(0) as u64, plan_sha256: hash,
        plan_summary: proposal.as_ref().map(|value| value.summary.clone()).unwrap_or_default(),
        required_acceptance: proposal.as_ref().map(|value| value.required_acceptance.clone()).unwrap_or_default(),
        proposed_tasks: proposal.as_ref().map(|value| value.tasks.clone()).unwrap_or_default(),
        budget: budget.map(|value| serde_json::from_str(&value)).transpose()?,
        usage,
        handoff: handoff.map(|value| serde_json::from_str(&value)).transpose()?,
        workflow: workflow.map(|value| serde_json::from_str(&value)).transpose()?,
        assignment: assignment.map(|value| serde_json::from_str(&value)).transpose()?,
        plan_mode: MissionPlanMode::parse(&plan_mode)?,
        confirmation: confirmation.map(|value| serde_json::from_str(&value)).transpose()?,
        tasks, created_at: created, updated_at: updated,
    }))
}

fn recompute_task_readiness_tx(transaction: &rusqlite::Transaction<'_>, mission_id: &str, now: &str) -> Result<(), Box<dyn Error>> {
    let mut statement = transaction.prepare(
        "SELECT task_id, status, spec_json FROM mission_tasks WHERE mission_id = ?1 ORDER BY position ASC",
    )?;
    let rows = statement.query_map(params![mission_id], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
    )))?.collect::<Result<Vec<_>, _>>()?;
    let mut statuses = BTreeMap::new();
    let mut specs = BTreeMap::new();
    for (id, status, spec) in rows {
        statuses.insert(id.clone(), status);
        specs.insert(id, serde_json::from_str::<MissionTaskSpec>(&spec)?);
    }
    for (id, spec) in specs {
        // Failed attempts are terminal until an explicit retry/follow-up creates
        // a new attempt. Recomputing readiness must never resurrect a failed
        // task to `ready`, or the dispatcher would silently loop forever.
        if matches!(statuses.get(&id).map(String::as_str), Some("canceled" | "completed" | "running" | "awaiting_verification" | "failed")) { continue; }
        let ready = spec.depends_on.iter().all(|dependency| statuses.get(dependency).map(String::as_str) == Some("completed"));
        let next = if ready { "ready" } else { "blocked" };
        if statuses.get(&id).map(String::as_str) != Some(next) {
            transaction.execute(
                "UPDATE mission_tasks SET status = ?3, updated_at = ?4 WHERE mission_id = ?1 AND task_id = ?2",
                params![mission_id, id, next, now],
            )?;
        }
    }
    transaction.execute(
        "UPDATE mission_runs SET updated_at = ?2 WHERE mission_id = ?1",
        params![mission_id, now],
    )?;
    Ok(())
}

fn append_event_tx(transaction: &rusqlite::Transaction<'_>, mission_id: &str, event_type: &str, task_id: &str, summary: impl Into<String>, payload: serde_json::Value, occurred_at: &str) -> Result<u64, Box<dyn Error>> {
    let current: String = transaction.query_row("SELECT value FROM mission_meta WHERE key = 'event_sequence'", [], |row| row.get(0))?;
    let sequence = current.parse::<u64>()?.saturating_add(1);
    transaction.execute("UPDATE mission_meta SET value = ?1 WHERE key = 'event_sequence'", params![sequence.to_string()])?;
    transaction.execute(
        "INSERT INTO mission_events(mission_id, sequence, event_id, event_type, task_id, summary, payload_json, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![mission_id, sequence as i64, format!("{mission_id}:{sequence}"), event_type, task_id, summary.into(), serde_json::to_string(&payload)?, occurred_at],
    )?;
    Ok(sequence)
}

fn list_events(connection: &Connection, mission_id: &str, after_sequence: u64) -> Result<Vec<MissionEvent>, Box<dyn Error>> {
    let mut statement = connection.prepare(
        "SELECT event_id, sequence, event_type, task_id, summary, payload_json, occurred_at
         FROM mission_events WHERE mission_id = ?1 AND sequence > ?2 ORDER BY sequence ASC LIMIT ?3",
    )?;
    let rows = statement.query_map(params![mission_id, after_sequence as i64, MAX_EVENTS as i64], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?,
        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
        row.get::<_, String>(6)?,
    )))?.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(|row| {
        let (event_id, sequence, event_type, task_id, summary, payload, occurred_at) = row;
        Ok(MissionEvent { event_id, mission_id: mission_id.to_string(), sequence: sequence.max(0) as u64,
            event_type, task_id, summary, payload: serde_json::from_str(&payload)?, occurred_at })
    }).collect()
}

fn usage_from_events(
    connection: &Connection,
    mission_id: &str,
) -> Result<Option<crate::agent_core_contracts::LocalRunUsage>, Box<dyn Error>> {
    let mut statement = connection.prepare(
        "SELECT payload_json FROM mission_events
         WHERE mission_id = ?1 AND event_type = 'run_usage_recorded'
         ORDER BY sequence ASC",
    )?;
    let rows = statement
        .query_map(params![mission_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut usage = crate::agent_core_contracts::LocalRunUsage {
        input_tokens: 0,
        output_tokens: 0,
        estimated_cost: 0.0,
        currency: String::new(),
        billing_owner: String::new(),
    };
    for raw in rows {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        usage.input_tokens = usage.input_tokens.saturating_add(
            value.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
        );
        usage.output_tokens = usage.output_tokens.saturating_add(
            value.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
        );
        usage.estimated_cost += value
            .get("estimated_cost")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        if usage.currency.is_empty() {
            usage.currency = value.get("currency").and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        }
        if usage.billing_owner.is_empty() {
            usage.billing_owner = value.get("billing_owner").and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        }
    }
    Ok(Some(usage))
}

fn new_mission_id() -> String { format!("mission_{}_{:08x}", Utc::now().timestamp_millis(), rand::random::<u32>()) }
fn now_stamp() -> String { Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::contract::{ExecutionBinding, MissionBudget, MissionWorkspace};

    fn store(name: &str) -> MissionStore {
        let root = std::env::temp_dir().join(format!(
            "himind-mission-{name}-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        MissionStore::new(root.join(MISSION_DB_FILE))
    }

    fn handoff_package(store: &crate::workflow::WorkflowStore) -> (String, String, String) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("ui-craft-team");
        let installed = store.install_from_directory(&source).unwrap();
        let digest = crate::workflow::package_digest(&installed.package.source_root).unwrap();
        (installed.package.id, installed.package.version, digest)
    }

    fn imported_handoff(workflow: &crate::mission::MissionWorkflowPin) -> crate::mission::MissionHandoffInput {
        crate::mission::MissionHandoffInput {
            idempotency_key: "handoff-test-key".to_string(),
            source_client: "codex".to_string(),
            source_reference: "thread:test".to_string(),
            source_message_refs: vec!["message:1".to_string()],
            context_summary: "Reviewed plan".to_string(),
            title: "Handoff mission".to_string(),
            objective: "Implement reviewed plan".to_string(),
            workspace: MissionWorkspace {
                root: "F:/repo".to_string(),
                read_scope: vec!["frontend".to_string()],
                write_scope: vec!["frontend/src".to_string()],
            },
            acceptance_criteria: vec!["tests pass".to_string()],
            workflow: workflow.clone(),
            plan_mode: crate::mission::MissionPlanMode::Imported,
            imported_plan: Some(proposal()),
        }
    }

    #[test]
    fn legacy_database_without_idempotency_column_reopens_and_migrates() {
        let root = std::env::temp_dir().join(format!(
            "himind-mission-legacy-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = root.join(MISSION_DB_FILE);
        {
            let connection = Connection::open(&database).unwrap();
            connection.execute_batch(
                "CREATE TABLE mission_runs(
                    mission_id TEXT PRIMARY KEY, title TEXT NOT NULL, objective TEXT NOT NULL,
                    status TEXT NOT NULL, workspace_json TEXT NOT NULL, acceptance_json TEXT NOT NULL,
                    plan_revision INTEGER NOT NULL DEFAULT 0, plan_sha256 TEXT NOT NULL DEFAULT '',
                    plan_json TEXT NOT NULL DEFAULT '{}', confirmation_json TEXT, budget_json TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                 );
                 INSERT INTO mission_runs(mission_id,title,objective,status,workspace_json,acceptance_json,created_at,updated_at)
                 VALUES('legacy','Legacy','', 'draft', '{\"root\":\"F:/repo\",\"read_scope\":[],\"write_scope\":[]}', '[]', '2026-11-10T00:00:00Z', '2026-11-10T00:00:00Z');",
            ).unwrap();
        }
        let store = MissionStore::new(database);
        let mission = store.get("legacy").unwrap();
        assert_eq!(mission.mission_id, "legacy");
        let connection = store.connection().unwrap();
        let index_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_mission_runs_idempotency_key'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(index_count, 1);
        drop(connection);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handoff_is_idempotent_and_requires_the_exact_workflow_pin() {
        let store = store("handoff");
        let workflow_store = crate::workflow::WorkflowStore::new(store.path.parent().unwrap().join("workflows"));
        let (id, version, digest) = handoff_package(&workflow_store);
        let pin = crate::mission::MissionWorkflowPin {
            workflow_id: id, workflow_version: version, workflow_sha256: digest, entrypoint: String::new(),
        };
        let input = imported_handoff(&pin);
        let first = store.create_handoff_with_store(&input, &workflow_store).unwrap();
        assert_eq!(first.status, MissionStatus::AwaitingAssignment);
        assert_eq!(first.handoff.as_ref().unwrap().source_client, "codex");
        let repeated = store.create_handoff_with_store(&input, &workflow_store).unwrap();
        assert_eq!(first.mission_id, repeated.mission_id);
        let mut changed = input.clone();
        changed.objective = "different request".to_string();
        assert!(store.create_handoff_with_store(&changed, &workflow_store).unwrap_err().to_string().contains("different handoff"));
        let mut wrong_pin = input.clone();
        wrong_pin.idempotency_key = "handoff-wrong-pin".to_string();
        wrong_pin.workflow.workflow_sha256 = "0".repeat(64);
        assert!(store.create_handoff_with_store(&wrong_pin, &workflow_store).unwrap_err().to_string().contains("differs from the Mission pin"));
    }

    #[test]
    fn assigned_roles_are_required_before_planning_and_override_imported_bindings() {
        let store = store("assignment");
        let workflow_store = crate::workflow::WorkflowStore::new(store.path.parent().unwrap().join("workflows"));
        let (id, version, digest) = handoff_package(&workflow_store);
        let pin = crate::mission::MissionWorkflowPin {
            workflow_id: id, workflow_version: version, workflow_sha256: digest, entrypoint: String::new(),
        };
        let input = imported_handoff(&pin);
        let mission = store.create_handoff_with_store(&input, &workflow_store).unwrap();
        let assignment = crate::mission::MissionAssignmentInput {
            roles: vec![crate::mission::MissionRoleAssignment {
                role_id: "ui.implementer.shared".to_string(),
                label: "Implementer".to_string(),
                executor: "himind.builtin".to_string(),
                service_id: "service".to_string(),
                model: "himind:service:model".to_string(),
            }],
            task_roles: vec![
                crate::mission::MissionTaskRoleMapping {
                    task_id: "fix".to_string(), role_id: "ui.implementer.shared".to_string(),
                },
                crate::mission::MissionTaskRoleMapping {
                    task_id: "verify".to_string(), role_id: "ui.implementer.shared".to_string(),
                },
            ],
            applied_by: "local-user".to_string(),
            recommended: true,
        };
        assert!(store.propose_plan(&mission.mission_id, &proposal()).is_err());
        let assigned = store.assign_roles_with_store(&mission.mission_id, &assignment, &workflow_store).unwrap();
        assert_eq!(assigned.status, MissionStatus::AwaitingConfirmation);
        assert_eq!(assigned.assignment.as_ref().unwrap().roles[0].role_id, "ui.implementer.shared");
        assert_eq!(assigned.proposed_tasks[0].execution.service_id, "service");
    }

    fn create_input() -> MissionCreateInput {
        MissionCreateInput {
            title: "修复登录页焦点".to_string(),
            objective: "完成可验证的 UI 修复".to_string(),
            workspace: MissionWorkspace {
                root: "F:/repo".to_string(),
                read_scope: vec!["frontend".to_string()],
                write_scope: vec!["frontend".to_string()],
            },
            acceptance_criteria: vec!["npm test 通过".to_string()],
        }
    }

    fn task(id: &str) -> MissionTaskSpec {
        MissionTaskSpec {
            id: id.to_string(), title: format!("任务 {id}"), objective: String::new(),
            depends_on: Vec::new(), read_paths: vec!["frontend".to_string()],
            write_paths: vec!["frontend/src".to_string()],
            acceptance_criteria: vec!["检查通过".to_string()],
            required_checks: vec!["npm test".to_string()],
            execution: ExecutionBinding {
                executor: "himind.builtin".to_string(), model: String::new(),
                role_id: "ui.implementer".to_string(), service_id: String::new(),
            },
            repair_of: String::new(),
        }
    }

    fn proposal() -> MissionPlanProposal {
        let mut verification = task("verify");
        verification.depends_on = vec!["fix".to_string()];
        MissionPlanProposal {
            summary: "先修复再验证".to_string(),
            tasks: vec![task("fix"), verification],
            required_acceptance: vec!["全部检查通过".to_string()],
            budget: MissionBudget {
                max_rounds: 8,
                max_minutes: 120,
                max_repair_attempts: 2,
                max_total_tokens: None,
                max_cost: None,
            },
        }
    }

    #[test]
    fn unconfirmed_mission_never_exposes_ready_tasks() {
        let store = store("unconfirmed");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        assert_eq!(planned.status, MissionStatus::AwaitingConfirmation);
        assert!(planned.tasks.is_empty());
        assert_eq!(planned.proposed_tasks.len(), 2);
        assert!(planned.confirmation.is_none());
    }

    #[test]
    fn confirmation_binds_exact_plan_hash() {
        let store = store("hash");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        assert!(store.confirm_plan(&created.mission_id, &"0".repeat(64),
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user")
            .unwrap_err().to_string().contains("changed"));
        let confirmed = store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user").unwrap();
        assert_eq!(confirmed.status, MissionStatus::Active);
        assert_eq!(confirmed.confirmation.unwrap().plan_sha256, planned.plan_sha256);
        assert_eq!(confirmed.tasks[0].status, TaskStatus::Ready);
        assert_eq!(confirmed.tasks[1].status, TaskStatus::Blocked);
    }

    #[test]
    fn dependency_cycle_is_rejected() {
        let store = store("cycle");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].depends_on = vec!["verify".to_string()];
        plan.tasks[1].depends_on = vec!["fix".to_string()];
        assert!(store.propose_plan(&created.mission_id, &plan).unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn absolute_task_paths_wait_for_explicit_approval() {
        let store = store("absolute-paths");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].read_paths = vec!["F:/approved-external/absolute-input.txt".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        let error = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user",
        ).unwrap_err();
        assert!(error.to_string().contains("reads outside approved paths"));
        let confirmed = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &["F:/approved-external/absolute-input.txt".to_string()], &[], "test-user",
        ).unwrap();
        assert_eq!(confirmed.confirmation.unwrap().approved_absolute_read_paths, vec!["F:/approved-external/absolute-input.txt"]);
    }

    #[test]
    fn parent_directory_segments_are_rejected() {
        let store = store("parent-paths");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].write_paths = vec!["frontend/../../outside".to_string()];
        let error = store.propose_plan(&created.mission_id, &plan).unwrap_err();
        assert!(error.to_string().contains("path escape"));
    }

    #[test]
    fn absolute_write_requires_separate_explicit_approval() {
        let store = store("absolute-write");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].write_paths = vec!["F:/repo/external/config.json".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        let selected = ["fix".to_string(), "verify".to_string()];
        let denied = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256, &selected,
            &["frontend/src".to_string()], &[], &[], "test-user",
        ).unwrap_err();
        assert!(denied.to_string().contains("writes outside approved paths"));
        let confirmed = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256, &selected,
            &["frontend/src".to_string()], &[], &["F:/repo/external/config.json".to_string()], "test-user",
        ).unwrap();
        assert_eq!(
            confirmed.confirmation.unwrap().approved_absolute_write_paths,
            vec!["F:/repo/external/config.json"]
        );
    }

    #[test]
    fn absolute_approval_must_match_a_selected_plan_path() {
        let store = store("absolute-unplanned-approval");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].read_paths = vec!["F:/repo/external/input.json".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        let error = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()],
            &["F:/repo/other/secret.txt".to_string()], &[], "test-user",
        ).unwrap_err();
        assert!(error.to_string().contains("absent from selected tasks"));
    }

    #[test]
    fn device_and_parent_absolute_paths_are_rejected() {
        let store = store("dangerous-absolute-paths");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].read_paths = vec!["C:/safe/../outside".to_string()];
        assert!(store.propose_plan(&created.mission_id, &plan).unwrap_err().to_string().contains("path escape"));
        plan.tasks[0].read_paths = vec![r"\\.\C:\device\path".to_string()];
        assert!(store.propose_plan(&created.mission_id, &plan).unwrap_err().to_string().contains("dangerous path"));
    }

    #[test]
    fn selected_task_requires_selected_dependency() {
        let store = store("deps");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[1].depends_on = vec!["fix".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        assert!(store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user")
            .unwrap_err().to_string().contains("unselected dependency"));
    }

    #[test]
    fn follow_up_tasks_cannot_write_outside_confirmed_scope() {
        let store = store("scope");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user").unwrap();
        let mut follow_up = task("repair");
        follow_up.write_paths = vec!["backend".to_string()];
        assert!(store.add_follow_up_tasks(&created.mission_id, &[follow_up], "修复失败")
            .unwrap_err().to_string().contains("outside confirmed scope"));
    }

    #[test]
    fn follow_up_tasks_cannot_add_unapproved_absolute_paths() {
        let store = store("followup-absolute-scope");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].read_paths = vec!["F:/repo/external/input.json".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()],
            &["frontend/src".to_string()],
            &["F:/repo/external/input.json".to_string()],
            &[],
            "test-user",
        ).unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-failed").unwrap();
        store.complete_task_attempt(&created.mission_id, "fix", "run-failed", false, &[], "failed").unwrap();
        let mut follow_up = task("repair-absolute");
        follow_up.read_paths = vec!["F:/repo/external/other.json".to_string()];
        assert!(store.add_follow_up_tasks(&created.mission_id, &[follow_up], "修复失败")
            .unwrap_err().to_string().contains("outside confirmed absolute scope"));
    }

    #[test]
    fn event_cursor_returns_only_new_events() {
        let store = store("events");
        let created = store.create(&create_input()).unwrap();
        let first = store.snapshot(&created.mission_id, 0).unwrap();
        assert_eq!(first.events.len(), 1);
        store.append_event(&created.mission_id, "task_progress", "", "协调 Agent 正在检查依赖",
            serde_json::json!({"phase": "planning"})).unwrap();
        let incremental = store.snapshot(&created.mission_id, first.cursor).unwrap();
        assert_eq!(incremental.events.len(), 1);
        assert_eq!(incremental.events[0].event_type, "task_progress");
        assert!(incremental.cursor > first.cursor);
    }

    #[test]
    fn plan_change_invalidates_previous_confirmation() {
        let store = store("invalidate");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user").unwrap();
        let mut changed = proposal();
        changed.summary = "改变验收范围".to_string();
        let revised = store.propose_plan(&created.mission_id, &changed).unwrap();
        assert_eq!(revised.status, MissionStatus::AwaitingConfirmation);
        assert!(revised.confirmation.is_none());
        assert_ne!(revised.plan_sha256, planned.plan_sha256);
    }

    fn confirmed_single_task(store: &MissionStore, name: &str) -> MissionView {
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string()], &["frontend/src".to_string()], &[], &[], "test-user").unwrap();
        let _ = name;
        created
    }

    #[test]
    fn workspace_claim_is_idempotent_persistent_and_exclusive() {
        let root = std::env::temp_dir().join(format!(
            "himind-mission-reopen-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = root.join(MISSION_DB_FILE);
        let store = MissionStore::new(database.clone());
        let first = store.create(&create_input()).unwrap();
        let second = store.create(&create_input()).unwrap();
        for mission in [&first, &second] {
            let planned = store.propose_plan(&mission.mission_id, &proposal()).unwrap();
            store.confirm_plan(
                &mission.mission_id, &planned.plan_sha256,
                &["fix".to_string()], &["frontend/src".to_string()], &[], &[], "test-user",
            ).unwrap();
        }
        let claimed = store.begin_task_attempt(&first.mission_id, "fix", "run-first").unwrap();
        assert_eq!(claimed.tasks[0].status, TaskStatus::Running);
        let duplicate = store.begin_task_attempt(&first.mission_id, "fix", "run-first").unwrap();
        assert_eq!(duplicate.tasks[0].status, TaskStatus::Running);
        let blocked = store.begin_task_attempt(&second.mission_id, "fix", "run-second").unwrap_err();
        assert!(blocked.to_string().contains("workspace already has an active mission attempt"));
        let connection = store.connection().unwrap();
        connection.execute("DELETE FROM mission_workspace_claims WHERE run_id = 'run-first'", []).unwrap();
        drop(connection);
        drop(store);

        let reopened = MissionStore::new(database);
        let persisted = reopened.begin_task_attempt(&first.mission_id, "fix", "run-first").unwrap();
        assert_eq!(persisted.tasks[0].status, TaskStatus::Running);
        let dispatched_events = reopened.snapshot(&first.mission_id, 0).unwrap().events
            .into_iter().filter(|event| event.event_type == "task_dispatched").count();
        assert_eq!(dispatched_events, 1);
        let failed = reopened.complete_task_attempt(
            &first.mission_id, "fix", "run-first", false, &[], "runtime stopped",
        ).unwrap();
        assert_eq!(failed.tasks[0].status, TaskStatus::Failed);
        let repeated = reopened.complete_task_attempt(
            &first.mission_id, "fix", "run-first", false, &[], "runtime stopped",
        ).unwrap();
        assert_eq!(repeated.tasks[0].status, TaskStatus::Failed);
        assert!(reopened.complete_task_attempt(
            &first.mission_id, "fix", "run-first", true, &["conflicting".to_string()], "",
        ).unwrap_err().to_string().contains("conflicting result"));
        let failed_events = reopened.snapshot(&first.mission_id, 0).unwrap().events
            .into_iter().filter(|event| event.event_type == "task_failed").count();
        assert_eq!(failed_events, 1);
        let second_claim = reopened.begin_task_attempt(&second.mission_id, "fix", "run-second").unwrap();
        assert_eq!(second_claim.tasks[0].status, TaskStatus::Running);
    }

    #[test]
    fn execution_binding_change_invalidates_plan_hash() {
        let store = store("execution-binding");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        let execution = crate::mission::ExecutionBinding {
            executor: "himind.builtin".to_string(),
            model: "himind:service:model".to_string(),
            role_id: "ui.reviewer".to_string(),
            service_id: "service".to_string(),
        };
        let updated = store.update_task_execution(
            &created.mission_id,
            "fix",
            &planned.plan_sha256,
            &execution,
        ).unwrap();
        assert_eq!(updated.status, MissionStatus::AwaitingConfirmation);
        assert!(updated.confirmation.is_none());
        assert_ne!(updated.plan_sha256, planned.plan_sha256);
        assert_eq!(updated.proposed_tasks[0].execution, execution);
        let binding_event = store.snapshot(&created.mission_id, 0).unwrap().events
            .into_iter().find(|event| event.event_type == "task_execution_updated").unwrap();
        assert_eq!(binding_event.payload["executor"], execution.executor);
        assert_eq!(binding_event.payload["model"], execution.model);
        assert_eq!(binding_event.payload["role_id"], execution.role_id);
    }

    #[test]
    fn execution_binding_rejects_unsupported_or_mismatched_assignments() {
        let store = store("execution-binding-negative");
        let created = store.create(&create_input()).unwrap();
        let mut unsupported = proposal();
        unsupported.tasks[0].execution.executor = "external.unknown".to_string();
        assert!(store.propose_plan(&created.mission_id, &unsupported).unwrap_err().to_string().contains("unsupported executor"));

        let mut mismatched = proposal();
        mismatched.tasks[0].execution.executor = "personal.codex".to_string();
        mismatched.tasks[0].execution.model = "himind:service:model".to_string();
        assert!(store.propose_plan(&created.mission_id, &mismatched).unwrap_err().to_string().contains("model/provider"));

        let mut missing_role = proposal();
        missing_role.tasks[0].execution.role_id.clear();
        assert!(store.propose_plan(&created.mission_id, &missing_role).unwrap_err().to_string().contains("role_id"));
    }


    #[test]
    fn implementation_cannot_pass_without_independent_verification() {
        let store = store("verification");
        let created = confirmed_single_task(&store, "verification");
        let second = store.create(&create_input()).unwrap();
        let mut second_plan = proposal();
        second_plan.tasks.truncate(1);
        let second_plan = store.propose_plan(&second.mission_id, &second_plan).unwrap();
        store.confirm_plan(
            &second.mission_id, &second_plan.plan_sha256, &["fix".to_string()],
            &["frontend/src".to_string()], &[], &[], "test-user",
        ).unwrap();
        let running = store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        assert_eq!(running.tasks[0].status, TaskStatus::Running);
        let pending = store.await_task_verification(&created.mission_id, "fix", "run-impl", "送验").unwrap();
        assert_eq!(pending.tasks[0].status, TaskStatus::AwaitingVerification);
        assert!(store.workspace_claim(&pending.workspace.root).unwrap().is_some());
        assert!(store.begin_task_attempt(&second.mission_id, "fix", "run-second").unwrap_err()
            .to_string().contains("workspace already has an active mission attempt"));
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-impl", true,
            &["local-run:run-impl".to_string()], "").unwrap_err().to_string().contains("cannot come"));
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &[], "").unwrap_err().to_string().contains("evidence"));
        store.begin_verification_attempt(&created.mission_id, "fix", "run-verify").unwrap();
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &["local-run:run-verify".to_string()], "").unwrap_err().to_string().contains("missing host"));
        let current = store.get(&created.mission_id).unwrap();
        store.append_event(&created.mission_id, "host_verification_checked", "fix", "unit test evidence", serde_json::json!({
            "passed": true, "plan_sha256": current.plan_sha256,
            "implementation_run_id": "run-impl", "verification_run_id": "run-verify",
        })).unwrap();
        let completed = store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &["local-run:run-impl".to_string(), "local-run:run-verify".to_string()], "").unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        assert_eq!(completed.tasks[0].verification_passed, Some(true));
        assert!(store.workspace_claim(&completed.workspace.root).unwrap().is_none());
        let repeated = store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &["local-run:run-impl".to_string(), "local-run:run-verify".to_string()], "").unwrap();
        assert_eq!(repeated.status, MissionStatus::Completed);
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", false,
            &["conflicting-evidence".to_string()], "").unwrap_err().to_string().contains("conflicting evidence"));
        let second_claim = store.begin_task_attempt(&second.mission_id, "fix", "run-second").unwrap();
        assert_eq!(second_claim.tasks[0].status, TaskStatus::Running);
    }

    #[test]
    fn follow_up_validation_requires_confirmed_scope() {
        let store = store("followup-ok");
        let created = confirmed_single_task(&store, "followup-ok");
        store.begin_task_attempt(&created.mission_id, "fix", "run-failed").unwrap();
        store.complete_task_attempt(&created.mission_id, "fix", "run-failed", false, &[], "failed").unwrap();
        let mut follow_up = task("repair");
        follow_up.repair_of = "fix".to_string();
        let updated = store.add_follow_up_tasks(&created.mission_id, &[follow_up], "验收失败").unwrap();
        assert!(updated.tasks.iter().any(|task| task.spec.id == "repair"));
        assert_eq!(updated.tasks.iter().find(|task| task.spec.id == "repair").unwrap().status, TaskStatus::Ready);
    }

    #[test]
    fn verification_rejects_unadmitted_run() {
        let store = store("unadmitted-verification");
        let created = confirmed_single_task(&store, "unadmitted");
        store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        store.await_task_verification(&created.mission_id, "fix", "run-impl", "送验").unwrap();
        let error = store.record_verification_result(
            &created.mission_id, "fix", "run-impl", "run-forged", true,
            &["local-run:run-forged".to_string()], "",
        ).unwrap_err();
        assert!(error.to_string().contains("not admitted"), "{error}");
        assert_eq!(store.get(&created.mission_id).unwrap().tasks[0].status, TaskStatus::AwaitingVerification);
    }

    #[test]
    fn verification_rejects_stale_implementation_run() {
        let store = store("stale-verification");
        let created = confirmed_single_task(&store, "stale");
        store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        store.await_task_verification(&created.mission_id, "fix", "run-impl", "送验").unwrap();
        store.begin_verification_attempt(&created.mission_id, "fix", "run-verify").unwrap();
        let error = store.record_verification_result(&created.mission_id, "fix", "run-stale", "run-verify", true,
            &["local-run:run-verify".to_string()], "").unwrap_err();
        assert!(error.to_string().contains("bound to implementation run"));
        assert_eq!(store.get(&created.mission_id).unwrap().tasks[0].status, TaskStatus::AwaitingVerification);
    }

    #[test]
    fn dispatch_budget_is_persisted_and_terminal() {
        let store = store("dispatch-budget");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_rounds = 1;
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string()],
            &["frontend/src".to_string()],
            &[],
            &[],
            "test-user",
        ).unwrap();
        assert!(store.next_ready_task(&created.mission_id).unwrap().is_some());
        store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        store.complete_task_attempt(&created.mission_id, "fix", "run-impl", false, &[], "failed").unwrap();
        assert!(store.next_ready_task(&created.mission_id).unwrap().is_none());
        let mission = store.get(&created.mission_id).unwrap();
        assert_eq!(mission.status, MissionStatus::Waiting);
        assert!(store.snapshot(&created.mission_id, 0).unwrap().events
            .iter().any(|event| event.event_type == "budget_exhausted"));
    }

    #[test]
    fn token_budget_uses_actual_usage_and_stops_before_next_dispatch() {
        let store = store("token-budget");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_total_tokens = Some(100);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string()],
            &["frontend/src".to_string()],
            &[],
            &[],
            "test-user",
        ).unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-usage").unwrap();
        let usage = store.record_run_usage(
            &created.mission_id,
            "fix",
            "run-usage",
            &crate::agent_core_contracts::LocalRunUsage {
                input_tokens: 90,
                output_tokens: 20,
                estimated_cost: 0.0,
                currency: String::new(),
                billing_owner: "local".to_string(),
            },
        ).unwrap();
        assert_eq!(usage.input_tokens + usage.output_tokens, 110);
        assert!(store.next_ready_task(&created.mission_id).unwrap().is_none());
        let reopened = MissionStore::new(store.path.clone());
        let mission = reopened.get(&created.mission_id).unwrap();
        assert_eq!(mission.status, MissionStatus::Waiting);
        assert_eq!(mission.usage.unwrap().input_tokens, 90);
        assert!(reopened.snapshot(&created.mission_id, 0).unwrap().events
            .iter().any(|event| event.event_type == "budget_exhausted"
                && event.summary.contains("Token")));
    }

    #[test]
    fn token_budget_fails_closed_without_usage_evidence() {
        let store = store("token-unreported");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_total_tokens = Some(1_000);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string()],
            &["frontend/src".to_string()],
            &[],
            &[],
            "test-user",
        ).unwrap();
        let mission = store.get(&created.mission_id).unwrap();
        assert!(store.budget_exhaustion_reason(&mission).unwrap().is_none());
        assert!(store.begin_task_attempt(&created.mission_id, "fix", "run-unreported").is_ok());
        store.complete_task_attempt(
            &created.mission_id,
            "fix",
            "run-unreported",
            false,
            &[],
            "runtime ended before usage was reported",
        ).unwrap();
        let reopened = MissionStore::new(store.path.clone());
        let mission = reopened.get(&created.mission_id).unwrap();
        assert_eq!(mission.usage, None);
        assert!(mission.tasks.iter().all(|task| task.status != TaskStatus::Ready));
    }

    #[test]
    fn usage_recording_is_idempotent_per_run() {
        let store = store("usage-idempotent");
        let created = confirmed_single_task(&store, "usage-idempotent");
        store.begin_task_attempt(&created.mission_id, "fix", "run-once").unwrap();
        let usage = crate::agent_core_contracts::LocalRunUsage {
            input_tokens: 7,
            output_tokens: 3,
            estimated_cost: 0.01,
            currency: "CNY".to_string(),
            billing_owner: "local".to_string(),
        };
        let first = store.record_run_usage(&created.mission_id, "fix", "run-once", &usage).unwrap();
        let second = store.record_run_usage(&created.mission_id, "fix", "run-once", &usage).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.get(&created.mission_id).unwrap().usage.unwrap().input_tokens, 7);
        assert_eq!(store.snapshot(&created.mission_id, 0).unwrap().events
            .iter().filter(|event| event.event_type == "run_usage_recorded").count(), 1);
    }

    #[test]
    fn declared_cost_limit_fails_closed_when_usage_does_not_report_cost() {
        let store = store("cost-unreported");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_cost = Some(1.0);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string()],
            &["frontend/src".to_string()],
            &[],
            &[],
            "test-user",
        ).unwrap();
        // With a cost limit declared, no trusted usage has been reported yet,
        // so dispatch must already fail closed instead of starting unpriced work.
        assert!(store.begin_task_attempt(&created.mission_id, "fix", "run-cost-unreported")
            .unwrap_err().to_string().contains("成本"));
        // Simulate a Runtime that completed without a billable amount. The store
        // keeps the usage event but the budget gate remains closed.
        store.append_event(&created.mission_id, "run_usage_recorded", "fix", "untrusted cost",
            serde_json::json!({
                "run_id": "run-cost-unreported",
                "input_tokens": 10,
                "output_tokens": 2,
                "estimated_cost": 0.0,
                "currency": "",
                "billing_owner": "local",
            })).unwrap();
        let mission = store.get(&created.mission_id).unwrap();
        assert!(store.budget_exhaustion_reason(&mission).unwrap().is_some_and(|reason| reason.contains("成本")));
    }

    #[test]
    fn repair_budget_counts_follow_up_tasks() {
        let store = store("repair-budget");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_repair_attempts = 1;
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id,
            &planned.plan_sha256,
            &["fix".to_string()],
            &["frontend/src".to_string()],
            &[],
            &[],
            "test-user",
        ).unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-failed").unwrap();
        store.complete_task_attempt(&created.mission_id, "fix", "run-failed", false, &[], "failed").unwrap();
        let mut follow_up = task("repair");
        follow_up.repair_of = "fix".to_string();
        let updated = store.add_follow_up_tasks(&created.mission_id, &[follow_up], "返工").unwrap();
        assert_eq!(updated.status, MissionStatus::Active);
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id, "repair");
        let mut excess = task("repair-excess");
        excess.repair_of = "fix".to_string();
        assert!(store.add_follow_up_tasks(&created.mission_id, &[excess], "more").unwrap_err().to_string().contains("repair budget"));
    }

    #[test]
    fn pause_resume_and_cancel_survive_reopen_without_reauthorizing_work() {
        let store = store("mission-control");
        let created = confirmed_single_task(&store, "control");
        let paused = store.control(&created.mission_id, "pause").unwrap();
        assert_eq!(paused.status, MissionStatus::Waiting);
        let reopened = MissionStore::new(store.path.clone());
        assert!(reopened.next_ready_task(&created.mission_id).unwrap().is_none());
        assert!(reopened.begin_task_attempt(&created.mission_id, "fix", "paused-run").is_err());
        let active = reopened.control(&created.mission_id, "resume").unwrap();
        assert_eq!(active.status, MissionStatus::Active);
        assert!(active.confirmation.is_some());
        reopened.begin_task_attempt(&created.mission_id, "fix", "cancel-run").unwrap();
        reopened.control(&created.mission_id, "cancel").unwrap();
        assert!(reopened.workspace_claim(&active.workspace.root).unwrap().is_some());
        reopened.release_canceled_claims(&created.mission_id).unwrap();
        assert!(reopened.workspace_claim(&active.workspace.root).unwrap().is_none());
        assert!(reopened.next_ready_task(&created.mission_id).unwrap().is_none());
        assert!(reopened.control(&created.mission_id, "resume").is_err());
        assert!(reopened.begin_task_attempt(&created.mission_id, "fix", "late-run").is_err());
        assert_eq!(reopened.get(&created.mission_id).unwrap().status, MissionStatus::Canceled);
    }

    #[test]
    fn independently_verified_repair_unblocks_the_original_dependency() {
        let store = store("repair-dependency");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], &[], &[], "test-user").unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-failed").unwrap();
        assert!(store.complete_task_attempt(&created.mission_id, "fix", "run-failed", true, &[], "self-pass")
            .unwrap_err().to_string().contains("independent"));
        store.complete_task_attempt(&created.mission_id, "fix", "run-failed", false, &[], "failed").unwrap();
        let mut repair = task("repair"); repair.repair_of = "fix".to_string();
        store.add_follow_up_tasks(&created.mission_id, &[repair], "repair").unwrap();
        store.begin_task_attempt(&created.mission_id, "repair", "run-repair").unwrap();
        store.await_task_verification(&created.mission_id, "repair", "run-repair", "verify repair").unwrap();
        store.begin_verification_attempt(&created.mission_id, "repair", "run-repair-verify").unwrap();
        store.append_event(&created.mission_id, "host_verification_checked", "repair", "unit test host evidence", serde_json::json!({
            "passed": true, "plan_sha256": planned.plan_sha256,
            "implementation_run_id": "run-repair", "verification_run_id": "run-repair-verify",
        })).unwrap();
        let updated = store.record_verification_result(&created.mission_id, "repair", "run-repair", "run-repair-verify", true,
            &["local-run:run-repair-verify".to_string()], "").unwrap();
        assert_eq!(updated.tasks.iter().find(|task| task.spec.id == "fix").unwrap().status, TaskStatus::Completed);
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id, "verify");
    }

    #[test]
    fn failed_task_is_not_resurrected_as_ready() {
        let store = store("failed-terminal");
        let created = confirmed_single_task(&store, "failed-terminal");
        store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        let failed = store.complete_task_attempt(&created.mission_id, "fix", "run-impl", false, &[], "runtime unavailable").unwrap();
        assert_eq!(failed.tasks[0].status, TaskStatus::Failed);
        assert!(store.next_ready_task(&created.mission_id).unwrap().is_none());
        assert_eq!(store.get(&created.mission_id).unwrap().tasks[0].status, TaskStatus::Failed);
    }
}
