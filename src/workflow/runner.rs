use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::path::PathBuf;

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
        self.start_with_parent(agent_id, package, request_id, input, "")
    }

    pub(crate) fn start_with_parent(
        &self,
        agent_id: &str,
        package: &WorkflowPackage,
        request_id: &str,
        input: &Value,
        parent_run_id: &str,
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
            parent_run_id: parent_run_id.to_string(),
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
        run: LocalRun,
        workflow_input: &Value,
        executor: &dyn WorkflowStepExecutor,
    ) -> Result<WorkflowRunOutcome, Box<dyn Error>> {
        self.run_ready_internal(package, run, workflow_input, executor, true, None)
    }

    fn run_ready_internal(
        &self,
        package: &WorkflowPackage,
        mut run: LocalRun,
        workflow_input: &Value,
        executor: &dyn WorkflowStepExecutor,
        enforce_required_artifacts: bool,
        loop_context: Option<&Value>,
    ) -> Result<WorkflowRunOutcome, Box<dyn Error>> {
        package.validate().map_err(std::io::Error::other)?;
        if run.status.is_terminal() {
            return Ok(outcome(run, String::new(), Vec::new()));
        }
        let mut completed_steps = Vec::new();

        loop {
            let Some(step) = next_ready_step(package, &run)? else {
                let all_succeeded = run.steps.iter().all(|step| {
                    matches!(
                        step.status,
                        LocalStepStatus::Succeeded | LocalStepStatus::Skipped
                    )
                });
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
                if all_succeeded && enforce_required_artifacts {
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

            let condition_context =
                workflow_context_value(package, &run, workflow_input, &self.ledger, loop_context)?;
            if let Some(condition) = step.when.as_ref() {
                if !super::evaluate_condition(condition, &condition_context)? {
                    if let Some(run_step) = run
                        .steps
                        .iter_mut()
                        .find(|candidate| candidate.step_id == step.id)
                    {
                        run_step.status = LocalStepStatus::Skipped;
                        run_step.finished_at = timestamp();
                    }
                    run.updated_at = timestamp();
                    self.persist_run(&run)?;
                    self.append_event(
                        &run,
                        step,
                        RuntimeEventType::Progress,
                        serde_json::json!({"skipped": true, "reason": "condition_false"}),
                    )?;
                    continue;
                }
            }

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

            let result = if let Some(loop_config) = step.loop_config.as_ref() {
                self.execute_loop_step(
                    package,
                    &run,
                    workflow_input,
                    executor,
                    step,
                    loop_config,
                    &condition_context,
                )
            } else if step.capability_id.trim().is_empty() && step.approval_required {
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "manual_approved": true,
                    "step_id": step.id,
                })))
            } else {
                let step_input = execution_input_from_context(
                    package,
                    &run,
                    workflow_input,
                    &condition_context,
                    step,
                )?;
                executor.execute(package, step, &step_input)
            };
            let candidate = candidate_for_step(package, &run, step)?;
            let run_step = run
                .steps
                .iter_mut()
                .find(|candidate| candidate.step_id == step.id)
                .ok_or("workflow step state disappeared")?;
            run_step.finished_at = timestamp();
            match result {
                Ok(execution) => {
                    let artifacts = normalize_artifacts(
                        package,
                        step,
                        &execution.artifacts,
                        candidate.as_ref(),
                    )?;
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

    fn execute_loop_step(
        &self,
        package: &WorkflowPackage,
        parent_run: &LocalRun,
        workflow_input: &Value,
        executor: &dyn WorkflowStepExecutor,
        step: &WorkflowStep,
        loop_config: &super::WorkflowLoop,
        base_context: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        let mut history = Vec::new();
        let mut artifacts = Vec::new();
        let child_package = WorkflowPackage {
            steps: loop_config.steps.clone(),
            candidate: None,
            ..package.clone()
        };
        for iteration in 1..=loop_config.max_iterations {
            let request_id = run_suffix(&format!(
                "{}_loop_{}_{}",
                parent_run.run_id, step.id, iteration
            ));
            let loop_context = serde_json::json!({
                "id": step.id,
                "iteration": iteration,
                "history": history.clone(),
            });
            let child_run = self.start_with_parent(
                "local-agent",
                &child_package,
                &request_id,
                workflow_input,
                &parent_run.run_id,
            )?;
            let outcome = self.run_ready_internal(
                &child_package,
                child_run,
                workflow_input,
                executor,
                false,
                Some(&loop_context),
            )?;
            match outcome.run.status {
                LocalRunStatus::Succeeded => {}
                status => {
                    return Err(format!(
                        "workflow loop {} iteration {} ended with status {:?}: {}",
                        step.id, iteration, status, outcome.run.error
                    )
                    .into())
                }
            }
            let latest = loop_iteration_output(&outcome.run, &self.ledger)?;
            history.push(latest.clone());
            for artifact in &outcome.run.artifacts {
                merge_artifact(&mut artifacts, artifact.clone());
            }
            let mut evaluation_context = base_context.clone();
            if let Some(object) = evaluation_context.as_object_mut() {
                let loops = object
                    .entry("loops".to_string())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                loops[step.id.as_str()] = serde_json::json!({
                    "id": step.id,
                    "iteration": iteration,
                    "latest": latest,
                    "history": history.clone(),
                });
            }
            let should_exit = loop_config
                .exit_when
                .as_ref()
                .map(|condition| super::evaluate_condition(condition, &evaluation_context))
                .transpose()?
                .unwrap_or(false);
            let should_continue = loop_config
                .continue_when
                .as_ref()
                .map(|condition| super::evaluate_condition(condition, &evaluation_context))
                .transpose()?
                .unwrap_or(true);
            self.append_event(
                parent_run,
                step,
                RuntimeEventType::Progress,
                serde_json::json!({
                    "loop_id": step.id,
                    "iteration": iteration,
                    "should_exit": should_exit,
                    "should_continue": should_continue,
                    "latest": latest,
                }),
            )?;
            if should_exit || !should_continue {
                return Ok(WorkflowStepExecution {
                    output: serde_json::json!({
                        "loop_id": step.id,
                        "iterations": history,
                        "exit": if should_exit { "exit_when" } else { "continue_when_false" },
                    }),
                    artifacts: artifacts
                        .into_iter()
                        .map(local_artifact_to_output)
                        .collect(),
                    usage: None,
                });
            }
        }
        Err(format!(
            "workflow loop {} reached max_iterations {} without exit",
            step.id, loop_config.max_iterations
        )
        .into())
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
    candidate: Option<&Value>,
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
        let normalized_artifact = LocalRunArtifact {
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
        };
        validate_artifact_contract(package, definition, &normalized_artifact)?;
        validate_candidate_artifact_binding(definition, &normalized_artifact, candidate)?;
        normalized.push(normalized_artifact);
    }
    Ok(normalized)
}

fn validate_candidate_artifact_binding(
    definition: &super::WorkflowArtifact,
    artifact: &LocalRunArtifact,
    candidate: Option<&Value>,
) -> Result<(), Box<dyn Error>> {
    let Some(candidate) = candidate else {
        return Ok(());
    };
    let candidate_commit = candidate
        .get("commit_sha")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(path) = artifact_path(&artifact.uri) else {
        return Ok(());
    };
    let Ok(instance) = serde_json::from_slice::<Value>(&std::fs::read(path)?) else {
        return Ok(());
    };
    let Some(commit_sha) = instance.get("commit_sha").and_then(Value::as_str) else {
        if matches!(
            definition.artifact_type.as_str(),
            "wechat_preview" | "wechat_experience_version" | "release_record"
        ) {
            return Err(format!("workflow artifact {} must bind commit_sha", definition.id).into());
        }
        return Ok(());
    };
    if !candidate_commit.is_empty() && commit_sha != candidate_commit {
        return Err(format!(
            "workflow artifact {} commit_sha does not match the frozen candidate",
            definition.id
        )
        .into());
    }
    Ok(())
}

fn validate_artifact_contract(
    package: &WorkflowPackage,
    definition: &super::WorkflowArtifact,
    artifact: &LocalRunArtifact,
) -> Result<(), Box<dyn Error>> {
    let path = artifact_path(&artifact.uri).ok_or_else(|| {
        format!(
            "workflow artifact {} requires a local file URI for validation",
            definition.id
        )
    })?;
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(format!("workflow artifact {} is not a file", definition.id).into());
    }
    if metadata.len() > definition.max_bytes {
        return Err(format!(
            "workflow artifact {} exceeds max_bytes {}",
            definition.id, definition.max_bytes
        )
        .into());
    }
    if artifact.size_bytes != 0 && artifact.size_bytes != metadata.len() {
        return Err(format!(
            "workflow artifact {} size does not match the file",
            definition.id
        )
        .into());
    }
    if definition.schema.trim().is_empty() {
        return Ok(());
    }
    let root = package.source_root.canonicalize().map_err(|error| {
        format!(
            "workflow package root is unavailable for artifact {}: {error}",
            definition.id
        )
    })?;
    let schema_path = root.join(&definition.schema).canonicalize()?;
    if !schema_path.starts_with(&root) {
        return Err(format!(
            "workflow artifact {} schema escapes the package",
            definition.id
        )
        .into());
    }
    let schema: Value = serde_json::from_slice(&std::fs::read(&schema_path)?)?;
    let instance: Value = serde_json::from_slice(&std::fs::read(&path)?).map_err(|error| {
        format!(
            "workflow artifact {} must be valid JSON for schema validation: {error}",
            definition.id
        )
    })?;
    let validator = jsonschema::validator_for(&schema)?;
    let errors = validator
        .iter_errors(&instance)
        .map(|error| format!("{error} at {}", error.instance_path))
        .collect::<Vec<_>>();
    if errors.is_empty() {
        return Ok(());
    }
    let mode = if definition.validation == "advisory" {
        "advisory"
    } else {
        "strict"
    };
    let message = format!(
        "workflow artifact {} schema validation ({mode}) failed: {}",
        definition.id,
        errors.join("; ")
    );
    if mode == "advisory" {
        eprintln!("{message}");
        Ok(())
    } else {
        Err(message.into())
    }
}

