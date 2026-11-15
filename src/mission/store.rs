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
    MissionBudgetIncrease, MissionFeedbackInput, MissionVerificationRetry, MissionEvent, MissionHandoff, MissionHandoffInput, MissionPlanMode, MissionPlanProposal,
    MissionStatus, MissionTaskSpec, MissionTaskView, MissionView, MissionWorkflowPin, TaskStatus,
};

/// Unlike validate_task_paths, this inspects the actual filesystem. Only fresh
/// explicit read extensions use it; exact replay must never recheck existence.
fn follow_up_read_metadata(path: &std::path::Path) -> Result<fs::Metadata, Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if linked { return Err("read_scope_extension rejects symlinks and reparse points in every ancestor".into()); }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("read_scope_extension requires an existing regular file or directory".into());
    }
    Ok(metadata)
}

fn validate_follow_up_read_extension_paths(root: &str, paths: &[String]) -> Result<(), Box<dyn Error>> {
    let root = std::path::Path::new(root);
    // Include the configured root and its own ancestors: a linked workspace
    // alias must not conceal an extension outside its reviewed physical root.
    for ancestor in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        if !follow_up_read_metadata(ancestor)?.is_dir() {
            return Err("read_scope_extension workspace ancestors must be directories".into());
        }
    }
    let real_root = fs::canonicalize(root)?;
    for relative in paths {
        // Lexical validation is performed by the typed request, before DB open.
        let mut current = root.to_path_buf();
        let parts = relative.split('/').collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            current.push(part);
            let metadata = follow_up_read_metadata(&current)?;
            if index + 1 < parts.len() && !metadata.is_dir() {
                return Err("read_scope_extension intermediate ancestors must be directories".into());
            }
            let real = fs::canonicalize(&current)?;
            if real == real_root || !real.starts_with(&real_root) {
                return Err("read_scope_extension canonical path must remain inside the real workspace root".into());
            }
        }
    }
    Ok(())
}

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
const MAX_FEEDBACK_CONTEXT_BYTES: usize = 16_384;
const FEEDBACK_SUMMARY: &str = "已提交审查意见，供后续输入参考；不改变任务状态、绑定或授权";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingRepairCancellation {
    pub schema_version: String,
    pub mission_id: String,
    pub expected_plan_sha256: String,
    pub task_id: String,
    pub request_id: String,
    pub reason: String,
}

