use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;

use super::{WorkflowPackage, WorkflowStep};
use crate::agent_core_contracts::{
    InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport,
    LocalApprovalStatus, LocalRun, LocalRunApproval, LocalRunArtifact, LocalRunStatus,
    LocalRunStep, LocalRunUsage, LocalStepStatus, RuntimeEvent, RuntimeEventType,
    INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION, RUNTIME_EVENT_SCHEMA_VERSION,
};
use crate::store::local_runs::LocalRunLedger;

pub(crate) trait WorkflowStepExecutor {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WorkflowArtifactOutput {
    #[serde(default)]
    pub artifact_id: String,
    pub artifact_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WorkflowStepExecution {
    #[serde(default)]
    pub output: Value,
    #[serde(default)]
    pub artifacts: Vec<WorkflowArtifactOutput>,
    #[serde(default)]
    pub usage: Option<LocalRunUsage>,
}

impl WorkflowStepExecution {
    pub(crate) fn output(output: Value) -> Self {
        Self {
            output,
            artifacts: Vec::new(),
            usage: None,
        }
    }
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
            business_context: serde_json::json!({
                "workflow": {
                    "id": package.id,
                    "version": package.version,
                },
                "input": input,
            }),
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
            workspace_ref: input
                .get("workspace_root")
                .or_else(|| input.get("project_root"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
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
        self.persist_run(&run)?;
        Ok(run)
    }

    pub(crate) fn run_ready(
        &self,
        package: &WorkflowPackage,
        mut run: LocalRun,
        workflow_input: &Value,
        executor: &dyn WorkflowStepExecutor,
    ) -> Result<WorkflowRunOutcome, Box<dyn Error>> {
        package.validate().map_err(std::io::Error::other)?;
        if run.status.is_terminal() {
            return Ok(outcome(run, String::new(), Vec::new()));
        }
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
                if all_succeeded {
                    if let Err(error) = validate_required_artifacts(package, &run) {
                        run.status = LocalRunStatus::Failed;
                        run.error = error.to_string();
                    }
                }
                run.current_step_id.clear();
                run.updated_at = timestamp();
                self.persist_run(&run)?;
                return Ok(outcome(run, String::new(), completed_steps));
            };

            if step.approval_required
                && approval_status(&run, &step.id) != Some(LocalApprovalStatus::Approved)
            {
                if let Some(run_step) = run
                    .steps
                    .iter_mut()
                    .find(|candidate| candidate.step_id == step.id)
                {
                    run_step.status = LocalStepStatus::Waiting;
                }
                run.status = LocalRunStatus::Waiting;
                run.current_step_id = step.id.clone();
                run.updated_at = timestamp();
                self.persist_run(&run)?;
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
            self.persist_run(&run)?;
            self.append_event(
                &run,
                step,
                RuntimeEventType::ToolStarted,
                Value::Object(serde_json::Map::new()),
            )?;

            let result = if step.capability_id.trim().is_empty() && step.approval_required {
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "manual_approved": true,
                    "step_id": step.id,
                })))
            } else {
                let step_input =
                    execution_input(package, &run, workflow_input, &self.ledger, step)?;
                executor.execute(package, step, &step_input)
            };
            let run_step = run
                .steps
                .iter_mut()
                .find(|candidate| candidate.step_id == step.id)
                .ok_or("workflow step state disappeared")?;
            run_step.finished_at = timestamp();
            match result {
                Ok(execution) => {
                    let artifacts = normalize_artifacts(package, step, &execution.artifacts)?;
                    for artifact in artifacts {
                        merge_artifact(&mut run.artifacts, artifact);
                    }
                    if execution.usage.is_some() {
                        run.usage = execution.usage;
                    }
                    run_step.status = LocalStepStatus::Succeeded;
                    completed_steps.push(step.id.clone());
                    run.updated_at = timestamp();
                    self.persist_run(&run)?;
                    self.append_event(
                        &run,
                        step,
                        RuntimeEventType::ToolCompleted,
                        serde_json::json!({
                            "output_recorded": !execution.output.is_null(),
                            "output": execution.output,
                            "artifact_ids": run.artifacts.iter().map(|artifact| &artifact.artifact_id).collect::<Vec<_>>(),
                        }),
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
                    self.persist_run(&run)?;
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
        if run.current_step_id != step_id {
            return Err(
                format!("workflow run is not waiting for approval on step {step_id}").into(),
            );
        }
        let approval = run
            .approvals
            .iter_mut()
            .find(|approval| approval.approval_id.ends_with(step_id))
            .ok_or("workflow step has no approval record")?;
        if approval.status != LocalApprovalStatus::Pending {
            return Err(format!("workflow approval for {step_id} is not pending").into());
        }
        approval.status = LocalApprovalStatus::Approved;
        let step = run
            .steps
            .iter_mut()
            .find(|step| step.step_id == step_id)
            .ok_or("workflow step state is missing")?;
        step.status = LocalStepStatus::Pending;
        run.status = LocalRunStatus::Queued;
        run.current_step_id.clear();
        run.updated_at = timestamp();
        self.persist_run(&run)?;
        self.append_approval_event(&run, step_id, "approved")?;
        Ok(run)
    }

    pub(crate) fn reject_step(
        &self,
        mut run: LocalRun,
        step_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        if run.current_step_id != step_id {
            return Err(
                format!("workflow run is not waiting for approval on step {step_id}").into(),
            );
        }
        let approval = run
            .approvals
            .iter_mut()
            .find(|approval| approval.approval_id.ends_with(step_id))
            .ok_or("workflow step has no approval record")?;
        if approval.status != LocalApprovalStatus::Pending {
            return Err(format!("workflow approval for {step_id} is not pending").into());
        }
        approval.status = LocalApprovalStatus::Rejected;
        if let Some(step) = run.steps.iter_mut().find(|step| step.step_id == step_id) {
            step.status = LocalStepStatus::Canceled;
            step.finished_at = timestamp();
            step.error = "approval rejected".to_string();
        }
        run.status = LocalRunStatus::Canceled;
        run.error = format!("workflow approval rejected for {step_id}");
        run.current_step_id.clear();
        run.updated_at = timestamp();
        self.persist_run(&run)?;
        self.append_approval_event(&run, step_id, "rejected")?;
        Ok(run)
    }

    pub(crate) fn cancel(
        &self,
        mut run: LocalRun,
        reason: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        if run.status.is_terminal() {
            return Ok(run);
        }
        let canceled_at = timestamp();
        for step in &mut run.steps {
            if matches!(
                step.status,
                LocalStepStatus::Succeeded | LocalStepStatus::Failed | LocalStepStatus::Canceled
            ) {
                continue;
            }
            step.status = LocalStepStatus::Canceled;
            step.finished_at = canceled_at.clone();
            step.error = reason.to_string();
        }
        for approval in &mut run.approvals {
            if approval.status == LocalApprovalStatus::Pending {
                approval.status = LocalApprovalStatus::Interrupted;
            }
        }
        run.status = LocalRunStatus::Canceled;
        run.error = reason.to_string();
        run.current_step_id.clear();
        run.updated_at = canceled_at.clone();
        self.persist_run(&run)?;
        let sequence = self.ledger.next_runtime_sequence(&run.run_id)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("{}:workflow:{sequence}", run.run_id),
            run_id: run.run_id.clone(),
            step_id: "workflow".to_string(),
            capability_id: String::new(),
            sequence,
            occurred_at: canceled_at,
            provider: run.runtime_provider.clone(),
            event_type: RuntimeEventType::Error,
            payload: serde_json::json!({"canceled": true, "reason": reason}),
        })?;
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

    fn persist_run(&self, run: &LocalRun) -> Result<(), Box<dyn Error>> {
        self.ledger.save_run(run)?;
        crate::agent_core_service::AgentCoreRunRecorder::with_ledger(self.ledger.clone())
            .project(run)?;
        Ok(())
    }

    fn append_approval_event(
        &self,
        run: &LocalRun,
        step_id: &str,
        decision: &str,
    ) -> Result<(), Box<dyn Error>> {
        let step = run
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .ok_or("workflow step state is missing")?;
        let sequence = self.ledger.next_runtime_sequence(&run.run_id)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("{}:{}:{sequence}", run.run_id, step.step_id),
            run_id: run.run_id.clone(),
            step_id: step.step_id.clone(),
            capability_id: step.capability_id.clone(),
            sequence,
            occurred_at: timestamp(),
            provider: run.runtime_provider.clone(),
            event_type: RuntimeEventType::ApprovalResolved,
            payload: serde_json::json!({ "decision": decision }),
        })?;
        Ok(())
    }
}