fn artifact_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if let Ok(parsed) = url::Url::parse(uri) {
        if parsed.scheme() == "file" {
            return parsed.to_file_path().ok();
        }
    }
    let path = PathBuf::from(uri);
    path.is_absolute().then_some(path)
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

fn loop_iteration_output(run: &LocalRun, ledger: &LocalRunLedger) -> Result<Value, Box<dyn Error>> {
    let mut steps = serde_json::Map::new();
    for event in ledger.list_events(&run.run_id)? {
        if event.event_type != RuntimeEventType::ToolCompleted {
            continue;
        }
        if let Some(output) = event.payload.get("output") {
            steps.insert(event.step_id, output.clone());
        }
    }
    Ok(serde_json::json!({
        "run_id": run.run_id,
        "status": run.status,
        "steps": steps,
        "artifacts": run.artifacts,
    }))
}

fn local_artifact_to_output(artifact: LocalRunArtifact) -> WorkflowArtifactOutput {
    WorkflowArtifactOutput {
        artifact_id: artifact.artifact_id,
        artifact_type: artifact.artifact_type,
        name: artifact.name,
        uri: artifact.uri,
        sha256: artifact.sha256,
        size_bytes: artifact.size_bytes,
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

fn workflow_context_value(
    package: &WorkflowPackage,
    run: &LocalRun,
    workflow_input: &Value,
    ledger: &LocalRunLedger,
    loop_context: Option<&Value>,
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
    let candidate = package
        .candidate
        .as_ref()
        .and_then(|policy| {
            run.artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == policy.artifact_id)
        })
        .map(|artifact| super::read_candidate(&artifact.uri))
        .transpose()?;
    let mut loops = serde_json::Map::new();
    if let Some(loop_context) = loop_context {
        if let Some(loop_id) = loop_context.get("id").and_then(Value::as_str) {
            loops.insert(loop_id.to_string(), loop_context.clone());
        }
    }
    Ok(serde_json::json!({
        "workflow": {
            "id": package.id,
            "version": package.version,
        },
        "input": workflow_input,
        "steps": output_map,
        "candidate": candidate,
        "loops": loops,
        "artifacts": run.artifacts,
        "run": {
            "run_id": run.run_id,
            "status": run.status,
            "workspace_ref": run.workspace_ref,
        }
    }))
}

fn execution_input_from_context(
    package: &WorkflowPackage,
    run: &LocalRun,
    workflow_input: &Value,
    workflow_context: &Value,
    step: &WorkflowStep,
) -> Result<Value, Box<dyn Error>> {
    let candidate = candidate_for_step(package, run, step)?;
    let mut workflow_context = workflow_context.clone();
    workflow_context["step_id"] = Value::String(step.id.clone());
    let mut input = match workflow_input {
        Value::Object(object) => object.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(step_input) = step.input.as_object() {
        for (name, value) in step_input {
            input.insert(name.clone(), value.clone());
        }
    }
    if let Some(candidate) = candidate.as_ref() {
        input.insert("candidate".to_string(), candidate.clone());
    }
    if step.candidate_action == "freeze" {
        if let Some(policy) = package.candidate.as_ref() {
            input.insert(
                "candidate_artifact_id".to_string(),
                Value::String(policy.artifact_id.clone()),
            );
            input.insert("allow_dirty".to_string(), Value::Bool(policy.allow_dirty));
        }
    }
    input.insert("workflow_context".to_string(), workflow_context);
    Ok(Value::Object(input))
}

fn candidate_for_step(
    package: &WorkflowPackage,
    run: &LocalRun,
    step: &WorkflowStep,
) -> Result<Option<Value>, Box<dyn Error>> {
    if step.candidate_action != "require" {
        return Ok(None);
    }
    let policy = package
        .candidate
        .as_ref()
        .ok_or("workflow candidate action requires a candidate policy")?;
    let artifact = run
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == policy.artifact_id)
        .ok_or_else(|| {
            format!(
                "workflow step {} requires frozen candidate artifact {}",
                step.id, policy.artifact_id
            )
        })?;
    Ok(Some(super::read_candidate(&artifact.uri)?))
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
        WorkflowPackage {
            schema_version: super::super::WORKFLOW_PACKAGE_SCHEMA_VERSION.to_string(),
            id: "com.himind.workflow.runner-test".to_string(),
            version: "1.0.0".to_string(),
            name: "Runner test".to_string(),
            description: String::new(),
            min_agent_version: "0.3.47".to_string(),
            local_requirements: serde_json::json!({}),
            optional_providers: Vec::new(),
            capabilities: vec!["test.first".to_string(), "test.second".to_string()],
            dependencies: Default::default(),
            candidate: None,
            steps: vec![
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
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "read_only".to_string(),
                    approval_required: false,
                    depends_on: vec!["STEP-1".to_string()],
                },
                WorkflowStep {
                    id: "APPROVE".to_string(),
                    title: "Approve".to_string(),
                    kind: "manual".to_string(),
                    capability_id: String::new(),
                    runtime: None,
                    loop_config: None,
                    when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "R3".to_string(),
                    approval_required: true,
                    depends_on: vec!["STEP-2".to_string()],
                },
            ],
            artifacts: Vec::new(),
            ui: super::super::WorkflowUi {
                mode: "standard".to_string(),
                entry: String::new(),
                surfaces: Vec::new(),
            },
            supported_runtimes: vec!["personal.codex".to_string()],
            created_at: String::new(),
            source_root: PathBuf::new(),
        }
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
        assert_eq!(blocked.blocked_step_id, "APPROVE");
        assert_eq!(blocked.run.status, LocalRunStatus::Waiting);
        assert_eq!(executor.steps.borrow().as_slice(), &["STEP-1", "STEP-2"]);

        let approved = runner.approve_step(blocked.run, "APPROVE").unwrap();
        let completed = runner
            .run_ready(&package, approved, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(completed.blocked_step_id, "");
        assert_eq!(completed.run.status, LocalRunStatus::Succeeded);
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
                if step.id == "STEP-2" {
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
        assert_eq!(outcome.completed_steps, vec!["STEP-1"]);
    }

    #[test]
    fn runner_records_artifacts_and_passes_prior_output_to_resumed_steps() {
        struct ArtifactExecutor {
            second_step_input: RefCell<Value>,
            artifact_path: PathBuf,
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
                            uri: self.artifact_path.to_string_lossy().to_string(),
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
        let artifact_root = std::env::temp_dir().join(format!(
            "himind-workflow-artifact-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&artifact_root).unwrap();
        let artifact_path = artifact_root.join("record.json");
        std::fs::write(&artifact_path, br#"{"value":42}"#).unwrap();
        std::fs::write(
            artifact_root.join("record.schema.json"),
            br#"{
              "type":"object",
              "required":["value"],
              "properties":{"value":{"type":"integer"}},
              "additionalProperties":false
            }"#,
        )
        .unwrap();
        workflow.source_root = artifact_root.clone();
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
            schema: "record.schema.json".to_string(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 1024,
        }];
        let executor = ArtifactExecutor {
            second_step_input: RefCell::new(Value::Null),
            artifact_path,
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
        assert_eq!(seen["workflow_context"]["steps"]["STEP-1"]["value"], 42);
        let _ = std::fs::remove_dir_all(artifact_root);
    }

    #[test]
    fn strict_artifact_schema_rejects_invalid_content() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-artifact-invalid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("record.json"), br#"{"value":"wrong"}"#).unwrap();
        std::fs::write(
            root.join("record.schema.json"),
            br#"{
              "type":"object",
              "required":["value"],
              "properties":{"value":{"type":"integer"}},
              "additionalProperties":false
            }"#,
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "record".to_string(),
            artifact_type: "record".to_string(),
            name: "Record".to_string(),
            schema: "record.schema.json".to_string(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 1024,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "record".to_string(),
            artifact_type: "record".to_string(),
            name: "Record".to_string(),
            uri: root.join("record.json").to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let mut workflow = package();
        workflow.source_root = root.clone();
        assert!(
            validate_artifact_contract(&workflow, &definition, &artifact)
                .unwrap_err()
                .to_string()
                .contains("schema validation")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn loop_executes_child_runs_until_exit_condition() {
        struct LoopExecutor {
            calls: RefCell<u32>,
        }
        impl WorkflowStepExecutor for LoopExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                let mut calls = self.calls.borrow_mut();
                *calls += 1;
                let decision = if *calls >= 2 { "accepted" } else { "rejected" };
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "feedback": {"decision": decision}
                })))
            }
        }

        let ledger = ledger("loop");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        workflow.candidate = None;
        workflow.artifacts.clear();
        workflow.steps = vec![WorkflowStep {
            id: "DEV-LOOP".to_string(),
            title: "Development loop".to_string(),
            kind: "loop".to_string(),
            capability_id: String::new(),
            runtime: None,
            loop_config: Some(Box::new(super::super::WorkflowLoop {
                max_iterations: 4,
                continue_when: Some(super::super::WorkflowCondition {
                    operator: "not_equals".to_string(),
                    path: "loops.DEV-LOOP.latest.steps.BODY.feedback.decision".to_string(),
                    value: serde_json::json!("accepted"),
                    conditions: Vec::new(),
                }),
                exit_when: Some(super::super::WorkflowCondition {
                    operator: "equals".to_string(),
                    path: "loops.DEV-LOOP.latest.steps.BODY.feedback.decision".to_string(),
                    value: serde_json::json!("accepted"),
                    conditions: Vec::new(),
                }),
                steps: vec![WorkflowStep {
                    id: "BODY".to_string(),
                    title: "Change and verify".to_string(),
                    kind: "capability".to_string(),
                    capability_id: "test.body".to_string(),
                    runtime: None,
                    loop_config: None,
                    when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "local_write".to_string(),
                    approval_required: false,
                    depends_on: Vec::new(),
                }],
            })),
            when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "long_running".to_string(),
            risk_level: "local_write".to_string(),
            approval_required: false,
            depends_on: Vec::new(),
        }];
        let run = runner
            .start("agent-1", &workflow, "loop-request", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(
                &workflow,
                run,
                &serde_json::json!({}),
                &LoopExecutor {
                    calls: RefCell::new(0),
                },
            )
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);
        assert_eq!(outcome.completed_steps, vec!["DEV-LOOP"]);
        let child_runs = ledger
            .list_runs(20)
            .unwrap()
            .into_iter()
            .filter(|run| run.parent_run_id == outcome.run.run_id)
            .collect::<Vec<_>>();
        assert_eq!(child_runs.len(), 2);
    }
}
