use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agent_core_contracts::{
    InteractionEnvelope, LocalRun, LocalRunStatus, RuntimeEvent, RuntimeEventType,
};

pub(crate) const LOCAL_RUN_DB_FILE: &str = "local-runs.sqlite3";
const SCHEMA_VERSION: i64 = 1;
// Schema creation is idempotent, but SQLite still takes a write lock for DDL
// and migration inserts. Serialize initialization inside one Agent process so
// concurrent readers/writers do not contend while opening the same ledger.
static SCHEMA_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub(crate) struct LocalRunLedger {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProjectionOutboxRecord {
    pub id: i64,
    pub projection_type: String,
    pub aggregate_id: String,
    pub dedupe_key: String,
    pub payload: Value,
    pub status: String,
    pub attempts: u32,
    pub next_attempt_at: String,
    pub last_error: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionOutboxSummary {
    pub total: u64,
    pub pending: u64,
    pub retrying: u64,
    pub projected: u64,
    pub dead_letter: u64,
    pub oldest_pending_at: String,
    pub last_error: String,
}

/// 死信按 last_error 归组后的结果：排障时先看「卡在哪几类错误上」，再决定重投范围。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionDeadLetterGroup {
    pub last_error: String,
    pub count: u64,
    pub oldest_at: String,
    pub newest_at: String,
}

impl LocalRunLedger {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        let root = crate::store::paths::agent_home();
        fs::create_dir_all(&root)?;
        Ok(Self::new(root.join(LOCAL_RUN_DB_FILE)))
    }

    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn record_interaction(
        &self,
        interaction: &InteractionEnvelope,
    ) -> Result<bool, Box<dyn Error>> {
        interaction.validate().map_err(std::io::Error::other)?;
        let payload = serde_json::to_string(interaction)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;

        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT payload_json, idempotency_key
                 FROM local_interactions
                 WHERE interaction_id = ?1",
                params![interaction.interaction_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_payload, existing_idempotency)) = existing {
            if existing_payload != payload || existing_idempotency != interaction.idempotency_key {
                return Err(std::io::Error::other(
                    "local interaction identity conflicts with persisted data",
                )
                .into());
            }
            transaction.commit()?;
            return Ok(false);
        }

        let conflicting_id: Option<String> = transaction
            .query_row(
                "SELECT interaction_id
                 FROM local_interactions
                 WHERE idempotency_key = ?1",
                params![interaction.idempotency_key],
                |row| row.get(0),
            )
            .optional()?;
        if conflicting_id
            .as_deref()
            .is_some_and(|value| value != interaction.interaction_id)
        {
            return Err(std::io::Error::other(
                "idempotency_key is already bound to another interaction",
            )
            .into());
        }

        transaction.execute(
            "INSERT INTO local_interactions(
                interaction_id,
                correlation_id,
                idempotency_key,
                source,
                payload_json,
                created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                interaction.interaction_id,
                interaction.correlation_id,
                interaction.idempotency_key,
                serde_json::to_value(&interaction.source)?
                    .as_str()
                    .unwrap_or_default(),
                payload,
                interaction.created_at,
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn save_run(&self, run: &LocalRun) -> Result<bool, Box<dyn Error>> {
        run.validate().map_err(std::io::Error::other)?;
        let payload = serde_json::to_string(run)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;

        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT status, payload_json
                 FROM local_runs
                 WHERE run_id = ?1",
                params![run.run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_status, existing_payload)) = existing {
            if existing_payload == payload {
                transaction.commit()?;
                return Ok(false);
            }
            let existing_status: LocalRunStatus =
                serde_json::from_value(Value::String(existing_status))?;
            if existing_status.is_terminal() && existing_status != run.status {
                return Err(
                    std::io::Error::other("terminal local run status cannot be changed").into(),
                );
            }
            transaction.execute(
                "UPDATE local_runs
                 SET interaction_id = ?2,
                     parent_run_id = ?3,
                     status = ?4,
                     runtime_provider = ?5,
                     workspace_ref = ?6,
                     current_step_id = ?7,
                     payload_json = ?8,
                     updated_at = ?9
                 WHERE run_id = ?1",
                params![
                    run.run_id,
                    run.interaction_id,
                    run.parent_run_id,
                    serde_json::to_value(&run.status)?
                        .as_str()
                        .unwrap_or_default(),
                    run.runtime_provider,
                    run.workspace_ref,
                    run.current_step_id,
                    payload,
                    run.updated_at,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO local_runs(
                    run_id,
                    interaction_id,
                    parent_run_id,
                    status,
                    runtime_provider,
                    workspace_ref,
                    current_step_id,
                    payload_json,
                    created_at,
                    updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    run.run_id,
                    run.interaction_id,
                    run.parent_run_id,
                    serde_json::to_value(&run.status)?
                        .as_str()
                        .unwrap_or_default(),
                    run.runtime_provider,
                    run.workspace_ref,
                    run.current_step_id,
                    payload,
                    run.created_at,
                    run.updated_at,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn get_run(&self, run_id: &str) -> Result<Option<LocalRun>, Box<dyn Error>> {
        let connection = self.connection()?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload_json FROM local_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn get_interaction(
        &self,
        interaction_id: &str,
    ) -> Result<Option<InteractionEnvelope>, Box<dyn Error>> {
        let connection = self.connection()?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload_json
                 FROM local_interactions
                 WHERE interaction_id = ?1",
                params![interaction_id],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn list_runs(&self, limit: usize) -> Result<Vec<LocalRun>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM local_runs
             ORDER BY updated_at DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 500) as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(serde_json::from_str(&row?)?);
        }
        Ok(runs)
    }

    pub(crate) fn acquire_run_lease(
        &self,
        run_id: &str,
        owner: &str,
        ttl_seconds: u64,
    ) -> Result<bool, Box<dyn Error>> {
        require_text("run lease owner", owner)?;
        let status: Option<String> = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "SELECT status FROM local_runs WHERE run_id = ?1",
                    params![run_id],
                    |row| row.get(0),
                )
                .optional()?
        };
        let Some(status) = status else {
            return Ok(false);
        };
        if status != "queued" && status != "running" && status != "waiting" {
            return Ok(false);
        }
        let now = unix_now_i64();
        let expires_at = now.saturating_add(ttl_seconds.max(30) as i64);
        let connection = self.connection()?;
        let changed = connection.execute(
            "INSERT INTO local_run_leases(run_id, owner, expires_at, heartbeat_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                owner = excluded.owner,
                expires_at = excluded.expires_at,
                heartbeat_at = excluded.heartbeat_at
             WHERE local_run_leases.owner = excluded.owner
                OR local_run_leases.expires_at <= ?4",
            params![run_id, owner, expires_at, now],
        )?;
        Ok(changed > 0)
    }

    pub(crate) fn renew_run_lease(
        &self,
        run_id: &str,
        owner: &str,
        ttl_seconds: u64,
    ) -> Result<bool, Box<dyn Error>> {
        let now = unix_now_i64();
        let expires_at = now.saturating_add(ttl_seconds.max(30) as i64);
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE local_run_leases
             SET expires_at = ?3, heartbeat_at = ?4
             WHERE run_id = ?1 AND owner = ?2",
            params![run_id, owner, expires_at, now],
        )?;
        Ok(changed > 0)
    }

    pub(crate) fn release_run_lease(
        &self,
        run_id: &str,
        owner: &str,
    ) -> Result<bool, Box<dyn Error>> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM local_run_leases WHERE run_id = ?1 AND owner = ?2",
            params![run_id, owner],
        )?;
        Ok(changed > 0)
    }

    pub(crate) fn recover_running_runs(
        &self,
        force: bool,
        limit: usize,
    ) -> Result<Vec<LocalRun>, Box<dyn Error>> {
        let run_ids = if force {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT run_id
                 FROM local_runs
                 WHERE status = 'running'
                 ORDER BY updated_at ASC
                 LIMIT ?1",
            )?;
            let run_ids = statement
                .query_map(params![limit.clamp(1, 100) as i64], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            run_ids
        } else {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT local_runs.run_id
                 FROM local_runs
                 JOIN local_run_leases
                   ON local_run_leases.run_id = local_runs.run_id
                 WHERE local_runs.status = 'running'
                   AND local_run_leases.expires_at <= ?2
                 ORDER BY local_run_leases.expires_at ASC
                 LIMIT ?1",
            )?;
            let run_ids = statement
                .query_map(params![limit.clamp(1, 100) as i64, unix_now_i64()], |row| {
                    row.get(0)
                })?
                .collect::<Result<Vec<String>, _>>()?;
            run_ids
        };
        let mut recovered = Vec::new();
        for run_id in run_ids {
            let Some(mut run) = self.get_run(&run_id)? else {
                continue;
            };
            // 这个函数负责“恢复到可重跑”的状态，只处理正在运行的运行。
            if run.status != LocalRunStatus::Running {
                continue;
            }
            let interrupted_step_id = run.current_step_id.clone();
            for step in &mut run.steps {
                if step.status == crate::agent_core_contracts::LocalStepStatus::Running {
                    step.status = crate::agent_core_contracts::LocalStepStatus::Pending;
                    step.started_at.clear();
                    step.finished_at.clear();
                    step.error.clear();
                }
            }
            run.status = LocalRunStatus::Queued;
            run.error = "workflow run recovered after lease expiry".to_string();
            run.current_step_id.clear();
            run.updated_at = unix_now_string();
            self.save_run(&run)?;
            self.clear_run_lease(&run_id)?;
            let sequence = self.next_runtime_sequence(&run_id)?;
            self.append_event(&RuntimeEvent {
                schema_version: crate::agent_core_contracts::RUNTIME_EVENT_SCHEMA_VERSION
                    .to_string(),
                event_id: format!("{run_id}:recovery:{sequence}"),
                run_id: run_id.clone(),
                step_id: interrupted_step_id,
                capability_id: String::new(),
                sequence,
                occurred_at: run.updated_at.clone(),
                provider: run.runtime_provider.clone(),
                event_type: RuntimeEventType::Error,
                payload: serde_json::json!({"recovered": true}),
            })?;
            recovered.push(run);
        }
        Ok(recovered)
    }

    /// 把「进程已经消失」的运行收尾为 failed。
    ///
    /// 运行期间持有租约，进程活着会不断续租；租约过期就说明执行者已经不在了。
    /// `recover_running_runs` 把这类运行改回 `queued`（等人工 dispatch），但没人
    /// dispatch 时它同样是个不会结束的僵尸状态 —— 界面上会一直显示“排队中”。
    /// 这里给出诚实的终态：失败 + 明确原因，需要重跑就重新发起。
    ///
    /// `grace_seconds` 用于避开刚启动、还没来得及取租约的运行。
    pub(crate) fn abandon_stale_runs(
        &self,
        reason: &str,
        grace_seconds: i64,
        limit: usize,
    ) -> Result<Vec<LocalRun>, Box<dyn Error>> {
        let now = unix_now_i64();
        let run_ids = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT local_runs.run_id
                 FROM local_runs
                 LEFT JOIN local_run_leases
                   ON local_run_leases.run_id = local_runs.run_id
                 WHERE local_runs.status IN ('running', 'queued')
                   AND (
                     local_run_leases.run_id IS NULL
                     OR local_run_leases.expires_at <= ?2
                   )
                   -- queued 可能正等着被 dispatch：给更长的宽限期，避免误杀排队中的运行。
                   AND (
                     local_runs.status = 'running'
                     OR CAST(local_runs.updated_at AS INTEGER) <= ?4
                   )
                   AND CAST(local_runs.updated_at AS INTEGER) <= ?3
                 ORDER BY local_runs.updated_at ASC
                 LIMIT ?1",
            )?;
            let run_ids = statement
                .query_map(
                    params![
                        limit.clamp(1, 500) as i64,
                        now,
                        now.saturating_sub(grace_seconds.max(0)),
                        now.saturating_sub(grace_seconds.max(0).saturating_mul(15)),
                    ],
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<String>, _>>()?;
            run_ids
        };
        let mut abandoned = Vec::new();
        for run_id in run_ids {
            let Some(mut run) = self.get_run(&run_id)? else {
                continue;
            };
            // queued 也在这里收尾：被 recover 过、又没人 dispatch 的运行同样是僵尸。
            if !matches!(run.status, LocalRunStatus::Running | LocalRunStatus::Queued) {
                continue;
            }
            for step in &mut run.steps {
                if step.status == crate::agent_core_contracts::LocalStepStatus::Running {
                    step.status = crate::agent_core_contracts::LocalStepStatus::Failed;
                    step.finished_at = unix_now_string();
                    step.error = reason.to_string();
                } else if step.status == crate::agent_core_contracts::LocalStepStatus::Pending
                    && run.status == LocalRunStatus::Queued
                {
                    // 排队中且长期没人调度的步骤：标成跳过，别让它看起来还会执行。
                    step.status = crate::agent_core_contracts::LocalStepStatus::Skipped;
                    step.finished_at = unix_now_string();
                    step.error = reason.to_string();
                }
            }
            run.status = LocalRunStatus::Failed;
            run.error = reason.to_string();
            run.current_step_id.clear();
            run.updated_at = unix_now_string();
            self.save_run(&run)?;
            self.clear_run_lease(&run_id)?;
            let sequence = self.next_runtime_sequence(&run_id)?;
            self.append_event(&RuntimeEvent {
                schema_version: crate::agent_core_contracts::RUNTIME_EVENT_SCHEMA_VERSION
                    .to_string(),
                event_id: format!("{run_id}:abandoned:{sequence}"),
                run_id: run_id.clone(),
                step_id: String::new(),
                capability_id: String::new(),
                sequence,
                occurred_at: run.updated_at.clone(),
                provider: run.runtime_provider.clone(),
                event_type: RuntimeEventType::Error,
                payload: serde_json::json!({
                    "abandoned": true,
                    "reason": reason,
                }),
            })?;
            abandoned.push(run);
        }
        Ok(abandoned)
    }

    fn clear_run_lease(&self, run_id: &str) -> Result<(), Box<dyn Error>> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM local_run_leases WHERE run_id = ?1",
            params![run_id],
        )?;
        Ok(())
    }

    pub(crate) fn append_event(&self, event: &RuntimeEvent) -> Result<bool, Box<dyn Error>> {
        event.validate().map_err(std::io::Error::other)?;
        let payload = serde_json::to_string(event)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;

        let run_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_runs WHERE run_id = ?1)",
            params![event.run_id],
            |row| row.get(0),
        )?;
        if !run_exists {
            return Err(std::io::Error::other("runtime event run does not exist").into());
        }

        let existing_payload: Option<String> = transaction
            .query_row(
                "SELECT payload_json FROM runtime_events WHERE event_id = ?1",
                params![event.event_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_payload) = existing_payload {
            if existing_payload != payload {
                return Err(std::io::Error::other(
                    "runtime event id conflicts with persisted data",
                )
                .into());
            }
            transaction.commit()?;
            return Ok(false);
        }

        let sequence_owner: Option<String> = transaction
            .query_row(
                "SELECT event_id
                 FROM runtime_events
                 WHERE run_id = ?1 AND sequence = ?2",
                params![event.run_id, event.sequence as i64],
                |row| row.get(0),
            )
            .optional()?;
        if sequence_owner
            .as_deref()
            .is_some_and(|value| value != event.event_id)
        {
            return Err(std::io::Error::other("runtime event sequence is already occupied").into());
        }

        transaction.execute(
            "INSERT INTO runtime_events(
                event_id,
                run_id,
                step_id,
                capability_id,
                sequence,
                provider,
                event_type,
                occurred_at,
                payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.event_id,
                event.run_id,
                event.step_id,
                event.capability_id,
                event.sequence as i64,
                event.provider,
                serde_json::to_value(&event.event_type)?
                    .as_str()
                    .unwrap_or_default(),
                event.occurred_at,
                payload,
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn list_events(&self, run_id: &str) -> Result<Vec<RuntimeEvent>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM runtime_events
             WHERE run_id = ?1
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map(params![run_id], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    pub(crate) fn next_runtime_sequence(&self, run_id: &str) -> Result<u64, Box<dyn Error>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_runs WHERE run_id = ?1)",
            params![run_id],
            |row| row.get(0),
        )?;
        if !run_exists {
            return Err(std::io::Error::other("runtime event run does not exist").into());
        }
        let next: i64 = transaction.query_row(
            "INSERT INTO runtime_sequence_allocations(run_id, next_sequence)
             VALUES (?1, 1)
             ON CONFLICT(run_id) DO UPDATE
                SET next_sequence = runtime_sequence_allocations.next_sequence + 1
             RETURNING next_sequence",
            params![run_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(u64::try_from(next).unwrap_or(u64::MAX))
    }

    pub(crate) fn enqueue_projection(
        &self,
        projection_type: &str,
        aggregate_id: &str,
        dedupe_key: &str,
        payload: &Value,
    ) -> Result<i64, Box<dyn Error>> {
        require_text("projection_type", projection_type)?;
        require_text("dedupe_key", dedupe_key)?;
        let payload = serde_json::to_string(payload)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO projection_outbox(
                projection_type,
                aggregate_id,
                dedupe_key,
                payload_json,
                status,
                attempts,
                next_attempt_at,
                last_error,
                created_at,
                updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'pending', 0, '', '', ?5, ?5)
             ON CONFLICT(dedupe_key) DO UPDATE SET
                projection_type = excluded.projection_type,
                aggregate_id = excluded.aggregate_id,
                payload_json = excluded.payload_json,
                status = CASE
                    WHEN projection_outbox.payload_json = excluded.payload_json
                         AND projection_outbox.status = 'projected'
                    THEN 'projected'
                    ELSE 'pending'
                END,
                attempts = CASE
                    WHEN projection_outbox.payload_json = excluded.payload_json
                    THEN projection_outbox.attempts
                    ELSE 0
                END,
                next_attempt_at = '',
                last_error = CASE
                    WHEN projection_outbox.payload_json = excluded.payload_json
                    THEN projection_outbox.last_error
                    ELSE ''
                END,
                updated_at = excluded.updated_at",
            params![
                projection_type,
                aggregate_id,
                dedupe_key,
                payload,
                unix_now_string(),
            ],
        )?;
        let id = transaction.query_row(
            "SELECT id FROM projection_outbox WHERE dedupe_key = ?1",
            params![dedupe_key],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(id)
    }

    pub(crate) fn pending_projections(
        &self,
        limit: usize,
    ) -> Result<Vec<ProjectionOutboxRecord>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,
                    projection_type,
                    aggregate_id,
                    dedupe_key,
                    payload_json,
                    status,
                    attempts,
                    next_attempt_at,
                    last_error
             FROM projection_outbox
             WHERE status = 'pending'
             ORDER BY id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 500) as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                id,
                projection_type,
                aggregate_id,
                dedupe_key,
                payload,
                status,
                attempts,
                next_attempt_at,
                last_error,
            ) = row?;
            records.push(ProjectionOutboxRecord {
                id,
                projection_type,
                aggregate_id,
                dedupe_key,
                payload: serde_json::from_str(&payload)?,
                status,
                attempts: attempts.max(0) as u32,
                next_attempt_at,
                last_error,
            });
        }
        Ok(records)
    }

    pub(crate) fn projections_for_aggregate(
        &self,
        aggregate_id: &str,
        limit: usize,
    ) -> Result<Vec<ProjectionOutboxRecord>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,
                    projection_type,
                    aggregate_id,
                    dedupe_key,
                    payload_json,
                    status,
                    attempts,
                    next_attempt_at,
                    last_error
             FROM projection_outbox
             WHERE aggregate_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows =
            statement.query_map(params![aggregate_id, limit.clamp(1, 500) as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                id,
                projection_type,
                aggregate_id,
                dedupe_key,
                payload,
                status,
                attempts,
                next_attempt_at,
                last_error,
            ) = row?;
            records.push(ProjectionOutboxRecord {
                id,
                projection_type,
                aggregate_id,
                dedupe_key,
                payload: serde_json::from_str(&payload)?,
                status,
                attempts: attempts.max(0) as u32,
                next_attempt_at,
                last_error,
            });
        }
        Ok(records)
    }

    pub(crate) fn projection_outbox_summary(
        &self,
    ) -> Result<ProjectionOutboxSummary, Box<dyn Error>> {
        let connection = self.connection()?;
        let total = connection.query_row("SELECT COUNT(*) FROM projection_outbox", [], |row| {
            row.get::<_, i64>(0)
        })?;
        let pending = connection.query_row(
            "SELECT COUNT(*) FROM projection_outbox WHERE status = 'pending'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let retrying = connection.query_row(
            "SELECT COUNT(*) FROM projection_outbox WHERE status = 'pending' AND attempts > 0",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let projected = connection.query_row(
            "SELECT COUNT(*) FROM projection_outbox WHERE status = 'projected'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let dead_letter = connection.query_row(
            "SELECT COUNT(*) FROM projection_outbox WHERE status = 'dead_letter'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let oldest_pending_at = connection
            .query_row(
                "SELECT updated_at FROM projection_outbox
                 WHERE status = 'pending'
                 ORDER BY updated_at ASC, id ASC
                 LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default();
        let last_error = connection
            .query_row(
                "SELECT last_error FROM projection_outbox
                 WHERE last_error <> ''
                 ORDER BY updated_at DESC, id DESC
                 LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default();
        Ok(ProjectionOutboxSummary {
            total: total.max(0) as u64,
            pending: pending.max(0) as u64,
            retrying: retrying.max(0) as u64,
            projected: projected.max(0) as u64,
            dead_letter: dead_letter.max(0) as u64,
            oldest_pending_at,
            last_error,
        })
    }

    pub(crate) fn mark_projection_projected(&self, id: i64) -> Result<(), Box<dyn Error>> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE projection_outbox
             SET status = 'projected',
                 last_error = '',
                 next_attempt_at = '',
                 updated_at = ?2
             WHERE id = ?1",
            params![id, unix_now_string()],
        )?;
        Ok(())
    }

    pub(crate) fn mark_projection_failed(
        &self,
        id: i64,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<(), Box<dyn Error>> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE projection_outbox
             SET status = 'pending',
                 attempts = attempts + 1,
                 next_attempt_at = ?2,
                 last_error = ?3,
                 updated_at = ?4
             WHERE id = ?1",
            params![id, next_attempt_at, error, unix_now_string()],
        )?;
        Ok(())
    }

    pub(crate) fn mark_projection_dead_letter(
        &self,
        id: i64,
        error: &str,
    ) -> Result<(), Box<dyn Error>> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE projection_outbox
             SET status = 'dead_letter',
                 attempts = attempts + 1,
                 next_attempt_at = '',
                 last_error = ?2,
                 updated_at = ?3
             WHERE id = ?1",
            params![id, error, unix_now_string()],
        )?;
        Ok(())
    }

    pub(crate) fn requeue_dead_letter_projections_with_error_fragment(
        &self,
        error_fragment: &str,
    ) -> Result<usize, Box<dyn Error>> {
        self.requeue_dead_letter_projections(Some(error_fragment))
    }

    /// 重投死信：`None` 覆盖全部死信，`Some(片段)` 只覆盖 `last_error` 命中该片段的记录。
    ///
    /// 重投只把记录放回待发队列并清零重试计数，不修改内容；若故障仍未修复，记录会重新落回死信，
    /// 因此调用方不需要担心「重投即丢数据」。
    pub(crate) fn requeue_dead_letter_projections(
        &self,
        error_fragment: Option<&str>,
    ) -> Result<usize, Box<dyn Error>> {
        let fragment = match error_fragment {
            None => None,
            Some(value) => match value.trim() {
                "" => return Ok(0),
                trimmed => Some(trimmed),
            },
        };
        let connection = self.connection()?;
        let changed = match fragment {
            Some(fragment) => connection.execute(
                "UPDATE projection_outbox
                 SET status = 'pending',
                     attempts = 0,
                     next_attempt_at = '',
                     last_error = '',
                     updated_at = ?2
                 WHERE status = 'dead_letter'
                   AND instr(last_error, ?1) > 0",
                params![fragment, unix_now_string()],
            )?,
            None => connection.execute(
                "UPDATE projection_outbox
                 SET status = 'pending',
                     attempts = 0,
                     next_attempt_at = '',
                     last_error = '',
                     updated_at = ?1
                 WHERE status = 'dead_letter'",
                params![unix_now_string()],
            )?,
        };
        Ok(changed)
    }

    /// 死信按错误归组，供 CLI 与界面回答「同步失败卡在哪一类原因上」。
    pub(crate) fn dead_letter_projection_groups(
        &self,
        limit: usize,
    ) -> Result<Vec<ProjectionDeadLetterGroup>, Box<dyn Error>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT last_error,
                    COUNT(*),
                    MIN(updated_at),
                    MAX(updated_at)
             FROM projection_outbox
             WHERE status = 'dead_letter'
             GROUP BY last_error
             ORDER BY COUNT(*) DESC, last_error ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 20) as i64], |row| {
            Ok(ProjectionDeadLetterGroup {
                last_error: row.get(0)?,
                count: row.get::<_, i64>(1)?.max(0) as u64,
                oldest_at: row.get(2)?,
                newest_at: row.get(3)?,
            })
        })?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    fn connection(&self) -> Result<Connection, Box<dyn Error>> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let database_exists = self.path.is_file();
        let connection = Connection::open(&self.path)?;
        // Set the SQLite busy handler before schema initialization and before
        // any transaction can contend with a concurrent Agent thread.
        connection.busy_timeout(Duration::from_secs(30))?;
        if !database_exists {
            // Two Agent threads can open a brand-new ledger at the same time.
            // The first connection wins the WAL transition; the loser must
            // continue using the already-initialized database instead of
            // surfacing a transient SQLITE_BUSY/LOCKED error.
            if let Err(error) = connection.execute_batch("PRAGMA journal_mode = WAL;") {
                if !is_sqlite_busy(&error) {
                    return Err(error.into());
                }
            }
        }
        let _schema_guard = SCHEMA_INIT_LOCK
            .lock()
            .map_err(|_| std::io::Error::other("local ledger schema lock is poisoned"))?;
        initialize_schema(&connection)?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), Box<dyn Error>> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS local_schema_migrations(
            version INTEGER PRIMARY KEY
         );
         CREATE TABLE IF NOT EXISTS local_interactions(
            interaction_id TEXT PRIMARY KEY,
            correlation_id TEXT NOT NULL,
            idempotency_key TEXT NOT NULL UNIQUE,
            source TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS local_runs(
            run_id TEXT PRIMARY KEY,
            interaction_id TEXT NOT NULL,
            parent_run_id TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            runtime_provider TEXT NOT NULL DEFAULT '',
            workspace_ref TEXT NOT NULL DEFAULT '',
            current_step_id TEXT NOT NULL DEFAULT '',
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_local_runs_interaction
            ON local_runs(interaction_id);
         CREATE INDEX IF NOT EXISTS idx_local_runs_status_updated
            ON local_runs(status, updated_at DESC);
         CREATE TABLE IF NOT EXISTS local_run_leases(
            run_id TEXT PRIMARY KEY,
            owner TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            heartbeat_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_local_run_leases_expiry
            ON local_run_leases(expires_at);
         CREATE TABLE IF NOT EXISTS runtime_events(
            event_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES local_runs(run_id) ON DELETE CASCADE,
            step_id TEXT NOT NULL DEFAULT '',
            capability_id TEXT NOT NULL DEFAULT '',
            sequence INTEGER NOT NULL,
            provider TEXT NOT NULL,
            event_type TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            UNIQUE(run_id, sequence)
         );
         CREATE TABLE IF NOT EXISTS runtime_sequence_allocations(
            run_id TEXT PRIMARY KEY,
            next_sequence INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO runtime_sequence_allocations(run_id, next_sequence)
            SELECT run_id, COALESCE(MAX(sequence), -1) + 1
            FROM runtime_events
            GROUP BY run_id;
         CREATE INDEX IF NOT EXISTS idx_runtime_events_run_sequence
            ON runtime_events(run_id, sequence);
         CREATE TABLE IF NOT EXISTS projection_outbox(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            projection_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL DEFAULT '',
            dedupe_key TEXT NOT NULL UNIQUE,
            payload_json TEXT NOT NULL,
            status TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT NOT NULL DEFAULT '',
            last_error TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_projection_outbox_status
            ON projection_outbox(status, next_attempt_at, id);",
    )?;
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT OR IGNORE INTO local_schema_migrations(version) VALUES (?1)",
        params![SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

fn is_sqlite_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn require_text(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() {
        return Err(std::io::Error::other(format!("{name} is required")).into());
    }
    Ok(())
}

fn unix_now_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core_contracts::{
        InteractionSource, InteractionTransport, LocalApprovalStatus, LocalRunApproval,
        LocalRunArtifact, LocalRunStatus, LocalRunStep, LocalStepStatus, RuntimeEventType,
        INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION,
        RUNTIME_EVENT_SCHEMA_VERSION,
    };
    use serde_json::json;

    fn ledger() -> LocalRunLedger {
        let root = std::env::temp_dir().join(format!(
            "himind-local-runs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        LocalRunLedger::new(root.join(LOCAL_RUN_DB_FILE))
    }

    fn interaction() -> InteractionEnvelope {
        serde_json::from_value(json!({
            "schema_version": INTERACTION_ENVELOPE_SCHEMA_VERSION,
            "interaction_id": "int-1",
            "correlation_id": "corr-1",
            "idempotency_key": "idem-1",
            "source": "mcp",
            "transport": "stdio",
            "principal": {"local_principal_id": "ai-client:codex"},
            "agent_id": "agent-1",
            "created_at": "2026-09-16T00:00:00Z"
        }))
        .unwrap()
    }

    fn run(status: LocalRunStatus) -> LocalRun {
        LocalRun {
            schema_version: LOCAL_RUN_SCHEMA_VERSION.to_string(),
            run_id: "run-1".to_string(),
            interaction_id: "int-1".to_string(),
            parent_run_id: String::new(),
            source: InteractionSource::Mcp,
            transport: InteractionTransport::Stdio,
            status,
            runtime_provider: "himind.builtin".to_string(),
            workspace_ref: "F:/workspace".to_string(),
            current_step_id: "step-1".to_string(),
            completion_mode: "full".to_string(),
            execution_plan: None,
            steps: vec![LocalRunStep {
                step_id: "step-1".to_string(),
                title: "Inspect".to_string(),
                status: LocalStepStatus::Running,
                capability_id: "workspace.inspect".to_string(),
                runtime_provider: String::new(),
                attempt: 1,
                started_at: String::new(),
                finished_at: String::new(),
                error: String::new(),
            }],
            approvals: vec![LocalRunApproval {
                approval_id: "approval-1".to_string(),
                capability_id: "workspace.inspect".to_string(),
                risk_level: "R2".to_string(),
                status: LocalApprovalStatus::Pending,
                owner: "agent".to_string(),
                expires_at: String::new(),
            }],
            artifacts: vec![LocalRunArtifact {
                artifact_id: "artifact-1".to_string(),
                artifact_type: "report".to_string(),
                name: "report.json".to_string(),
                uri: String::new(),
                sha256: String::new(),
                size_bytes: 0,
            }],
            usage: None,
            error: String::new(),
            created_at: "2026-09-16T00:00:00Z".to_string(),
            updated_at: "2026-09-16T00:00:01Z".to_string(),
        }
    }

    fn event(event_id: &str, sequence: u64) -> RuntimeEvent {
        RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: event_id.to_string(),
            run_id: "run-1".to_string(),
            step_id: "step-1".to_string(),
            capability_id: "workspace.inspect".to_string(),
            sequence,
            occurred_at: "2026-09-16T00:00:01Z".to_string(),
            provider: "himind.builtin".to_string(),
            event_type: RuntimeEventType::ToolCompleted,
            payload: json!({"ok": true}),
        }
    }

    #[test]
    fn interaction_roundtrip_is_idempotent() {
        let ledger = ledger();
        let interaction = interaction();
        assert!(ledger.record_interaction(&interaction).unwrap());
        assert!(!ledger.record_interaction(&interaction).unwrap());
    }

    #[test]
    fn run_roundtrip_and_terminal_status_guard() {
        let ledger = ledger();
        ledger.record_interaction(&interaction()).unwrap();
        assert!(ledger.save_run(&run(LocalRunStatus::Running)).unwrap());
        assert!(!ledger.save_run(&run(LocalRunStatus::Running)).unwrap());

        let mut succeeded = run(LocalRunStatus::Succeeded);
        succeeded.current_step_id = String::new();
        succeeded.steps[0].status = LocalStepStatus::Succeeded;
        succeeded.updated_at = "2026-09-16T00:00:02Z".to_string();
        assert!(ledger.save_run(&succeeded).unwrap());

        let mut failed = succeeded.clone();
        failed.status = LocalRunStatus::Failed;
        failed.updated_at = "2026-09-16T00:00:03Z".to_string();
        assert!(ledger.save_run(&failed).is_err());

        let loaded = ledger.get_run("run-1").unwrap().unwrap();
        assert_eq!(loaded.status, LocalRunStatus::Succeeded);
    }

    #[test]
    fn runtime_events_are_idempotent_and_sequence_safe() {
        let ledger = ledger();
        ledger.record_interaction(&interaction()).unwrap();
        ledger.save_run(&run(LocalRunStatus::Running)).unwrap();
        assert!(ledger.append_event(&event("event-1", 1)).unwrap());
        assert!(!ledger.append_event(&event("event-1", 1)).unwrap());
        assert!(ledger.append_event(&event("event-2", 1)).is_err());
    }

    #[test]
    fn runtime_sequence_allocations_are_atomic_across_threads() {
        let ledger = ledger();
        ledger.record_interaction(&interaction()).unwrap();
        ledger.save_run(&run(LocalRunStatus::Running)).unwrap();
        let handles = (0..4)
            .map(|_| {
                let ledger = ledger.clone();
                std::thread::spawn(move || {
                    (0..25)
                        .map(|_| ledger.next_runtime_sequence("run-1").unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut sequences = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=100).collect::<Vec<_>>());
    }

    #[test]
    fn projection_outbox_deduplicates_and_marks_projected() {
        let ledger = ledger();
        let payload = json!({"run_id": "run-1", "status": "running"});
        let first = ledger
            .enqueue_projection("local_run", "run-1", "local-run:run-1", &payload)
            .unwrap();
        let second = ledger
            .enqueue_projection("local_run", "run-1", "local-run:run-1", &payload)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(ledger.pending_projections(10).unwrap().len(), 1);
        ledger.mark_projection_projected(first).unwrap();
        assert!(ledger.pending_projections(10).unwrap().is_empty());
    }

    #[test]
    fn projection_outbox_can_enter_dead_letter() {
        let ledger = ledger();
        let payload = json!({"run_id": "run-1"});
        let id = ledger
            .enqueue_projection("run_projection", "run-1", "run:run-1", &payload)
            .unwrap();
        ledger
            .mark_projection_dead_letter(id, "payload conflict")
            .unwrap();
        assert!(ledger.pending_projections(10).unwrap().is_empty());
        let connection = ledger.connection().unwrap();
        let status: String = connection
            .query_row(
                "SELECT status FROM projection_outbox WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "dead_letter");
    }

    #[test]
    fn stale_queued_runs_are_abandoned_instead_of_waiting_forever() {
        let ledger = ledger();
        let mut queued = run(LocalRunStatus::Queued);
        queued.steps[0].status = LocalStepStatus::Pending;
        queued.current_step_id.clear();
        // 一小时前恢复、从此没人调度的运行。
        queued.updated_at = (unix_now_i64() - 3600).to_string();
        ledger.save_run(&queued).unwrap();
        ledger.clear_run_lease("run-1").unwrap();

        let abandoned = ledger
            .abandon_stale_runs("运行中断：执行进程已退出（租约过期，未续租）", 120, 10)
            .unwrap();
        assert_eq!(abandoned.len(), 1);
        let stored = ledger.get_run("run-1").unwrap().unwrap();
        assert_eq!(stored.status, LocalRunStatus::Failed);
        assert!(stored.error.contains("运行中断"));
        // 排队的步骤不能继续显示成“待执行”。
        assert_eq!(stored.steps[0].status, LocalStepStatus::Skipped);
    }

    #[test]
    fn fresh_queued_runs_are_left_alone() {
        let ledger = ledger();
        let mut queued = run(LocalRunStatus::Queued);
        queued.steps[0].status = LocalStepStatus::Pending;
        queued.updated_at = unix_now_string();
        ledger.save_run(&queued).unwrap();
        ledger.clear_run_lease("run-1").unwrap();
        // 刚排队的运行可能正在等待 dispatch，不能立刻判死。
        assert!(ledger
            .abandon_stale_runs("stale", 120, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recoverable_dead_letters_can_be_requeued_without_replaying_conflicts() {
        let ledger = ledger();
        let recoverable = ledger
            .enqueue_projection(
                "run_projection",
                "run-recoverable",
                "projection:run-recoverable",
                &serde_json::json!({"run_id": "run-recoverable"}),
            )
            .unwrap();
        let conflict = ledger
            .enqueue_projection(
                "run_projection",
                "run-conflict",
                "projection:run-conflict",
                &serde_json::json!({"run_id": "run-conflict"}),
            )
            .unwrap();
        ledger
            .mark_projection_dead_letter(
                recoverable,
                "Dashboard projection returned HTTP 401 Unauthorized",
            )
            .unwrap();
        ledger
            .mark_projection_dead_letter(conflict, "projection payload conflict")
            .unwrap();

        let changed = ledger
            .requeue_dead_letter_projections_with_error_fragment("HTTP 401 Unauthorized")
            .unwrap();
        assert_eq!(changed, 1);
        assert_eq!(ledger.pending_projections(10).unwrap().len(), 1);
        let summary = ledger.projection_outbox_summary().unwrap();
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.dead_letter, 1);
    }

    #[test]
    fn requeue_without_fragment_covers_every_dead_letter() {
        let ledger = ledger();
        let unauthorized = ledger
            .enqueue_projection(
                "run_projection",
                "run-unauthorized",
                "projection:run-unauthorized",
                &serde_json::json!({"run_id": "run-unauthorized"}),
            )
            .unwrap();
        let contract = ledger
            .enqueue_projection(
                "run_projection",
                "run-contract",
                "projection:run-contract",
                &serde_json::json!({"run_id": "run-contract"}),
            )
            .unwrap();
        let projected = ledger
            .enqueue_projection(
                "run_projection",
                "run-ok",
                "projection:run-ok",
                &serde_json::json!({"run_id": "run-ok"}),
            )
            .unwrap();
        ledger
            .mark_projection_dead_letter(unauthorized, "HTTP 401 Unauthorized")
            .unwrap();
        ledger
            .mark_projection_dead_letter(contract, "invalid json")
            .unwrap();
        ledger.mark_projection_projected(projected).unwrap();

        // 契约类错误以前没有重投入口，正是「全量重投」要覆盖的场景。
        assert_eq!(ledger.requeue_dead_letter_projections(None).unwrap(), 2);
        let summary = ledger.projection_outbox_summary().unwrap();
        assert_eq!(summary.pending, 2);
        assert_eq!(summary.dead_letter, 0);
        assert_eq!(summary.projected, 1);
        // 已成功上报的记录不能被重投逻辑带回来。
        assert_eq!(ledger.pending_projections(10).unwrap().len(), 2);
    }

    #[test]
    fn empty_requeue_fragment_leaves_dead_letters_untouched() {
        let ledger = ledger();
        let dead = ledger
            .enqueue_projection(
                "run_projection",
                "run-dead",
                "projection:run-dead",
                &serde_json::json!({"run_id": "run-dead"}),
            )
            .unwrap();
        ledger
            .mark_projection_dead_letter(dead, "invalid json")
            .unwrap();
        // 空白片段等于「没有指定范围」：宁可什么都不做，也不要把全部死信一次性重投。
        assert_eq!(
            ledger.requeue_dead_letter_projections(Some("   ")).unwrap(),
            0
        );
        assert_eq!(ledger.projection_outbox_summary().unwrap().dead_letter, 1);
    }

    #[test]
    fn dead_letter_groups_summarize_failures_by_reason() {
        let ledger = ledger();
        for (aggregate, error) in [
            ("run-a", "invalid json"),
            ("run-b", "invalid json"),
            ("run-c", "HTTP 401 Unauthorized"),
        ] {
            let id = ledger
                .enqueue_projection(
                    "run_projection",
                    aggregate,
                    &format!("projection:{aggregate}"),
                    &serde_json::json!({ "run_id": aggregate }),
                )
                .unwrap();
            ledger.mark_projection_dead_letter(id, error).unwrap();
        }

        let groups = ledger.dead_letter_projection_groups(5).unwrap();
        assert_eq!(groups.len(), 2);
        // 占多数的错误排在前面，排障时先看到的就是主要矛盾。
        assert_eq!(groups[0].last_error, "invalid json");
        assert_eq!(groups[0].count, 2);
        assert!(!groups[0].oldest_at.is_empty());
        assert!(groups[0].newest_at >= groups[0].oldest_at);
        assert_eq!(groups[1].last_error, "HTTP 401 Unauthorized");
        assert_eq!(groups[1].count, 1);
    }

    #[test]
    fn projection_summary_reports_pending_retry_and_dead_letter() {
        let ledger = ledger();
        let pending = ledger
            .enqueue_projection(
                "run_projection",
                "run-pending",
                "run:run-pending",
                &json!({"run_id": "run-pending"}),
            )
            .unwrap();
        let retry = ledger
            .enqueue_projection(
                "run_projection",
                "run-retry",
                "run:run-retry",
                &json!({"run_id": "run-retry"}),
            )
            .unwrap();
        let dead = ledger
            .enqueue_projection(
                "run_projection",
                "run-dead",
                "run:run-dead",
                &json!({"run_id": "run-dead"}),
            )
            .unwrap();
        let projected = ledger
            .enqueue_projection(
                "run_projection",
                "run-projected",
                "run:run-projected",
                &json!({"run_id": "run-projected"}),
            )
            .unwrap();
        ledger
            .mark_projection_failed(retry, "dashboard unavailable", "9999999999")
            .unwrap();
        ledger
            .mark_projection_dead_letter(dead, "payload conflict")
            .unwrap();
        ledger.mark_projection_projected(projected).unwrap();

        let summary = ledger.projection_outbox_summary().unwrap();
        assert_eq!(summary.total, 4);
        assert_eq!(summary.pending, 2);
        assert_eq!(summary.retrying, 1);
        assert_eq!(summary.projected, 1);
        assert_eq!(summary.dead_letter, 1);
        assert!(!summary.oldest_pending_at.is_empty());
        assert_eq!(summary.last_error, "payload conflict");
        let _ = pending;
    }

    #[test]
    fn expired_run_lease_recovers_running_step_to_pending() {
        let ledger = ledger();
        ledger.record_interaction(&interaction()).unwrap();
        ledger.save_run(&run(LocalRunStatus::Running)).unwrap();
        assert!(ledger.acquire_run_lease("run-1", "owner-a", 300).unwrap());
        assert!(!ledger.acquire_run_lease("run-1", "owner-b", 300).unwrap());
        assert!(ledger.recover_running_runs(false, 10).unwrap().is_empty());

        let connection = ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE local_run_leases SET expires_at = 0 WHERE run_id = 'run-1'",
                [],
            )
            .unwrap();
        drop(connection);

        let recovered = ledger.recover_running_runs(false, 10).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, LocalRunStatus::Queued);
        assert_eq!(recovered[0].steps[0].status, LocalStepStatus::Pending);
        assert!(ledger.list_events("run-1").unwrap().iter().any(|event| {
            event.payload.get("recovered").and_then(Value::as_bool) == Some(true)
        }));
    }

    #[test]
    fn sqlite_schema_contains_core_ledger_tables() {
        let ledger = ledger();
        let connection = ledger.connection().unwrap();
        for table in [
            "local_schema_migrations",
            "local_interactions",
            "local_runs",
            "local_run_leases",
            "runtime_events",
            "runtime_sequence_allocations",
            "projection_outbox",
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = ?1
                     )",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing table {table}");
        }
    }
}
