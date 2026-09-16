use serde_json::Value;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent_core_contracts::{
    InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport, LocalRun,
    LocalRunStatus, LocalRunStep, LocalStepStatus, RunProjection, RuntimeEvent, RuntimeEventType,
    INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION, RUNTIME_EVENT_SCHEMA_VERSION,
    RUN_PROJECTION_SCHEMA_VERSION,
};
use crate::capability::types::{InvocationContext, InvocationSource, InvocationTransport};
use crate::store::local_runs::LocalRunLedger;

#[allow(dead_code)]
pub(crate) struct AgentCoreRunRecorder {
    ledger: LocalRunLedger,
}

impl AgentCoreRunRecorder {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            ledger: LocalRunLedger::open_default()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_ledger(ledger: LocalRunLedger) -> Self {
        Self { ledger }
    }

    pub(crate) fn begin(
        &self,
        agent_id: &str,
        context: &InvocationContext,
        capability_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        let interaction = self.interaction(agent_id, context);
        let now = unix_timestamp_string();
        let step_id = "capability".to_string();
        let run = LocalRun {
            schema_version: LOCAL_RUN_SCHEMA_VERSION.to_string(),
            run_id: format!("run_{}", context.request_id),
            interaction_id: interaction.interaction_id.clone(),
            parent_run_id: String::new(),
            source: interaction.source.clone(),
            transport: interaction.transport.clone(),
            status: LocalRunStatus::Running,
            runtime_provider: "himind.agent".to_string(),
            workspace_ref: String::new(),
            current_step_id: step_id.clone(),
            steps: vec![LocalRunStep {
                step_id,
                title: capability_id.to_string(),
                status: LocalStepStatus::Running,
                capability_id: capability_id.to_string(),
                runtime_provider: "himind.agent".to_string(),
                attempt: 1,
                started_at: now.clone(),
                finished_at: String::new(),
                error: String::new(),
            }],
            approvals: Vec::new(),
            artifacts: Vec::new(),
            usage: None,
            error: String::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.ledger.record_interaction(&interaction)?;
        self.ledger.save_run(&run)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("event_{}_started", context.request_id),
            run_id: run.run_id.clone(),
            step_id: "capability".to_string(),
            capability_id: capability_id.to_string(),
            sequence: 0,
            occurred_at: run.created_at.clone(),
            provider: "himind.agent".to_string(),
            event_type: RuntimeEventType::ToolStarted,
            payload: Value::Object(serde_json::Map::new()),
        })?;
        Ok(run)
    }

    pub(crate) fn complete(
        &self,
        mut run: LocalRun,
        result: &Value,
    ) -> Result<LocalRun, Box<dyn Error>> {
        let now = unix_timestamp_string();
        run.status = LocalRunStatus::Succeeded;
        run.current_step_id.clear();
        run.updated_at = now.clone();
        if let Some(step) = run.steps.first_mut() {
            step.status = LocalStepStatus::Succeeded;
            step.finished_at = now.clone();
        }
        self.ledger.save_run(&run)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("event_{}_completed", run.run_id),
            run_id: run.run_id.clone(),
            step_id: "capability".to_string(),
            capability_id: run
                .steps
                .first()
                .map(|step| step.capability_id.clone())
                .unwrap_or_default(),
            sequence: 1,
            occurred_at: now,
            provider: "himind.agent".to_string(),
            event_type: RuntimeEventType::ToolCompleted,
            payload: serde_json::json!({"result_recorded": !result.is_null()}),
        })?;
        self.project(&run)?;
        Ok(run)
    }

    pub(crate) fn fail(&self, mut run: LocalRun, error: &str) -> Result<LocalRun, Box<dyn Error>> {
        let now = unix_timestamp_string();
        run.status = LocalRunStatus::Failed;
        run.current_step_id.clear();
        run.error = error.to_string();
        run.updated_at = now.clone();
        if let Some(step) = run.steps.first_mut() {
            step.status = LocalStepStatus::Failed;
            step.finished_at = now.clone();
            step.error = error.to_string();
        }
        self.ledger.save_run(&run)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("event_{}_failed", run.run_id),
            run_id: run.run_id.clone(),
            step_id: "capability".to_string(),
            capability_id: run
                .steps
                .first()
                .map(|step| step.capability_id.clone())
                .unwrap_or_default(),
            sequence: 1,
            occurred_at: now,
            provider: "himind.agent".to_string(),
            event_type: RuntimeEventType::Error,
            payload: serde_json::json!({"error": error}),
        })?;
        self.project(&run)?;
        Ok(run)
    }

    pub(crate) fn project(&self, run: &LocalRun) -> Result<(), Box<dyn Error>> {
        let interaction = self
            .ledger
            .get_interaction(&run.interaction_id)?
            .ok_or("local run interaction is missing")?;
        let projection = RunProjection {
            schema_version: RUN_PROJECTION_SCHEMA_VERSION.to_string(),
            projection_id: format!("projection_{}_{}", run.run_id, run.updated_at),
            idempotency_key: format!("projection:{}:{}", run.run_id, run.updated_at),
            sent_at: unix_timestamp_string(),
            interaction,
            run: run.clone(),
        };
        projection.validate().map_err(std::io::Error::other)?;
        let idempotency_key = projection.idempotency_key.clone();
        let payload = serde_json::to_value(projection)?;
        self.ledger.enqueue_projection(
            "run_projection",
            &run.run_id,
            &idempotency_key,
            &payload,
        )?;
        Ok(())
    }

    fn interaction(&self, agent_id: &str, context: &InvocationContext) -> InteractionEnvelope {
        let source = match context.source {
            InvocationSource::LocalHttp => InteractionSource::Local,
            InvocationSource::Tauri => InteractionSource::Tauri,
            InvocationSource::DashboardWorker => InteractionSource::Dashboard,
            InvocationSource::Cli => InteractionSource::Cli,
            InvocationSource::Mcp => InteractionSource::Mcp,
        };
        let transport = match context.transport {
            InvocationTransport::LocalHttp => InteractionTransport::Http,
            InvocationTransport::Stdio => InteractionTransport::Stdio,
            InvocationTransport::Tauri => InteractionTransport::Local,
            InvocationTransport::Cli => InteractionTransport::Local,
            InvocationTransport::Internal => InteractionTransport::Queue,
        };
        InteractionEnvelope {
            schema_version: INTERACTION_ENVELOPE_SCHEMA_VERSION.to_string(),
            interaction_id: format!("int_{}", context.request_id),
            correlation_id: context.request_id.clone(),
            idempotency_key: format!("interaction:{}", context.request_id),
            source,
            transport,
            principal: InteractionPrincipal {
                local_principal_id: context.principal.clone(),
                delegated_user_id: String::new(),
                ai_client_id: String::new(),
            },
            agent_id: agent_id.to_string(),
            device_id: String::new(),
            workspace_ref: String::new(),
            business_context: Value::Object(serde_json::Map::new()),
            reply_target: Value::Object(serde_json::Map::new()),
            attachments: Vec::new(),
            policy_context: Value::Object(serde_json::Map::new()),
            runtime_hint: "himind.agent".to_string(),
            created_at: unix_timestamp_string(),
        }
    }
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core_contracts::LocalRunStatus;

    fn ledger(name: &str) -> LocalRunLedger {
        LocalRunLedger::new(
            std::env::temp_dir()
                .join(format!(
                    "himind-agent-core-service-{name}-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
                .join("local-runs.sqlite3"),
        )
    }

    #[test]
    fn recorder_completes_and_projects_run() {
        let ledger = ledger("complete");
        let recorder = AgentCoreRunRecorder::with_ledger(ledger);
        let context = InvocationContext::local_http();
        let run = recorder
            .begin("agent-1", &context, "workspace.inspect")
            .unwrap();
        let completed = recorder
            .complete(run, &serde_json::json!({"ok": true}))
            .unwrap();
        assert_eq!(completed.status, LocalRunStatus::Succeeded);
        assert_eq!(recorder.ledger.pending_projections(10).unwrap().len(), 1);
    }

    #[test]
    fn recorder_failure_is_terminal_and_projected() {
        let ledger = ledger("failure");
        let recorder = AgentCoreRunRecorder::with_ledger(ledger);
        let context = InvocationContext::local_http();
        let run = recorder
            .begin("agent-1", &context, "workspace.inspect")
            .unwrap();
        let failed = recorder.fail(run, "boom").unwrap();
        assert_eq!(failed.status, LocalRunStatus::Failed);
        assert_eq!(failed.error, "boom");
    }
}
