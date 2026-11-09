use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::contract::{
    normalized_paths, path_allowed, plan_hash, require_id, require_sha256, require_text,
    validate_create, validate_proposal, validate_relative_task_paths, validate_selection, validate_task_graph,
    MissionConfirmation, MissionCreateInput, MissionEvent, MissionPlanProposal, MissionStatus,
    MissionTaskSpec, MissionTaskView, MissionView, TaskStatus,
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
                confirmation_json, budget_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, '', '{}', NULL, NULL, ?7, ?8)",
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

    pub(crate) fn propose_plan(
        &self,
        mission_id: &str,
        proposal: &MissionPlanProposal,
    ) -> Result<MissionView, Box<dyn Error>> {
        validate_proposal(proposal)?;
        validate_relative_task_paths(&proposal.tasks)?;
        let mission = self.get(mission_id)?;
        if matches!(mission.status, MissionStatus::Completed | MissionStatus::Canceled) {
            return Err("completed or canceled mission cannot accept a new plan".into());
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
        validate_selection(&proposal, &selected, approved_paths)?;
        let now = now_stamp();
        let confirmation = MissionConfirmation {
            plan_sha256: mission.plan_sha256.clone(),
            selected_task_ids: selected.iter().cloned().collect(),
            approved_paths: normalized_paths(approved_paths),
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
                "confirmed_by": confirmation.confirmed_by,
            }),
            &now,
        )?;
        recompute_task_readiness_tx(&transaction, mission_id, &now)?;
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
        let mission = self.get(mission_id)?;
        let confirmation = mission.confirmation.as_ref().ok_or("mission has no confirmed plan")?;
        if !matches!(mission.status, MissionStatus::Active | MissionStatus::Waiting | MissionStatus::Planned) {
            return Err("mission is not active for follow-up work".into());
        }
        let mut all = mission.tasks.iter().map(|task| task.spec.clone()).collect::<Vec<_>>();
        all.extend_from_slice(tasks);
        validate_task_graph(&all)?;
        validate_relative_task_paths(tasks)?;
        let existing = mission.tasks.iter().map(|task| task.spec.id.as_str()).collect::<BTreeSet<_>>();
        if tasks.iter().any(|task| existing.contains(task.id.as_str())) {
            return Err("follow-up task id already exists in mission".into());
        }
        for task in tasks {
            for path in &task.write_paths {
                if !path_allowed(path, &confirmation.approved_paths) {
                    return Err(format!(
                        "follow-up task {} writes outside confirmed scope: {path}",
                        task.id
                    ).into());
                }
            }
            if !task.repair_of.is_empty() && !existing.contains(task.repair_of.as_str()) {
                return Err(format!(
                    "follow-up task {} references unknown repair target {}",
                    task.id, task.repair_of
                ).into());
            }
        }
        let now = now_stamp();
        let offset = mission.tasks.len() as i64;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| row.get(0),
        )?;
        if status != TaskStatus::Ready.as_str() {
            return Err(format!("task {task_id} is not ready (status={status})").into());
        }
        let active_writers: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM mission_tasks WHERE mission_id = ?1 AND status = ?2",
            params![mission_id, TaskStatus::Running.as_str()],
            |row| row.get(0),
        )?;
        if active_writers > 0 {
            return Err("another mission task is already running; MVP allows one source writer".into());
        }
        transaction.execute(
            "UPDATE mission_tasks SET status = ?3, attempt_run_id = ?4, updated_at = ?5
             WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id, TaskStatus::Running.as_str(), run_id, now],
        )?;
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
        if mission.status != MissionStatus::Active {
            return Err("mission is not active for task completion".into());
        }
        let now = now_stamp();
        let next = if passed { TaskStatus::Completed } else { TaskStatus::Failed };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, bound_run): (String, String) = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != TaskStatus::Running.as_str() || bound_run != run_id {
            return Err(format!("task {task_id} is not running on attempt {run_id}").into());
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
        let (status, bound_run): (String, String) = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != TaskStatus::AwaitingVerification.as_str() {
            return Err(format!("task {task_id} is not awaiting verification (status={status})").into());
        }
        if bound_run.trim().is_empty() || bound_run == run_id {
            return Err("verification must use a different run from the implementation attempt".into());
        }
        transaction.execute(
            "UPDATE mission_tasks SET updated_at = ?3 WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id, now],
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
        let (status, bound_run): (String, String) = transaction.query_row(
            "SELECT status, attempt_run_id FROM mission_tasks WHERE mission_id = ?1 AND task_id = ?2",
            params![mission_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != TaskStatus::AwaitingVerification.as_str() {
            return Err(format!("task {task_id} is not awaiting verification").into());
        }
        if bound_run != implementation_run_id {
            return Err(format!(
                "verification evidence is bound to implementation run {bound_run}, not {implementation_run_id}"
            ).into());
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

    pub(crate) fn next_ready_task(&self, mission_id: &str) -> Result<Option<MissionTaskSpec>, Box<dyn Error>> {
        let mission = self.get(mission_id)?;
        if mission.status != MissionStatus::Active {
            return Ok(None);
        }
        Ok(mission.tasks.into_iter()
            .find(|task| task.status == TaskStatus::Ready)
            .map(|task| task.spec))
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
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS mission_tasks(
            mission_id TEXT NOT NULL REFERENCES mission_runs(mission_id) ON DELETE CASCADE,
            task_id TEXT NOT NULL, position INTEGER NOT NULL, status TEXT NOT NULL,
            spec_json TEXT NOT NULL, attempt_run_id TEXT NOT NULL DEFAULT '',
            verification_passed INTEGER, evidence_json TEXT NOT NULL DEFAULT '[]',
            updated_at TEXT NOT NULL, PRIMARY KEY(mission_id, task_id)
         );
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
    Ok(())
}

fn load_mission(connection: &Connection, mission_id: &str) -> Result<Option<MissionView>, Box<dyn Error>> {
    let row = connection.query_row(
        "SELECT mission_id, title, objective, status, workspace_json, acceptance_json,
                plan_revision, plan_sha256, plan_json, confirmation_json, budget_json,
                created_at, updated_at
         FROM mission_runs WHERE mission_id = ?1",
        params![mission_id],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?, row.get::<_, Option<String>>(10)?,
            row.get::<_, String>(11)?, row.get::<_, String>(12)?,
        )),
    ).optional()?;
    let Some((id, title, objective, status, workspace, acceptance, revision, hash, plan_json, confirmation, budget, created, updated)) = row else { return Ok(None); };
    let proposal = serde_json::from_str::<MissionPlanProposal>(&plan_json).ok();
    let mut statement = connection.prepare(
        "SELECT status, spec_json, attempt_run_id, verification_passed, evidence_json
         FROM mission_tasks WHERE mission_id = ?1 ORDER BY position ASC",
    )?;
    let tasks = statement.query_map(params![id], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
        row.get::<_, Option<i64>>(3)?, row.get::<_, String>(4)?,
    )))?
    .map(|row| {
        let (status, spec, run_id, verified, evidence) = row?;
        Ok(MissionTaskView {
            spec: serde_json::from_str(&spec)?, status: TaskStatus::parse(&status)?,
            attempt_run_id: run_id, verification_passed: verified.map(|value| value != 0),
            evidence_refs: serde_json::from_str(&evidence)?,
        })
    }).collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(Some(MissionView {
        schema_version: "mission.v1".to_string(), mission_id: id, title, objective,
        status: MissionStatus::parse(&status)?, workspace: serde_json::from_str(&workspace)?,
        acceptance_criteria: serde_json::from_str(&acceptance)?,
        plan_revision: revision.max(0) as u64, plan_sha256: hash,
        plan_summary: proposal.as_ref().map(|value| value.summary.clone()).unwrap_or_default(),
        required_acceptance: proposal.as_ref().map(|value| value.required_acceptance.clone()).unwrap_or_default(),
        proposed_tasks: proposal.as_ref().map(|value| value.tasks.clone()).unwrap_or_default(),
        budget: budget.map(|value| serde_json::from_str(&value)).transpose()?,
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
                role_id: "ui.implementer".to_string(),
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
            budget: MissionBudget { max_rounds: 8, max_minutes: 120, max_repair_attempts: 2 },
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
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], "test-user")
            .unwrap_err().to_string().contains("changed"));
        let confirmed = store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], "test-user").unwrap();
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
    fn absolute_task_paths_are_rejected() {
        let store = store("absolute-paths");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].read_paths = vec!["C:/outside".to_string()];
        let error = store.propose_plan(&created.mission_id, &plan).unwrap_err();
        assert!(error.to_string().contains("workspace-relative"));
    }

    #[test]
    fn parent_directory_segments_are_rejected() {
        let store = store("parent-paths");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[0].write_paths = vec!["frontend/../../outside".to_string()];
        let error = store.propose_plan(&created.mission_id, &plan).unwrap_err();
        assert!(error.to_string().contains("cannot escape"));
    }

    #[test]
    fn selected_task_requires_selected_dependency() {
        let store = store("deps");
        let created = store.create(&create_input()).unwrap();
        let mut plan = proposal();
        plan.tasks[1].depends_on = vec!["fix".to_string()];
        let planned = store.propose_plan(&created.mission_id, &plan).unwrap();
        assert!(store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["verify".to_string()], &["frontend/src".to_string()], "test-user")
            .unwrap_err().to_string().contains("unselected dependency"));
    }

    #[test]
    fn follow_up_tasks_cannot_write_outside_confirmed_scope() {
        let store = store("scope");
        let created = store.create(&create_input()).unwrap();
        let planned = store.propose_plan(&created.mission_id, &proposal()).unwrap();
        store.confirm_plan(&created.mission_id, &planned.plan_sha256,
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], "test-user").unwrap();
        let mut follow_up = task("repair");
        follow_up.write_paths = vec!["backend".to_string()];
        assert!(store.add_follow_up_tasks(&created.mission_id, &[follow_up], "修复失败")
            .unwrap_err().to_string().contains("outside confirmed scope"));
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
            &["fix".to_string(), "verify".to_string()], &["frontend/src".to_string()], "test-user").unwrap();
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
            &["fix".to_string()], &["frontend/src".to_string()], "test-user").unwrap();
        let _ = name;
        created
    }

    #[test]
    fn implementation_cannot_pass_without_independent_verification() {
        let store = store("verification");
        let created = confirmed_single_task(&store, "verification");
        let running = store.begin_task_attempt(&created.mission_id, "fix", "run-impl").unwrap();
        assert_eq!(running.tasks[0].status, TaskStatus::Running);
        let pending = store.await_task_verification(&created.mission_id, "fix", "run-impl", "送验").unwrap();
        assert_eq!(pending.tasks[0].status, TaskStatus::AwaitingVerification);
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-impl", true,
            &["local-run:run-impl".to_string()], "").unwrap_err().to_string().contains("cannot come"));
        assert!(store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &[], "").unwrap_err().to_string().contains("evidence"));
        let completed = store.record_verification_result(&created.mission_id, "fix", "run-impl", "run-verify", true,
            &["local-run:run-impl".to_string(), "local-run:run-verify".to_string()], "").unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        assert_eq!(completed.tasks[0].verification_passed, Some(true));
    }

    #[test]
    fn follow_up_validation_requires_confirmed_scope() {
        let store = store("followup-ok");
        let created = confirmed_single_task(&store, "followup-ok");
        let mut follow_up = task("repair");
        follow_up.repair_of = "fix".to_string();
        let updated = store.add_follow_up_tasks(&created.mission_id, &[follow_up], "验收失败").unwrap();
        assert!(updated.tasks.iter().any(|task| task.spec.id == "repair"));
        assert_eq!(updated.tasks.iter().find(|task| task.spec.id == "repair").unwrap().status, TaskStatus::Ready);
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