fn normalize_artifacts(
    package: &WorkflowPackage,
    step: &WorkflowStep,
    artifacts: &[WorkflowArtifactOutput],
) -> Result<Vec<LocalRunArtifact>, Box<dyn Error>> {
    let mut normalized = Vec::new();
    for artifact in artifacts {
        if artifact.artifact_type.trim().is_empty() {
            return Err(
                format!("workflow step {} returned an empty artifact type", step.id).into(),
            );
        }
        let definition = if artifact.artifact_id.trim().is_empty() {
            package
                .artifacts
                .iter()
                .find(|definition| definition.artifact_type == artifact.artifact_type)
        } else {
            package
                .artifacts
                .iter()
                .find(|definition| definition.id == artifact.artifact_id)
        }
        .ok_or_else(|| {
            format!(
                "workflow step {} returned an artifact not declared by the package: {}",
                step.id,
                if artifact.artifact_id.trim().is_empty() {
                    artifact.artifact_type.as_str()
                } else {
                    artifact.artifact_id.as_str()
                }
            )
        })?;
        if definition.artifact_type != artifact.artifact_type {
            return Err(format!(
                "workflow step {} artifact {} type does not match package declaration",
                step.id, definition.id
            )
            .into());
        }
        normalized.push(LocalRunArtifact {
            artifact_id: definition.id.clone(),
            artifact_type: definition.artifact_type.clone(),
            name: if artifact.name.trim().is_empty() {
                definition.name.clone()
            } else {
                artifact.name.clone()
            },
            uri: artifact.uri.clone(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        });
    }
    Ok(normalized)
}

fn merge_artifact(artifacts: &mut Vec<LocalRunArtifact>, artifact: LocalRunArtifact) {
    if let Some(existing) = artifacts
        .iter_mut()
        .find(|candidate| candidate.artifact_id == artifact.artifact_id)
    {
        *existing = artifact;
    } else {
        artifacts.push(artifact);
    }
}

fn validate_required_artifacts(
    package: &WorkflowPackage,
    run: &LocalRun,
) -> Result<(), Box<dyn Error>> {
    let missing = package
        .artifacts
        .iter()
        .filter(|artifact| artifact.required)
        .filter(|artifact| {
            !run.artifacts
                .iter()
                .any(|candidate| candidate.artifact_id == artifact.id)
        })
        .map(|artifact| artifact.id.as_str())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "workflow completed without required artifacts: {}",
        missing.join(", ")
    )
    .into())
}

