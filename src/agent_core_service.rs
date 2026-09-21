use serde_json::Value;
use std::error::Error;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent_core_contracts::{
    InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport, LocalRun,
    LocalRunStatus, LocalRunStep, LocalStepStatus, RunProjection, RuntimeEvent, RuntimeEventType,
    INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION, RUNTIME_EVENT_SCHEMA_VERSION,
    RUN_PROJECTION_SCHEMA_VERSION,
};
use crate::capability::types::{InvocationContext, InvocationSource, InvocationTransport};
use crate::store::local_runs::LocalRunLedger;

pub(crate) fn current_agent_attribution(state_path: &Path) -> (String, String) {
    let state = crate::api::client::load_agent_state(state_path).ok();
    let agent_id = state
        .as_ref()
        .map(|state| state.agent_id.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("local-agent")
        .to_string();
    let device_id = state
        .as_ref()
        .map(|state| state.device_id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HIMIND_AGENT_DEVICE_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
    (agent_id, device_id)
}

pub(crate) fn current_device_id(state_path: &Path) -> String {
    current_agent_attribution(state_path).1
}

#[allow(dead_code)]
pub(crate) struct AgentCoreRunRecorder {
    ledger: LocalRunLedger,
}

impl AgentCoreRunRecorder {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        #[cfg(test)]
        {
            return Err("default Agent Core ledger is disabled in unit tests".into());
        }
        #[cfg(not(test))]
        Ok(Self {
            ledger: LocalRunLedger::open_default()?,
        })
    }

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
            workspace_ref: context.workspace_ref.clone(),
            current_step_id: step_id.clone(),
            completion_mode: "full".to_string(),
            execution_plan: None,
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

    pub(crate) fn cancel(
        &self,
        mut run: LocalRun,
        reason: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        let now = unix_timestamp_string();
        run.status = LocalRunStatus::Canceled;
        run.current_step_id.clear();
        run.error = reason.to_string();
        run.updated_at = now.clone();
        if let Some(step) = run.steps.first_mut() {
            step.status = LocalStepStatus::Canceled;
            step.finished_at = now.clone();
            step.error = reason.to_string();
        }
        self.ledger.save_run(&run)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("event_{}_canceled", run.run_id),
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
            payload: serde_json::json!({"canceled": true, "reason": reason}),
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
            InvocationSource::Acp => InteractionSource::Acp,
            InvocationSource::Workflow => InteractionSource::Workflow,
            InvocationSource::Scheduler => InteractionSource::Cron,
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
                delegated_user_id: context.delegated_user_id.clone(),
                ai_client_id: context.ai_client_id.clone(),
            },
            agent_id: agent_id.to_string(),
            device_id: context.device_id.clone(),
            workspace_ref: context.workspace_ref.clone(),
            business_context: context.business_context.clone(),
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

    #[test]
    fn recorder_persists_delegated_client_and_device_attribution() {
        let ledger = ledger("attribution");
        let recorder = AgentCoreRunRecorder::with_ledger(ledger);
        let context = InvocationContext::new(InvocationSource::Mcp, "ai-client:codex")
            .with_ai_client_id("mcp:codex")
            .with_device_id("dev-test");
        let run = recorder
            .begin("agent-1", &context, "workspace.inspect")
            .unwrap();
        let interaction = recorder
            .ledger
            .get_interaction(&run.interaction_id)
            .unwrap()
            .unwrap();
        assert_eq!(interaction.principal.local_principal_id, "ai-client:codex");
        assert_eq!(interaction.principal.ai_client_id, "mcp:codex");
        assert_eq!(interaction.device_id, "dev-test");
    }

    #[test]
    fn recorder_persists_work_item_and_workspace_context() {
        let ledger = ledger("business-context");
        let recorder = AgentCoreRunRecorder::with_ledger(ledger);
        let context =
            InvocationContext::new(InvocationSource::DashboardWorker, "dashboard-user:usr_123")
                .with_workspace_ref("/workspace/project")
                .with_business_context(serde_json::json!({
                    "agent_run": {
                        "run_id": "agent_run_1",
                        "work_item_id": "ai_work_1",
                        "task_id": "tsk_1",
                        "attempt_no": 2
                    }
                }));
        let run = recorder
            .begin("agent-1", &context, "personal.codex")
            .unwrap();
        assert_eq!(run.workspace_ref, "/workspace/project");
        let interaction = recorder
            .ledger
            .get_interaction(&run.interaction_id)
            .unwrap()
            .unwrap();
        assert_eq!(interaction.workspace_ref, "/workspace/project");
        assert_eq!(
            interaction.business_context["agent_run"]["work_item_id"],
            "ai_work_1"
        );
    }
}
