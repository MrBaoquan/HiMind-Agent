use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::agent_core_contracts::{InteractionEnvelope, LocalRun, LocalRunStatus, RuntimeEvent};

pub(crate) const LOCAL_RUN_DB_FILE: &str = "local-runs.sqlite3";
const SCHEMA_VERSION: i64 = 1;

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

    pub(crate) fn next_runtime_sequence(&self, run_id: &str) -> Result<u64, Box<dyn Error>> {
        let connection = self.connection()?;
        let next: i64 = connection.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1
             FROM runtime_events
             WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(next.max(0) as u64)
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

    fn connection(&self) -> Result<Connection, Box<dyn Error>> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        initialize_schema(&connection)?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), Box<dyn Error>> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
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
    fn sqlite_schema_contains_core_ledger_tables() {
        let ledger = ledger();
        let connection = ledger.connection().unwrap();
        for table in [
            "local_schema_migrations",
            "local_interactions",
            "local_runs",
            "runtime_events",
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