impl PendingRepairCancellation {
    fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != "mission.task.cancel_pending.v1" {
            return Err("unsupported pending repair cancellation version".into());
        }
        require_id("mission_id", &self.mission_id)?;
        require_id("task_id", &self.task_id)?;
        require_id("request_id", &self.request_id)?;
        require_sha256(&self.expected_plan_sha256)?;
        require_text("reason", &self.reason, 2_000)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MissionSnapshot {
    pub mission: MissionView,
    pub events: Vec<MissionEvent>,
    pub cursor: u64,
    /// Delta is incomplete or caller cursor is ahead; replace with mission snapshot.
    pub requires_snapshot: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MissionStore {
    path: PathBuf,
}

/// Prove that a reviewed delivery's first revision was withdrawn before it
/// started. Historical successful repairs do not make that delivery a leaf;
/// the replacement must retain every original rework/review constraint.
fn withdrawn_review_roots(
    connection: &Connection, mission: &MissionView, tasks: &[MissionTaskSpec],
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut roots = BTreeSet::new();
    for revision in tasks {
        let Some(parent) = mission.tasks.iter().find(|t| t.spec.id == revision.repair_of
            && t.spec.repair_of.is_empty() && t.status == TaskStatus::Failed) else { continue; };
        let children = mission.tasks.iter().filter(|t| t.spec.repair_of == parent.spec.id).collect::<Vec<_>>();
        if children.is_empty() { continue; }
        let receipt: Option<(i64, String)> = connection.query_row(
            "SELECT sequence,payload_json FROM mission_events WHERE mission_id=?1 AND task_id=?2
             AND event_type='task_reopened_for_review' ORDER BY sequence DESC LIMIT 1",
            params![mission.mission_id,parent.spec.id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        let Some((sequence, raw)) = receipt else { continue; };
        let proof: serde_json::Value = serde_json::from_str(&raw)?;
        if proof["plan_sha256"] != mission.plan_sha256
            || proof["previous_implementation_run_id"] != parent.attempt_run_id
            || proof["previous_verification_run_id"] != parent.verification_run_id { continue; }
        let Some(ids) = proof["revision_task_ids"].as_array().filter(|ids| !ids.is_empty()) else { continue; };
        let mut valid = true;
        for id in ids {
            let Some(id) = id.as_str() else { valid=false; break; };
            if !children.iter().any(|t| t.spec.id == id && t.status == TaskStatus::Canceled) {
                valid=false; break;
            }
            let canceled: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM mission_events WHERE mission_id=?1 AND task_id=?2
                 AND event_type='pending_repair_canceled' AND sequence>?3
                 AND json_extract(payload_json,'$.cancel_request.mission_id')=?1
                 AND json_extract(payload_json,'$.cancel_request.task_id')=?2
                 AND json_extract(payload_json,'$.cancel_request.expected_plan_sha256')=?4)",
                params![mission.mission_id,id,sequence,mission.plan_sha256], |row| row.get(0))?;
            if !canceled { valid=false; break; }
        }
        if !valid { continue; }
        for child in &children {
            match child.status {
                TaskStatus::Canceled => (),
                TaskStatus::Completed => {
                    let last_success: Option<i64> = connection.query_row(
                        "SELECT MAX(sequence) FROM mission_events WHERE mission_id=?1 AND task_id=?2
                         AND event_type IN ('task_completed','task_repaired')",
                        params![mission.mission_id,child.spec.id], |row| row.get(0))?;
                    if !last_success.is_some_and(|success| success < sequence) { valid=false; break; }
                },
                _ => { valid=false; break; },
            }
        }
        if valid { roots.insert(parent.spec.id.clone()); }
    }
    if !roots.is_empty() {
        let claimed: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mission_workspace_claims WHERE mission_id=?1)",
            params![mission.mission_id], |row| row.get(0))?;
        if mission.status != MissionStatus::Waiting || claimed {
            return Err("pause and drain reviewed deliveries before replacing withdrawn revisions".into());
        }
    }
    Ok(roots)
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
        let mut raw_connection = self.connection()?;
        let connection = raw_connection.transaction()?;
        let mut statement = connection.prepare(
            "SELECT mission_id FROM mission_runs ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let ids = statement
        .query_map(params![limit.clamp(1, 500) as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| load_mission(&connection, &id)?.ok_or_else(|| "mission not found".into())).collect()
    }

    pub(crate) fn get(&self, mission_id: &str) -> Result<MissionView, Box<dyn Error>> {
        require_id("mission_id", mission_id)?;
        let mut connection = self.connection()?;
        let snapshot = connection.transaction()?;
        load_mission(&snapshot, mission_id)?
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
        let resuming_verification = action == "resume" && mission.status == MissionStatus::Waiting
            && mission.tasks.iter().any(|task| task.status == TaskStatus::AwaitingVerification);
        if resuming_verification && !owned_pending_verification_on(&transaction, &mission)? {
            return Err("pending verification requires the current confirmed plan and workspace claim before resume".into());
        }
        if action == "resume" && mission.status == MissionStatus::Waiting
            && !resuming_verification {
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
            return Err("完全访问模式需要选择 HiMind AI、Codex 或 ACP 执行器，请调整专家指派或改用工作区访问".into());
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
        Self::add_follow_up_tasks_tx(&transaction, &mission, tasks, reason, None)?;
        transaction.commit()?;
        self.get(mission_id)
    }

    /// Task ids are the idempotency key. Hash, receipt, state and insertion are
    /// checked under the same write reservation; no preflight get/write race.
    pub(crate) fn add_follow_up_tasks_for_mcp(
        &self,
        request: &super::follow_up::MissionFollowUpAdd,
    ) -> Result<MissionView, Box<dyn Error>> {
        request.validate()?;
        // This control path must not run schema migrations or legacy claim
        // repair before deciding an exact replay or a rejected request.
        // An existing confirmed Mission necessarily has an initialized DB.
        let mut connection = Connection::open_with_flags(
            &self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&tx, &request.mission_id)?.ok_or("mission not found")?;
        let confirmation = mission.confirmation.as_ref().ok_or("mission has no confirmed plan")?;
        if mission.plan_sha256 != request.expected_plan_sha256
            || confirmation.plan_sha256 != request.expected_plan_sha256 {
            return Err("follow-up expected plan does not match the current confirmed plan".into());
        }
        let tasks = request.tasks.iter().map(|task| {
            let parent = mission.tasks.iter().find(|parent| parent.spec.id == task.repair_of)
                .ok_or("follow-up must reference a repair target")?;
            Ok(task.bind(parent.spec.execution.clone()))
        }).collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let existing = tasks.iter().filter(|task| mission.tasks.iter().any(|current| current.spec.id == task.id)).count();
        if existing > 0 {
            if existing != tasks.len() { return Err("partial follow-up task id replay is not allowed".into()); }
            // The complete original request (including reason and batch order)
            // is stored in the very event written with these tasks. Query the
            // receipt directly, independent of event-list pagination limits.
            let receipt: Option<String> = tx.query_row(
                "SELECT payload_json FROM mission_events WHERE mission_id=?1
                 AND event_type='follow_up_tasks_added'
                 AND json_extract(payload_json,'$.follow_up_request.tasks[0].id')=?2
                 ORDER BY sequence DESC LIMIT 1",
                params![request.mission_id, tasks[0].id], |row| row.get(0),
            ).optional()?;
            let identical = if let Some(raw) = receipt {
                let payload: serde_json::Value = serde_json::from_str(&raw)?;
                let original: super::follow_up::MissionFollowUpAdd = serde_json::from_value(payload["follow_up_request"].clone())?;
                original == *request && tasks.iter().all(|task|
                    mission.tasks.iter().any(|current| current.spec == *task))
            } else { false };
            if !identical { return Err("conflicting follow-up task id replay".into()); }
            // Replays remain valid after task progress or budget exhaustion.
            // No readiness recomputation, event, timestamp or budget mutation.
            tx.commit()?;
            return Ok(mission);
        }
        let continued = withdrawn_review_roots(&tx, &mission, &tasks)?;
        // Validation only: these roots still require the same ordered rework
        // and independent review as a completed delivery. Their real Failed
        // state, run identities and historical reopening events stay intact.
        let mut review_mission = mission.clone();
        for root in &mut review_mission.tasks {
            if continued.contains(&root.spec.id) { root.status = TaskStatus::Completed; }
        }
        if !continued.is_empty() { review_rework_targets(&review_mission, &tasks)?; }
        // Failed targets must still be repair leaves. A completed producer may
        // be revised only through the shared downstream-review validation:
        // idle work, ordered revisions and a separate failed-review retry.
        let existing_specs = mission.tasks.iter().map(|task| task.spec.clone()).collect::<Vec<_>>();
        let mut added_reads = BTreeSet::new();
        for task in &tasks {
            let parent = review_mission.tasks.iter().find(|parent| parent.spec.id == task.repair_of)
                .ok_or("repair target missing")?;
            if parent.status == TaskStatus::Completed {
                // Dependencies name delivery roots, not historical repair
                // aliases. Reopen only roots, so no downstream is overlooked.
                if !parent.spec.repair_of.is_empty() {
                    return Err("completed rework must target the original delivery root".into());
                }
                let independent = tasks.iter().any(|retry| {
                    mission.tasks.iter().any(|review| review.spec.id == retry.repair_of
                        && review.status == TaskStatus::Failed
                        && review.spec.execution.role_id != parent.spec.execution.role_id
                        && matches!((&parent.spec.execution.expert, &review.spec.execution.expert),
                            (Some(producer), Some(reviewer)) if producer.expert_id != reviewer.expert_id)
                        && task_reaches(&existing_specs, &review.spec.id, &parent.spec.id, true))
                        && task_reaches(&tasks, &retry.id, &task.id, false)
                });
                if !independent {
                    return Err("completed rework requires a dependent failed review by a different assigned role and pinned expert".into());
                }
            }
            if parent.status != TaskStatus::Completed && (parent.status != TaskStatus::Failed
                || mission.tasks.iter().any(|child|
                    child.spec.repair_of == parent.spec.id && child.status != TaskStatus::Canceled)) {
                return Err("native follow-up target must be a failed repair leaf".into());
            }
            for path in task.read_paths.iter().filter(|p| !path_allowed(p, &parent.spec.read_paths)) {
                if request.read_scope_extension.is_none() {
                    return Err("native follow-up reads outside its repair target scope".into());
                }
                added_reads.insert(path.as_str());
            }
        }
        if let Some(extension) = &request.read_scope_extension {
            // Loaded after BEGIN IMMEDIATE, not from a preflight snapshot.
            // Replay above keeps its original confirmation guards, but ignores
            // later activity, budget exhaustion and disappearing input paths.
            if mission.status != MissionStatus::Waiting
                || mission.tasks.iter().any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
                return Err("read_scope_extension requires waiting dispatch and no running/awaiting_verification tasks".into());
            }
            let declared = extension.paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
            if declared != added_reads {
                return Err("read_scope_extension paths must exactly equal newly added task read_paths; unused or existing parent grants are forbidden".into());
            }
            validate_follow_up_read_extension_paths(&mission.workspace.root, &extension.paths)?;
        }
        Self::add_follow_up_tasks_tx(&tx, &mission, &tasks, &request.reason, Some(request))?;
        let current = load_mission(&tx, &request.mission_id)?.ok_or("mission disappeared")?;
        tx.commit()?;
        Ok(current)
    }

    /// Shared authoritative validation/mutation. The UI retains its original
    /// assignment and event semantics; the native path supplies parent bindings
    /// and an atomic receipt, after its extra hash/idempotency/scope guards.
    fn add_follow_up_tasks_tx(
        transaction: &rusqlite::Transaction<'_>,
        mission: &MissionView,
        tasks: &[MissionTaskSpec],
        reason: &str,
        request: Option<&super::follow_up::MissionFollowUpAdd>,
    ) -> Result<(), Box<dyn Error>> {
        let mission_id = mission.mission_id.as_str();
        let confirmation = mission.confirmation.as_ref().ok_or("mission has no confirmed plan")?;
        if !matches!(mission.status, MissionStatus::Active | MissionStatus::Waiting | MissionStatus::Planned) {
            return Err("mission is not active for follow-up work".into());
        }
        let mut bound_tasks = tasks.to_vec();
        if let Some(assignment) = mission.assignment.as_ref().filter(|_| request.is_none()) {
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
        let reopened = review_rework_targets(mission, tasks)?;
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
        if let Some(reason) = budget_exhaustion_reason_on(transaction, mission)? {
            return Err(reason.into());
        }
        for id in &reopened {
            let parent = mission.tasks.iter().find(|task| &task.spec.id == id).ok_or("revision target missing")?;
            transaction.execute("UPDATE mission_tasks SET status='failed', verification_passed=0, updated_at=?3 WHERE mission_id=?1 AND task_id=?2",
                params![mission_id, id, now])?;
            append_event_tx(transaction, mission_id, "task_reopened_for_review", id,
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
        let mut payload = serde_json::json!({
            "task_ids": tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
            "reason": reason.trim(),
            "plan_sha256": confirmation.plan_sha256,
        });
        if let Some(request) = request {
            payload["follow_up_request"] = serde_json::to_value(request)?;
        }
        append_event_tx(
            transaction,
            mission_id,
            "follow_up_tasks_added",
            "",
            if request.and_then(|r| r.read_scope_extension.as_ref()).is_some() {
                format!("MCP 客户端经显式只读扩展授权新增 {} 项跟进任务；写入、执行绑定及原检查不变", tasks.len())
            } else if request.is_some() { format!("MCP 客户端在已确认范围内新增 {} 项跟进任务", tasks.len()) }
                else { format!("协调 Agent 在已确认范围内新增 {} 项跟进任务", tasks.len()) },
            payload,
            &now,
        )?;
        recompute_task_readiness_tx(transaction, mission_id, &now)?;
        Ok(())
    }

    /// Withdraw only an unstarted repair leaf while dispatch is stopped. This
    /// does not satisfy or cancel the original delivery, nor change its scope.
    pub(crate) fn validate_pending_repair_cancellation(input: &serde_json::Value) -> Result<(), Box<dyn Error>> {
        let request: PendingRepairCancellation = serde_json::from_value(input.clone())?;
        request.validate()
    }

    pub(crate) fn cancel_pending_repair_for_mcp(
        &self, input: &serde_json::Value,
    ) -> Result<MissionView, Box<dyn Error>> {
        let request: PendingRepairCancellation = serde_json::from_value(input.clone())?;
        self.cancel_pending_repair_request(&request)
    }

    fn cancel_pending_repair_request(
        &self, request: &PendingRepairCancellation,
    ) -> Result<MissionView, Box<dyn Error>> {
        request.validate()?;
        // Rejection and exact replay must not initialize/migrate the database
        // or reconstruct legacy claims before the transaction checks them.
        let mut connection = Connection::open_with_flags(&self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&tx, &request.mission_id)?.ok_or("mission not found")?;
        if mission.plan_sha256 != request.expected_plan_sha256
            || mission.confirmation.as_ref().map(|c| &c.plan_sha256) != Some(&request.expected_plan_sha256) {
            return Err("pending repair cancellation requires the current confirmed plan".into());
        }
        let receipt: Option<String> = tx.query_row(
            "SELECT payload_json FROM mission_events WHERE mission_id=?1
             AND event_type='pending_repair_canceled'
             AND json_extract(payload_json,'$.cancel_request.request_id')=?2
             ORDER BY sequence DESC LIMIT 1",
            params![request.mission_id, request.request_id], |row| row.get(0),
        ).optional()?;
        if let Some(raw) = receipt {
            let payload: serde_json::Value = serde_json::from_str(&raw)?;
            let original: PendingRepairCancellation = serde_json::from_value(payload["cancel_request"].clone())?;
            if original != *request { return Err("pending repair cancellation request_id has different values".into()); }
            if !mission.tasks.iter().any(|t| t.spec.id == request.task_id && t.status == TaskStatus::Canceled) {
                return Err("pending repair cancellation receipt conflicts with task state".into());
            }
            tx.commit()?;
            return Ok(mission);
        }
        if mission.status != MissionStatus::Waiting
            || mission.tasks.iter().any(|t| matches!(t.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            return Err("pause dispatch and wait for running work before canceling a pending repair".into());
        }
        let task = mission.tasks.iter().find(|t| t.spec.id == request.task_id).ok_or("pending repair task not found")?;
        if task.spec.repair_of.is_empty()
            || !matches!(task.status, TaskStatus::Ready | TaskStatus::Blocked)
            || !task.attempt_run_id.is_empty() || !task.verification_run_id.is_empty()
            || task.verification_passed.is_some() || !task.evidence_refs.is_empty() {
            return Err("only an unstarted pending repair may be canceled; original deliveries and attempted work are retained".into());
        }
        if !mission.tasks.iter().any(|parent| parent.spec.id == task.spec.repair_of && parent.status == TaskStatus::Failed) {
            return Err("pending repair must belong to a failed delivery".into());
        }
        if mission.tasks.iter().any(|other| other.status != TaskStatus::Canceled
            && (other.spec.repair_of == task.spec.id || other.spec.depends_on.contains(&task.spec.id))) {
            return Err("pending repair has active dependents; cancel dependent repair leaves first".into());
        }
        let claimed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mission_workspace_claims WHERE mission_id=?1)",
            params![request.mission_id], |row| row.get(0))?;
        if claimed { return Err("pending repair cancellation requires drained workspace claims".into()); }
        let now = now_stamp();
        let changed = tx.execute(
            "UPDATE mission_tasks SET status='canceled', updated_at=?3
             WHERE mission_id=?1 AND task_id=?2 AND status IN ('ready','blocked') AND attempt_run_id=''
             AND verification_run_id='' AND verification_passed IS NULL",
            params![request.mission_id, request.task_id, now])?;
        if changed != 1 { return Err("pending repair changed before cancellation".into()); }
        tx.execute("UPDATE mission_runs SET updated_at=?2 WHERE mission_id=?1",
            params![request.mission_id, now])?;
        append_event_tx(&tx, &request.mission_id, "pending_repair_canceled", &request.task_id,
            "已撤销尚未启动的返工方案；原交付责任、失败记录和权限保留",
            serde_json::json!({"cancel_request":request,"repair_of":task.spec.repair_of}), &now)?;
        let current = load_mission(&tx, &request.mission_id)?.ok_or("mission disappeared")?;
        tx.commit()?;
        Ok(current)
    }

    pub(crate) fn snapshot(&self, mission_id: &str, after_sequence: u64) -> Result<MissionSnapshot, Box<dyn Error>> {
        let mut raw_connection = self.connection()?;
        let connection = raw_connection.transaction()?;
        let mission = load_mission(&connection, mission_id)?.ok_or("mission not found")?;
        let events = list_events(&connection, mission_id, after_sequence)?;
        let snapshot_head = mission.effective_wait.as_ref().map(|w| w.cursor).unwrap_or(0);
        // State freshness and event-page continuation are different cursors.
        // Preserve lossless pagination for existing events.list consumers.
        let cursor = events.last().map(|event| event.sequence).unwrap_or(after_sequence);
        let numerical_gap = events.first().is_some_and(|e| e.sequence != after_sequence.saturating_add(1))
            || events.windows(2).any(|pair| pair[1].sequence != pair[0].sequence.saturating_add(1));
        let requires_snapshot = after_sequence > snapshot_head || snapshot_head > cursor
            || if numerical_gap && cursor > after_sequence {
                // The clock is global. Rows belonging to other Missions are
                // valid gaps in this Mission's stream, not missing history.
                let global_rows: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM mission_events WHERE sequence>?1 AND sequence<=?2",
                    params![after_sequence as i64, cursor as i64], |row| row.get(0))?;
                (global_rows.max(0) as u64) < cursor.saturating_sub(after_sequence)
            } else { false };
        Ok(MissionSnapshot { mission, events, cursor, requires_snapshot })
    }

    /// Append only; no task, confirmation, plan, usage or workspace claim writes.
    /// The immediate transaction also serializes concurrent request-id replays.
    pub(crate) fn submit_feedback(
        &self, mission_id: &str, input: &MissionFeedbackInput,
    ) -> Result<MissionView, Box<dyn Error>> {
        require_id("mission_id", mission_id)?;
        input.validate()?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        if !matches!(mission.status, MissionStatus::Active | MissionStatus::Waiting) {
            return Err("feedback requires an active or waiting Mission".into());
        }
        if mission.plan_sha256 != input.expected_plan_sha256 || !mission.confirmation.as_ref()
            .is_some_and(|confirmation| confirmation.plan_sha256 == mission.plan_sha256) {
            return Err("feedback requires the current confirmed plan; stale plan rejected".into());
        }
        for id in &input.target_task_ids {
            if !mission.tasks.iter().any(|task| task.spec.id == *id && task.status != TaskStatus::Canceled) {
                return Err(format!("feedback target is unknown or canceled: {id}").into());
            }
        }
        let request = serde_json::to_value(input)?;
        let payload_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        if let Some(prior) = decision_event(&tx,
            "mission_id=?1 AND event_type='mission_feedback_submitted'
                AND json_extract(payload_json,'$.request_id')=?2",
            params![mission_id, input.request_id], false)? {
            if prior.payload["input"] != request || prior.payload["payload_sha256"] != payload_sha256 {
                return Err("feedback request_id already has a different payload".into());
            }
        } else {
            append_event_tx(&tx, mission_id, "mission_feedback_submitted", "", FEEDBACK_SUMMARY,
                serde_json::json!({"request_id":input.request_id,"plan_sha256":mission.plan_sha256,
                    "payload_sha256":payload_sha256,"input":request}), &now_stamp())?;
        }
        tx.commit()?;
        Ok(mission)
    }

    /// Query matching feedback across the whole ledger, not a UI event page.
    /// Only repair_of ancestors propagate observations; dependency peers do not.
    pub(crate) fn feedback_context(
        &self, mission_id: &str, expected_plan_sha256: &str, target_task_ids: &[String],
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        if !(1..=8).contains(&target_task_ids.len()) { return Err("feedback context needs 1..8 targets".into()); }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        if mission.plan_sha256 != expected_plan_sha256 || !mission.confirmation.as_ref()
            .is_some_and(|confirmation| confirmation.plan_sha256 == mission.plan_sha256) {
            return Err("feedback context requires the current confirmed plan".into());
        }
        let mut matching = BTreeSet::new();
        for target in target_task_ids {
            let mut current = target.as_str();
            let mut seen = BTreeSet::new();
            while !current.is_empty() && seen.insert(current) {
                let task = mission.tasks.iter().find(|task| task.spec.id == current)
                    .ok_or("feedback context target or repair ancestor missing")?;
                if task.status == TaskStatus::Canceled { return Err("feedback context target or ancestor canceled".into()); }
                matching.insert(current.to_string());
                current = &task.spec.repair_of;
            }
        }
        let mut context = serde_json::json!({"policy":super::FEEDBACK_CONTEXT_POLICY,
            "plan_sha256":expected_plan_sha256,"observations":[],"truncated":false});
        {
            let mut statement = tx.prepare(
                "SELECT event_id, sequence, payload_json FROM mission_events
                 WHERE mission_id=?1 AND event_type='mission_feedback_submitted'
                   AND json_extract(payload_json,'$.plan_sha256')=?2
                   AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.input.target_task_ids') AS target
                       JOIN json_each(?3) AS wanted ON target.value=wanted.value)
                 ORDER BY sequence DESC LIMIT 9")?;
            let rows = statement.query_map(params![mission_id, expected_plan_sha256, serde_json::to_string(&matching)?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)))?;
            for row in rows {
                let (event_id, sequence, raw) = row?;
                if context["observations"].as_array().unwrap().len() == 8 {
                    context["truncated"] = serde_json::json!(true);
                    break;
                }
                let payload: serde_json::Value = serde_json::from_str(&raw)?;
                let input: MissionFeedbackInput = serde_json::from_value(payload["input"].clone())?;
                input.validate()?;
                if input.expected_plan_sha256 != expected_plan_sha256 { continue; }
                let message = input.message.chars().take(2_000).collect::<String>();
                let refs = input.evidence_refs.iter().take(4)
                    .map(|reference| reference.chars().take(256).collect::<String>()).collect::<Vec<_>>();
                let truncated = message != input.message || refs != input.evidence_refs;
                let observation = serde_json::json!({"event_id":event_id,"sequence":sequence,
                    "request_id":input.request_id,"payload_sha256":payload["payload_sha256"],
                    "target_task_ids":input.target_task_ids,"message":message,"evidence_refs":refs,
                    "truncated":truncated});
                context["observations"].as_array_mut().unwrap().push(observation);
                if serde_json::to_vec(&context)?.len() > MAX_FEEDBACK_CONTEXT_BYTES {
                    context["observations"].as_array_mut().unwrap().pop();
                    context["truncated"] = serde_json::json!(true);
                    break;
                }
                if truncated { context["truncated"] = serde_json::json!(true); }
            }
        }
        tx.commit()?;
        Ok(context)
    }

    /// Read the current failed leaf and at most its failure/host event pair.
    /// Task state, target selection and run bindings share one SQLite snapshot.
    pub(crate) fn follow_up_failure_context(
        &self, mission_id: &str,
    ) -> Result<(MissionView, String, Vec<MissionEvent>), Box<dyn Error>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        if let Some(reason) = budget_exhaustion_reason_on(&tx, &mission)? { return Err(reason.into()); }
        let failed = mission.tasks.iter().filter(|task| task.status == TaskStatus::Failed).collect::<Vec<_>>();
        if failed.is_empty() { return Err("当前没有验证失败的任务，不能生成跟进任务".into()); }
        let parents = mission.tasks.iter().filter(|task| !task.spec.repair_of.is_empty())
            .map(|task| task.spec.repair_of.as_str()).collect::<BTreeSet<_>>();
        // Preserve the coordinator's existing failed-repair/leaf eligibility.
        let leaves = failed.iter().copied().filter(|task|
            !task.spec.repair_of.is_empty() || !parents.contains(task.spec.id.as_str())
        ).collect::<Vec<_>>();
        let eligible = if leaves.is_empty() { &failed } else { &leaves };
        let mut latest: Option<MissionEvent> = None;
        for task in eligible {
            let event = decision_event(&tx,
                "mission_id=?1 AND task_id=?2 AND (
                    (event_type='task_failed' AND ?3<>'' AND json_extract(payload_json,'$.run_id')=?3)
                    OR (event_type='verification_failed' AND ?3<>'' AND ?4<>''
                        AND json_extract(payload_json,'$.implementation_run_id')=?3
                        AND json_extract(payload_json,'$.verification_run_id')=?4))",
                params![mission_id, task.spec.id, task.attempt_run_id, task.verification_run_id], true)?;
            if let Some(event) = event {
                if latest.as_ref().is_none_or(|prior| event.sequence > prior.sequence) { latest = Some(event); }
            }
        }
        // Legacy records without a bound failure retain the old empty-evidence
        // fallback; never borrow a different run's review to fill the gap.
        let target_id = latest.as_ref().map(|event| event.task_id.clone())
            .unwrap_or_else(|| eligible[0].spec.id.clone());
        let mut events = Vec::with_capacity(2);
        if let Some(failure) = latest {
            if failure.event_type == "verification_failed" {
                let run_id = failure.payload["verification_run_id"].as_str().unwrap_or_default();
                let implementation = failure.payload["implementation_run_id"].as_str().unwrap_or_default();
                if let Some(host) = decision_event(&tx,
                    "mission_id=?1 AND task_id=?2 AND event_type='host_verification_checked'
                        AND sequence<?3 AND json_extract(payload_json,'$.verification_run_id')=?4
                        AND json_extract(payload_json,'$.implementation_run_id')=?5",
                    params![mission_id, target_id, failure.sequence as i64, run_id, implementation], true)? {
                    events.push(host);
                }
            }
            events.push(failure);
        }
        tx.commit()?;
        Ok((mission, target_id, events))
    }

    /// Return only the latest recovery event and latest failure of this exact
    /// fingerprint. A different fingerprint must not hide a suppression marker.
    pub(crate) fn follow_up_control_context(
        &self, mission_id: &str, fingerprint: impl FnOnce(&MissionView) -> Option<String>,
    ) -> Result<(MissionView, Vec<MissionEvent>), Box<dyn Error>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        let Some(fingerprint) = fingerprint(&mission) else {
            tx.commit()?;
            return Ok((mission, Vec::new()));
        };
        let mut events = Vec::with_capacity(2);
        if let Some(event) = decision_event(&tx,
            "mission_id=?1 AND event_type IN ('mission_resume','budget_extended')",
            params![mission_id], true)? { events.push(event); }
        if let Some(event) = decision_event(&tx,
            "mission_id=?1 AND event_type='mission_follow_up_failed'
                AND json_extract(payload_json,'$.fingerprint')=?2",
            params![mission_id, fingerprint], true)? { events.push(event); }
        events.sort_by_key(|event| event.sequence);
        tx.commit()?;
        Ok((mission, events))
    }

    /// Baseline/retry evidence for the implementation being verified, not the
    /// newest event of an unrelated attempt or a bounded UI history page.
    pub(crate) fn verification_event_context(
        &self, mission_id: &str, task_id: &str, implementation_run_id: &str,
    ) -> Result<(Option<MissionEvent>, Option<MissionEvent>), Box<dyn Error>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        let task = mission.tasks.iter().find(|task| task.spec.id == task_id).ok_or("task not found")?;
        if implementation_run_id.is_empty() || task.attempt_run_id != implementation_run_id {
            return Err("verification implementation binding changed".into());
        }
        let result = verification_events_for_run(&tx, mission_id, task_id, implementation_run_id)?;
        tx.commit()?;
        Ok(result)
    }

    /// Handoff decisions must not use a bounded UI event page or mix task
    /// projections from before/after a repair acceptance transaction.
    pub(crate) fn executor_handoff_context(&self, mission_id: &str)
        -> Result<(MissionView, Vec<MissionEvent>), Box<dyn Error>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let mission = load_mission(&tx, mission_id)?.ok_or("mission not found")?;
        let mut events = Vec::new();
        for task in &mission.tasks {
            for kind in ["task_repaired", "task_completed", "host_verification_checked"] {
                if let Some(event) = decision_event(&tx,
                    "mission_id=?1 AND task_id=?2 AND event_type=?3",
                    params![mission_id, task.spec.id, kind], true)? { events.push(event); }
            }
        }
        tx.commit()?;
        Ok((mission, events))
    }

    #[cfg(test)]
    pub(crate) fn seed_test_progress(&self, mission_id: &str, count: usize) -> Result<usize, Box<dyn Error>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        for _ in 0..count {
            append_event_tx(&tx, mission_id, "task_progress", "", "pagination fixture",
                serde_json::json!({}), &now_stamp())?;
        }
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM mission_events WHERE mission_id=?1",
            params![mission_id], |row| row.get(0))?;
        tx.commit()?;
        Ok(total as usize)
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
                    "currency":usage.currency,"billing_owner":usage.billing_owner,
                    "trusted": usage.billing_owner != "unreported" && !usage.currency.trim().is_empty(),
                    "complete": usage.billing_owner != "unreported"}), &now)?;
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
        let cost = cost_usage_on(&transaction, mission_id)?;
        if cost.comparable {
            let (unit, amount) = cost.buckets.iter().next().unwrap();
            total.currency = unit.clone();
            total.estimated_cost = *amount;
        } else {
            total.currency.clear();
            total.estimated_cost = 0.0;
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
        // The first request owns its id even when it is beyond the UI page.
        if let Some(event) = decision_event(&tx,
            "mission_id=?1 AND event_type='verification_retry_requested'
                AND json_extract(payload_json,'$.request_id')=?2",
            params![mission_id, input.request_id], false)? {
                if event.task_id != task_id || event.payload["request_sha256"] != digest {
                    return Err("verification request_id already used with different values".into());
                }
                tx.commit()?;
                return self.get(mission_id);
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
        let (_, baseline_event) = verification_events_for_run(&tx, mission_id, task_id, &task.attempt_run_id)?;
        let prior_baseline = baseline_event.and_then(|event| event.payload.get("protected_baseline").cloned());
        let baseline = if let Some(baseline) = prior_baseline {
            if input.source_baseline.is_some() { return Err("an existing baseline cannot be replaced".into()); }
            baseline
        } else {
            let source = input.source_baseline.as_ref().ok_or("no execution baseline; provide a sourced pre-run baseline")?;
            require_text("source_reference", &source.source_reference, 1024)?;
            require_sha256(&source.artifact_sha256)?;
            let captured = chrono::DateTime::parse_from_rfc3339(&source.captured_at)?;
            // Preserve the original mission-wide first-dispatch timestamp rule.
            let started = decision_event(&tx, "mission_id=?1 AND event_type='task_dispatched'",
                params![mission_id], false)?
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
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status != MissionStatus::Active || !owned_pending_verification_on(&transaction, &mission)? {
            return Err("verification admission requires an active confirmed plan and its current workspace claim".into());
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

    pub(crate) fn record_budget_exhausted(&self, mission_id: &str, _observed_reason: &str) -> Result<(), Box<dyn Error>> {
        let now = now_stamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A scan snapshot is advisory. Serialize with admission, completion,
        // explicit controls and budget extensions before writing a wait.
        let mission = load_mission(&transaction, mission_id)?.ok_or("mission not found")?;
        if mission.status != MissionStatus::Active || mission.confirmation.as_ref()
            .is_none_or(|confirmation| confirmation.plan_sha256 != mission.plan_sha256)
            || mission.tasks.iter().any(|task|
                matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
            return Ok(());
        }
        let Some(reason) = budget_exhaustion_reason_on(&transaction, &mission)?
            .or_else(|| mission.budget.as_ref().and_then(|budget| repair_exhaustion_reason(&mission, budget)))
            else { return Ok(()); };
        transaction.execute(
            "UPDATE mission_runs SET status = ?2, updated_at = ?3 WHERE mission_id = ?1",
            params![mission_id, MissionStatus::Waiting.as_str(), now],
        )?;
        append_event_tx(&transaction, mission_id, "budget_exhausted", "", &reason,
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
        if mission.status != MissionStatus::Active || mission.tasks.iter().any(|task|
            matches!(task.status, TaskStatus::Running | TaskStatus::AwaitingVerification)) {
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

// Narrow exception for an explicit resume only: it cannot admit an
// implementation, replace a current worker, invent a claim or extend budget.
fn owned_pending_verification_on(connection: &Connection, mission: &MissionView) -> Result<bool, Box<dyn Error>> {
    if mission.plan_sha256.is_empty()
        || mission.confirmation.as_ref().is_none_or(|c| c.plan_sha256 != mission.plan_sha256)
        || mission.tasks.iter().any(|task| task.status == TaskStatus::Running) {
        return Ok(false);
    }
    let Some(task) = mission.tasks.iter().find(|task| task.status == TaskStatus::AwaitingVerification) else {return Ok(false);};
    if task.attempt_run_id.is_empty() {return Ok(false);}
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM mission_workspace_claims
        WHERE workspace_key=?1 AND mission_id=?2 AND task_id=?3 AND run_id=?4)",
        params![workspace_claim_key(&mission.workspace.root), mission.mission_id, task.spec.id, task.attempt_run_id],
        |row| row.get(0))?)
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
        restore_legacy_claim(connection,&workspace.root,&mission_id,&task_id,&run_id,&acquired_at)?;
    }
    Ok(())
}

fn restore_legacy_claim(connection:&Connection,workspace:&str,mission:&str,task:&str,run:&str,at:&str)
    ->rusqlite::Result<usize> {
    connection.execute("INSERT OR IGNORE INTO mission_workspace_claims(workspace_key,mission_id,task_id,run_id,acquired_at)
        SELECT ?1,?2,?3,?4,?5 WHERE EXISTS(SELECT 1 FROM mission_tasks WHERE mission_id=?2 AND task_id=?3
        AND attempt_run_id=?4 AND status IN ('running','awaiting_verification'))",
        params![workspace_claim_key(workspace),mission,task,run,at])
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
        let cost_usage = cost_usage_on(connection, &mission.mission_id)?;
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
            let unit = budget.currency.as_deref().unwrap_or_default().trim().to_ascii_uppercase();
            if unit.is_empty() || !cost_usage.comparable || !cost_usage.buckets.contains_key(&unit) {
                return Ok(Some("成本币种未知、混合或成本不可信，无法与声明限额比较，已停止继续派发".into()));
            }
            let estimated_cost = cost_usage.buckets[&unit];
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
    let mut mission = MissionView {
        schema_version: "mission.v1".to_string(), mission_id: id.clone(), title, objective,
        status: MissionStatus::parse(&status)?, workspace: serde_json::from_str(&workspace)?,
        acceptance_criteria: serde_json::from_str(&acceptance)?,
        plan_revision: revision.max(0) as u64, plan_sha256: hash,
        plan_summary: proposal.as_ref().map(|value| value.summary.clone()).unwrap_or_default(),
        required_acceptance: proposal.as_ref().map(|value| value.required_acceptance.clone()).unwrap_or_default(),
        proposed_tasks: proposal.as_ref().map(|value| value.tasks.clone()).unwrap_or_default(),
        budget: budget.map(|value| serde_json::from_str(&value)).transpose()?,
        usage,
        cost_usage: cost_usage_on(connection, &id)?,
        effective_wait: None,
        handoff: handoff.map(|value| serde_json::from_str(&value)).transpose()?,
        workflow: workflow.map(|value| serde_json::from_str(&value)).transpose()?,
        assignment: assignment.map(|value| serde_json::from_str(&value)).transpose()?,
        plan_mode: MissionPlanMode::parse(&plan_mode)?,
        confirmation: confirmation.map(|value| serde_json::from_str(&value)).transpose()?,
        tasks, created_at: created, updated_at: updated,
    };
    mission.effective_wait = Some(effective_wait_on(connection, &mission)?);
    Ok(Some(mission))
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

/// Every caller supplies a fixed predicate and bound values; only one event is
/// materialized. `latest=false` preserves first-writer request id semantics.
fn decision_event(
    connection: &Connection, predicate: &str, values: impl rusqlite::Params, latest: bool,
) -> Result<Option<MissionEvent>, Box<dyn Error>> {
    let order = if latest { "DESC" } else { "ASC" };
    let sql = format!("SELECT event_id,mission_id,sequence,event_type,task_id,summary,payload_json,occurred_at
        FROM mission_events AS event WHERE {predicate} ORDER BY sequence {order} LIMIT 1");
    let row = connection.query_row(&sql, values, |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
        row.get::<_, String>(6)?, row.get::<_, String>(7)?,
    ))).optional()?;
    row.map(|(event_id, mission_id, sequence, event_type, task_id, summary, payload, occurred_at)| {
        Ok(MissionEvent { event_id, mission_id, sequence: sequence.max(0) as u64,
            event_type, task_id, summary, payload: serde_json::from_str(&payload)?, occurred_at })
    }).transpose()
}

/// Legacy retry events carry `previous_verification_run_id`, not an
/// implementation id. Bind them through their earlier, exact failed run instead
/// of changing the event format or replacing an existing baseline.
fn verification_events_for_run(
    connection: &Connection, mission_id: &str, task_id: &str, implementation_run_id: &str,
) -> Result<(Option<MissionEvent>, Option<MissionEvent>), Box<dyn Error>> {
    let retry = decision_event(connection,
        "event.mission_id=?1 AND event.task_id=?2 AND event.event_type='verification_retry_requested'
            AND EXISTS (SELECT 1 FROM mission_events AS failed
                WHERE failed.mission_id=event.mission_id AND failed.task_id=event.task_id
                    AND failed.event_type='verification_failed' AND failed.sequence<event.sequence
                    AND json_extract(failed.payload_json,'$.implementation_run_id')=?3
                    AND json_extract(failed.payload_json,'$.verification_run_id')<>''
                    AND json_extract(failed.payload_json,'$.verification_run_id')=
                        json_extract(event.payload_json,'$.previous_verification_run_id'))",
        params![mission_id, task_id, implementation_run_id], true)?;
    let captured = decision_event(connection,
        "mission_id=?1 AND task_id=?2 AND event_type='task_baseline_captured'
            AND json_extract(payload_json,'$.implementation_run_id')=?3",
        params![mission_id, task_id, implementation_run_id], true)?;
    // The original rule chooses the latest baseline-producing event, including
    // explicit null/unknown values; do not silently fall back to an older one.
    let baseline = match (&retry, captured) {
        (Some(retry), Some(captured)) if captured.sequence > retry.sequence => Some(captured),
        (Some(retry), _) => Some(retry.clone()),
        (None, captured) => captured,
    };
    Ok((retry, baseline))
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

fn cost_usage_on(connection: &Connection, mission_id: &str) -> Result<super::contract::MissionCostUsage, Box<dyn Error>> {
    let mut result = super::contract::MissionCostUsage::default();
    for raw in effective_usage_payloads(connection, mission_id)? {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let unit = value["currency"].as_str().unwrap_or_default().trim().to_ascii_uppercase();
        let cost = value["estimated_cost"].as_f64();
        let trusted = value.get("trusted").and_then(|v| v.as_bool()) == Some(true)
            && value.get("complete").and_then(|v| v.as_bool()) == Some(true)
            && value.get("billing_owner").and_then(|v| v.as_str()) != Some("unreported");
        if unit.is_empty() || !trusted || cost.is_none_or(|v| !v.is_finite() || v < 0.0) {
            result.unknown_runs += 1;
        } else {
            *result.buckets.entry(unit).or_default() += cost.unwrap();
        }
    }
    result.comparable = result.unknown_runs == 0 && result.buckets.len() == 1
        && result.buckets.values().all(|v| v.is_finite());
    Ok(result)
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
    let cost = cost_usage_on(connection, mission_id)?;
    // Compatibility scalar is explicitly non-comparable, never a mixed sum.
    if cost.comparable {
        let (unit, amount) = cost.buckets.iter().next().unwrap();
        usage.currency = unit.clone();
        usage.estimated_cost = *amount;
    } else {
        usage.currency = String::new();
        usage.estimated_cost = 0.0;
    }
    Ok(Some(usage))
}

fn new_mission_id() -> String { format!("mission_{}_{:08x}", Utc::now().timestamp_millis(), rand::random::<u32>()) }
fn now_stamp() -> String { Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true) }

fn effective_wait_on(connection: &Connection, mission: &MissionView) -> Result<super::contract::MissionEffectiveWait, Box<dyn Error>> {
    let cursor: i64 = connection.query_row("SELECT COALESCE(MAX(sequence),0) FROM mission_events WHERE mission_id=?1", params![mission.mission_id], |r| r.get(0))?;
    let associated = !mission.plan_sha256.is_empty() && mission.confirmation.as_ref()
        .is_some_and(|c| c.plan_sha256 == mission.plan_sha256);
    let waiting = mission.status == MissionStatus::Waiting;
    let running = mission.tasks.iter().any(|t| t.status == TaskStatus::Running);
    let verifying = mission.tasks.iter().any(|t| t.status == TaskStatus::AwaitingVerification);
    // Publish the same narrow resume admission as control(), so a user pause
    // of already-admitted verification does not hide its only recovery action.
    let admitted_verification = verifying && owned_pending_verification_on(connection, mission)?;
    let environment = associated && runtime_blocked_on(connection, mission)?;
    let budget_reason = if associated { budget_exhaustion_reason_on(connection, mission)?
        .or_else(|| mission.budget.as_ref().and_then(|b| repair_exhaustion_reason(mission,b))) } else { None };
    let reason = if !waiting {
            if mission.status == MissionStatus::Active && associated && verifying { "system_verification" } else { "not_waiting" }
        } else if !associated { "unknown" }
        else if budget_reason.is_some() && !admitted_verification { "budget_exhausted" } else if environment { "environment_blocked" }
        else if running { "system_verification" } else {
            let action: Option<String> = connection.query_row("SELECT event_type FROM mission_events WHERE mission_id=?1 AND event_type IN ('mission_pause','mission_resume','budget_extended') ORDER BY sequence DESC LIMIT 1", params![mission.mission_id], |r| r.get(0)).optional()?;
            if verifying && !admitted_verification { "unknown" }
            else if action.as_deref() == Some("mission_pause") { "manual_pause" }
            // Waiting is not dispatched by the supervisor. An owned submitted
            // implementation needs an explicit user resume; the active state
            // above belongs to the system. Do not infer liveness from a saved
            // verification ID or change the existing narrow resume admission.
            else if admitted_verification { "verification_resume_required" } else { "unknown" }
        };
    let actor = match reason { "budget_exhausted" | "environment_blocked" | "manual_pause" | "verification_resume_required" => "user", "system_verification" => "system", "not_waiting" => "none", _ => "unknown" };
    Ok(super::contract::MissionEffectiveWait {
        contract_version: "mission.wait.v1".into(), plan_sha256: mission.plan_sha256.clone(), cursor: cursor.max(0) as u64,
        reason: reason.into(), actor: actor.into(), budget_reason: budget_reason.clone(), environment_blocked: environment,
        can_increase_budget: waiting && associated && !running && !verifying && mission.budget.is_some(),
        can_resume: waiting && associated && !running && !environment
            && (admitted_verification || (!verifying && budget_reason.is_none())),
    })
}

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
#[path = "budget_remediation_tests.rs"]
mod budget_remediation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::contract::{ExecutionBinding, MissionBudget, MissionWorkspace};

    #[test]
    fn snapshot_pagination_keeps_delivery_cursor_lossless_after_10000_events() {
        let store = store("cursor-pagination");
        let mission = store.create(&MissionCreateInput {
            title: "Pagination".into(), objective: "Preserve every event".into(),
            workspace: super::super::MissionWorkspace {
                permission_mode: String::new(), root: "F:/pagination-fixture".into(),
                read_scope: vec!["src".into()], write_scope: vec!["src".into()],
            },
            acceptance_criteria: vec!["lossless cursor".into()],
        }).unwrap();
        let mut connection = store.connection().unwrap();
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let stamp = now_stamp();
        for index in 0..10_005 {
            append_event_tx(&tx, &mission.mission_id, "task_progress", "",
                format!("fixture {index}"), serde_json::json!({}), &stamp).unwrap();
        }
        tx.commit().unwrap();
        let mut cursor = 0;
        let mut delivered = Vec::new();
        loop {
            let page = store.snapshot(&mission.mission_id, cursor).unwrap();
            assert_eq!(page.cursor, page.events.last().map(|event| event.sequence).unwrap_or(cursor),
                "continuation cursor must never skip events omitted by MAX_EVENTS");
            if page.events.is_empty() { break; }
            assert!(page.events.iter().all(|event| event.sequence > cursor));
            delivered.extend(page.events.iter().map(|event| event.event_id.clone()));
            cursor = page.cursor;
        }
        assert_eq!(delivered.len(), 10_006); // mission_created plus fixture events
        assert_eq!(delivered.iter().collect::<std::collections::BTreeSet<_>>().len(), delivered.len());
        let tail = store.snapshot(&mission.mission_id, cursor).unwrap();
        assert!(tail.events.is_empty());
        assert_eq!(tail.cursor, cursor);
    }

    #[test]
    fn snapshot_global_sequence_gaps_from_other_missions_are_complete() {
        let store = store("cursor-interleaved");
        let input = MissionCreateInput {
            title: "Interleaved".into(), objective: "Global event sequence".into(),
            workspace: super::super::MissionWorkspace {
                permission_mode: String::new(), root: "F:/pagination-fixture".into(),
                read_scope: vec!["src".into()], write_scope: vec!["src".into()],
            },
            acceptance_criteria: vec!["valid gaps are not missing events".into()],
        };
        let first = store.create(&input).unwrap();
        let second = store.create(&input).unwrap();
        store.append_event(&first.mission_id, "task_progress", "", "first", serde_json::json!({})).unwrap();
        store.append_event(&second.mission_id, "task_progress", "", "second", serde_json::json!({})).unwrap();
        store.append_event(&first.mission_id, "task_progress", "", "first again", serde_json::json!({})).unwrap();
        let page = store.snapshot(&first.mission_id, 0).unwrap();
        assert_eq!(page.events.len(), 3);
        assert!(!page.requires_snapshot, "global sequence gaps occupied by another Mission are valid");
        assert_eq!(page.cursor, page.events.last().unwrap().sequence);
        assert_eq!(page.mission.effective_wait.as_ref().unwrap().cursor, page.cursor);
    }

    // effective_wait.cursor is a derived event-head projection; feedback
    // advances that cursor without changing the persisted Mission state.
    fn feedback_state(value: &MissionView) -> serde_json::Value {
        let mut value = serde_json::to_value(value).unwrap();
        if let Some(wait) = value["effective_wait"].as_object_mut() { wait.remove("cursor"); }
        value
    }


    fn native_follow_up_fixture(name: &str) -> (MissionStore, super::super::follow_up::MissionFollowUpAdd) {
        native_follow_up_fixture_at(name, None)
    }

    fn native_follow_up_fixture_at(name: &str, workspace_root: Option<&std::path::Path>) -> (MissionStore, super::super::follow_up::MissionFollowUpAdd) {
        use super::super::follow_up::{FollowUpSchemaVersion, MissionFollowUpAdd, MissionFollowUpTask};
        let store = store(name);
        let mut input = create_input();
        if let Some(root) = workspace_root { input.workspace.root = root.to_string_lossy().into_owned(); }
        let created = store.create(&input).unwrap();
        let mut plan = proposal();
        plan.tasks[1].id = "other".into();
        plan.tasks[1].depends_on.clear();
        for (i, t) in plan.tasks.iter_mut().enumerate() {
            t.execution.executor = "personal.codex".into();
            t.execution.role_id = format!("specialist-{i}");
            t.required_checks = vec!["npm test".into(), "cargo test".into()];
        }
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256, &["fix".into(), "other".into()],
            &["frontend/src".into()], &[], &[], "test-user").unwrap();
        for id in ["fix", "other"] {
            let run = format!("failed-{id}");
            store.begin_task_attempt(&created.mission_id, id, &run).unwrap();
            store.complete_task_attempt(&created.mission_id, id, &run, false, &[], "fixture failure").unwrap();
        }
        let request = MissionFollowUpAdd {
            schema_version: FollowUpSchemaVersion::V1, mission_id: created.mission_id,
            expected_plan_sha256: planned.plan_sha256, reason: "Explicit user correction".into(),
            read_scope_extension: None,
            tasks: plan.tasks.iter().map(|t| MissionFollowUpTask {
                id: format!("repair-{}", t.id), title: "Precise repair".into(),
                objective: "Consume authorized runtime configuration and deliver real checks".into(),
                repair_of: t.id.clone(), depends_on: vec![], read_paths: t.read_paths.clone(),
                write_paths: t.write_paths.clone(), acceptance_criteria: t.acceptance_criteria.clone(),
                required_checks: t.required_checks.clone(),
            }).collect(),
        };
        (store, request)
    }

    fn pending_cancel_fixture(name: &str) -> (MissionStore, PendingRepairCancellation) {
        let (store, mut follow) = native_follow_up_fixture(name);
        // Leave one existing repair-budget slot for the replacement; a
        // withdrawal never refunds the historical draft's budget count.
        follow.tasks.truncate(1);
        store.add_follow_up_tasks_for_mcp(&follow).unwrap();
        store.control(&follow.mission_id, "pause").unwrap();
        let cancel = PendingRepairCancellation {
            schema_version: "mission.task.cancel_pending.v1".into(),
            mission_id: follow.mission_id, expected_plan_sha256: follow.expected_plan_sha256,
            task_id: "repair-fix".into(), request_id: "user-scope-adjustment-1".into(),
            reason: "User explicitly deferred two acceptance items; replace the unstarted repair".into(),
        };
        (store, cancel)
    }

    fn pending_cancel_tables(store: &MissionStore) -> Vec<Vec<Vec<String>>> {
        let raw = Connection::open(&store.path).unwrap();
        ["mission_runs", "mission_tasks", "mission_events", "mission_meta", "mission_workspace_claims"]
            .iter().map(|name| {
                let mut statement = raw.prepare(&format!("SELECT * FROM {name} ORDER BY rowid")).unwrap();
                let count = statement.column_count();
                statement.query_map([], |row| Ok((0..count).map(|i|
                    format!("{:?}", row.get_ref(i).unwrap())).collect::<Vec<_>>()))
                    .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
            }).collect()
    }

    #[test]
    fn pending_repair_cancel_preserves_delivery_history_and_allows_exact_replacement() {
        let (store, cancel) = pending_cancel_fixture("pending-cancel-replace");
        let before = store.snapshot(&cancel.mission_id, 0).unwrap();
        let after = store.cancel_pending_repair_request(&cancel).unwrap();
        assert_eq!(after.status, MissionStatus::Waiting);
        assert_eq!(after.plan_sha256, before.mission.plan_sha256);
        assert_eq!(after.assignment, before.mission.assignment);
        assert_eq!(after.confirmation, before.mission.confirmation);
        assert_eq!(after.budget, before.mission.budget);
        for original in &before.mission.tasks {
            let now = after.tasks.iter().find(|t| t.spec.id == original.spec.id).unwrap();
            assert_eq!(now.spec, original.spec);
            if original.spec.id == cancel.task_id { assert_eq!(now.status, TaskStatus::Canceled); }
            else { assert_eq!(now, original); }
        }
        let snapshot = store.snapshot(&cancel.mission_id, 0).unwrap();
        assert_eq!(&snapshot.events[..before.events.len()], &before.events);
        assert_eq!(snapshot.events.last().unwrap().event_type, "pending_repair_canceled");
        // The canceled draft cannot release its original failed delivery.
        assert_eq!(after.tasks.iter().find(|t| t.spec.id == "fix").unwrap().status, TaskStatus::Failed);
        let (_, mut replacement) = native_follow_up_fixture("pending-cancel-template");
        replacement.mission_id = cancel.mission_id.clone();
        replacement.expected_plan_sha256 = cancel.expected_plan_sha256.clone();
        replacement.tasks.truncate(1);
        replacement.tasks[0].id = "scope-corrected-repair".into();
        store.add_follow_up_tasks_for_mcp(&replacement).unwrap();
        store.control(&cancel.mission_id, "resume").unwrap();
        let tables = pending_cancel_tables(&store);
        let replay = store.cancel_pending_repair_request(&cancel).unwrap();
        assert_eq!(replay.status, MissionStatus::Active);
        assert_eq!(pending_cancel_tables(&store), tables);
    }

    #[test]
    fn pending_repair_cancel_rejects_live_terminal_started_original_and_stale_without_writes() {
        for status in ["active", "completed", "failed", "canceled", "planned"] {
            let (store, cancel) = pending_cancel_fixture(&format!("pending-mission-{status}"));
            Connection::open(&store.path).unwrap().execute("UPDATE mission_runs SET status=?2 WHERE mission_id=?1",
                params![cancel.mission_id, status]).unwrap();
            let before = pending_cancel_tables(&store);
            assert!(store.cancel_pending_repair_request(&cancel).is_err(), "{status}");
            assert_eq!(pending_cancel_tables(&store), before, "{status}");
        }
        for field in ["running", "awaiting_verification", "completed", "failed", "canceled", "attempt", "verification", "outcome", "evidence", "original", "stale", "unknown"] {
            let (store, mut cancel) = pending_cancel_fixture(&format!("pending-target-{field}"));
            let raw = Connection::open(&store.path).unwrap();
            match field {
                "attempt" => { raw.execute("UPDATE mission_tasks SET attempt_run_id='old-run' WHERE task_id='repair-fix'", []).unwrap(); }
                "verification" => { raw.execute("UPDATE mission_tasks SET verification_run_id='old-verify' WHERE task_id='repair-fix'", []).unwrap(); }
                "outcome" => { raw.execute("UPDATE mission_tasks SET verification_passed=0 WHERE task_id='repair-fix'", []).unwrap(); }
                "evidence" => { raw.execute("UPDATE mission_tasks SET evidence_json='[\"prior-evidence\"]' WHERE task_id='repair-fix'", []).unwrap(); }
                "original" => cancel.task_id = "fix".into(),
                "stale" => cancel.expected_plan_sha256 = "0".repeat(64),
                "unknown" => cancel.task_id = "missing".into(),
                status => { raw.execute("UPDATE mission_tasks SET status=?1 WHERE task_id='repair-fix'", params![status]).unwrap(); }
            }
            let before = pending_cancel_tables(&store);
            assert!(store.cancel_pending_repair_request(&cancel).is_err(), "{field}");
            assert_eq!(pending_cancel_tables(&store), before, "{field}");
        }
    }

    #[test]
    fn pending_repair_cancel_rejects_dependents_until_leaf_is_withdrawn() {
        let (store, mut follow) = native_follow_up_fixture("pending-dependent");
        follow.tasks[1].depends_on = vec!["repair-fix".into()];
        store.add_follow_up_tasks_for_mcp(&follow).unwrap();
        store.control(&follow.mission_id, "pause").unwrap();
        let mut cancel = PendingRepairCancellation { schema_version:"mission.task.cancel_pending.v1".into(),
            mission_id:follow.mission_id, expected_plan_sha256:follow.expected_plan_sha256,
            task_id:"repair-fix".into(), request_id:"first".into(), reason:"Replace obsolete draft".into() };
        let before = pending_cancel_tables(&store);
        assert!(store.cancel_pending_repair_request(&cancel).unwrap_err().to_string().contains("dependents"));
        assert_eq!(pending_cancel_tables(&store), before);
        cancel.task_id = "repair-other".into();
        store.cancel_pending_repair_request(&cancel).unwrap();
        cancel.task_id = "repair-fix".into(); cancel.request_id = "second".into();
        store.cancel_pending_repair_request(&cancel).unwrap();
    }

    #[test]
    fn pending_repair_cancel_is_transactional_and_request_conflicts_do_not_mutate() {
        let (store, cancel) = pending_cancel_fixture("pending-event-rollback");
        let raw = Connection::open(&store.path).unwrap();
        raw.execute_batch("CREATE TRIGGER fail_pending_cancel BEFORE INSERT ON mission_events
            WHEN NEW.event_type='pending_repair_canceled' BEGIN SELECT RAISE(ABORT,'forced cancellation event failure'); END;").unwrap();
        let before = pending_cancel_tables(&store);
        assert!(store.cancel_pending_repair_request(&cancel).is_err());
        assert_eq!(pending_cancel_tables(&store), before);
        raw.execute_batch("DROP TRIGGER fail_pending_cancel;").unwrap();
        store.cancel_pending_repair_request(&cancel).unwrap();
        let before = pending_cancel_tables(&store);
        for field in ["reason", "task", "request"] {
            let mut conflict = cancel.clone();
            match field {
                "reason" => conflict.reason.push_str(" changed"),
                "task" => conflict.task_id = "repair-other".into(),
                _ => conflict.request_id.push_str("-another"),
            }
            assert!(store.cancel_pending_repair_request(&conflict).is_err(), "{field}");
            assert_eq!(pending_cancel_tables(&store), before, "{field}");
        }
    }

    #[test]
    fn pending_repair_cancel_serializes_identical_concurrent_requests() {
        let (store, cancel) = pending_cancel_fixture("pending-concurrent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = (0..2).map(|_| {
            let store = store.clone(); let cancel = cancel.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || { barrier.wait(); store.cancel_pending_repair_request(&cancel).map_err(|e| e.to_string()) })
        }).collect::<Vec<_>>();
        for handle in handles { handle.join().unwrap().unwrap(); }
        assert_eq!(store.snapshot(&cancel.mission_id, 0).unwrap().events.iter()
            .filter(|e| e.event_type == "pending_repair_canceled").count(), 1);
    }

    #[test]
    fn pending_repair_cancel_rejects_claims_without_legacy_recovery_or_other_writes() {
        let (store, cancel) = pending_cancel_fixture("pending-claims");
        let raw = Connection::open(&store.path).unwrap();
        raw.execute("INSERT INTO mission_workspace_claims(workspace_key,mission_id,task_id,run_id,acquired_at)
            VALUES ('fixture-key',?1,'repair-other','claimed-run','2026-11-13T00:00:00Z')",
            params![cancel.mission_id]).unwrap();
        let before = pending_cancel_tables(&store);
        assert!(store.cancel_pending_repair_request(&cancel).unwrap_err().to_string().contains("claims"));
        assert_eq!(pending_cancel_tables(&store), before);
    }

    #[test]
    fn pending_repair_cancel_typed_input_rejects_authority_fields_and_missing_database() {
        let (store, cancel) = pending_cancel_fixture("pending-typed");
        let valid = serde_json::to_value(&cancel).unwrap();
        for field in ["execution", "approved_paths", "confirmed_by", "scope", "schema_version", "task_id", "reason"] {
            let mut invalid = valid.clone();
            invalid[field] = match field {
                "schema_version" => serde_json::json!("mission.task.cancel_pending.v2"),
                "task_id" => serde_json::json!("../outside"),
                "reason" => serde_json::json!(""),
                _ => serde_json::json!({"unauthorized":true}),
            };
            let before = pending_cancel_tables(&store);
            assert!(store.cancel_pending_repair_for_mcp(&invalid).is_err(), "{field}");
            assert_eq!(pending_cancel_tables(&store), before);
        }
        let missing = store.path.with_extension("absent.sqlite3");
        assert!(!missing.exists());
        assert!(MissionStore::new(&missing).cancel_pending_repair_for_mcp(&valid).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn mission_follow_up_keeps_assigned_expert_executor_service_model_and_role() {
        let _lock = crate::store::paths::test_env_lock();
        let (store, workflows, pin, mut assignment, _home) = assignment_fixture("native-assignment");
        let mut input = imported_handoff(&pin);
        input.plan_mode = MissionPlanMode::Generated; input.imported_plan = None;
        let mission = store.create_handoff_with_store(&input, &workflows).unwrap();
        assignment.task_roles.clear();
        store.assign_roles_with_store(&mission.mission_id, &assignment, &workflows).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.tasks[0].execution.role_id = "ui.implementer.shared".into();
        let planned = store.propose_plan(&mission.mission_id, &plan).unwrap();
        let confirmed = store.confirm_plan(&mission.mission_id, &planned.plan_sha256,
            &["fix".into()], &["frontend/src".into()], &[], &[], "test-user").unwrap();
        let parent = confirmed.tasks[0].spec.clone();
        assert!(parent.execution.expert.is_some());
        assert!(!parent.execution.service_id.is_empty());
        assert!(!parent.execution.model.is_empty());
        store.begin_task_attempt(&mission.mission_id, "fix", "native-assigned-failed").unwrap();
        store.complete_task_attempt(&mission.mission_id, "fix", "native-assigned-failed", false, &[], "failed").unwrap();
        let request = super::super::follow_up::MissionFollowUpAdd {
            schema_version: super::super::follow_up::FollowUpSchemaVersion::V1,
            mission_id: mission.mission_id.clone(), expected_plan_sha256: planned.plan_sha256,
            reason: "Explicit assigned follow-up".into(),
            read_scope_extension: None,
            tasks: vec![super::super::follow_up::MissionFollowUpTask {
                id: "native-assigned-repair".into(), title: "Precise correction".into(), objective: "Use authorized resources".into(),
                repair_of: "fix".into(), depends_on: vec![], read_paths: parent.read_paths.clone(),
                write_paths: parent.write_paths.clone(), acceptance_criteria: parent.acceptance_criteria.clone(),
                required_checks: parent.required_checks.clone(),
            }],
        };
        let result = store.add_follow_up_tasks_for_mcp(&request).unwrap();
        assert_eq!(result.assignment, confirmed.assignment);
        assert_eq!(result.tasks.iter().find(|t| t.spec.id == "native-assigned-repair").unwrap().spec.execution, parent.execution);
    }


    fn native_read_extension_fixture(name: &str) -> (MissionStore, super::super::follow_up::MissionFollowUpAdd, PathBuf) {
        // canonicalize() produces a verbatim \\?\ path on Windows, which is
        // intentionally rejected by the public workspace-root contract.
        let root = std::env::temp_dir()
            .join(format!("himind-read-extension-{name}-{}", rand::random::<u64>()));
        assert!(root.is_absolute());
        for dir in ["runtime-profiles", "contracts", "workflows"] { fs::create_dir_all(root.join(dir)).unwrap(); }
        fs::write(root.join("runtime-profiles/config.json"), b"{}").unwrap();
        let (store, mut request) = native_follow_up_fixture_at(name, Some(&root));
        request.tasks[0].read_paths.push("runtime-profiles/config.json".into());
        request.tasks[1].read_paths.extend(["contracts".into(), "workflows".into()]);
        request.read_scope_extension = Some(super::super::follow_up::ReadScopeExtension {
            paths: vec!["runtime-profiles/config.json".into(), "contracts".into(), "workflows".into()],
            authorized_by: "user:existing-explicit-permission".into(),
            reason: "Independent QA3 needs the real compilation inputs".into(),
        });
        store.control(&request.mission_id, "pause").unwrap();
        (store, request, root)
    }

    #[test]
    fn mission_follow_up_read_extension_preserves_original_plan_failed_history_and_verification_gate() {
        let (store, request, root) = native_read_extension_fixture("read-preserve");
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        let updated = store.add_follow_up_tasks_for_mcp(&request).unwrap();
        assert_eq!(updated.status, MissionStatus::Waiting);
        assert_eq!(updated.workspace, before.mission.workspace);
        assert_eq!(updated.confirmation, before.mission.confirmation);
        assert_eq!(updated.plan_sha256, before.mission.plan_sha256);
        assert_eq!(updated.plan_revision, before.mission.plan_revision);
        assert_eq!(updated.proposed_tasks, before.mission.proposed_tasks);
        assert_eq!(updated.required_acceptance, before.mission.required_acceptance);
        assert_eq!(updated.assignment, before.mission.assignment);
        assert_eq!(updated.budget, before.mission.budget);
        assert_eq!(updated.usage, before.mission.usage);
        assert_eq!(&updated.tasks[..before.mission.tasks.len()], before.mission.tasks.as_slice());
        assert!(updated.tasks[..2].iter().all(|t| t.status == TaskStatus::Failed && !t.attempt_run_id.is_empty()));
        for task in &request.tasks {
            let repair = updated.tasks.iter().find(|t| t.spec.id == task.id).unwrap();
            let parent = before.mission.tasks.iter().find(|t| t.spec.id == task.repair_of).unwrap();
            assert_eq!(repair.spec.read_paths, task.read_paths);
            assert_eq!(repair.spec.write_paths, parent.spec.write_paths);
            assert_eq!(repair.spec.required_checks, parent.spec.required_checks);
            assert_eq!(repair.spec.execution, parent.spec.execution);
        }
        let after = store.snapshot(&request.mission_id, 0).unwrap();
        assert_eq!(&after.events[..before.events.len()], before.events.as_slice());
        let receipts = after.events.iter().filter(|e| e.event_type == "follow_up_tasks_added").collect::<Vec<_>>();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].payload["follow_up_request"], serde_json::to_value(&request).unwrap());
        assert!(receipts[0].summary.contains("只读"));
        store.control(&request.mission_id, "resume").unwrap();
        store.begin_task_attempt(&request.mission_id, "repair-fix", "extended-implementation").unwrap();
        assert!(store.complete_task_attempt(&request.mission_id, "repair-fix", "extended-implementation", true,
            &["implementation:self-report".into()], "must not bypass independent host verification").is_err());
        store.await_task_verification(&request.mission_id, "repair-fix", "extended-implementation", "real independent checks required").unwrap();
        assert!(store.begin_verification_attempt(&request.mission_id, "repair-fix", "extended-implementation").is_err());
        let awaiting = store.get(&request.mission_id).unwrap();
        assert_eq!(awaiting.tasks.iter().find(|t| t.spec.id == "repair-fix").unwrap().status, TaskStatus::AwaitingVerification);
        assert_eq!(awaiting.tasks.iter().find(|t| t.spec.id == "fix").unwrap().status, TaskStatus::Failed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_read_extension_requires_exact_used_delta_and_retains_all_guards_atomically() {
        let (store, request, root) = native_read_extension_fixture("read-invalid");
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        for field in ["absent", "unused", "parent_grant", "wider_directory", "omitted", "non_normal_read", "write", "checks", "graph", "stale"] {
            let mut invalid = request.clone();
            match field {
                "absent" => invalid.read_scope_extension = None,
                "unused" => invalid.read_scope_extension.as_mut().unwrap().paths.push("unused".into()),
                "parent_grant" => invalid.read_scope_extension.as_mut().unwrap().paths.push("frontend".into()),
                "wider_directory" => invalid.read_scope_extension.as_mut().unwrap().paths[0] = "runtime-profiles".into(),
                "omitted" => { invalid.read_scope_extension.as_mut().unwrap().paths.pop(); },
                "non_normal_read" => invalid.tasks[0].read_paths[1] = "runtime-profiles//config.json".into(),
                "write" => invalid.tasks[0].write_paths.push("contracts".into()),
                "checks" => { invalid.tasks[0].required_checks.pop(); },
                "graph" => invalid.tasks[1].depends_on.push("missing".into()),
                _ => invalid.expected_plan_sha256 = "0".repeat(64),
            }
            assert!(store.add_follow_up_tasks_for_mcp(&invalid).is_err(), "{field}");
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before, "{field}");
        }
        // A fresh grant must name existing inputs; replay has different rules.
        fs::remove_file(root.join("runtime-profiles/config.json")).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&request).is_err());
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_read_extension_requires_waiting_and_drained_work_from_fresh_transaction() {
        for (status, task_status) in [("active", "failed"), ("planned", "failed"), ("waiting", "running"), ("waiting", "awaiting_verification")] {
            let (store, mut request, root) = native_read_extension_fixture("read-activity");
            // Keep the changing task outside this repair batch: a running
            // repair target is rejected by the earlier failed-leaf guard.
            request.tasks.truncate(1);
            request.read_scope_extension.as_mut().unwrap().paths.truncate(1);
            // Simulate status changing after approval was reviewed. The apply
            // operation must load the new state in its write transaction.
            let reviewed = store.get(&request.mission_id).unwrap();
            assert_eq!(reviewed.status, MissionStatus::Waiting);
            let raw = Connection::open(&store.path).unwrap();
            raw.execute("UPDATE mission_runs SET status=?2 WHERE mission_id=?1", params![request.mission_id, status]).unwrap();
            raw.execute("UPDATE mission_tasks SET status=?2 WHERE mission_id=?1 AND task_id='other'", params![request.mission_id, task_status]).unwrap();
            let before = store.snapshot(&request.mission_id, 0).unwrap();
            let error = store.add_follow_up_tasks_for_mcp(&request).unwrap_err().to_string();
            assert!(error.contains("requires waiting"), "{status}/{task_status}: {error}");
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mission_follow_up_read_extension_exact_replay_precedes_activity_filesystem_and_budget_checks() {
        let (store, request, root) = native_read_extension_fixture("read-replay");
        store.add_follow_up_tasks_for_mcp(&request).unwrap();
        fs::remove_dir_all(root).unwrap();
        store.control(&request.mission_id, "resume").unwrap();
        store.begin_task_attempt(&request.mission_id, "repair-fix", "extended-live-repair").unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        let reopened = MissionStore::new(store.path.clone());
        assert_eq!(reopened.add_follow_up_tasks_for_mcp(&request).unwrap(), before.mission);
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        for field in ["paths", "authorized_by", "reason", "absent", "stale"] {
            let mut conflict = request.clone();
            match field {
                "paths" => conflict.read_scope_extension.as_mut().unwrap().paths.push("missing".into()),
                "authorized_by" => conflict.read_scope_extension.as_mut().unwrap().authorized_by.push_str(" changed"),
                "reason" => conflict.read_scope_extension.as_mut().unwrap().reason.push(' '),
                "absent" => conflict.read_scope_extension = None,
                _ => conflict.expected_plan_sha256 = "0".repeat(64),
            }
            let error = reopened.add_follow_up_tasks_for_mcp(&conflict).unwrap_err().to_string();
            assert!(if field == "stale" { error.contains("confirmed plan") } else { error.contains("conflicting") }, "{field}: {error}");
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        }
        store.control(&request.mission_id, "cancel").unwrap();
        let canceled = store.snapshot(&request.mission_id, 0).unwrap();
        assert_eq!(reopened.add_follow_up_tasks_for_mcp(&request).unwrap(), canceled.mission);
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), canceled);
    }

    #[test]
    fn mission_follow_up_read_extension_rolls_back_entire_batch_and_authorization_receipt() {
        let (store, request, root) = native_read_extension_fixture("read-rollback");
        store.connection().unwrap().execute_batch("CREATE TRIGGER reject_extended_receipt BEFORE INSERT ON mission_events
            WHEN NEW.event_type='follow_up_tasks_added' BEGIN SELECT RAISE(ABORT,'extension receipt failure'); END;").unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&request).unwrap_err().to_string().contains("extension receipt failure"));
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(unix, windows))]
    fn create_read_extension_directory_link(target: &std::path::Path, link: &std::path::Path) {
        #[cfg(unix)] std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)] {
            // Directory junctions exercise FILE_ATTRIBUTE_REPARSE_POINT without
            // requiring the symbolic-link privilege on a Windows test host.
            let output = std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"]).arg(link).arg(target).output().unwrap();
            assert!(output.status.success(), "junction fixture failed: {}", String::from_utf8_lossy(&output.stderr));
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn mission_follow_up_read_extension_rejects_linked_ancestors_inside_and_outside_root() {
        for escape in [false, true] {
            let (store, request, root) = native_read_extension_fixture("read-link");
            let target = if escape { root.with_extension("outside") } else { root.join("real-inputs") };
            fs::rename(root.join("runtime-profiles"), &target).unwrap();
            let link = root.join("runtime-profiles");
            create_read_extension_directory_link(&target, &link);
            let before = store.snapshot(&request.mission_id, 0).unwrap();
            assert!(store.add_follow_up_tasks_for_mcp(&request).unwrap_err().to_string().contains("symlinks and reparse points"));
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
            #[cfg(unix)] fs::remove_file(&link).unwrap();
            #[cfg(windows)] fs::remove_dir(&link).unwrap();
            fs::remove_dir_all(root).unwrap();
            if escape { fs::remove_dir_all(target).unwrap(); }
        }
    }

    #[test]
    #[cfg(unix)]
    fn mission_follow_up_read_extension_rejects_symlink_leaf_even_with_internal_target() {
        let (store, request, root) = native_read_extension_fixture("read-leaf-link");
        let leaf = root.join("runtime-profiles/config.json");
        fs::rename(&leaf, root.join("real-config.json")).unwrap();
        std::os::unix::fs::symlink(root.join("real-config.json"), &leaf).unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&request).is_err());
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_preserves_plan_history_scope_binding_and_checks() {
        let (store, request) = native_follow_up_fixture("native-preserve");
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        let updated = store.add_follow_up_tasks_for_mcp(&request).unwrap();
        assert_eq!(updated.mission_id, before.mission.mission_id);
        assert_eq!(updated.objective, before.mission.objective);
        assert_eq!(updated.plan_sha256, before.mission.plan_sha256);
        assert_eq!(updated.plan_revision, before.mission.plan_revision);
        assert_eq!(updated.proposed_tasks, before.mission.proposed_tasks);
        assert_eq!(updated.required_acceptance, before.mission.required_acceptance);
        assert_eq!(updated.acceptance_criteria, before.mission.acceptance_criteria);
        assert_eq!(updated.confirmation, before.mission.confirmation);
        assert_eq!(updated.assignment, before.mission.assignment);
        assert_eq!(updated.workspace, before.mission.workspace);
        assert_eq!(updated.budget, before.mission.budget);
        assert_eq!(updated.usage, before.mission.usage);
        assert_eq!(&updated.tasks[..before.mission.tasks.len()], before.mission.tasks.as_slice());
        for task in &request.tasks {
            let repair = updated.tasks.iter().find(|t| t.spec.id == task.id).unwrap();
            let parent = before.mission.tasks.iter().find(|t| t.spec.id == task.repair_of).unwrap();
            assert_eq!(repair.spec.execution, parent.spec.execution);
            assert_eq!(repair.spec.required_checks, parent.spec.required_checks);
            assert_eq!(repair.spec.objective, task.objective);
        }
        let after = store.snapshot(&request.mission_id, 0).unwrap();
        assert_eq!(&after.events[..before.events.len()], before.events.as_slice());
        let added = after.events.iter().filter(|e| e.event_type == "follow_up_tasks_added").collect::<Vec<_>>();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].payload["follow_up_request"], serde_json::to_value(&request).unwrap());
    }

    #[test]
    fn mission_follow_up_replay_returns_current_state_after_progress_and_exhaustion() {
        let (store, request) = native_follow_up_fixture("native-replay");
        store.add_follow_up_tasks_for_mcp(&request).unwrap();
        store.begin_task_attempt(&request.mission_id, "repair-fix", "repair-failed").unwrap();
        store.complete_task_attempt(&request.mission_id, "repair-fix", "repair-failed", false, &[], "still failed").unwrap();
        let mut next = request.clone(); next.tasks.truncate(1);
        next.tasks[0].id = "repair-next".into(); next.tasks[0].repair_of = "repair-fix".into();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&next).unwrap_err().to_string().contains("repair budget"));
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        let reopened = MissionStore::new(store.path.clone());
        assert_eq!(reopened.add_follow_up_tasks_for_mcp(&request).unwrap(), before.mission);
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        store.control(&request.mission_id, "cancel").unwrap();
        let canceled = store.snapshot(&request.mission_id, 0).unwrap();
        assert_eq!(reopened.add_follow_up_tasks_for_mcp(&request).unwrap(), canceled.mission);
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), canceled);
    }

    #[test]
    fn mission_follow_up_rejects_conflicting_partial_and_legacy_replays_without_writes() {
        let (store, request) = native_follow_up_fixture("native-conflict");
        let mut first = request.clone(); first.tasks.truncate(1);
        store.add_follow_up_tasks_for_mcp(&first).unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        for field in ["objective", "reason", "title", "checks", "partial"] {
            let mut changed = first.clone();
            match field {
                "objective" => changed.tasks[0].objective.push_str(" changed"),
                "reason" => changed.reason.push(' '),
                "title" => changed.tasks[0].title.push('!'),
                "checks" => changed.tasks[0].required_checks.reverse(),
                _ => changed = request.clone(),
            }
            assert!(store.add_follow_up_tasks_for_mcp(&changed).is_err(), "{field}");
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        }
        let mut legacy = request.clone(); legacy.tasks.remove(0);
        let parent = before.mission.tasks.iter().find(|t| t.spec.id == "other").unwrap();
        let spec = legacy.tasks[0].bind(parent.spec.execution.clone());
        store.add_follow_up_tasks(&legacy.mission_id, &[spec], &legacy.reason).unwrap();
        let before = store.snapshot(&legacy.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&legacy).unwrap_err().to_string().contains("conflicting"));
        assert_eq!(store.snapshot(&legacy.mission_id, 0).unwrap(), before);
    }

    #[test]
    fn mission_follow_up_replay_and_rejection_do_not_backfill_legacy_claims() {
        let (store, request) = native_follow_up_fixture("native-no-initialization-write");
        store.add_follow_up_tasks_for_mcp(&request).unwrap();
        store.begin_task_attempt(&request.mission_id, "repair-fix", "active-repair").unwrap();
        // Raw connections are deliberate: store.get/snapshot would itself
        // trigger the existing, independently supported legacy claim recovery.
        let raw = Connection::open(&store.path).unwrap();
        raw.execute("DELETE FROM mission_workspace_claims", []).unwrap();
        let tables = || {
            ["mission_runs", "mission_tasks", "mission_events", "mission_meta", "mission_workspace_claims"]
                .iter().map(|name| {
                    let mut statement = raw.prepare(&format!("SELECT * FROM {name} ORDER BY rowid")).unwrap();
                    let count = statement.column_count();
                    statement.query_map([], |row| Ok((0..count).map(|i|
                        format!("{:?}", row.get_ref(i).unwrap())).collect::<Vec<_>>()))
                        .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
                }).collect::<Vec<_>>()
        };
        let before = tables();
        assert!(store.add_follow_up_tasks_for_mcp(&request).is_ok());
        assert_eq!(tables(), before);
        let mut conflict = request.clone(); conflict.reason.push_str(" changed");
        assert!(store.add_follow_up_tasks_for_mcp(&conflict).is_err());
        assert_eq!(tables(), before);
        let mut stale = request; stale.expected_plan_sha256 = "0".repeat(64);
        assert!(store.add_follow_up_tasks_for_mcp(&stale).is_err());
        assert_eq!(tables(), before);
        // Prove the missing claim was a real supported legacy-recovery case.
        drop(store.connection().unwrap());
        assert_eq!(raw.query_row("SELECT COUNT(*) FROM mission_workspace_claims", [],
            |row| row.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn mission_follow_up_rejects_stale_plan_leaf_scope_checks_and_graph_atomically() {
        let (store, request) = native_follow_up_fixture("native-guards");
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        for field in ["hash", "read", "absolute_read", "write", "absolute_write", "checks", "graph", "cycle", "parent"] {
            let mut invalid = request.clone();
            match field {
                "hash" => invalid.expected_plan_sha256 = "0".repeat(64),
                "read" => invalid.tasks[1].read_paths = vec!["private".into()],
                "absolute_read" => invalid.tasks[1].read_paths = vec!["C:/private/input.txt".into()],
                "write" => invalid.tasks[1].write_paths = vec!["backend".into()],
                "absolute_write" => invalid.tasks[1].write_paths = vec!["C:/private/output.txt".into()],
                "checks" => invalid.tasks[1].required_checks.pop().map(|_| ()).unwrap(),
                "graph" => invalid.tasks[1].depends_on = vec!["missing".into()],
                "cycle" => { invalid.tasks[0].depends_on = vec![invalid.tasks[1].id.clone()]; invalid.tasks[1].depends_on = vec![invalid.tasks[0].id.clone()]; }
                _ => invalid.tasks[1].repair_of = "missing".into(),
            }
            assert!(store.add_follow_up_tasks_for_mcp(&invalid).is_err(), "{field}");
            assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
        }
        let mut first = request.clone(); first.tasks.truncate(1);
        store.add_follow_up_tasks_for_mcp(&first).unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        let mut sibling = first; sibling.tasks[0].id = "parallel-repair".into();
        assert!(store.add_follow_up_tasks_for_mcp(&sibling).unwrap_err().to_string().contains("failed repair leaf"));
        let mut ready = request; ready.tasks.truncate(1);
        ready.tasks[0].id = "repair-ready-child".into(); ready.tasks[0].repair_of = "repair-fix".into();
        assert!(store.add_follow_up_tasks_for_mcp(&ready).unwrap_err().to_string().contains("failed repair leaf"));
        assert_eq!(store.snapshot(&ready.mission_id, 0).unwrap(), before);
    }

    #[test]
    fn mission_follow_up_requires_current_confirmation_even_on_replay() {
        let (store, request) = native_follow_up_fixture("native-confirmation");
        store.add_follow_up_tasks_for_mcp(&request).unwrap();
        let mut plan = proposal(); plan.summary = "A new review revision".into();
        store.propose_plan(&request.mission_id, &plan).unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&request).is_err());
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
    }

    #[test]
    fn mission_follow_up_rolls_back_tasks_when_event_insert_fails() {
        let (store, request) = native_follow_up_fixture("native-rollback");
        // This trigger is installed only in the isolated unit-test database.
        store.connection().unwrap().execute_batch("CREATE TRIGGER reject_native_receipt BEFORE INSERT ON mission_events
            WHEN NEW.event_type='follow_up_tasks_added' BEGIN SELECT RAISE(ABORT,'receipt failure'); END;").unwrap();
        let before = store.snapshot(&request.mission_id, 0).unwrap();
        assert!(store.add_follow_up_tasks_for_mcp(&request).unwrap_err().to_string().contains("receipt failure"));
        assert_eq!(store.snapshot(&request.mission_id, 0).unwrap(), before);
    }

    #[test]
    fn mission_follow_up_receipt_is_not_hidden_by_event_pagination() {
        let (store, request) = native_follow_up_fixture("native-receipt-pages");
        store.add_follow_up_tasks_for_mcp(&request).unwrap();
        let mut connection = store.connection().unwrap();
        let tx = connection.transaction().unwrap();
        for _ in 0..10_001 {
            append_event_tx(&tx, &request.mission_id, "fixture_observation", "", "test-only observation",
                serde_json::json!({}), &now_stamp()).unwrap();
        }
        tx.commit().unwrap();
        let event_count = || store.connection().unwrap().query_row(
            "SELECT COUNT(*) FROM mission_events WHERE mission_id=?1", params![request.mission_id],
            |row| row.get::<_, i64>(0)).unwrap();
        let before = store.get(&request.mission_id).unwrap();
        let count = event_count();
        assert_eq!(store.add_follow_up_tasks_for_mcp(&request).unwrap(), before);
        assert_eq!(event_count(), count);
    }

    #[test]
    fn mission_follow_up_concurrent_identical_and_conflicting_requests_serialize() {
        for conflict in [false, true] {
            let (store, request) = native_follow_up_fixture("native-race");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let handles = (0..2).map(|i| {
                let store = store.clone(); let barrier = barrier.clone(); let mut request = request.clone();
                if conflict && i == 1 { request.tasks[0].objective.push_str(" conflicting"); }
                std::thread::spawn(move || {
                    barrier.wait();
                    store.add_follow_up_tasks_for_mcp(&request).map_err(|e| e.to_string())
                })
            }).collect::<Vec<_>>();
            barrier.wait();
            let results = handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>();
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), if conflict { 1 } else { 2 });
            let after = store.snapshot(&request.mission_id, 0).unwrap();
            assert_eq!(after.mission.tasks.len(), 4);
            assert_eq!(after.events.iter().filter(|e| e.event_type == "follow_up_tasks_added").count(), 1);
        }
    }

    fn feedback_mission(store: &MissionStore) -> MissionView {
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".into(), "verify".into()], &["frontend/src".into()], &[], &[], "review-user").unwrap()
    }

    #[test]
    fn executor_legacy_claim_restore_rechecks_current_task_atomically() {
        let store=store("legacy-claim-race");let mission=feedback_mission(&store);
        store.begin_task_attempt(&mission.mission_id,"fix","current-run").unwrap();
        let db=store.connection().unwrap();
        db.execute("DELETE FROM mission_workspace_claims WHERE mission_id=?1",[&mission.mission_id]).unwrap();
        assert_eq!(restore_legacy_claim(&db,&mission.workspace.root,&mission.mission_id,"fix","stale-run","old").unwrap(),0);
        assert_eq!(restore_legacy_claim(&db,&mission.workspace.root,&mission.mission_id,"fix","current-run","old").unwrap(),1);
        store.complete_task_attempt(&mission.mission_id,"fix","current-run",false,&[],"original failure").unwrap();
        assert_eq!(restore_legacy_claim(&db,&mission.workspace.root,&mission.mission_id,"fix","current-run","old").unwrap(),0);
        assert!(store.workspace_claim(&mission.workspace.root).unwrap().is_none());
    }

    fn feedback(mission: &MissionView, request_id: &str, target: &str) -> MissionFeedbackInput {
        MissionFeedbackInput { request_id: request_id.into(), expected_plan_sha256: mission.plan_sha256.clone(),
            target_task_ids: vec![target.into()], message: "P1: recheck the output\nagainst the source".into(),
            evidence_refs: vec!["C:/unapproved/private/evidence.txt".into()] }
    }

    #[test]
    fn mission_feedback_is_append_only_idempotent_and_preserves_running_authority() {
        let store = store("feedback-submit");
        let active = feedback_mission(&store);
        let running = store.begin_task_attempt(&active.mission_id, "fix", "feedback-running").unwrap();
        let claim = store.workspace_claim(&running.workspace.root).unwrap();
        let before = store.snapshot(&active.mission_id, 0).unwrap();
        let input = feedback(&running, "codex-review-1", "fix");
        assert_eq!(feedback_state(&store.submit_feedback(&active.mission_id, &input).unwrap()), feedback_state(&running));
        assert_eq!(feedback_state(&store.submit_feedback(&active.mission_id, &input).unwrap()), feedback_state(&running));
        assert_eq!(store.workspace_claim(&running.workspace.root).unwrap(), claim);
        let after = store.snapshot(&active.mission_id, 0).unwrap();
        assert_eq!(feedback_state(&after.mission), feedback_state(&before.mission));
        assert_eq!(after.mission.effective_wait.as_ref().unwrap().cursor, after.cursor);
        assert_eq!(&after.events[..before.events.len()], before.events.as_slice());
        assert_eq!(after.events.len(), before.events.len() + 1);
        let event = after.events.last().unwrap();
        assert_eq!(event.event_type, "mission_feedback_submitted");
        assert_eq!(event.summary, FEEDBACK_SUMMARY);
        assert!(!event.summary.contains("P1"));
        assert_eq!(event.payload["input"], serde_json::to_value(&input).unwrap());
        assert_eq!(event.payload["plan_sha256"], running.plan_sha256);
        assert_eq!(event.payload["payload_sha256"].as_str().unwrap().len(), 64);
        // Waiting also accepts observations without resuming the worker.
        let waiting = store.control(&active.mission_id, "pause").unwrap();
        assert_eq!(feedback_state(&store.submit_feedback(&active.mission_id, &feedback(&waiting, "paused-review", "verify")).unwrap()), feedback_state(&waiting));
    }

    #[test]
    fn mission_feedback_rejects_stale_unknown_canceled_terminal_and_unconfirmed_without_events() {
        let store = store("feedback-reject");
        let mission = feedback_mission(&store);
        let initial = store.snapshot(&mission.mission_id, 0).unwrap();
        let valid = feedback(&mission, "reject-review", "fix");
        for field in ["plan", "task", "duplicate"] {
            let mut input = valid.clone();
            match field {
                "plan" => input.expected_plan_sha256 = "0".repeat(64),
                "task" => input.target_task_ids = vec!["missing".into()],
                _ => input.target_task_ids.push("fix".into()),
            }
            assert!(store.submit_feedback(&mission.mission_id, &input).is_err());
        }
        assert_eq!(store.snapshot(&mission.mission_id, 0).unwrap(), initial);
        // Fixture state variants; never synthesize failure/verification events.
        let connection = store.connection().unwrap();
        connection.execute("UPDATE mission_tasks SET status='canceled' WHERE mission_id=?1 AND task_id='fix'",
            params![mission.mission_id]).unwrap();
        assert!(store.submit_feedback(&mission.mission_id, &valid).is_err());
        connection.execute("UPDATE mission_tasks SET status='completed' WHERE mission_id=?1 AND task_id='fix'",
            params![mission.mission_id]).unwrap();
        let completed_target = store.get(&mission.mission_id).unwrap();
        assert_eq!(store.submit_feedback(&mission.mission_id, &valid).unwrap(), completed_target);
        let count = store.snapshot(&mission.mission_id, 0).unwrap().events.len();
        for status in ["completed", "failed", "canceled", "draft", "planned", "awaiting_confirmation"] {
            connection.execute("UPDATE mission_runs SET status=?2 WHERE mission_id=?1", params![mission.mission_id, status]).unwrap();
            assert!(store.submit_feedback(&mission.mission_id, &valid).is_err(), "{status}");
        }
        connection.execute("UPDATE mission_runs SET status='active', confirmation_json=NULL WHERE mission_id=?1",
            params![mission.mission_id]).unwrap();
        assert!(store.submit_feedback(&mission.mission_id, &valid).is_err());
        connection.execute("UPDATE mission_runs SET confirmation_json=?2 WHERE mission_id=?1",
            params![mission.mission_id, serde_json::to_string(&MissionConfirmation {
                plan_sha256:"f".repeat(64), ..mission.confirmation.unwrap()
            }).unwrap()]).unwrap();
        assert!(store.submit_feedback(&mission.mission_id, &valid).is_err());
        assert_eq!(store.snapshot(&mission.mission_id, 0).unwrap().events.len(), count);
    }

    #[test]
    fn mission_feedback_request_conflicts_compare_all_fields_and_survive_reopen() {
        let store = store("feedback-conflict");
        let mission = feedback_mission(&store);
        let input = feedback(&mission, "stable-request", "fix");
        store.submit_feedback(&mission.mission_id, &input).unwrap();
        let reopened = MissionStore::new(&store.path);
        let before = reopened.snapshot(&mission.mission_id, 0).unwrap();
        for field in ["message", "refs", "targets"] {
            let mut changed = input.clone();
            match field {
                "message" => changed.message.push('!'),
                "refs" => changed.evidence_refs.push("new-reference".into()),
                _ => changed.target_task_ids.push("verify".into()),
            }
            assert!(reopened.submit_feedback(&mission.mission_id, &changed).unwrap_err()
                .to_string().contains("different payload"));
        }
        assert_eq!(feedback_state(&reopened.submit_feedback(&mission.mission_id, &input).unwrap()), feedback_state(&mission));
        assert_eq!(reopened.snapshot(&mission.mission_id, 0).unwrap(), before);
        let mut multiple = feedback(&mission, "ordered-targets", "fix");
        multiple.target_task_ids.push("verify".into());
        reopened.submit_feedback(&mission.mission_id, &multiple).unwrap();
        let recorded = reopened.snapshot(&mission.mission_id, 0).unwrap();
        multiple.target_task_ids.reverse();
        assert!(reopened.submit_feedback(&mission.mission_id, &multiple).unwrap_err()
            .to_string().contains("different payload"));
        assert_eq!(reopened.snapshot(&mission.mission_id, 0).unwrap(), recorded);
    }

    #[test]
    fn mission_feedback_concurrent_replay_records_once() {
        let store = store("feedback-concurrent");
        let mission = feedback_mission(&store);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads = (0..2).map(|_| {
            let store = store.clone(); let mission = mission.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.submit_feedback(&mission.mission_id, &feedback(&mission, "same-request", "fix"))
                    .map_err(|error| error.to_string())
            })
        }).collect::<Vec<_>>();
        for thread in threads { assert_eq!(feedback_state(&thread.join().unwrap().unwrap()), feedback_state(&mission)); }
        assert_eq!(store.snapshot(&mission.mission_id, 0).unwrap().events.iter()
            .filter(|event| event.event_type == "mission_feedback_submitted").count(), 1);
    }

    #[test]
    fn mission_feedback_context_matches_repair_ancestors_across_pages_and_isolates_other_targets_and_plans() {
        let store = store("feedback-ancestors");
        let mission = feedback_mission(&store);
        store.submit_feedback(&mission.mission_id, &feedback(&mission, "root-observation", "fix")).unwrap();
        store.begin_task_attempt(&mission.mission_id, "fix", "impl-root").unwrap();
        store.complete_task_attempt(&mission.mission_id, "fix", "impl-root", false, &[], "actual failure").unwrap();
        let mut repair = task("repair-1"); repair.repair_of = "fix".into();
        store.add_follow_up_tasks(&mission.mission_id, &[repair.clone()], "repair").unwrap();
        store.submit_feedback(&mission.mission_id, &feedback(&mission, "repair-observation", "repair-1")).unwrap();
        store.begin_task_attempt(&mission.mission_id, "repair-1", "impl-repair").unwrap();
        store.complete_task_attempt(&mission.mission_id, "repair-1", "impl-repair", false, &[], "actual failure").unwrap();
        repair.id = "repair-2".into(); repair.repair_of = "repair-1".into();
        store.add_follow_up_tasks(&mission.mission_id, &[repair], "repair again").unwrap();
        store.submit_feedback(&mission.mission_id, &feedback(&mission, "direct-observation", "repair-2")).unwrap();
        store.seed_test_progress(&mission.mission_id, 10_001).unwrap();
        for n in 0..12 {
            store.submit_feedback(&mission.mission_id, &feedback(&mission, &format!("peer-{n}"), "verify")).unwrap();
        }
        let mut old = feedback(&mission, "old-plan", "fix"); old.expected_plan_sha256 = "0".repeat(64);
        store.append_event(&mission.mission_id, "mission_feedback_submitted", "", FEEDBACK_SUMMARY,
            serde_json::json!({"request_id":old.request_id,"plan_sha256":old.expected_plan_sha256,"input":old})).unwrap();
        let other = feedback_mission(&store);
        store.submit_feedback(&other.mission_id, &feedback(&other, "other-mission", "fix")).unwrap();
        let context = store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["repair-2".into()]).unwrap();
        let ids = context["observations"].as_array().unwrap().iter().map(|entry|
            entry["request_id"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["direct-observation", "repair-observation", "root-observation"]);
        let root = store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["fix".into()]).unwrap();
        assert_eq!(root["observations"].as_array().unwrap().len(), 1, "descendant feedback must not flow backwards");
        assert!(store.feedback_context(&mission.mission_id, &"0".repeat(64), &["fix".into()]).is_err());
        assert!(store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["missing".into()]).is_err());
    }

    #[test]
    fn mission_feedback_context_limits_recent_count_and_serialized_text() {
        let store = store("feedback-bounds");
        let mission = feedback_mission(&store);
        for n in 0..12 {
            store.submit_feedback(&mission.mission_id, &feedback(&mission, &format!("note-{n}"), "fix")).unwrap();
            if n == 7 {
                let exact = store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["fix".into()]).unwrap();
                assert_eq!(exact["observations"].as_array().unwrap().len(), 8);
                assert_eq!(exact["truncated"], false, "exactly eight short notes are complete");
            }
        }
        let context = store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["fix".into()]).unwrap();
        assert_eq!(context["observations"].as_array().unwrap().len(), 8);
        assert_eq!(context["observations"][0]["request_id"], "note-11");
        assert_eq!(context["observations"][7]["request_id"], "note-4");
        assert_eq!(context["truncated"], true, "older matching notes were omitted");
        for n in 0..12 {
            let mut input = feedback(&mission, &format!("large-{n}"), "fix");
            input.message = "界".repeat(8_000);
            input.evidence_refs = vec!["证".repeat(1_024); 16];
            store.submit_feedback(&mission.mission_id, &input).unwrap();
        }
        let context = store.feedback_context(&mission.mission_id, &mission.plan_sha256, &["fix".into()]).unwrap();
        assert!(serde_json::to_vec(&context).unwrap().len() <= MAX_FEEDBACK_CONTEXT_BYTES);
        let observations = context["observations"].as_array().unwrap();
        assert!(!observations.is_empty() && observations.len() <= 8);
        assert_eq!(observations[0]["request_id"], "large-11");
        assert_eq!(context["truncated"], true);
        assert!(observations.iter().all(|entry| entry["message"].as_str().unwrap().chars().count() <= 2_000));
    }

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
                currency: None,
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
    fn source_retry_uses_first_dispatch_and_keeps_baseline_after_10000_events() {
        let store = store("source-retry-pages");
        let root = store.path.parent().unwrap().join("workspace");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("read.txt"), "protected").unwrap();
        let mut input = create_input();
        input.workspace.root = root.to_string_lossy().into();
        input.workspace.read_scope = vec!["read.txt".into()];
        input.workspace.write_scope = vec!["src".into()];
        let created = store.create(&input).unwrap();
        let mut plan = proposal();
        plan.tasks.truncate(1);
        plan.tasks[0].read_paths = vec!["read.txt".into()];
        plan.tasks[0].write_paths = vec!["src".into()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".into()], &["src".into()], &[], &[], "test").unwrap();
        assert!(store.seed_test_progress(&created.mission_id, 10_001).unwrap() > 10_000);
        store.begin_task_attempt(&created.mission_id, "fix", "impl").unwrap();
        store.await_task_verification(&created.mission_id, "fix", "impl", "ready").unwrap();
        store.begin_verification_attempt(&created.mission_id, "fix", "verify-one").unwrap();
        store.record_verification_result(&created.mission_id, "fix", "impl", "verify-one",
            false, &["local-run:verify-one".into()], "missing baseline").unwrap();
        let files = super::super::protected_files(&root, &plan.tasks[0]).unwrap();
        let mut request: MissionVerificationRetry = serde_json::from_value(serde_json::json!({
            "request_id":"source-one", "expected_plan_sha256":planned.plan_sha256,
            "expected_verification_run_id":"verify-one", "source_baseline":{
                "source_reference":"test:pre-run", "captured_at":(Utc::now()+chrono::Duration::minutes(1)).to_rfc3339(),
                "artifact_sha256":"a".repeat(64), "files":files,
            },
        })).unwrap();
        assert!(store.retry_verification(&created.mission_id, "fix", &request).unwrap_err()
            .to_string().contains("predate"));
        request.source_baseline.as_mut().unwrap().captured_at = (Utc::now()-chrono::Duration::minutes(1)).to_rfc3339();
        store.retry_verification(&created.mission_id, "fix", &request).unwrap();
        let (_, baseline) = store.verification_event_context(&created.mission_id, "fix", "impl").unwrap();
        let baseline = baseline.unwrap().payload["protected_baseline"].clone();
        assert_eq!(baseline["provenance"], "source_client_pre_run_attestation");
        assert_eq!(baseline["files"], files);
        assert!(store.seed_test_progress(&created.mission_id, 10_001).unwrap() > 20_000);
        let store = MissionStore::new(&store.path);
        let before = store.seed_test_progress(&created.mission_id, 0).unwrap();
        store.retry_verification(&created.mission_id, "fix", &request).unwrap();
        assert_eq!(store.seed_test_progress(&created.mission_id, 0).unwrap(), before);
        let mut conflict = request.clone();
        conflict.source_baseline.as_mut().unwrap().source_reference = "different".into();
        assert!(store.retry_verification(&created.mission_id, "fix", &conflict).unwrap_err()
            .to_string().contains("different values"));
        store.begin_verification_attempt(&created.mission_id, "fix", "verify-two").unwrap();
        store.record_verification_result(&created.mission_id, "fix", "impl", "verify-two",
            false, &["local-run:verify-two".into()], "review failed").unwrap();
        request.request_id = "source-two".into();
        request.expected_verification_run_id = "verify-two".into();
        assert!(store.retry_verification(&created.mission_id, "fix", &request).unwrap_err()
            .to_string().contains("cannot be replaced"));
        request.source_baseline = None;
        store.retry_verification(&created.mission_id, "fix", &request).unwrap();
        let (_, latest) = store.verification_event_context(&created.mission_id, "fix", "impl").unwrap();
        assert_eq!(latest.unwrap().payload["protected_baseline"], baseline);
        fs::remove_dir_all(store.path.parent().unwrap()).unwrap();
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
    fn executor_recovery_projection_missing_claim_fails_closed_before_legacy_restore() {
        let store = store("recovery-missing-claim");
        let created = confirmed_single_task(&store, "recovery-missing-claim");
        let id = &created.mission_id;
        store.begin_task_attempt(id, "fix", "implementation").unwrap();
        store.await_task_verification(id, "fix", "implementation", "submitted").unwrap();
        let connection = store.connection().unwrap();
        connection.execute("UPDATE mission_runs SET status='waiting' WHERE mission_id=?1", [id]).unwrap();
        connection.execute("DELETE FROM mission_workspace_claims WHERE mission_id=?1", [id]).unwrap();
        // Directly read the real projection before reopening/migration. A
        // missing claim itself is never an admission or system-liveness proof.
        let missing = load_mission(&connection,id).unwrap().unwrap();
        let wait = missing.effective_wait.as_ref().unwrap();
        assert!(!wait.can_resume);
        assert_eq!((&*wait.reason,&*wait.actor),("unknown","unknown"));
        assert!(!owned_pending_verification_on(&connection,&missing).unwrap());
        let cursor = wait.cursor;
        drop(connection);
        // Preserve the accepted legacy recovery: opening the Store may restore
        // only this still-current submitted attempt, then explicit resume works.
        let recovered = store.get(id).unwrap();
        let wait = recovered.effective_wait.as_ref().unwrap();
        assert!(wait.can_resume);
        assert_eq!((&*wait.reason,&*wait.actor),("verification_resume_required","user"));
        assert_eq!(wait.cursor,cursor);
        assert_eq!(store.control(id,"resume").unwrap().tasks[0].attempt_run_id,"implementation");
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
        assert_eq!(store.get(&created.mission_id).unwrap().status, MissionStatus::Active,
            "already admitted implementation may finish; a new task cannot start");
        store.complete_task_attempt(&created.mission_id, "fix", "run-usage", false, &[], "fixture boundary").unwrap();
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
    fn executor_handoff_native_multihop_both_directions_preserves_failed_parents() {
        use crate::agent_core_contracts::LocalRun;
        use crate::store::local_runs::LocalRunLedger;
        for (upstream,downstream) in [("personal.codex","himind.builtin"),("himind.builtin","personal.codex")] {
            for stale_verification in [false,true] {
                let root=std::env::temp_dir().join(format!("native-handoff-{:016x}",rand::random::<u64>()));
                std::fs::create_dir_all(&root).unwrap();
                std::fs::create_dir_all(root.join("frontend/src")).unwrap();
                std::fs::write(root.join("frontend/protected.txt"),b"parent before failed attempt").unwrap();
                let store=MissionStore::new(root.join("mission.sqlite3"));
                let ledger=LocalRunLedger::new(root.join("runs.sqlite3"));
                let mut input=create_input();input.workspace.root=root.to_string_lossy().into();
                let created=store.create(&input).unwrap();
                let mut plan=proposal();plan.tasks[0].execution.executor=upstream.into();plan.tasks[1].execution.executor=downstream.into();
                let planned=store.propose_plan(&created.mission_id,&plan).unwrap();
                store.confirm_plan(&created.mission_id,&planned.plan_sha256,&["fix".into(),"verify".into()],&["frontend".into()],&[],&[],"test").unwrap();
                let save=|id:&str,status:&str,provider:&str| {
                    let run:LocalRun=serde_json::from_value(serde_json::json!({"schema_version":"local_run.v1",
                        "run_id":id,"interaction_id":"fixture-interaction","source":"workflow","transport":"local",
                        "status":status,"runtime_provider":provider,"created_at":"1","updated_at":"2"})).unwrap();
                    ledger.save_run(&run).unwrap();
                };
                store.begin_task_attempt(&created.mission_id,"fix","original-failed-run").unwrap();
                store.append_event(&created.mission_id,"task_baseline_captured","fix","stale parent baseline",serde_json::json!({
                    "implementation_run_id":"original-failed-run","protected_baseline":{"files":super::super::protected_files(&root,&plan.tasks[0]).unwrap()}})).unwrap();
                save("original-failed-run","failed",upstream);
                if stale_verification {
                    store.await_task_verification(&created.mission_id,"fix","original-failed-run","review").unwrap();
                    store.begin_verification_attempt(&created.mission_id,"fix","original-failed-review").unwrap();
                    store.record_verification_result(&created.mission_id,"fix","original-failed-run","original-failed-review",false,&["rejected".into()],"negative").unwrap();
                } else { store.complete_task_attempt(&created.mission_id,"fix","original-failed-run",false,&[],"failed").unwrap(); }
                let failed=store.get(&created.mission_id).unwrap().tasks[0].clone();
                let mut repair=task("repair-one");repair.repair_of="fix".into();
                let current=store.add_follow_up_tasks(&created.mission_id,&[repair.clone()],"repair").unwrap();
                let context=crate::mission::executor_handoff::build(&current,&repair,&ledger,&store).unwrap();
                assert_eq!(context[0]["context_kind"],"failed_parent_context_not_accepted_output");
                assert!(context[0]["acceptance"].is_null());
                store.begin_task_attempt(&created.mission_id,"repair-one","middle-failed-run").unwrap();
                save("middle-failed-run","failed",upstream);
                store.complete_task_attempt(&created.mission_id,"repair-one","middle-failed-run",false,&[],"failed again").unwrap();
                let mut leaf=task("repair-two");leaf.repair_of="repair-one".into();
                std::fs::write(root.join("frontend/protected.txt"),b"leaf captured before execution").unwrap();
                let protected_baseline=serde_json::json!({"files":super::super::protected_files(&root,&leaf).unwrap(),
                    "provenance":"native_pre_run_fixture","captured_at":"before-implementation"});
                assert!(!protected_baseline["files"].as_object().unwrap().is_empty());
                store.add_follow_up_tasks(&created.mission_id,&[leaf],"repair again").unwrap();
                store.begin_task_attempt(&created.mission_id,"repair-two","accepted-leaf-run").unwrap();
                let baseline_event=store.append_event(&created.mission_id,"task_baseline_captured","repair-two","nonempty native leaf baseline",serde_json::json!({
                    "implementation_run_id":"accepted-leaf-run","protected_baseline":protected_baseline})).unwrap();
                save("accepted-leaf-run","succeeded",upstream);save("accepted-leaf-review","succeeded",downstream);
                std::fs::create_dir_all(root.join("frontend/src")).unwrap();
                std::fs::write(root.join("frontend/src/leaf.json"),b"accepted artifact bytes").unwrap();
                ledger.append_event(&crate::agent_core_contracts::RuntimeEvent {
                    schema_version:crate::agent_core_contracts::RUNTIME_EVENT_SCHEMA_VERSION.into(),event_id:"leaf-output".into(),
                    run_id:"accepted-leaf-run".into(),step_id:"leaf".into(),capability_id:"leaf".into(),sequence:1,
                    provider:upstream.into(),event_type:crate::agent_core_contracts::RuntimeEventType::ToolCompleted,occurred_at:"2".into(),
                    payload:serde_json::json!({"output":{"passed":true,"evidence":["frontend/src/leaf.json","reports/outside.json","../outside.json"]}}),
                }).unwrap();
                store.await_task_verification(&created.mission_id,"repair-two","accepted-leaf-run","review").unwrap();
                store.begin_verification_attempt(&created.mission_id,"repair-two","accepted-leaf-review").unwrap();
                let current=store.get(&created.mission_id).unwrap();
                store.append_event(&created.mission_id,"host_verification_checked","repair-two","fixture host review",serde_json::json!({
                    "passed":true,"plan_sha256":current.plan_sha256,"implementation_run_id":"accepted-leaf-run",
                    "verification_run_id":"accepted-leaf-review","candidate_before":{"candidate_id":"historical-fixture-candidate"},
                    "checks":[{"command":"npm test","exit_code":0}]})).unwrap();
                let accepted=store.record_verification_result(&created.mission_id,"repair-two","accepted-leaf-run","accepted-leaf-review",true,&["fixture-host-event".into()],"").unwrap();
                let original=accepted.tasks.iter().find(|t|t.spec.id=="fix").unwrap();
                assert_eq!(original.attempt_run_id,failed.attempt_run_id);
                assert_eq!(original.verification_run_id,failed.verification_run_id);
                let receiver=&accepted.tasks.iter().find(|t|t.spec.id=="verify").unwrap().spec;
                let envelope=crate::mission::executor_handoff::build(&accepted,receiver,&ledger,&store).unwrap();
                assert_eq!(envelope[0]["logical_dependency_task_id"],"fix");
                assert_eq!(envelope[0]["actual_producer_task_id"],"repair-two");
                assert_eq!(envelope[0]["upstream_run_id"],"accepted-leaf-run");
                assert_eq!(envelope[0]["verification_run_id"],"accepted-leaf-review");
                assert_eq!(envelope[0]["acceptance"]["candidate"]["candidate_id"],"historical-fixture-candidate");
                assert_eq!(envelope[0]["acceptance"]["checks"][0]["command"],"npm test");
                assert_eq!(envelope[0]["acceptance"]["implementation_run_id"],"accepted-leaf-run");
                assert_eq!(envelope[0]["source_baseline_identity"]["task_id"],"repair-two");
                assert_eq!(envelope[0]["source_baseline_identity"]["event_id"],baseline_event.event_id);
                assert_eq!(envelope[0]["source_baseline_identity"]["verification_run_id"],"accepted-leaf-review");
                assert_eq!(envelope[0]["source_baseline_sha256"],format!("{:x}",sha2::Sha256::digest(serde_json::to_vec(&protected_baseline).unwrap())));
                assert_eq!(envelope[0]["artifacts"].as_array().unwrap().len(),1);
                assert_eq!(envelope[0]["artifacts"][0]["sha256"],format!("{:x}",sha2::Sha256::digest(b"accepted artifact bytes")));
                // Current observations cannot replace immutable baseline history.
                std::fs::write(root.join("frontend/src/leaf.json"),b"later current observation").unwrap();
                let changed=crate::mission::executor_handoff::build(&accepted,receiver,&ledger,&store).unwrap();
                assert_eq!(changed[0]["source_baseline_sha256"],envelope[0]["source_baseline_sha256"]);
                assert_ne!(changed[0]["artifacts"],envelope[0]["artifacts"]);
                assert_eq!(envelope[0]["source"]["actual_executor"],upstream);
                assert_eq!(envelope[0]["logical_failed_history"]["attempt_run_id"],"original-failed-run");
                assert!(envelope[0]["hash_observation"].as_str().unwrap().contains("not historical"));
                let (_,events)=store.executor_handoff_context(&created.mission_id).unwrap();
                for invalid in ["missing-event","wrong-run","wrong-candidate","failed-check","no-acceptance","cycle","missing-task","ambiguity"] {
                    let mut broken=accepted.clone();let mut events=events.clone();
                    match invalid {
                        "missing-event"=>events.retain(|e|e.event_type!="task_repaired"),
                        "wrong-run"=>events.iter_mut().filter(|e|e.event_type=="host_verification_checked").for_each(|e|e.payload["implementation_run_id"]=serde_json::json!("original-failed-run")),
                        "wrong-candidate"=>events.iter_mut().filter(|e|e.event_type=="host_verification_checked").for_each(|e|e.payload["candidate_before"]=serde_json::Value::Null),
                        "failed-check"=>events.iter_mut().filter(|e|e.event_type=="host_verification_checked").for_each(|e|e.payload["checks"][0]["exit_code"]=serde_json::json!(7)),
                        "no-acceptance"=>events.retain(|e|e.event_type!="task_completed"),
                        "cycle"=>broken.tasks.iter_mut().find(|t|t.spec.id=="repair-two").unwrap().spec.repair_of="repair-two".into(),
                        "missing-task"=>broken.tasks.retain(|t|t.spec.id!="repair-one"),
                        _=>{let mut extra=broken.tasks.iter().find(|t|t.spec.id=="repair-two").unwrap().clone();extra.spec.id="ambiguous".into();broken.tasks.push(extra);}
                    }
                    assert!(crate::mission::executor_handoff::producer(&broken,"fix",&events).is_err(),"{invalid}");
                }
                let file=root.join("handoff-contract.json");std::fs::write(&file,serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
                println!("native_fixture_handoff {upstream} -> {downstream} stale={stale_verification}: {}",file.display());
            }
        }
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
        assert_downstream_review_rework(false, "valid");
    }

    #[test]
    fn mission_follow_up_completed_producer_rework_preserves_history_and_requires_review() {
        assert_downstream_review_rework(true, "valid");
    }

    #[test]
    fn mission_follow_up_withdrawn_review_revisions_resume_without_rewriting_history() {
        assert_downstream_review_rework(true, "withdrawn");
    }

    #[test]
    fn mission_follow_up_completed_rework_rejects_same_role_expert_and_missing_pins() {
        for variant in ["same-role", "same-expert", "same-expert-other-version", "missing-producer", "missing-reviewer"] {
            assert_downstream_review_rework(true, variant);
        }
    }

    fn assert_downstream_review_rework(native: bool, variant: &str) {
        let store = store(if native { "native-review-feedback" } else { "review-feedback" });
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.budget.max_rounds = 40;
        plan.budget.max_repair_attempts = 2;
        for task in &mut plan.tasks { task.objective = "Deliver and independently check the scoped correction".into(); }
        plan.tasks[0].write_paths = vec!["frontend/src/design.md".into()];
        plan.tasks[1].write_paths = vec!["frontend/src/acceptance.json".into()];
        plan.tasks[1].execution.role_id = "independent-reviewer".into();
        if native {
            for (i, task) in plan.tasks.iter_mut().enumerate() {
                task.execution.expert = Some(serde_json::from_value(serde_json::json!({
                    "expert_id":format!("test.expert-{i}"), "expert_version":"1.0.0",
                    "expert_digest":format!("sha256:{}", "a".repeat(64)),
                })).unwrap());
            }
            match variant {
                "same-role" => plan.tasks[1].execution.role_id = plan.tasks[0].execution.role_id.clone(),
                "same-expert" | "same-expert-other-version" => {
                    plan.tasks[1].execution.expert = plan.tasks[0].execution.expert.clone();
                    if variant.ends_with("other-version") {
                        plan.tasks[1].execution.expert.as_mut().unwrap().expert_version = "2.0.0".into();
                    }
                }
                "missing-producer" => plan.tasks[0].execution.expert = None,
                "missing-reviewer" => plan.tasks[1].execution.expert = None,
                _ => (),
            }
        }
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        let add = |tasks: &[MissionTaskSpec], reason: &str| {
            if !native { return store.add_follow_up_tasks(&created.mission_id, tasks, reason); }
            use super::super::follow_up::{FollowUpSchemaVersion, MissionFollowUpAdd, MissionFollowUpTask};
            store.add_follow_up_tasks_for_mcp(&MissionFollowUpAdd {
                schema_version: FollowUpSchemaVersion::V1,
                mission_id: created.mission_id.clone(), expected_plan_sha256: planned.plan_sha256.clone(),
                read_scope_extension: None,
                reason: reason.into(), tasks: tasks.iter().map(|t| MissionFollowUpTask {
                    id: t.id.clone(), title: t.title.clone(), objective: t.objective.clone(),
                    repair_of: t.repair_of.clone(), depends_on: t.depends_on.clone(),
                    read_paths: t.read_paths.clone(), write_paths: t.write_paths.clone(),
                    acceptance_criteria: t.acceptance_criteria.clone(), required_checks: t.required_checks.clone(),
                }).collect(),
            })
        };
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
        if native {
            // Real Missions may have completed their root through earlier
            // repairs. Those successes remain historical evidence, and must
            // not close the reopened root before its new revision is checked.
            store.begin_task_attempt(&created.mission_id, "fix", "producer-v1").unwrap();
            store.complete_task_attempt(&created.mission_id, "fix", "producer-v1", false, &[], "earlier defect").unwrap();
            let mut history = plan.tasks[0].clone();
            history.id = "historical-producer-repair".into(); history.repair_of = "fix".into();
            add(&[history], "earlier repair").unwrap();
            verify("historical-producer-repair", "producer-v1-repair");
        } else {
            verify("fix", "producer-v1");
        }
        store.begin_task_attempt(&created.mission_id,"verify","review-v1").unwrap();
        store.complete_task_attempt(&created.mission_id,"verify","review-v1",false,&[],"upstream design conflict").unwrap();
        let mut first_retry = plan.tasks[1].clone();
        first_retry.id = "review-retry".into(); first_retry.repair_of = "verify".into(); first_retry.depends_on.clear();
        add(&[first_retry.clone()],"review retry").unwrap();
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
        let before = store.snapshot(&created.mission_id, 0).unwrap();
        assert!(add(&[correction.clone()],"unreviewed revision").is_err());
        assert_eq!(store.snapshot(&created.mission_id, 0).unwrap(), before);
        let mut unordered = final_review.clone(); unordered.depends_on.clear();
        assert!(add(&[correction.clone(),unordered],"unordered review").is_err());
        if native && variant != "valid" && variant != "withdrawn" {
            assert!(add(&[correction, final_review], "not independent").unwrap_err().to_string()
                .contains("different assigned role and pinned expert"), "{variant}");
            assert_eq!(store.snapshot(&created.mission_id, 0).unwrap(), before);
            return;
        }
        if native {
            let mut alias = correction.clone(); alias.repair_of = "historical-producer-repair".into();
            assert!(add(&[alias,final_review.clone()], "historical alias").unwrap_err().to_string()
                .contains("original delivery root"));
            assert_eq!(store.snapshot(&created.mission_id, 0).unwrap(), before);
        }
        let mut widened = correction.clone(); widened.write_paths.push("frontend/src/acceptance.json".into());
        assert!(add(&[widened,final_review.clone()],"widened writer").unwrap_err().to_string().contains("repair target scope"));
        assert_eq!(store.snapshot(&created.mission_id, 0).unwrap(), before);
        let mut revisions = [correction,final_review];
        let mut revised = add(&revisions,"review found real defect").unwrap();
        assert_eq!(revised.plan_sha256, before.mission.plan_sha256);
        assert_eq!(revised.confirmation, before.mission.confirmation);
        for task in &revisions {
            let parent = before.mission.tasks.iter().find(|t| t.spec.id == task.repair_of).unwrap();
            let child = revised.tasks.iter().find(|t| t.spec.id == task.id).unwrap();
            assert_eq!(parent.spec.execution, child.spec.execution);
            assert_eq!(parent.spec.required_checks, child.spec.required_checks);
        }
        if native {
            let accepted = store.snapshot(&created.mission_id, 0).unwrap();
            assert_eq!(add(&revisions,"review found real defect").unwrap(), accepted.mission);
            assert_eq!(store.snapshot(&created.mission_id, 0).unwrap(), accepted);
        }
        if variant == "withdrawn" {
            store.control(&created.mission_id, "pause").unwrap();
            for (index, revision) in revisions.iter().rev().enumerate() {
                store.cancel_pending_repair_request(&PendingRepairCancellation {
                    schema_version:"mission.task.cancel_pending.v1".into(), mission_id:created.mission_id.clone(),
                    expected_plan_sha256:planned.plan_sha256.clone(), task_id:revision.id.clone(),
                    request_id:format!("withdrawn-review-{index}"), reason:"Human corrected scope before dispatch".into(),
                }).unwrap();
            }
            let before = store.snapshot(&created.mission_id,0).unwrap();
            for revision in &mut revisions { revision.id.push_str("-replacement"); }
            revisions[1].depends_on = vec![revisions[0].id.clone()];
            assert!(add(&revisions[..1],"missing independent review").is_err());
            let mut unordered = revisions.clone(); unordered[1].depends_on.clear();
            assert!(add(&unordered,"unordered replacement").is_err());
            assert_eq!(store.snapshot(&created.mission_id,0).unwrap(),before);
            let raw = Connection::open(&store.path).unwrap();
            let event = before.events.iter().rev().find(|e|e.event_type=="task_reopened_for_review").unwrap();
            for field in ["plan_sha256","previous_implementation_run_id","previous_verification_run_id","revision_task_ids"] {
                let mut invalid = event.payload.clone();
                invalid[field]=if field=="revision_task_ids" { serde_json::json!(["missing"]) } else { serde_json::json!("wrong") };
                raw.execute("UPDATE mission_events SET payload_json=?2 WHERE sequence=?1",
                    params![event.sequence,serde_json::to_string(&invalid).unwrap()]).unwrap();
                assert!(add(&revisions,"invalid reopening receipt").is_err(),"{field}");
                raw.execute("UPDATE mission_events SET payload_json=?2 WHERE sequence=?1",
                    params![event.sequence,serde_json::to_string(&event.payload).unwrap()]).unwrap();
                assert_eq!(store.snapshot(&created.mission_id,0).unwrap(),before);
            }
            let canceled = before.events.iter().find(|e|e.event_type=="pending_repair_canceled" && e.task_id=="producer-revision").unwrap();
            raw.execute("UPDATE mission_events SET event_type='fixture_non_native_cancel' WHERE sequence=?1",params![canceled.sequence]).unwrap();
            assert!(add(&revisions,"canceled state alone is insufficient").is_err());
            raw.execute("UPDATE mission_events SET event_type='pending_repair_canceled' WHERE sequence=?1",params![canceled.sequence]).unwrap();
            raw.execute("UPDATE mission_tasks SET status='failed' WHERE mission_id=?1 AND task_id='historical-producer-repair'",params![created.mission_id]).unwrap();
            assert!(add(&revisions,"failed historical child").is_err());
            raw.execute("UPDATE mission_tasks SET status='completed' WHERE mission_id=?1 AND task_id='historical-producer-repair'",params![created.mission_id]).unwrap();
            assert_eq!(store.snapshot(&created.mission_id,0).unwrap(),before);
            revised=add(&revisions,"reviewed replacement after withdrawal").unwrap();
            assert_eq!(&revised.tasks[..before.mission.tasks.len()],before.mission.tasks.as_slice());
            let added=store.snapshot(&created.mission_id,0).unwrap();
            assert_eq!(&added.events[..before.events.len()],before.events.as_slice());
            assert_eq!(added.events.iter().filter(|e|e.event_type=="task_reopened_for_review").count(),1);
            assert_eq!(add(&revisions,"reviewed replacement after withdrawal").unwrap(),added.mission);
            assert_eq!(store.snapshot(&created.mission_id,0).unwrap(),added);
            store.control(&created.mission_id,"resume").unwrap();
        }
        assert_eq!(revised.tasks.iter().find(|t| t.spec.id=="fix").unwrap().status,TaskStatus::Failed);
        assert_eq!(revised.tasks.iter().find(|t| t.spec.id==revisions[1].id).unwrap().status,TaskStatus::Blocked);
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id,revisions[0].id);
        let reopened = MissionStore::new(store.path.clone());
        assert!(reopened.snapshot(&created.mission_id,0).unwrap().events.iter().any(|e|
            e.event_type=="task_reopened_for_review" && e.payload["previous_implementation_run_id"]=="producer-v1"));
        verify(&revisions[0].id,"producer-v2");
        assert_eq!(store.next_ready_task(&created.mission_id).unwrap().unwrap().id,revisions[1].id);
        let completed = verify(&revisions[1].id,"review-v3");
        assert_eq!(completed.status,MissionStatus::Completed);
        assert!(completed.tasks.iter().all(|t| t.status==TaskStatus::Completed
            || (variant=="withdrawn" && t.status==TaskStatus::Canceled)));
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
