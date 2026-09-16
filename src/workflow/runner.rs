use serde::Serialize;
use serde_json::Value;
use std::error::Error;

use super::{WorkflowPackage, WorkflowStep};
use crate::agent_core_contracts::{
    InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport,
    LocalApprovalStatus, LocalRun, LocalRunApproval, LocalRunStatus, LocalRunStep, LocalStepStatus,
    RuntimeEvent, RuntimeEventType, INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION,
    RUNTIME_EVENT_SCHEMA_VERSION,
};
use crate::store::local_runs::LocalRunLedger;

pub(crate) trait WorkflowStepExecutor {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<Value, Box<dyn Error>>;
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkflowStepExecution {
    pub step_id: String,
    pub output: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkflowRunOutcome {
    pub run: LocalRun,
    pub blocked_step_id: String,
    pub completed_steps: Vec<String>,
}

pub(crate) struct WorkflowRunner {
    ledger: LocalRunLedger,
}

impl WorkflowRunner {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            ledger: LocalRunLedger::open_default()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_ledger(ledger: LocalRunLedger) -> Self {
        Self { ledger }
    }

    pub(crate) fn start(
        &self,
        agent_id: &str,
        package: &WorkflowPackage,
        request_id: &str,
        input: &Value,
    ) -> Result<LocalRun, Box<dyn Error>> {
        package.validate().map_err(std::io::Error::other)?;
        let now = timestamp();
        let interaction = InteractionEnvelope {
            schema_version: INTERACTION_ENVELOPE_SCHEMA_VERSION.to_string(),
            interaction_id: format!("workflow_int_{request_id}"),
            correlation_id: request_id.to_string(),
            idempotency_key: format!("workflow:{}:{request_id}", package.id),
            source: InteractionSource::Workflow,
            transport: InteractionTransport::Local,
            principal: InteractionPrincipal {
                local_principal_id: "local-user".to_string(),
                delegated_user_id: String::new(),
                ai_client_id: String::new(),
            },
            agent_id: agent_id.to_string(),
            device_id: String::new(),
            workspace_ref: String::new(),
            business_context: input.clone(),
            reply_target: Value::Object(serde_json::Map::new()),
            attachments: Vec::new(),
            policy_context: Value::Object(serde_json::Map::new()),
            runtime_hint: package
                .supported_runtimes
                .first()
                .cloned()
                .unwrap_or_else(|| "himind.builtin".to_string()),
            created_at: now.clone(),
        };
        let run = LocalRun {
            schema_version: LOCAL_RUN_SCHEMA_VERSION.to_string(),
            run_id: format!("workflow_run_{request_id}"),
            interaction_id: interaction.interaction_id.clone(),
            parent_run_id: String::new(),
            source: InteractionSource::Workflow,
            transport: InteractionTransport::Local,
            status: LocalRunStatus::Queued,
            runtime_provider: interaction.runtime_hint.clone(),
            workspace_ref: String::new(),
            current_step_id: String::new(),
            steps: package
                .steps
                .iter()
                .map(|step| LocalRunStep {
                    step_id: step.id.clone(),
                    title: step.title.clone(),
                    status: LocalStepStatus::Pending,
                    capability_id: if step.capability_id.trim().is_empty() {
                        step.id.clone()
                    } else {
                        step.capability_id.clone()
                    },
                    runtime_provider: interaction.runtime_hint.clone(),
                    attempt: 0,
                    started_at: String::new(),
                    finished_at: String::new(),
                    error: String::new(),
                })
                .collect(),
            approvals: package
                .steps
                .iter()
                .filter(|step| step.approval_required)
                .map(|step| LocalRunApproval {
                    approval_id: format!(
                        "workflow_approval_{}_{}",
                        run_suffix(request_id),
                        step.id
                    ),
                    capability_id: if step.capability_id.trim().is_empty() {
                        step.id.clone()
                    } else {
                        step.capability_id.clone()
                    },
                    risk_level: step.risk_level.clone(),
                    status: LocalApprovalStatus::Pending,
                    owner: "local".to_string(),
                    expires_at: String::new(),
                })
                .collect(),
            artifacts: Vec::new(),
            usage: None,
            error: String::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        run.validate().map_err(std::io::Error::other)?;
        self.ledger.record_interaction(&interaction)?;
        self.ledger.save_run(&run)?;
        Ok(run)
    }

    pub(crate) fn run_ready(
        &self,
        package: &WorkflowPackage,
        mut run: LocalRun,
        executor: &dyn WorkflowStepExecutor,
    ) -> Result<WorkflowRunOutcome, Box<dyn Error>> {
        package.validate().map_err(std::io::Error::other)?;
        if run.status.is_terminal() {
            return Ok(outcome(run, String::new(), Vec::new()));
        }
        let input = Value::Object(serde_json::Map::new());
        let mut completed_steps = Vec::new();

        loop {
            let Some(step) = next_ready_step(package, &run)? else {
                let all_succeeded = run
                    .steps
                    .iter()
                    .all(|step| step.status == LocalStepStatus::Succeeded);
                run.status = if all_succeeded {
                    LocalRunStatus::Succeeded
                } else if run
                    .steps
                    .iter()
                    .any(|step| step.status == LocalStepStatus::Failed)
                {
                    LocalRunStatus::Failed
                } else {
                    LocalRunStatus::Waiting
                };
                run.current_step_id.clear();
                run.updated_at = timestamp();
                self.ledger.save_run(&run)?;
                return Ok(outcome(run, String::new(), completed_steps));
            };

            if step.approval_required
                && approval_status(&run, &step.id) != Some(LocalApprovalStatus::Approved)
            {
                run.status = LocalRunStatus::Waiting;
                run.current_step_id = step.id.clone();
                run.updated_at = timestamp();
                self.ledger.save_run(&run)?;
                self.append_event(
                    &run,
                    &step,
                    RuntimeEventType::ApprovalRequested,
                    serde_json::json!({"risk_level": step.risk_level}),
                )?;
                return Ok(outcome(run, step.id.clone(), completed_steps));
            }

            let run_step = run
                .steps
                .iter_mut()
                .find(|candidate| candidate.step_id == step.id)
                .ok_or("workflow step state is missing")?;
            run_step.status = LocalStepStatus::Running;
            run_step.attempt = run_step.attempt.saturating_add(1);
            run_step.started_at = timestamp();
            run.current_step_id = step.id.clone();
            run.status = LocalRunStatus::Running;
            run.updated_at = timestamp();
            self.ledger.save_run(&run)?;
            self.append_event(
                &run,
                step,
                RuntimeEventType::ToolStarted,
                Value::Object(serde_json::Map::new()),
            )?;

            let result = executor.execute(package, step, &input);
            let run_step = run
                .steps
                .iter_mut()
                .find(|candidate| candidate.step_id == step.id)
                .ok_or("workflow step state disappeared")?;
            run_step.finished_at = timestamp();
            match result {
                Ok(output) => {
                    run_step.status = LocalStepStatus::Succeeded;
                    completed_steps.push(step.id.clone());
                    run.updated_at = timestamp();
                    self.ledger.save_run(&run)?;
                    self.append_event(
                        &run,
                        step,
                        RuntimeEventType::ToolCompleted,
                        serde_json::json!({"output_recorded": !output.is_null()}),
                    )?;
                }
                Err(error) => {
                    let message = error.to_string();
                    run_step.status = LocalStepStatus::Failed;
                    run_step.error = message.clone();
                    run.status = LocalRunStatus::Failed;
                    run.error = message.clone();
                    run.current_step_id.clear();
                    run.updated_at = timestamp();
                    self.ledger.save_run(&run)?;
                    self.append_event(
                        &run,
                        step,
                        RuntimeEventType::Error,
                        serde_json::json!({"error": message}),
                    )?;
                    return Ok(outcome(run, String::new(), completed_steps));
                }
            }
        }
    }

    pub(crate) fn approve_step(
        &self,
        mut run: LocalRun,
        step_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        let approval = run
            .approvals
            .iter_mut()
            .find(|approval| approval.approval_id.ends_with(step_id))
            .ok_or("workflow step has no approval record")?;
        approval.status = LocalApprovalStatus::Approved;
        run.status = LocalRunStatus::Queued;
        run.current_step_id.clear();
        run.updated_at = timestamp();
        self.ledger.save_run(&run)?;
        Ok(run)
    }

    fn append_event(
        &self,
        run: &LocalRun,
        step: &WorkflowStep,
        event_type: RuntimeEventType,
        payload: Value,
    ) -> Result<(), Box<dyn Error>> {
        let sequence = self.ledger.next_runtime_sequence(&run.run_id)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("{}:{}:{sequence}", run.run_id, step.id),
            run_id: run.run_id.clone(),
            step_id: step.id.clone(),
            capability_id: step.capability_id.clone(),
            sequence,
            occurred_at: timestamp(),
            provider: run.runtime_provider.clone(),
            event_type,
            payload,
        })?;
        Ok(())
    }
}

fn next_ready_step<'a>(
    package: &'a WorkflowPackage,
    run: &LocalRun,
) -> Result<Option<&'a WorkflowStep>, Box<dyn Error>> {
    for step in &package.steps {
        let run_step = run
            .steps
            .iter()
            .find(|candidate| candidate.step_id == step.id)
            .ok_or("workflow run is missing a package step")?;
        if run_step.status != LocalStepStatus::Pending {
            continue;
        }
        let dependencies_succeeded = step.depends_on.iter().all(|dependency| {
            run.steps
                .iter()
                .find(|candidate| candidate.step_id == *dependency)
                .is_some_and(|candidate| candidate.status == LocalStepStatus::Succeeded)
        });
        if dependencies_succeeded {
            return Ok(Some(step));
        }
    }
    Ok(None)
}

