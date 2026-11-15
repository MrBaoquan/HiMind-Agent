use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::contract::{
    absolute_path_approved, is_absolute_path, normalized_paths, path_allowed, plan_hash_for_workspace as plan_hash, require_id, require_sha256, require_text,
    apply_assignment_to_plan, validate_assignment, validate_create, validate_handoff_input,
    validate_proposal, validate_task_paths, validate_selection_with_scopes, validate_task_graph,
    MissionAssignment, MissionAssignmentInput, MissionConfirmation, MissionCreateInput,
    MissionBudgetIncrease, MissionVerificationRetry, MissionEvent, MissionHandoff, MissionHandoffInput, MissionPlanMode, MissionPlanProposal,
    MissionStatus, MissionTaskSpec, MissionTaskView, MissionView, MissionWorkflowPin, TaskStatus,
};

/// Follow both dependency edges and, when requested, a repair's original task.
fn task_reaches(tasks: &[MissionTaskSpec], from: &str, target: &str, repairs: bool) -> bool {
    let mut pending = vec![from.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) { continue; }
        if let Some(task) = tasks.iter().find(|task| task.id == id) {
            for next in task.depends_on.iter().chain(repairs.then_some(&task.repair_of).into_iter()) {
                if next == target { return true; }
                if !next.is_empty() { pending.push(next.clone()); }
            }
        }
    }
    false
}

fn delivery_root<'a>(tasks: &'a [MissionTaskSpec], id: &'a str) -> &'a str {
    let mut current = id;
    let mut seen = BTreeSet::new();
    while seen.insert(current) {
        let Some(task) = tasks.iter().find(|task| task.id == current) else { break; };
        if task.repair_of.is_empty() { break; }
        current = &task.repair_of;
    }
    current
}