fn execution_input(
    package: &WorkflowPackage,
    run: &LocalRun,
    workflow_input: &Value,
    ledger: &LocalRunLedger,
    step: &WorkflowStep,
) -> Result<Value, Box<dyn Error>> {
    let mut output_map = serde_json::Map::new();
    for event in ledger.list_events(&run.run_id)? {
        if event.event_type != RuntimeEventType::ToolCompleted {
            continue;
        }
        if let Some(output) = event.payload.get("output") {
            output_map.insert(event.step_id, output.clone());
        }
    }
    let mut input = match workflow_input {
        Value::Object(object) => object.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(step_input) = step.input.as_object() {
        for (name, value) in step_input {
            input.insert(name.clone(), value.clone());
        }
    }
    input.insert(
        "workflow_context".to_string(),
        serde_json::json!({
            "workflow_id": package.id,
            "workflow_version": package.version,
            "run_id": run.run_id,
            "step_id": step.id,
            "step_outputs": output_map,
        }),
    );
    Ok(Value::Object(input))
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
        ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
            self.steps.borrow_mut().push(step.id.clone());
            Ok(WorkflowStepExecution::output(
                serde_json::json!({"step": step.id}),
            ))
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
        let blocked = runner
            .run_ready(&package, run, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(blocked.blocked_step_id, "WX-07");
        assert_eq!(blocked.run.status, LocalRunStatus::Waiting);
        assert_eq!(
            executor.steps.borrow().as_slice(),
            &["WX-01", "WX-02", "WX-03", "WX-04", "WX-05", "WX-06"]
        );

        let approved = runner.approve_step(blocked.run, "WX-07").unwrap();
        let waiting = runner
            .run_ready(&package, approved, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(waiting.blocked_step_id, "WX-08");
        assert_eq!(waiting.run.status, LocalRunStatus::Waiting);

        let approved = runner.approve_step(waiting.run, "WX-08").unwrap();
        let waiting = runner
            .run_ready(&package, approved, &serde_json::json!({}), &executor)
            .unwrap();
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
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                if step.id == "WX-02" {
                    return Err("inspect failed".into());
                }
                Ok(WorkflowStepExecution::output(Value::Null))
            }
        }

        let runner = WorkflowRunner::with_ledger(ledger("failure"));
        let package = package();
        let run = runner
            .start("agent-1", &package, "request-2", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(&package, run, &serde_json::json!({}), &FailingExecutor)
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Failed);
        assert_eq!(outcome.run.error, "inspect failed");
        assert_eq!(outcome.completed_steps, vec!["WX-01"]);
    }

    #[test]
    fn runner_records_artifacts_and_passes_prior_output_to_resumed_steps() {
        struct ArtifactExecutor {
            second_step_input: RefCell<Value>,
        }

        impl WorkflowStepExecutor for ArtifactExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                step: &WorkflowStep,
                input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                if step.id == "STEP-1" {
                    return Ok(WorkflowStepExecution {
                        output: serde_json::json!({"value": 42}),
                        artifacts: vec![WorkflowArtifactOutput {
                            artifact_id: "record".to_string(),
                            artifact_type: "record".to_string(),
                            name: "Record".to_string(),
                            uri: "file:///record.json".to_string(),
                            sha256: "abc".to_string(),
                            size_bytes: 12,
                        }],
                        usage: None,
                    });
                }
                *self.second_step_input.borrow_mut() = input.clone();
                Ok(WorkflowStepExecution::output(
                    serde_json::json!({"ok": true}),
                ))
            }
        }

        let mut workflow = package();
        workflow.steps = vec![
            WorkflowStep {
                id: "STEP-1".to_string(),
                title: "First".to_string(),
                kind: "capability".to_string(),
                capability_id: "test.first".to_string(),
                runtime: None,
                loop_config: None,
                when: None,
                candidate_action: String::new(),
                input: serde_json::json!({}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                depends_on: Vec::new(),
            },
            WorkflowStep {
                id: "STEP-2".to_string(),
                title: "Second".to_string(),
                kind: "capability".to_string(),
                capability_id: "test.second".to_string(),
                runtime: None,
                loop_config: None,
                when: None,
                candidate_action: String::new(),
                input: serde_json::json!({"fixed": true}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                depends_on: vec!["STEP-1".to_string()],
            },
        ];
        workflow.artifacts = vec![super::super::WorkflowArtifact {
            id: "record".to_string(),
            artifact_type: "record".to_string(),
            name: "Record".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 1024,
        }];
        let executor = ArtifactExecutor {
            second_step_input: RefCell::new(Value::Null),
        };
        let runner = WorkflowRunner::with_ledger(ledger("artifacts"));
        let run = runner
            .start(
                "agent-1",
                &workflow,
                "request-artifact",
                &serde_json::json!({"root": true}),
            )
            .unwrap();
        let outcome = runner
            .run_ready(
                &workflow,
                run,
                &serde_json::json!({"root": true}),
                &executor,
            )
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);
        assert_eq!(outcome.run.artifacts.len(), 1);
        let seen = executor.second_step_input.borrow();
        assert_eq!(seen["root"].as_bool(), Some(true));
        assert_eq!(seen["fixed"].as_bool(), Some(true));
        assert_eq!(
            seen["workflow_context"]["step_outputs"]["STEP-1"]["value"],
            42
        );
    }
}