fn approval_status(run: &LocalRun, step_id: &str) -> Option<LocalApprovalStatus> {
    run.approvals
        .iter()
        .find(|approval| approval.approval_id.ends_with(step_id))
        .map(|approval| approval.status.clone())
}

fn outcome(
    run: LocalRun,
    blocked_step_id: String,
    completed_steps: Vec<String>,
) -> WorkflowRunOutcome {
    WorkflowRunOutcome {
        run,
        blocked_step_id,
        completed_steps,
    }
}

fn run_suffix(request_id: &str) -> String {
    request_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn package() -> WorkflowPackage {
        super::super::load_from_directory(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("workflows")
                .join("wechat-miniprogram-delivery"),
        )
        .unwrap()
    }

    fn ledger(name: &str) -> LocalRunLedger {
        LocalRunLedger::new(
            std::env::temp_dir()
                .join(format!(
                    "himind-workflow-runner-{name}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
                .join("local-runs.sqlite3"),
        )
    }

    #[derive(Default)]
    struct RecordingExecutor {
        steps: RefCell<Vec<String>>,
    }

    impl WorkflowStepExecutor for RecordingExecutor {
        fn execute(
            &self,
            _package: &WorkflowPackage,
            step: &WorkflowStep,
            _input: &Value,
        ) -> Result<Value, Box<dyn Error>> {
            self.steps.borrow_mut().push(step.id.clone());
            Ok(serde_json::json!({"step": step.id}))
        }
    }

    #[test]
    fn runner_blocks_at_approval_and_completes_after_approval() {
        let runner = WorkflowRunner::with_ledger(ledger("approval"));
        let package = package();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "request-1", &serde_json::json!({}))
            .unwrap();
        let blocked = runner.run_ready(&package, run, &executor).unwrap();
        assert_eq!(blocked.blocked_step_id, "WX-08");
        assert_eq!(blocked.run.status, LocalRunStatus::Waiting);
        assert_eq!(
            executor.steps.borrow().as_slice(),
            &["WX-01", "WX-02", "WX-03", "WX-04", "WX-05", "WX-06", "WX-07"]
        );

        let approved = runner.approve_step(blocked.run, "WX-08").unwrap();
        let waiting = runner.run_ready(&package, approved, &executor).unwrap();
        assert_eq!(waiting.blocked_step_id, "WX-10");
        assert_eq!(waiting.run.status, LocalRunStatus::Waiting);
    }

    #[test]
    fn runner_stops_on_step_failure() {
        struct FailingExecutor;
        impl WorkflowStepExecutor for FailingExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                step: &WorkflowStep,
                _input: &Value,
            ) -> Result<Value, Box<dyn Error>> {
                if step.id == "WX-02" {
                    return Err("inspect failed".into());
                }
                Ok(Value::Null)
            }
        }

        let runner = WorkflowRunner::with_ledger(ledger("failure"));
        let package = package();
        let run = runner
            .start("agent-1", &package, "request-2", &serde_json::json!({}))
            .unwrap();
        let outcome = runner.run_ready(&package, run, &FailingExecutor).unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Failed);
        assert_eq!(outcome.run.error, "inspect failed");
        assert_eq!(outcome.completed_steps, vec!["WX-01"]);
    }
}