/// A failed downstream review may send a completed producer back to its
/// assigned expert only together with a dependent, separate review retry.
fn review_rework_targets(mission: &MissionView, tasks: &[MissionTaskSpec]) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let existing = mission.tasks.iter().map(|task| task.spec.clone()).collect::<Vec<_>>();
    let mut reopened = BTreeSet::new();
    for revision in tasks {
        let parent = mission.tasks.iter().find(|task| task.spec.id == revision.repair_of).ok_or("repair target missing")?;
        if parent.status != TaskStatus::Completed { continue; }
        let reviewed = tasks.iter().any(|review| {
            mission.tasks.iter().any(|failed| failed.spec.id == review.repair_of
                && failed.status == TaskStatus::Failed
                && task_reaches(&existing, &failed.spec.id, &parent.spec.id, true))
                && task_reaches(tasks, &review.id, &revision.id, false)
        });
        if !reviewed { return Err("completed producer revision requires a dependent retry of its failed downstream review".into()); }
        reopened.insert(parent.spec.id.clone());
    }
    if !reopened.is_empty() {
        if mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::Ready | TaskStatus::AwaitingVerification)) {
            return Err("wait for active work before reopening reviewed deliveries".into());
        }
        for task in &mission.tasks {
            // Completed repair attempts are historical validations of the same
            // delivery. Revising its root supersedes that delivery once, without
            // pretending those old attempts failed or replaying every attempt.
            let covered = reopened.iter().any(|id|
                delivery_root(&existing, &task.spec.id) == delivery_root(&existing, id));
            if task.status == TaskStatus::Completed && !covered
                && reopened.iter().any(|id| task_reaches(&existing, &task.spec.id, id, false)) {
                return Err("revision must also revalidate completed downstream deliveries".into());
            }
        }
        for downstream in tasks.iter().filter(|task| reopened.contains(&task.repair_of)) {
            for upstream in tasks.iter().filter(|task| reopened.contains(&task.repair_of)) {
                if delivery_root(&existing, &downstream.repair_of) != delivery_root(&existing, &upstream.repair_of)
                    && task_reaches(&existing, &downstream.repair_of, &upstream.repair_of, true)
                    && !task_reaches(tasks, &downstream.id, &upstream.id, false) {
                    return Err("downstream delivery revision must depend on its upstream revision".into());
                }
            }
        }
    }
    Ok(reopened)
}

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
            &mission_id, &input.workspace, &input.acceptance_criteria, proposal,
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
        let catalog = crate::app::mission_commands::mission_assignment_options_sync(mission_id)
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        crate::mission::validate_catalog_assignment(input, &catalog)?;
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
        for role in &input.roles {
            let pin = role.expert.as_ref().ok_or("请为每项职责选择已安装专家")?;
            crate::mission::validate_expert_pin(pin)?;
        }
        let (mission, proposal) = self.get_with_optional_plan(mission_id)?;
        if !matches!(mission.status, MissionStatus::AwaitingAssignment | MissionStatus::Planned | MissionStatus::AwaitingConfirmation) {
            return Err("mission is not awaiting role assignment".into());
        }
        if let Some(plan) = proposal.as_ref() {
            for task in &plan.tasks {
                if !input.task_roles.iter().any(|mapping| mapping.task_id == task.id) {
                    return Err(format!("task {} must select an assigned expert role", task.id).into());
                }
            }
            if input.task_roles.iter().any(|mapping| !plan.tasks.iter().any(|task| task.id == mapping.task_id)) {
                return Err("task role mapping references a task outside the imported plan".into());
            }
        }
        let workflow = mission.workflow.as_ref().ok_or("workflow-pinned mission has no workflow pin")?;
        let (package, _) = workflows.load_pinned_version(
            &workflow.workflow_id,
            &workflow.workflow_version,
            &workflow.workflow_sha256,
        )?;
        let allowed_roles = if package.roles.is_empty() {
            package.steps.iter().map(|step| step.role_id.trim()).filter(|role| !role.is_empty())
                .map(str::to_string).collect::<BTreeSet<_>>()
        } else { package.roles.keys().cloned().collect::<BTreeSet<_>>() };
        let assigned_roles = input.roles.iter().map(|role| role.role_id.clone()).collect::<BTreeSet<_>>();
        if assigned_roles != allowed_roles {
            return Err("assignment must cover every role declared by the pinned workflow".into());
        }
        if input.roles.iter().any(|role| !package.accepts_provider(&role.executor)) {
            return Err("assigned executor is not accepted by the pinned workflow".into());
        }
        let assignment = MissionAssignment {
            roles: input.roles.clone(), task_roles: input.task_roles.clone(),
            applied_by: input.applied_by.trim().to_string(),
            applied_at: now_stamp(), recommended: input.recommended,
        };
        let imported = mission.plan_mode == MissionPlanMode::Imported;
        let has_plan = proposal.as_ref().is_some_and(|plan| !plan.tasks.is_empty());
        let next_status = if has_plan { MissionStatus::AwaitingConfirmation } else { MissionStatus::Planned };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if imported && !has_plan {
            return Err("imported mission has no plan to assign".into());
        }
        if let Some(plan) = proposal.as_ref().filter(|plan| !plan.tasks.is_empty()) {
            let proposal = apply_assignment_to_plan(plan, input)?;
            let hash = plan_hash(&mission_id, &mission.workspace, &mission.acceptance_criteria, &proposal)?;
            transaction.execute(
                "UPDATE mission_runs SET assignment_json = ?2, status = ?3, plan_sha256 = ?4, plan_json = ?5,
                    confirmation_json = NULL, plan_revision = plan_revision + 1, updated_at = ?6 WHERE mission_id = ?1",
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

    pub(crate) fn set_permission_mode(&self, mission_id: &str, expected_plan: &str, mode: &str) -> Result<MissionView, Box<dyn Error>> {
        if !matches!(mode,"workspace_write"|"full_access") { return Err("不支持的执行权限模式".into()); }
        let (mission, proposal) = self.get_with_optional_plan(mission_id)?;
        if !matches!(mission.status, MissionStatus::Draft | MissionStatus::AwaitingAssignment | MissionStatus::Planned | MissionStatus::AwaitingConfirmation) {
            return Err("开始执行后不能在配置页更改权限；请在环境恢复入口选择".into());
        }
        if mission.plan_sha256 != expected_plan { return Err("任务计划已变化，请重新加载".into()); }
        let mut workspace = mission.workspace.clone();
        workspace.permission_mode = mode.into();
        let hash = proposal.as_ref().map(|plan| plan_hash(mission_id,&workspace,&mission.acceptance_criteria,plan)).transpose()?.unwrap_or_default();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_mission(&transaction,mission_id)?.ok_or("mission not found")?;
        if current.plan_sha256 != expected_plan || current.status != mission.status || current.workspace != mission.workspace {
            return Err("任务在操作期间已变化，请重新加载".into());
        }
        let now = now_stamp();
        transaction.execute("UPDATE mission_runs SET workspace_json=?2,plan_sha256=?3,confirmation_json=NULL,plan_revision=plan_revision+?4,updated_at=?5 WHERE mission_id=?1",
            params![mission_id,serde_json::to_string(&workspace)?,hash,if proposal.is_some(){1}else{0},now])?;
        append_event_tx(&transaction,mission_id,"mission_permission_changed","",
            if mode=="full_access" {"执行权限已设为完全访问，等待确认计划后生效"} else {"执行权限已设为工作区访问，等待确认计划后生效"},
            serde_json::json!({"permission_mode":mode,"plan_sha256":hash}),&now)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn runtime_blocked(&self, mission_id: &str) -> Result<bool, Box<dyn Error>> {
        let connection = self.connection()?;
        let mission = self.get(mission_id)?;
        runtime_blocked_on(&connection, &mission)
    }

    pub(crate) fn runtime_files_only(&self, mission_id: &str, plan_sha256: &str) -> Result<bool, Box<dyn Error>> {
        Ok(self.runtime_mode(mission_id, plan_sha256)? == "files_only")
    }

    pub(crate) fn runtime_mode(&self, mission_id: &str, plan_sha256: &str) -> Result<String, Box<dyn Error>> {
        let connection = self.connection()?;
        let payload: Option<String> = connection.query_row(
            "SELECT payload_json FROM mission_events WHERE mission_id=?1 AND event_type='runtime_workspace_resolved' ORDER BY sequence DESC LIMIT 1",
            params![mission_id], |row| row.get(0)).optional()?;
        let value: serde_json::Value = payload.map(|text| serde_json::from_str(&text)).transpose()?.unwrap_or_default();
        if value["plan_sha256"] == plan_sha256 {
            if let Some(mode) = value["mode"].as_str() { return Ok(mode.to_string()); }
        }
        let mission = self.get(mission_id)?;
        Ok(if mission.plan_sha256 == plan_sha256 && mission.workspace.permission_mode == "full_access" { "full_access" } else { "default" }.to_string())
    }

    pub(crate) fn suspend_runtime(&self, mission_id: &str, task_id: &str, diagnosis: &serde_json::Value) -> Result<MissionView, Box<dyn Error>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status != MissionStatus::Active { return Ok(mission); }
        let now = now_stamp();
        transaction.execute("UPDATE mission_runs SET status='waiting',updated_at=?2 WHERE mission_id=?1", params![mission_id,now])?;
        append_event_tx(&transaction, mission_id, "runtime_workspace_blocked", task_id,
            "命令环境待处理：专家尚未开始，请选择工作区授权或仅用文件工具继续",
            serde_json::json!({"environment":diagnosis,"plan_sha256":mission.plan_sha256}), &now)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn resolve_runtime_block(&self, mission_id: &str, plan_sha256: &str, mode: &str, evidence: &serde_json::Value) -> Result<MissionView, Box<dyn Error>> {
        if !matches!(mode,"default"|"files_only"|"full_access") { return Err("invalid runtime recovery mode".into()); }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status != MissionStatus::Waiting || mission.plan_sha256 != plan_sha256 || !runtime_blocked_on(&transaction, &mission)? {
            return Err("runtime recovery no longer matches the waiting task and confirmed plan".into());
        }
        let budget_reason = budget_exhaustion_reason_on(&transaction, &mission)?;
        let now=now_stamp();
        transaction.execute("UPDATE mission_runs SET status=?3,updated_at=?2 WHERE mission_id=?1",params![mission_id,now,
            if budget_reason.is_some() { "waiting" } else { "active" }])?;
        append_event_tx(&transaction,mission_id,"runtime_workspace_resolved","",
            if mode=="files_only" {"用户选择仅用文件工具继续，命令工具保持禁用"} else if mode=="full_access" {"用户授权本任务完全访问，按当前系统用户权限继续"} else {"工作区沙箱权限已准备，恢复原任务"},
            serde_json::json!({"mode":mode,"plan_sha256":plan_sha256,"evidence":evidence}),&now)?;
        if let Some(reason) = budget_reason {
            append_event_tx(&transaction, mission_id, "budget_exhausted", "", &reason,
                serde_json::json!({"reason":reason}), &now)?;
        }
        transaction.commit()?;
        self.get(mission_id)
    }

    pub(crate) fn control(&self, mission_id: &str, action: &str) -> Result<MissionView, Box<dyn Error>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if action == "resume" && runtime_blocked_on(&transaction, &mission)? {
            return Err("命令环境尚未处理，请先选择工作区授权、重新检查或仅用文件工具继续".into());
        }
        if action == "resume" && mission.status == MissionStatus::Waiting {
            if let Some(reason) = budget_exhaustion_reason_on(&transaction, &mission)? {
                let latest_event: Option<String> = transaction.query_row(
                    "SELECT event_type FROM mission_events WHERE mission_id = ?1 ORDER BY sequence DESC LIMIT 1",
                    params![mission_id],
                    |row| row.get(0),
                ).optional()?;
                let now = now_stamp();
                if latest_event.as_deref() != Some("budget_exhausted") {
                    transaction.execute(
                        "UPDATE mission_runs SET updated_at = ?2 WHERE mission_id = ?1",
                        params![mission_id, now],
                    )?;
                    append_event_tx(
                        &transaction, mission_id, "budget_exhausted", "", &reason,
                        serde_json::json!({"reason": reason}), &now,
                    )?;
                }
                transaction.commit()?;
                return Err(reason.into());
            }
        }
        let next = match (action, mission.status) {
            ("pause", MissionStatus::Active) => MissionStatus::Waiting,
            ("resume", MissionStatus::Waiting) => MissionStatus::Active,
            ("cancel", status) if !matches!(status, MissionStatus::Completed | MissionStatus::Canceled) => MissionStatus::Canceled,
            ("resume", MissionStatus::Active) if mission.tasks.iter().any(|task| task.status == TaskStatus::Failed)
                && !mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Running | TaskStatus::AwaitingVerification)) => MissionStatus::Active,
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

    pub(crate) fn increase_budget(
        &self,
        mission_id: &str,
        input: &MissionBudgetIncrease,
        increased_by: &str,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("mission_id", mission_id)?;
        require_text("budget request_id", &input.request_id, 200)?;
        require_sha256(&input.expected_plan_sha256)?;
        require_text("increased_by", increased_by, 160)?;
        if input.additional_minutes > 525_600
            || input.additional_tokens.is_some_and(|value| value == 0 || value > 9_000_000_000_000_000)
            || input.additional_cost.is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("budget increase values are outside the supported range".into());
        }
        if input.additional_tokens.is_some() && input.unlimited_tokens {
            return Err("choose either additional_tokens or unlimited_tokens".into());
        }

        let request_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(input)?));
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;

        let prior_payloads = {
            let mut statement = transaction.prepare(
                "SELECT payload_json FROM mission_events WHERE mission_id = ?1 AND event_type = 'budget_extended'",
            )?;
            let payloads = statement.query_map(params![mission_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            payloads
        };
        for raw in prior_payloads {
            let payload: serde_json::Value = serde_json::from_str(&raw)?;
            if payload.get("request_id").and_then(serde_json::Value::as_str) == Some(input.request_id.as_str()) {
                if payload.get("request_sha256").and_then(serde_json::Value::as_str) != Some(request_digest.as_str()) {
                    return Err("budget request_id was already used with different values".into());
                }
                transaction.commit()?;
                return self.get(mission_id);
            }
        }

        if mission.status != MissionStatus::Waiting {
            return Err("budget can only be increased while the mission is waiting".into());
        }
        if mission.plan_sha256 != input.expected_plan_sha256
            || mission.confirmation.as_ref().is_none_or(|confirmation| confirmation.plan_sha256 != input.expected_plan_sha256)
        {
            return Err("mission plan changed; reload before increasing its budget".into());
        }
        if mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            return Err("mission still has a running task or pending verification; wait for it to stop before continuing".into());
        }
        let reason = budget_exhaustion_reason_on(&transaction, &mission)?
            .or_else(|| mission.budget.as_ref().and_then(|budget| repair_exhaustion_reason(&mission, budget)))
            .unwrap_or_else(|| "用户主动增加后续执行预算".to_string());
        let mut budget = mission.budget.clone().ok_or("mission has no declared budget")?;
        let previous_budget = budget.clone();
        budget.max_rounds = budget.max_rounds.checked_add(input.additional_rounds)
            .ok_or("max_rounds exceeds the supported range")?;
        budget.max_minutes = budget.max_minutes.checked_add(input.additional_minutes)
            .ok_or("max_minutes exceeds the supported range")?;
        budget.max_repair_attempts = budget.max_repair_attempts.checked_add(input.additional_repair_attempts)
            .ok_or("max_repair_attempts exceeds the supported range")?;
        if input.unlimited_tokens {
            if budget.max_total_tokens.is_none() {
                return Err("token budget is already unlimited".into());
            }
            budget.max_total_tokens = None;
        } else if let Some(additional) = input.additional_tokens {
            let current = budget.max_total_tokens.ok_or("token budget is already unlimited")?;
            budget.max_total_tokens = Some(current.checked_add(additional).ok_or("max_total_tokens exceeds the supported range")?);
        }
        if let Some(additional) = input.additional_cost {
            let current = budget.max_cost.ok_or("cost budget is already unlimited")?;
            let increased = current + additional;
            if !increased.is_finite() {
                return Err("max_cost exceeds the supported range".into());
            }
            budget.max_cost = Some(increased);
        }
        if budget == previous_budget {
            return Err("provide at least one budget increase".into());
        }
        if let Some(remaining_reason) = budget_exhaustion_reason_with_budget(&transaction, &mission, &budget)?
            .or_else(|| repair_exhaustion_reason(&mission, &budget)) {
            return Err(format!("the increased budget still does not allow continuation: {remaining_reason}").into());
        }

        let now = now_stamp();
        transaction.execute(
            "UPDATE mission_runs SET budget_json = ?2, status = ?3, updated_at = ?4 WHERE mission_id = ?1",
            params![mission_id, serde_json::to_string(&budget)?,
                if runtime_blocked_on(&transaction, &mission)? { "waiting" } else { "active" }, now],
        )?;
        append_event_tx(
            &transaction, mission_id, "budget_extended", "",
            format!("预算已增加，任务从“{reason}”恢复执行"),
            serde_json::json!({
                "request_id": input.request_id,
                "request_sha256": request_digest,
                "plan_sha256": input.expected_plan_sha256,
                "increased_by": increased_by.trim(),
                "previous_budget": previous_budget,
                "budget": budget,
                "increase": input,
            }),
            &now,
        )?;
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
            &mission.workspace,
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
        if mission.workspace.permission_mode == "full_access" && proposal.tasks.iter().any(|task| selected.contains(&task.id) && !crate::workflow::supports_full_access(&task.execution.executor)) {
            return Err("完全访问模式需要选择 HiMind AI 或 ACP 执行器，请调整专家指派或改用工作区访问".into());
        }
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
        if let Some(assignment) = mission.assignment.as_ref() {
            let role = assignment.roles.iter().find(|role| role.role_id == execution.role_id)
                .ok_or("task must select an assigned expert role")?;
            let expected = crate::mission::ExecutionBinding {
                executor: role.executor.clone(), model: role.model.clone(), role_id: role.role_id.clone(),
                service_id: role.service_id.clone(), expert: role.expert.clone(),
            };
            if expected != *execution {
                return Err("change the expert assignment to edit its execution binding".into());
            }
        }
        task.execution = execution.clone();
        crate::mission::validate_execution_binding(task)?;
        validate_proposal(&proposal)?;
        validate_task_paths(&proposal.tasks, &mission.workspace.root)?;
        let plan_sha256 = plan_hash(
            &mission.mission_id,
            &mission.workspace,
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
        let mut bound_tasks = tasks.to_vec();
        if let Some(assignment) = mission.assignment.as_ref() {
            for task in &mut bound_tasks {
                let parent = mission.tasks.iter().find(|parent| parent.spec.id == task.repair_of)
                    .ok_or("follow-up must reference a failed repair target")?;
                let role = assignment.roles.iter().find(|role| role.role_id == parent.spec.execution.role_id)
                    .ok_or("follow-up target has no assigned expert role")?;
                task.execution = crate::mission::ExecutionBinding {
                    role_id: role.role_id.clone(), executor: role.executor.clone(),
                    model: role.model.clone(), service_id: role.service_id.clone(), expert: role.expert.clone(),
                };
            }
        }
        let tasks = bound_tasks.as_slice();
        let mut all = mission.tasks.iter().map(|task| task.spec.clone()).collect::<Vec<_>>();
        all.extend_from_slice(tasks);
        validate_task_graph(&all)?;
        validate_task_paths(tasks, &mission.workspace.root)?;
        let existing = mission.tasks.iter().map(|task| task.spec.id.as_str()).collect::<BTreeSet<_>>();
        if tasks.iter().any(|task| existing.contains(task.id.as_str())) {
            return Err("follow-up task id already exists in mission".into());
        }
        let reopened = review_rework_targets(&mission, tasks)?;
        for task in tasks {
            let parent = mission.tasks.iter().find(|parent| parent.spec.id == task.repair_of)
                .ok_or("follow-up must reference a repair target")?;
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
                // A read authorization is not a write grant. Each revision
                // retains its original specialist's exact output boundary.
                if !path_allowed(path, &parent.spec.write_paths) {
                    return Err(format!("follow-up task {} writes outside its repair target scope: {path}", task.id).into());
                }
            }
            if parent.status != TaskStatus::Failed && !reopened.contains(&parent.spec.id) {
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
        for id in &reopened {
            let parent = mission.tasks.iter().find(|task| &task.spec.id == id).ok_or("revision target missing")?;
            transaction.execute("UPDATE mission_tasks SET status='failed', verification_passed=0, updated_at=?3 WHERE mission_id=?1 AND task_id=?2",
                params![mission_id, id, now])?;
            append_event_tx(&transaction, mission_id, "task_reopened_for_review", id,
                "后续独立复核要求修改，原交付退回原专家；原验收证据保留",
                serde_json::json!({"plan_sha256": mission.plan_sha256,
                    "previous_implementation_run_id":parent.attempt_run_id,
                    "previous_verification_run_id":parent.verification_run_id,
                    "previous_evidence_refs":parent.evidence_refs,
                    "revision_task_ids":tasks.iter().filter(|task| &task.repair_of == id).map(|task| &task.id).collect::<Vec<_>>(),
                    "reason":reason}), &now)?;
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
        let previous = effective_usage_payloads(&transaction, mission_id)?.into_iter()
            .find(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()
                .and_then(|value| value.get("run_id").and_then(serde_json::Value::as_str).map(str::to_string))
                .as_deref() == Some(run_id));
        let reconciles = previous.as_ref().is_some_and(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()
            .is_some_and(|value| value["billing_owner"] == "unreported")) && usage.billing_owner != "unreported";
        if previous.is_none() || reconciles {
            append_event_tx(&transaction, mission_id,
                if reconciles { "run_usage_reconciled" } else { "run_usage_recorded" }, task_id,
                if reconciles { "依据运行时计量补记用量，保留原始未知记录" } else { "记录执行器用量" },
                serde_json::json!({"run_id":run_id,"input_tokens":usage.input_tokens,
                    "output_tokens":usage.output_tokens,"estimated_cost":usage.estimated_cost,
                    "currency":usage.currency,"billing_owner":usage.billing_owner}), &now)?;
        }
        let rows = effective_usage_payloads(&transaction, mission_id)?;
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

    /// Recheck an existing implementation without rewriting files or granting
    /// another repair. Prior failures remain in the append-only event history.
    pub(crate) fn retry_verification(
        &self, mission_id: &str, task_id: &str, input: &MissionVerificationRetry,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("task_id", task_id)?;
        require_text("request_id", &input.request_id, 200)?;
        require_sha256(&input.expected_plan_sha256)?;
        require_id("expected_verification_run_id", &input.expected_verification_run_id)?;
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(input)?));
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        let events = list_events(&tx, mission_id, 0)?;
        for event in &events {
            if event.event_type == "verification_retry_requested" && event.payload["request_id"] == input.request_id {
                if event.task_id != task_id || event.payload["request_sha256"] != digest {
                    return Err("verification request_id already used with different values".into());
                }
                tx.commit()?;
                return self.get(mission_id);
            }
        }
        if mission.status != MissionStatus::Active || mission.confirmation.as_ref()
            .is_none_or(|value| value.plan_sha256 != input.expected_plan_sha256)
            || mission.plan_sha256 != input.expected_plan_sha256
        { return Err("recheck requires the active confirmed plan".into()); }
        if let Some(reason) = budget_exhaustion_reason_on(&tx, &mission)? {
            return Err(format!("increase the exhausted budget before rechecking: {reason}").into());
        }
        if mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::Ready | TaskStatus::AwaitingVerification)) {
            return Err("wait for current work to finish before rechecking".into());
        }
        let task = mission.tasks.iter().find(|task| task.spec.id == task_id).ok_or("task not found")?;
        if task.status != TaskStatus::Failed || task.verification_passed != Some(false)
            || task.verification_run_id != input.expected_verification_run_id || task.attempt_run_id.is_empty()
        { return Err("recheck requires the latest failed independent verification".into()); }
        if mission.tasks.iter().any(|child| child.spec.repair_of == task_id && child.status != TaskStatus::Canceled) {
            return Err("recheck the latest repair task instead of its ancestor".into());
        }
        let prior_baseline = events.iter().rev().find(|event| event.task_id == task_id
            && matches!(event.event_type.as_str(), "task_baseline_captured" | "verification_retry_requested"))
            .and_then(|event| event.payload.get("protected_baseline")).cloned();
        let baseline = if let Some(baseline) = prior_baseline {
            if input.source_baseline.is_some() { return Err("an existing baseline cannot be replaced".into()); }
            baseline
        } else {
            let source = input.source_baseline.as_ref().ok_or("no execution baseline; provide a sourced pre-run baseline")?;
            require_text("source_reference", &source.source_reference, 1024)?;
            require_sha256(&source.artifact_sha256)?;
            let captured = chrono::DateTime::parse_from_rfc3339(&source.captured_at)?;
            let started = events.iter().find(|event| event.event_type == "task_dispatched")
                .ok_or("initial dispatch timestamp missing")?;
            if captured > chrono::DateTime::parse_from_rfc3339(&started.occurred_at)? {
                return Err("source baseline must predate the first implementation dispatch".into());
            }
            if source.files.is_empty() || source.files.len() > 10_000 { return Err("invalid baseline file count".into()); }
            for hash in source.files.values() { require_sha256(hash)?; }
            serde_json::json!({"provenance": "source_client_pre_run_attestation", "source": source,
                "files": source.files, "imported_at": now_stamp()})
        };
        let current = super::protected_files(std::path::Path::new(&mission.workspace.root), &task.spec)?;
        if baseline["files"] != current {
            return Err("protected files differ from the baseline or the baseline does not cover the read-only scope".into());
        }
        let now = now_stamp();
        tx.execute("INSERT INTO mission_workspace_claims (workspace_key, mission_id, task_id, run_id, acquired_at)
            VALUES (?1, ?2, ?3, ?4, ?5)", params![workspace_claim_key(&mission.workspace.root), mission_id, task_id, task.attempt_run_id, now])?;
        tx.execute("UPDATE mission_tasks SET status = 'awaiting_verification', verification_run_id = '',
            verification_passed = NULL, updated_at = ?3 WHERE mission_id = ?1 AND task_id = ?2", params![mission_id, task_id, now])?;
        tx.execute("UPDATE mission_runs SET updated_at = ?2 WHERE mission_id = ?1", params![mission_id, now])?;
        append_event_tx(&tx, mission_id, "verification_retry_requested", task_id,
            "已补充核验证据，重新检查同一实现；不增加代码返工次数",
            serde_json::json!({"request_id": input.request_id, "request_sha256": digest,
                "previous_verification_run_id": input.expected_verification_run_id,
                "plan_sha256": input.expected_plan_sha256, "protected_baseline": baseline}), &now)?;
        tx.commit()?;
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
            if passed { "独立验证通过".to_string() } else { format!("独立验证失败：{}", reason.trim()).chars().take(1900).collect() },
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
        let (mission, proposal) = self.get_with_optional_plan(mission_id)?;
        Ok((mission, proposal.ok_or("mission has no proposed plan")?))
    }

    fn get_with_optional_plan(&self, mission_id: &str) -> Result<(MissionView, Option<MissionPlanProposal>), Box<dyn Error>> {
        let connection = self.connection()?;
        let mission = load_mission(&connection, mission_id)?
            .ok_or_else(|| format!("mission not found: {mission_id}"))?;
        let plan_json: String = connection.query_row(
            "SELECT plan_json FROM mission_runs WHERE mission_id = ?1",
            params![mission_id],
            |row| row.get(0),
        )?;
        let proposal = if mission.plan_sha256.is_empty() && plan_json.trim() == "{}" {
            None
        } else { serde_json::from_str(&plan_json)? };
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

    pub(crate) fn follow_up_budget_exhaustion_reason(&self, mission: &MissionView) -> Result<Option<String>, Box<dyn Error>> {
        Ok(self.budget_exhaustion_reason(mission)?.or_else(||
            mission.budget.as_ref().and_then(|budget| repair_exhaustion_reason(mission, budget))))
    }

    pub(crate) fn record_budget_exhausted(&self, mission_id: &str, reason: &str) -> Result<(), Box<dyn Error>> {
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
            params![mission_id, MissionStatus::Waiting.as_str(), now],
        )?;
        append_event_tx(&transaction, mission_id, "budget_exhausted", "", reason,
            serde_json::json!({"reason": reason}), &now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn wait_for_follow_up(&self, mission_id: &str, message: &str, fingerprint: &str) -> Result<(), Box<dyn Error>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status != MissionStatus::Active || mission.tasks.iter().any(|task|
            matches!(task.status, TaskStatus::Ready | TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            return Ok(());
        }
        let now = now_stamp();
        transaction.execute("UPDATE mission_runs SET status = 'waiting', updated_at = ?2 WHERE mission_id = ?1", params![mission_id, now])?;
        append_event_tx(&transaction, mission_id, "mission_follow_up_waiting", "",
            "返工计划生成失败，任务已暂停；修复原因后恢复可重新生成",
            serde_json::json!({"error":message,"fingerprint":fingerprint,"recovery_action":"resume"}), &now)?;
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

fn repair_exhaustion_reason(mission: &MissionView, budget: &crate::mission::MissionBudget) -> Option<String> {
    let repairs = mission.tasks.iter().filter(|task| !task.spec.repair_of.is_empty()).count();
    (repairs >= budget.max_repair_attempts as usize
        && mission.tasks.iter().any(|task| task.status == TaskStatus::Failed)
        && !mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Running | TaskStatus::AwaitingVerification)))
        .then(|| format!("达到返工上限：已创建 {repairs}/{} 次返工", budget.max_repair_attempts))
}

fn budget_exhaustion_reason_on(connection: &Connection, mission: &MissionView) -> Result<Option<String>, Box<dyn Error>> {
        let Some(budget) = mission.budget.as_ref() else { return Ok(None); };
        budget_exhaustion_reason_with_budget(connection, mission, budget)
}

fn budget_exhaustion_reason_with_budget(
    connection: &Connection,
    mission: &MissionView,
    budget: &crate::mission::MissionBudget,
) -> Result<Option<String>, Box<dyn Error>> {
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
        let usage_json = effective_usage_payloads(connection, &mission.mission_id)?;
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

/// Corrections are append-only. Budget checks and UI totals use the newest
/// measurement per run, so a reconciliation is never counted twice.
fn effective_usage_payloads(connection: &Connection, mission_id: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let mut statement = connection.prepare("SELECT sequence,payload_json FROM mission_events
        WHERE mission_id=?1 AND event_type IN ('run_usage_recorded','run_usage_reconciled') ORDER BY sequence ASC")?;
    let rows = statement.query_map(params![mission_id], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?
        .collect::<Result<Vec<_>,_>>()?;
    let mut latest = std::collections::BTreeMap::new();
    for (sequence, raw) in rows {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let id = value.get("run_id").and_then(serde_json::Value::as_str).map(str::to_string)
            .unwrap_or_else(|| format!("legacy-event-{sequence}"));
        latest.insert(id, raw);
    }
    Ok(latest.into_values().collect())
}

fn usage_from_events(
    connection: &Connection,
    mission_id: &str,
) -> Result<Option<crate::agent_core_contracts::LocalRunUsage>, Box<dyn Error>> {
    let rows = effective_usage_payloads(connection, mission_id)?;
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

fn runtime_blocked_on(connection: &rusqlite::Connection, mission: &MissionView) -> Result<bool, Box<dyn Error>> {
    let latest: Option<(String, String)> = connection.query_row(
        "SELECT event_type,payload_json FROM mission_events WHERE mission_id=?1 AND event_type IN ('runtime_workspace_blocked','runtime_workspace_resolved') ORDER BY sequence DESC LIMIT 1",
        params![mission.mission_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    match latest {
        Some((event, payload)) if event == "runtime_workspace_blocked" => {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            Ok(payload["plan_sha256"] == mission.plan_sha256)
        },
        _ => Ok(false),
    }
}

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
                permission_mode: String::new(), root: "F:/repo".to_string(),
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

    struct ExpertTestHome(Option<std::ffi::OsString>);
    impl ExpertTestHome {
        fn new(root: &std::path::Path) -> Self {
            let previous = std::env::var_os("HIMIND_AGENT_HOME");
            std::env::set_var("HIMIND_AGENT_HOME", root);
            Self(previous)
        }
    }
    impl Drop for ExpertTestHome {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(previous) => std::env::set_var("HIMIND_AGENT_HOME", previous),
                None => std::env::remove_var("HIMIND_AGENT_HOME"),
            }
        }
    }

    fn assignment_fixture(name: &str) -> (MissionStore, crate::workflow::WorkflowStore, crate::mission::MissionWorkflowPin, crate::mission::MissionAssignmentInput, ExpertTestHome) {
        let store = store(name);
        let home = ExpertTestHome::new(store.path.parent().unwrap());
        let expert = crate::expert::save(serde_json::from_value(serde_json::json!({
            "id":"com.himind.expert.assignment-test", "name":"测试专家", "version":"1.0.0",
            "instructions":"ASSIGNMENT_METHOD: inspect real evidence before modifying files"
        })).unwrap()).unwrap();
        let expert_pin = crate::mission::MissionExpertPin {
            expert_id:expert.id, expert_version:expert.version, expert_digest:expert.digest,
        };
        let workflow_store = crate::workflow::WorkflowStore::new(store.path.parent().unwrap().join("workflows"));
        let (id, version, digest) = handoff_package(&workflow_store);
        let pin = crate::mission::MissionWorkflowPin {
            workflow_id:id, workflow_version:version, workflow_sha256:digest, entrypoint:String::new(),
        };
        let (package, _) = workflow_store.load_pinned_version(&pin.workflow_id, &pin.workflow_version, &pin.workflow_sha256).unwrap();
        let input = crate::mission::MissionAssignmentInput {
            roles: package.roles.keys().map(|role_id| crate::mission::MissionRoleAssignment {
                expert:Some(expert_pin.clone()), role_id:role_id.clone(), label:role_id.clone(),
                executor:"himind.builtin".into(), service_id:"service".into(), model:"himind:service:model".into(),
            }).collect(),
            task_roles: ["fix", "verify"].iter().map(|task_id| crate::mission::MissionTaskRoleMapping {
                task_id:task_id.to_string(), role_id:"ui.implementer.shared".into(),
            }).collect(),
            applied_by:"local-user".into(), recommended:true,
        };
        (store, workflow_store, pin, input, home)
    }

    #[test]
    fn assigned_roles_are_required_before_planning_and_override_imported_bindings() {
        let _lock = crate::store::paths::test_env_lock();
        let (store, workflows, pin, assignment, _home) = assignment_fixture("assignment");
        let mission = store.create_handoff_with_store(&imported_handoff(&pin), &workflows).unwrap();
        assert!(store.propose_plan(&mission.mission_id, &proposal()).is_err());
        let mut missing_expert = assignment.clone(); missing_expert.roles[0].expert = None;
        assert!(store.assign_roles_with_store(&mission.mission_id, &missing_expert, &workflows).is_err());
        let mut missing_slot = assignment.clone(); missing_slot.roles.pop();
        assert!(store.assign_roles_with_store(&mission.mission_id, &missing_slot, &workflows).is_err());
        let assigned = store.assign_roles_with_store(&mission.mission_id, &assignment, &workflows).unwrap();
        assert_eq!(assigned.status, MissionStatus::AwaitingConfirmation);
        assert_eq!(assigned.proposed_tasks[0].execution.service_id, "service");
        let expert = assigned.proposed_tasks[0].execution.expert.as_ref().unwrap();
        assert!(crate::mission::expert_instructions(expert).unwrap().contains("ASSIGNMENT_METHOD"));
        let reopened = MissionStore::new(store.path.clone()).get(&mission.mission_id).unwrap();
        assert_eq!(reopened.proposed_tasks[0].execution.expert.as_ref(), Some(expert));
    }

    #[test]
    fn generated_plan_reassignment_rehashes_and_follow_up_keeps_selected_expert() {
        let _lock = crate::store::paths::test_env_lock();
        let (store, workflows, pin, mut assignment, _home) = assignment_fixture("generated-assignment");
        let mut input = imported_handoff(&pin);
        input.plan_mode = MissionPlanMode::Generated; input.imported_plan = None;
        let mission = store.create_handoff_with_store(&input, &workflows).unwrap();
        assignment.task_roles.clear();
        store.assign_roles_with_store(&mission.mission_id, &assignment, &workflows).unwrap();
        let mut plan = proposal();
        for task in &mut plan.tasks { task.execution.role_id = "ui.implementer.shared".into(); }
        let original = store.propose_plan(&mission.mission_id, &plan).unwrap();
        for role in &mut assignment.roles { role.model = "himind:service:changed-model".into(); }
        assignment.task_roles = plan.tasks.iter().map(|task| crate::mission::MissionTaskRoleMapping {
            task_id:task.id.clone(), role_id:task.execution.role_id.clone(),
        }).collect();
        let reassigned = store.assign_roles_with_store(&mission.mission_id, &assignment, &workflows).unwrap();
        assert_eq!(reassigned.status, MissionStatus::AwaitingConfirmation);
        assert!(reassigned.confirmation.is_none());
        assert_ne!(original.plan_sha256, reassigned.plan_sha256);
        assert!(reassigned.plan_revision > original.plan_revision);
        assert_eq!(reassigned.proposed_tasks[0].execution.model, "himind:service:changed-model");
        let expected = reassigned.proposed_tasks[0].execution.clone();
        store.confirm_plan(&mission.mission_id, &reassigned.plan_sha256, &["fix".into()], &["frontend/src".into()], &[], &[], "test").unwrap();
        store.begin_task_attempt(&mission.mission_id, "fix", "failed-attempt").unwrap();
        store.complete_task_attempt(&mission.mission_id, "fix", "failed-attempt", false, &[], "failed").unwrap();
        let mut repair = plan.tasks[0].clone();
        repair.id = "repair".into(); repair.repair_of = "fix".into();
        repair.execution.role_id = "invented-role".into(); repair.execution.expert = None;
        repair.execution.model.clear(); repair.execution.service_id.clear();
        let repaired = store.add_follow_up_tasks(&mission.mission_id, &[repair], "repair").unwrap();
        assert_eq!(repaired.tasks.iter().find(|task| task.spec.id == "repair").unwrap().spec.execution, expected);
    }

    fn create_input() -> MissionCreateInput {
        MissionCreateInput {
            title: "修复登录页焦点".to_string(),
            objective: "完成可验证的 UI 修复".to_string(),
            workspace: MissionWorkspace {
                permission_mode: String::new(), root: "F:/repo".to_string(),
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
            execution: ExecutionBinding { expert: None,
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
        follow_up.repair_of = "fix".to_string();
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
        follow_up.repair_of = "fix".to_string();
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
    fn runtime_full_access_requires_plan_confirmation_and_persists_recovery_choice() {
        let store = store("runtime-full-access");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        let full = store.set_permission_mode(&created.mission_id,&planned.plan_sha256,"full_access").unwrap();
        assert_ne!(full.plan_sha256,planned.plan_sha256);
        assert!(store.confirm_plan(&created.mission_id,&planned.plan_sha256,&["fix".into()],&["frontend/src".into()],&[],&[],"user").is_err());
        assert_eq!(full.status,MissionStatus::AwaitingConfirmation);
        let active = store.confirm_plan(&created.mission_id,&full.plan_sha256,&["fix".into()],&["frontend/src".into()],&[],&[],"user").unwrap();
        assert_eq!(store.runtime_mode(&created.mission_id,&full.plan_sha256).unwrap(),"full_access");
        assert!(store.set_permission_mode(&created.mission_id,&full.plan_sha256,"workspace_write").is_err());
        assert_eq!(active.workspace.permission_mode,"full_access");
        store.suspend_runtime(&created.mission_id,"fix",&serde_json::json!({})).unwrap();
        store.resolve_runtime_block(&created.mission_id,&full.plan_sha256,"full_access",&serde_json::json!({"user_choice":true})).unwrap();
        let reopened=MissionStore::new(store.path.clone());
        assert_eq!(reopened.runtime_mode(&created.mission_id,&full.plan_sha256).unwrap(),"full_access");
        assert_ne!(reopened.runtime_mode(&created.mission_id,"stale-plan").unwrap(),"full_access");
    }

    #[test]
    fn runtime_recovery_preserves_ready_task_and_is_bound_to_current_plan() {
        let store = store("runtime-recovery");
        let created = confirmed_single_task(&store, "runtime");
        let before = store.get(&created.mission_id).unwrap();
        let blocked = store.suspend_runtime(&created.mission_id, "fix", &serde_json::json!({"code":"runtime_workspace_blocked"})).unwrap();
        assert_eq!(blocked.status, MissionStatus::Waiting);
        assert_eq!(blocked.tasks, before.tasks);
        assert_eq!(blocked.usage, before.usage);
        assert!(blocked.tasks[0].attempt_run_id.is_empty());
        assert!(store.control(&created.mission_id, "resume").is_err());
        assert!(store.resolve_runtime_block(&created.mission_id, "stale", "files_only", &serde_json::json!({})).is_err());
        let resumed = store.resolve_runtime_block(&created.mission_id, &before.plan_sha256, "files_only", &serde_json::json!({})).unwrap();
        assert_eq!(resumed.status, MissionStatus::Active);
        assert_eq!(resumed.tasks, before.tasks);
        assert_eq!(resumed.confirmation, before.confirmation);
        assert!(!store.runtime_blocked(&created.mission_id).unwrap());
        let reopened = MissionStore::new(store.path.clone());
        assert!(reopened.runtime_files_only(&created.mission_id, &before.plan_sha256).unwrap());
        assert!(!reopened.runtime_files_only(&created.mission_id, "another-plan").unwrap());
        assert!(reopened.resolve_runtime_block(&created.mission_id, &before.plan_sha256, "default", &serde_json::json!({})).is_err());
    }

    #[test]
    fn runtime_recovery_respects_cancellation_and_expired_budget() {
        let store = store("runtime-recovery-budget");
        let created = confirmed_single_task(&store, "runtime");
        let before = store.get(&created.mission_id).unwrap();
        store.suspend_runtime(&created.mission_id, "fix", &serde_json::json!({})).unwrap();
        let mut expired = before.confirmation.clone().unwrap();
        expired.confirmed_at = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        store.connection().unwrap().execute("UPDATE mission_runs SET confirmation_json=?2 WHERE mission_id=?1",
            params![created.mission_id, serde_json::to_string(&expired).unwrap()]).unwrap();
        let recovered = store.resolve_runtime_block(&created.mission_id, &before.plan_sha256, "files_only", &serde_json::json!({})).unwrap();
        assert_eq!(recovered.status, MissionStatus::Waiting);
        assert!(!store.runtime_blocked(&created.mission_id).unwrap());
        assert!(store.control(&created.mission_id, "resume").is_err());
        let resumed = store.increase_budget(&created.mission_id, &MissionBudgetIncrease {
            additional_repair_attempts: 0,
            request_id:"runtime-extra-time".into(), expected_plan_sha256:before.plan_sha256.clone(),
            additional_minutes: 4320, additional_rounds:0, additional_tokens:None, unlimited_tokens:false, additional_cost:None,
        }, "test-user").unwrap();
        assert_eq!(resumed.status, MissionStatus::Active);
        assert!(store.runtime_files_only(&created.mission_id, &before.plan_sha256).unwrap());
        store.suspend_runtime(&created.mission_id, "fix", &serde_json::json!({})).unwrap();
        store.control(&created.mission_id, "cancel").unwrap();
        assert!(store.resolve_runtime_block(&created.mission_id, &before.plan_sha256, "default", &serde_json::json!({})).is_err());
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
        let execution = crate::mission::ExecutionBinding { expert: None,
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
    fn recheck_preserves_failures_requires_baseline_and_is_idempotent() {
        let store = store("recheck-baseline");
        let root = std::env::temp_dir().join(format!("mission-recheck-{}", rand::random::<u64>()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("readonly.txt"), "protected").unwrap();
        let created = store.create(&MissionCreateInput {
            title: "recheck".into(), objective: String::new(),
            workspace: MissionWorkspace { permission_mode: String::new(), root: root.to_string_lossy().into(), read_scope: vec![".".into()], write_scope: vec!["src".into()] },
            acceptance_criteria: vec!["protected files unchanged".into()],
        }).unwrap();
        let mut plan = proposal();
        plan.tasks = vec![task("fix")];
        plan.tasks[0].read_paths = vec!["readonly.txt".into(), "src".into()];
        plan.tasks[0].write_paths = vec!["src".into()];
        plan.budget.max_repair_attempts = 0;
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256, &["fix".into()], &["src".into()], &[], &[], "user").unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "impl").unwrap();
        store.await_task_verification(&created.mission_id, "fix", "impl", "ready").unwrap();
        store.begin_verification_attempt(&created.mission_id, "fix", "verify1").unwrap();
        store.record_verification_result(&created.mission_id, "fix", "impl", "verify1", false, &["local-run:verify1".into()], "missing baseline").unwrap();
        let mut input = MissionVerificationRetry { request_id: "retry1".into(), expected_plan_sha256: planned.plan_sha256.clone(),
            expected_verification_run_id: "verify1".into(), source_baseline: None };
        assert!(store.retry_verification(&created.mission_id, "fix", &input).unwrap_err().to_string().contains("baseline"));
        let files = super::super::protected_files(&root, &plan.tasks[0]).unwrap();
        let mut source = super::super::MissionSourceBaseline {
            source_reference: "codex:pre-run-evidence".into(),
            captured_at: (Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(), artifact_sha256: "a".repeat(64),
            files: serde_json::from_value(files).unwrap(),
        };
        input.source_baseline = Some(source.clone());
        assert!(store.retry_verification(&created.mission_id, "fix", &input).unwrap_err().to_string().contains("predate"));
        source.captured_at = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        input.source_baseline = Some(source);
        let mut stale = input.clone(); stale.expected_plan_sha256 = "f".repeat(64);
        assert!(store.retry_verification(&created.mission_id, "fix", &stale).is_err());
        let mut wrong = input.clone(); wrong.source_baseline.as_mut().unwrap().files.insert("readonly.txt".into(), "0".repeat(64));
        assert!(store.retry_verification(&created.mission_id, "fix", &wrong).unwrap_err().to_string().contains("differ"));
        let retried = store.retry_verification(&created.mission_id, "fix", &input).unwrap();
        assert_eq!(retried.tasks[0].status, TaskStatus::AwaitingVerification);
        assert_eq!(retried.tasks[0].attempt_run_id, "impl");
        assert_eq!(retried.plan_sha256, planned.plan_sha256);
        assert_eq!(retried.budget.unwrap().max_repair_attempts, 0);
        assert!(store.workspace_claim(&created.workspace.root).unwrap().is_some());
        store.retry_verification(&created.mission_id, "fix", &input).unwrap();
        let events = store.snapshot(&created.mission_id, 0).unwrap().events;
        assert_eq!(events.iter().filter(|event| event.event_type == "verification_retry_requested").count(), 1);
        assert!(events.iter().any(|event| event.event_type == "verification_failed" && event.payload["verification_run_id"] == "verify1"));
        wrong.request_id = input.request_id.clone();
        assert!(store.retry_verification(&created.mission_id, "fix", &wrong).unwrap_err().to_string().contains("different values"));
        store.begin_verification_attempt(&created.mission_id, "fix", "verify2").unwrap();
        assert!(store.record_verification_result(&created.mission_id, "fix", "impl", "verify1", true, &["old".into()], "").is_err());
        store.record_verification_result(&created.mission_id, "fix", "impl", "verify2", false, &["local-run:verify2".into()], "review issue").unwrap();
        input.request_id = "retry2".into(); input.expected_verification_run_id = "verify2".into();
        assert!(store.retry_verification(&created.mission_id, "fix", &input).unwrap_err().to_string().contains("replaced"));
        input.source_baseline = None;
        fs::write(root.join("readonly.txt"), "tampered").unwrap();
        assert!(store.retry_verification(&created.mission_id, "fix", &input).unwrap_err().to_string().contains("differ"));
        fs::write(root.join("readonly.txt"), "protected").unwrap();
        store.retry_verification(&created.mission_id, "fix", &input).unwrap();
        let _ = fs::remove_dir_all(root);
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
    fn exhausted_token_budget_can_be_extended_and_resumed_idempotently() {
        let store = store("token-budget-extension");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_total_tokens = Some(100);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        let confirmed = store.confirm_plan(
            &created.mission_id, &planned.plan_sha256, &["fix".into()],
            &["frontend/src".into()], &[], &[], "test-user",
        ).unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-over-budget").unwrap();
        store.record_run_usage(&created.mission_id, "fix", "run-over-budget",
            &crate::agent_core_contracts::LocalRunUsage {
                input_tokens: 90, output_tokens: 20, estimated_cost: 0.0,
                currency: String::new(), billing_owner: "local".into(),
            }).unwrap();
        store.complete_task_attempt(&created.mission_id, "fix", "run-over-budget", false, &[], "budget test").unwrap();
        assert!(store.next_ready_task(&created.mission_id).unwrap().is_none());
        let waiting = store.get(&created.mission_id).unwrap();
        assert_eq!(waiting.status, MissionStatus::Waiting);
        let input = MissionBudgetIncrease {
            additional_repair_attempts: 0,
            request_id: "budget-extension-once".into(),
            expected_plan_sha256: planned.plan_sha256.clone(),
            additional_rounds: 2,
            additional_minutes: 30,
            additional_tokens: None,
            unlimited_tokens: true,
            additional_cost: None,
        };

        // Progress and diagnostic events do not replace the effective budget state.
        store.append_event(&created.mission_id, "task_progress", "fix", "late progress", serde_json::json!({})).unwrap();
        let resumed = store.increase_budget(&created.mission_id, &input, "test-user").unwrap();
        assert_eq!(resumed.status, MissionStatus::Active);
        assert_eq!(resumed.plan_sha256, confirmed.plan_sha256);
        assert_eq!(resumed.confirmation, confirmed.confirmation);
        assert_eq!(resumed.budget.as_ref().unwrap().max_total_tokens, None);
        assert_eq!(resumed.budget.as_ref().unwrap().max_rounds, plan.budget.max_rounds + 2);
        assert_eq!(resumed.budget.as_ref().unwrap().max_minutes, plan.budget.max_minutes + 30);

        let repeated = store.increase_budget(&created.mission_id, &input, "test-user").unwrap();
        assert_eq!(repeated.budget, resumed.budget);
        assert_eq!(store.snapshot(&created.mission_id, 0).unwrap().events
            .iter().filter(|event| event.event_type == "budget_extended").count(), 1);
        let mut conflicting_replay = input.clone();
        conflicting_replay.additional_minutes += 1;
        assert!(store.increase_budget(&created.mission_id, &conflicting_replay, "test-user")
            .unwrap_err().to_string().contains("different values"));
    }

    #[test]
    fn budget_extension_allows_explicit_proactive_increase_but_rejects_stale_plan() {
        let store = store("budget-extension-guards");
        let created = confirmed_single_task(&store, "budget-extension-guards");
        let confirmed = store.get(&created.mission_id).unwrap();
        let paused = store.control(&created.mission_id, "pause").unwrap();
        let input = MissionBudgetIncrease {
            additional_repair_attempts: 0,
            request_id: "manual-pause-extension".into(),
            expected_plan_sha256: confirmed.plan_sha256.clone(),
            additional_rounds: 1,
            additional_minutes: 0,
            additional_tokens: None,
            unlimited_tokens: false,
            additional_cost: None,
        };
        assert_eq!(paused.status, MissionStatus::Waiting);
        let increased = store.increase_budget(&created.mission_id, &input, "test-user").unwrap();
        assert_eq!(increased.status, MissionStatus::Active);
        assert_eq!(increased.budget.as_ref().unwrap().max_rounds, confirmed.budget.as_ref().unwrap().max_rounds + 1);
        assert_eq!(increased.confirmation, confirmed.confirmation);
        assert_eq!(store.increase_budget(&created.mission_id, &input, "test-user").unwrap().budget, increased.budget);
        let exhausted = store.create(&create_input()).unwrap();
        let mut plan = proposal(); plan.tasks.truncate(1); plan.budget.max_total_tokens = Some(1);
        let planned = store.propose_plan(&exhausted.mission_id, &plan).unwrap();
        store.confirm_plan(&exhausted.mission_id, &planned.plan_sha256, &["fix".into()],
            &["frontend/src".into()], &[], &[], "test-user").unwrap();
        store.begin_task_attempt(&exhausted.mission_id, "fix", "run-stale-plan").unwrap();
        store.record_run_usage(&exhausted.mission_id, "fix", "run-stale-plan",
            &crate::agent_core_contracts::LocalRunUsage {
                input_tokens: 2, output_tokens: 0, estimated_cost: 0.0,
                currency: String::new(), billing_owner: "local".into(),
            }).unwrap();
        store.complete_task_attempt(&exhausted.mission_id, "fix", "run-stale-plan", false, &[], "budget test").unwrap();
        store.next_ready_task(&exhausted.mission_id).unwrap();
        let stale = MissionBudgetIncrease {
            additional_repair_attempts: 0,
            request_id: "stale-plan-extension".into(),
            expected_plan_sha256: "0".repeat(64),
            additional_rounds: 1, additional_minutes: 0,
            additional_tokens: Some(1), unlimited_tokens: false, additional_cost: None,
        };
        assert!(store.increase_budget(&exhausted.mission_id, &stale, "test-user")
            .unwrap_err().to_string().contains("plan changed"));
    }

    #[test]
    fn proactive_budget_extension_preserves_runtime_blocks_and_rejects_inflight_work() {
        let store = store("proactive-budget-guards");
        let created = confirmed_single_task(&store, "proactive-budget-guards");
        let confirmed = store.get(&created.mission_id).unwrap();
        let input = MissionBudgetIncrease {
            request_id: "proactive-guards".into(), expected_plan_sha256: confirmed.plan_sha256.clone(),
            additional_rounds: 1, additional_minutes: 0, additional_repair_attempts: 0,
            additional_tokens: None, unlimited_tokens: false, additional_cost: None,
        };
        store.begin_task_attempt(&created.mission_id, "fix", "run-inflight").unwrap();
        store.control(&created.mission_id, "pause").unwrap();
        assert!(store.increase_budget(&created.mission_id, &input, "test-user").unwrap_err().to_string().contains("running task"));
        store.await_task_verification(&created.mission_id, "fix", "run-inflight", "ready").unwrap();
        assert!(store.increase_budget(&created.mission_id, &input, "test-user").unwrap_err().to_string().contains("pending verification"));

        let blocked = confirmed_single_task(&store, "proactive-blocked");
        let blocked = store.get(&blocked.mission_id).unwrap();
        let mut blocked_input = input;
        blocked_input.expected_plan_sha256 = blocked.plan_sha256.clone();
        store.suspend_runtime(&blocked.mission_id, "fix", &serde_json::json!({"code":"runtime_workspace_blocked"})).unwrap();
        store.append_event(&blocked.mission_id, "task_progress", "fix", "diagnostic", serde_json::json!({})).unwrap();
        let mut unchanged = blocked_input.clone(); unchanged.additional_rounds = 0;
        assert!(store.increase_budget(&blocked.mission_id, &unchanged, "test-user").unwrap_err().to_string().contains("at least one"));
        let increased = store.increase_budget(&blocked.mission_id, &blocked_input, "test-user").unwrap();
        assert_eq!(increased.status, MissionStatus::Waiting);
        assert_eq!(increased.budget.unwrap().max_rounds, blocked.budget.unwrap().max_rounds + 1);
        assert!(store.control(&blocked.mission_id, "resume").is_err());
    }

    #[test]
    fn resuming_manually_paused_over_budget_mission_records_exhaustion_and_allows_extension() {
        let store = store("paused-over-budget-extension");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.budget.max_total_tokens = Some(100);
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(
            &created.mission_id, &planned.plan_sha256, &["fix".into()],
            &["frontend/src".into()], &[], &[], "test-user",
        ).unwrap();
        store.begin_task_attempt(&created.mission_id, "fix", "run-paused-over-budget").unwrap();
        assert_eq!(store.control(&created.mission_id, "pause").unwrap().status, MissionStatus::Waiting);
        store.record_run_usage(&created.mission_id, "fix", "run-paused-over-budget",
            &crate::agent_core_contracts::LocalRunUsage {
                input_tokens: 90, output_tokens: 20, estimated_cost: 0.0,
                currency: String::new(), billing_owner: "local".into(),
            }).unwrap();
        store.complete_task_attempt(
            &created.mission_id, "fix", "run-paused-over-budget", false, &[], "paused run completed",
        ).unwrap();

        let reason = store.control(&created.mission_id, "resume").unwrap_err().to_string();
        assert!(reason.contains("Token"), "{reason}");
        assert_eq!(store.get(&created.mission_id).unwrap().status, MissionStatus::Waiting);
        let snapshot = store.snapshot(&created.mission_id, 0).unwrap();
        assert_eq!(snapshot.events.last().unwrap().event_type, "budget_exhausted");
        assert_eq!(snapshot.events.iter().filter(|event| event.event_type == "budget_exhausted").count(), 1);

        assert!(store.control(&created.mission_id, "resume").is_err());
        assert_eq!(store.snapshot(&created.mission_id, 0).unwrap().events
            .iter().filter(|event| event.event_type == "budget_exhausted").count(), 1);
        let resumed = store.increase_budget(&created.mission_id, &MissionBudgetIncrease {
            additional_repair_attempts: 0,
            request_id: "resume-paused-over-budget".into(),
            expected_plan_sha256: planned.plan_sha256,
            additional_rounds: 1,
            additional_minutes: 30,
            additional_tokens: None,
            unlimited_tokens: true,
            additional_cost: None,
        }, "test-user").unwrap();
        assert_eq!(resumed.status, MissionStatus::Active);
        assert_eq!(resumed.budget.unwrap().max_total_tokens, None);
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
    fn unknown_usage_can_be_reconciled_without_losing_history_or_double_counting() {
        let store = store("usage-reconciliation");
        let created = confirmed_single_task(&store, "usage-reconciliation");
        store.begin_task_attempt(&created.mission_id, "fix", "failed-run").unwrap();
        let unknown = crate::agent_core_contracts::LocalRunUsage { input_tokens:0, output_tokens:0,
            estimated_cost:0.0, currency:String::new(), billing_owner:"unreported".into() };
        store.record_run_usage(&created.mission_id,"fix","failed-run",&unknown).unwrap();
        let measured = crate::agent_core_contracts::LocalRunUsage { input_tokens:1200, output_tokens:80,
            billing_owner:"local".into(), ..unknown };
        for _ in 0..2 { store.record_run_usage(&created.mission_id,"fix","failed-run",&measured).unwrap(); }
        let actual = store.get(&created.mission_id).unwrap().usage.unwrap();
        assert_eq!(actual.input_tokens,1200);
        assert_eq!(actual.output_tokens,80);
        let events = store.snapshot(&created.mission_id,0).unwrap().events;
        assert_eq!(events.iter().filter(|event|event.event_type=="run_usage_recorded").count(),1);
        assert_eq!(events.iter().filter(|event|event.event_type=="run_usage_reconciled").count(),1);
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
    fn review_rework_covers_historical_repairs_but_requires_downstream_revision_order() {
        let store = store("review-history-order");
        let mut mission = store.create(&create_input()).unwrap();
        let mut ia = task("ia"); ia.depends_on.clear();
        let mut synthesis = task("synthesis"); synthesis.depends_on = vec!["ia".into()];
        let mut historical_synthesis = synthesis.clone();
        historical_synthesis.id = "previous-synthesis-repair".into(); historical_synthesis.repair_of = "synthesis".into();
        let mut review = task("review"); review.depends_on = vec!["synthesis".into()];
        let mut failed_review = review.clone(); failed_review.id = "failed-review-retry".into();
        failed_review.repair_of = "review".into(); failed_review.depends_on.clear();
        mission.tasks = [ia.clone(), synthesis.clone(), historical_synthesis, review, failed_review.clone()].into_iter().map(|spec| {
            let status = if spec.id.contains("review") { TaskStatus::Failed } else { TaskStatus::Completed };
            MissionTaskView {spec,status,attempt_run_id:String::new(),verification_run_id:String::new(),verification_passed:None,evidence_refs:vec![]}
        }).collect();
        ia.id = "ia-revision".into(); ia.repair_of = "ia".into();
        synthesis.id = "synthesis-revision".into(); synthesis.repair_of = "synthesis".into();
        synthesis.depends_on = vec![ia.id.clone()];
        failed_review.id = "review-revision".into(); failed_review.repair_of = "failed-review-retry".into();
        failed_review.depends_on = vec![synthesis.id.clone(),ia.id.clone()];
        assert_eq!(review_rework_targets(&mission, &[ia.clone(),synthesis.clone(),failed_review.clone()]).unwrap(),
            BTreeSet::from(["ia".to_string(),"synthesis".to_string()]));
        let mut unordered = synthesis.clone(); unordered.depends_on.clear();
        assert!(review_rework_targets(&mission, &[ia.clone(),unordered,failed_review.clone()])
            .unwrap_err().to_string().contains("must depend on its upstream revision"));
        failed_review.depends_on = vec![ia.id.clone()];
        assert!(review_rework_targets(&mission, &[ia,failed_review])
            .unwrap_err().to_string().contains("revalidate completed downstream"));
    }

    #[test]
    fn downstream_review_reopens_producer_then_requires_separate_reverification() {
        let store = store("review-feedback");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.budget.max_rounds = 40;
        plan.budget.max_repair_attempts = 2;
        plan.tasks[0].write_paths = vec!["frontend/src/design.md".into()];
        plan.tasks[1].write_paths = vec!["frontend/src/acceptance.json".into()];
        plan.tasks[1].execution.role_id = "independent-reviewer".into();
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256, &["fix".into(),"verify".into()],
            &["frontend/src".into()], &[], &[], "test-user").unwrap();
        let verify = |id: &str, run: &str| {
            let review = format!("{run}-review");
            store.begin_task_attempt(&created.mission_id, id, run).unwrap();
            store.await_task_verification(&created.mission_id, id, run, "review").unwrap();
            store.begin_verification_attempt(&created.mission_id, id, &review).unwrap();
            store.append_event(&created.mission_id, "host_verification_checked", id, "test host evidence", serde_json::json!({
                "passed":true,"plan_sha256":planned.plan_sha256,"implementation_run_id":run,"verification_run_id":review,
            })).unwrap();
            store.record_verification_result(&created.mission_id,id,run,&review,true,&[format!("local-run:{review}")],"").unwrap()
        };
        verify("fix", "producer-v1");
        store.begin_task_attempt(&created.mission_id,"verify","review-v1").unwrap();
        store.complete_task_attempt(&created.mission_id,"verify","review-v1",false,&[],"upstream design conflict").unwrap();
        let mut first_retry = plan.tasks[1].clone();
        first_retry.id = "review-retry".into(); first_retry.repair_of = "verify".into(); first_retry.depends_on.clear();
        store.add_follow_up_tasks(&created.mission_id,&[first_retry.clone()],"review retry").unwrap();
        store.begin_task_attempt(&created.mission_id,"review-retry","review-v2").unwrap();
        store.complete_task_attempt(&created.mission_id,"review-retry","review-v2",false,&[],"confirmed upstream issue").unwrap();
        store.control(&created.mission_id, "pause").unwrap();
        store.increase_budget(&created.mission_id, &MissionBudgetIncrease {
            request_id: "review-feedback-budget".into(), expected_plan_sha256: planned.plan_sha256.clone(),
            additional_rounds: 0, additional_minutes: 0, additional_repair_attempts: 6,
            additional_tokens: None, unlimited_tokens: false, additional_cost: None,
        }, "test-user").unwrap();
        let mut correction = plan.tasks[0].clone();
        correction.id = "producer-revision".into(); correction.repair_of = "fix".into();
        let mut final_review = first_retry;
        final_review.id = "review-revision".into(); final_review.repair_of = "review-retry".into();
        final_review.depends_on = vec![correction.id.clone()];
        assert!(store.add_follow_up_tasks(&created.mission_id,&[correction.clone()],"unreviewed revision").is_err());
        let mut unordered = final_review.clone(); unordered.depends_on.clear();
        assert!(store.add_follow_up_tasks(&created.mission_id,&[correction.clone(),unordered],"unordered review").is_err());
        let mut widened = correction.clone(); widened.write_paths.push("frontend/src/acceptance.json".into());
        assert!(store.add_follow_up_tasks(&created.mission_id,&[widened,final_review.clone()],"widened writer").unwrap_err().to_string().contains("repair target scope"));
        let revised = store.add_follow_up_tasks(&created.mission_id,&[correction,final_review],"review found real defect").unwrap();
        assert_eq!(revised.tasks.iter().find(|t| t.spec.id=="fix").unwrap().status,TaskStatus::Failed);
        assert_eq!(revised.tasks.iter().find(|t| t.spec.id=="review-revision").unwrap().status,TaskStatus::Blocked);
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id,"producer-revision");
        let reopened = MissionStore::new(store.path.clone());
        assert!(reopened.snapshot(&created.mission_id,0).unwrap().events.iter().any(|e|
            e.event_type=="task_reopened_for_review" && e.payload["previous_implementation_run_id"]=="producer-v1"));
        verify("producer-revision","producer-v2");
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id,"review-revision");
        let completed = verify("review-revision","review-v3");
        assert_eq!(completed.status,MissionStatus::Completed);
        assert!(completed.tasks.iter().all(|t| t.status==TaskStatus::Completed));
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
