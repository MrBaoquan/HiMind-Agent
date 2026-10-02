use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{WorkflowPackage, WorkflowStep};
use crate::agent_core_contracts::{
    InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport,
    LocalApprovalStatus, LocalRun, LocalRunApproval, LocalRunArtifact, LocalRunExecutionPlan,
    LocalRunStatus, LocalRunStep, LocalRunUsage, LocalStepStatus, RuntimeEvent, RuntimeEventType,
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkflowArtifactVerification {
    pub artifact_id: String,
    pub artifact_type: String,
    pub uri: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub schema_validation: String,
    pub candidate_bound: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkflowRunVerification {
    pub workflow_id: String,
    pub run_id: String,
    pub package_digest: String,
    pub signature_key_id: String,
    pub signature_algorithm: String,
    pub candidate_id: String,
    pub commit_sha: String,
    pub tree_digest: String,
    pub artifacts: Vec<WorkflowArtifactVerification>,
}

enum WorkflowStepOutcome {
    Execution(WorkflowStepExecution),
    LoopFeedback { iteration: u32, latest: Value },
}

pub(crate) struct WorkflowRunner {
    ledger: LocalRunLedger,
}

struct RunLeaseGuard {
    ledger: LocalRunLedger,
    run_id: String,
    owner: String,
    stop_tx: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RunLeaseGuard {
    fn acquire(ledger: LocalRunLedger, run_id: &str) -> Result<Self, Box<dyn Error>> {
        let owner = format!("workflow-pid-{}-{}", std::process::id(), run_id);
        if !ledger.acquire_run_lease(run_id, &owner, 300)? {
            return Err(format!("workflow run is leased by another process: {run_id}").into());
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread_ledger = ledger.clone();
        let thread_run_id = run_id.to_string();
        let thread_owner = owner.clone();
        let thread = thread::spawn(move || loop {
            match stop_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = thread_ledger.renew_run_lease(&thread_run_id, &thread_owner, 300);
                }
            }
        });
        Ok(Self {
            ledger,
            run_id: run_id.to_string(),
            owner,
            stop_tx,
            thread: Some(thread),
        })
    }
}

impl Drop for RunLeaseGuard {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.ledger.release_run_lease(&self.run_id, &self.owner);
    }
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
        // 无论从界面、定时计划还是命令行走进来，都先补齐启动表单声明的默认值，
        // 运行计划、上下文和步骤输入看到的是同一份输入。
        let merged_input = super::with_launch_defaults(package, input);
        let input = &merged_input;
        let now = timestamp();
        let execution_plan = Self::build_execution_plan(package, input)?;
        let completion_mode = execution_plan.completion_mode();
        let active_step_ids = execution_plan
            .active_step_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let package_digest = if package.source_root.is_dir() {
            super::store::package_digest(&package.source_root)?
        } else {
            String::new()
        };
        let signature_identity = if package.source_root.is_dir() {
            super::store::package_signature_identity(&package.source_root)?
        } else {
            None
        };
        let mut workflow_context = serde_json::json!({
            "id": package.id,
            "version": package.version,
            "package_digest": package_digest,
        });
        if let Some((key_id, algorithm)) = signature_identity {
            if let Some(object) = workflow_context.as_object_mut() {
                object.insert("signature_key_id".to_string(), Value::String(key_id));
                object.insert("signature_algorithm".to_string(), Value::String(algorithm));
            }
        }
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
                "workflow": workflow_context,
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
        let mut run = LocalRun {
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
            completion_mode,
            execution_plan: Some(execution_plan),
            steps: package
                .steps
                .iter()
                .map(|step| LocalRunStep {
                    step_id: step.id.clone(),
                    title: step.title.clone(),
                    status: if active_step_ids.contains(&step.id) {
                        LocalStepStatus::Pending
                    } else {
                        LocalStepStatus::Skipped
                    },
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
                .filter(|step| step.approval_required && active_step_ids.contains(&step.id))
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
        self.persist_run(&mut run)?;
        Ok(run)
    }

    pub(crate) fn build_execution_plan(
        package: &WorkflowPackage,
        input: &Value,
    ) -> Result<LocalRunExecutionPlan, Box<dyn Error>> {
        let execution = input
            .get("execution")
            .or_else(|| input.pointer("/workflow_context/execution"));
        let requested_entrypoint = execution
            .and_then(|value| value.get("entrypoint"))
            .or_else(|| input.get("entrypoint"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let requested_exitpoint = execution
            .and_then(|value| value.get("exitpoint"))
            .or_else(|| input.get("exitpoint"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let seed_artifacts = execution
            .and_then(|value| value.get("seed_artifacts"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut unique_seed_artifacts = HashSet::new();
        for artifact_id in &seed_artifacts {
            if !unique_seed_artifacts.insert(artifact_id.as_str()) {
                return Err(format!("workflow seed artifact is duplicated: {artifact_id}").into());
            }
            if !package
                .artifacts
                .iter()
                .any(|artifact| artifact.id == *artifact_id)
            {
                return Err(format!(
                    "workflow seed artifact is not declared by the package: {artifact_id}"
                )
                .into());
            }
        }

        if package.execution_policy == "strict" {
            if !requested_entrypoint.is_empty() || !requested_exitpoint.is_empty() {
                return Err(
                    "strict workflow does not allow selecting entrypoint or exitpoint".into(),
                );
            }
            let entry_step_id = package
                .steps
                .first()
                .map(|step| step.id.clone())
                .ok_or("workflow has no entry step")?;
            let exit_step_id = package
                .steps
                .last()
                .map(|step| step.id.clone())
                .ok_or("workflow has no exit step")?;
            let active_step_ids = package
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect::<Vec<_>>();
            return Ok(plan_with_digest(LocalRunExecutionPlan {
                workflow_id: package.id.clone(),
                execution_policy: package.execution_policy.clone(),
                entrypoint: "full".to_string(),
                exitpoint: "full".to_string(),
                entry_step_id,
                exit_step_id,
                active_step_ids,
                seed_artifacts,
                assumptions: Vec::new(),
                plan_digest: String::new(),
            })?);
        }

        // 调用方没指定就用包声明的默认端点：启动一个工作流不该逼人先选入口出口。
        let requested_entrypoint = if requested_entrypoint.is_empty() {
            package.default_entrypoint.trim()
        } else {
            requested_entrypoint
        };
        let requested_exitpoint = if requested_exitpoint.is_empty() {
            package.default_exitpoint.trim()
        } else {
            requested_exitpoint
        };
        let entry = select_endpoint(&package.entrypoints, requested_entrypoint, "entrypoint")?;
        let exit = select_endpoint(&package.exits, requested_exitpoint, "exit")?;
        for requirement in &entry.requires {
            if !requirement_satisfied(requirement, &seed_artifacts, input) {
                return Err(format!(
                    "workflow entrypoint {} requires seed artifact or verifiable fact: {}",
                    entry.id, requirement
                )
                .into());
            }
        }
        let dependencies = package
            .steps
            .iter()
            .map(|step| (step.id.clone(), step.depends_on.clone()))
            .collect::<HashMap<_, _>>();
        let entry_dependencies = transitive_dependencies(&entry.at_step, &dependencies);
        let exit_dependencies = transitive_dependencies(&exit.at_step, &dependencies);
        let mut active = HashSet::new();
        active.insert(entry.at_step.clone());
        active.insert(exit.at_step.clone());
        for step in &package.steps {
            let dependencies = transitive_dependencies(&step.id, &dependencies);
            if dependencies.contains(&entry.at_step) && exit_dependencies.contains(&step.id) {
                active.insert(step.id.clone());
            }
        }
        if !active.contains(&exit.at_step) {
            return Err(format!(
                "workflow exit {} is not reachable from entrypoint {}",
                exit.id, entry.id
            )
            .into());
        }
        let active_step_ids = package
            .steps
            .iter()
            .filter(|step| active.contains(&step.id))
            .map(|step| step.id.clone())
            .collect::<Vec<_>>();
        let mut assumptions = entry.requires.clone();
        for dependency in entry_dependencies {
            assumptions.push(format!("step:{dependency}:satisfied_by_seed"));
        }
        Ok(plan_with_digest(LocalRunExecutionPlan {
            workflow_id: package.id.clone(),
            execution_policy: package.execution_policy.clone(),
            entrypoint: entry.id.clone(),
            exitpoint: exit.id.clone(),
            entry_step_id: entry.at_step.clone(),
            exit_step_id: exit.at_step.clone(),
            active_step_ids,
            seed_artifacts,
            assumptions,
            plan_digest: String::new(),
        })?)
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
        let merged_workflow_input = super::with_launch_defaults(package, workflow_input);
        let workflow_input = &merged_workflow_input;
        if run.status.is_terminal() {
            return Ok(outcome(run, String::new(), Vec::new()));
        }
        let _lease = RunLeaseGuard::acquire(self.ledger.clone(), &run.run_id)?;
        let mut completed_steps = Vec::new();

        loop {
            if let Some(current) = self.ledger.get_run(&run.run_id)? {
                if current.status == LocalRunStatus::Canceled {
                    return Ok(outcome(current, String::new(), completed_steps));
                }
            }
            self.resume_feedback_loop(package, &mut run)?;
            if execution_plan_exit_reached(&run) {
                return self.finish_partial_run(package, run, completed_steps);
            }
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
                if all_succeeded && enforce_required_artifacts && run.completion_mode == "full" {
                    if let Err(error) = verify_run(package, &run) {
                        run.status = LocalRunStatus::Failed;
                        run.error = error.to_string();
                    }
                }
                run.current_step_id.clear();
                run.updated_at = timestamp();
                self.persist_run(&mut run)?;
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
                    self.persist_run(&mut run)?;
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
                self.persist_run(&mut run)?;
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
            self.persist_run(&mut run)?;
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
                Ok(WorkflowStepOutcome::Execution(
                    WorkflowStepExecution::output(serde_json::json!({
                        "manual_approved": true,
                        "step_id": step.id,
                    })),
                ))
            } else {
                let step_input = execution_input_from_context(
                    package,
                    &run,
                    workflow_input,
                    &condition_context,
                    step,
                )?;
                executor
                    .execute(package, step, &step_input)
                    .map(WorkflowStepOutcome::Execution)
            };
            if let Some(current) = self.ledger.get_run(&run.run_id)? {
                if current.status == LocalRunStatus::Canceled {
                    return Ok(outcome(current, String::new(), completed_steps));
                }
            }
            let candidate = candidate_for_step(package, &run, step)?;
            let run_step = run
                .steps
                .iter_mut()
                .find(|candidate| candidate.step_id == step.id)
                .ok_or("workflow step state disappeared")?;
            run_step.finished_at = timestamp();
            match result {
                Ok(WorkflowStepOutcome::LoopFeedback { iteration, latest }) => {
                    run_step.status = LocalStepStatus::Waiting;
                    run_step.finished_at = timestamp();
                    run.status = LocalRunStatus::Waiting;
                    run.current_step_id = step.id.clone();
                    run.updated_at = timestamp();
                    self.persist_run(&mut run)?;
                    self.append_event(
                        &run,
                        step,
                        RuntimeEventType::QuestionRequested,
                        serde_json::json!({
                            "question_id": format!("{}:{}:feedback", run.run_id, iteration),
                            "loop_id": step.id,
                            "iteration": iteration,
                            "prompt": "请提供下一轮开发需要处理的反馈",
                            "waiting_for_feedback": true,
                            "latest": latest,
                        }),
                    )?;
                    return Ok(outcome(run, step.id.clone(), completed_steps));
                }
                Ok(WorkflowStepOutcome::Execution(execution)) => {
                    if let Some(provider) = execution
                        .output
                        .get("provider")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                    {
                        run_step.runtime_provider = provider.to_string();
                    }
                    if let Some(message) = step_failure(step, &condition_context, &execution)? {
                        if tolerates_failure(step) {
                            let message = degraded_failure_message(&message);
                            run_step.status = LocalStepStatus::Skipped;
                            run_step.error = message.clone();
                            run_step.finished_at = timestamp();
                            run.updated_at = timestamp();
                            self.persist_run(&mut run)?;
                            self.append_event(
                                &run,
                                step,
                                RuntimeEventType::Error,
                                serde_json::json!({
                                    "error": message,
                                    "degraded": true,
                                    "on_failure": step.on_failure,
                                }),
                            )?;
                            continue;
                        }
                        run_step.status = LocalStepStatus::Failed;
                        run_step.error = message.clone();
                        run.status = LocalRunStatus::Failed;
                        run.error = message.clone();
                        run.current_step_id.clear();
                        run.updated_at = timestamp();
                        self.persist_run(&mut run)?;
                        self.append_event(
                            &run,
                            step,
                            RuntimeEventType::Error,
                            serde_json::json!({"error": message}),
                        )?;
                        return Ok(outcome(run, String::new(), completed_steps));
                    }
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
                    self.persist_run(&mut run)?;
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
                    if tolerates_failure(step) {
                        // The workflow declared this step as degradable, so a
                        // failure is recorded on the step and the run keeps
                        // going; downstream steps see the step as skipped and
                        // must handle the missing output themselves.
                        let message = degraded_failure_message(&message);
                        run_step.status = LocalStepStatus::Skipped;
                        run_step.error = message.clone();
                        run_step.finished_at = timestamp();
                        run.updated_at = timestamp();
                        self.persist_run(&mut run)?;
                        self.append_event(
                            &run,
                            step,
                            RuntimeEventType::Error,
                            serde_json::json!({
                                "error": message,
                                "degraded": true,
                                "on_failure": step.on_failure,
                            }),
                        )?;
                        continue;
                    }
                    run_step.status = LocalStepStatus::Failed;
                    run_step.error = message.clone();
                    run.status = LocalRunStatus::Failed;
                    run.error = message.clone();
                    run.current_step_id.clear();
                    run.updated_at = timestamp();
                    self.persist_run(&mut run)?;
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

    fn finish_partial_run(
        &self,
        package: &WorkflowPackage,
        mut run: LocalRun,
        completed_steps: Vec<String>,
    ) -> Result<WorkflowRunOutcome, Box<dyn Error>> {
        let plan = run
            .execution_plan
            .as_ref()
            .ok_or("partial workflow run is missing an execution plan")?;
        let exit_step_id = plan.exit_step_id.clone();
        let exitpoint = plan.exitpoint.clone();
        let plan_digest = plan.plan_digest.clone();
        let active = plan
            .active_step_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let now = timestamp();
        for step in &mut run.steps {
            if active.contains(step.step_id.as_str()) && step.status == LocalStepStatus::Pending {
                step.status = LocalStepStatus::Skipped;
                step.finished_at = now.clone();
                step.error = format!("skipped after workflow exitpoint {exitpoint}");
            }
        }
        run.status = if run
            .steps
            .iter()
            .any(|step| step.status == LocalStepStatus::Failed)
        {
            LocalRunStatus::Failed
        } else {
            LocalRunStatus::Succeeded
        };
        run.current_step_id.clear();
        run.updated_at = now;
        self.persist_run(&mut run)?;
        if let Some(exit_step) = package.steps.iter().find(|step| step.id == exit_step_id) {
            self.append_event(
                &run,
                exit_step,
                RuntimeEventType::Progress,
                serde_json::json!({
                    "completion_mode": "partial",
                    "exitpoint": exitpoint,
                    "plan_digest": plan_digest,
                }),
            )?;
        }
        Ok(outcome(run, String::new(), completed_steps))
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
        self.persist_run(&mut run)?;
        self.append_approval_event(&run, step_id, "approved")?;
        Ok(run)
    }

    fn resume_feedback_loop(
        &self,
        package: &WorkflowPackage,
        run: &mut LocalRun,
    ) -> Result<(), Box<dyn Error>> {
        if run.status != LocalRunStatus::Waiting || run.current_step_id.trim().is_empty() {
            return Ok(());
        }
        let Some(step) = package
            .steps
            .iter()
            .find(|step| step.id == run.current_step_id)
        else {
            return Ok(());
        };
        let Some(loop_config) = step.loop_config.as_ref() else {
            return Ok(());
        };
        if !loop_config.pause_for_feedback {
            return Ok(());
        }
        if let Some((_, iteration)) = self.pending_loop_question(run, &step.id)? {
            return Err(format!(
                "workflow loop {} is waiting for user feedback after iteration {}",
                step.id, iteration
            )
            .into());
        }
        if let Some(run_step) = run
            .steps
            .iter_mut()
            .find(|candidate| candidate.step_id == step.id)
        {
            run_step.status = LocalStepStatus::Pending;
        }
        run.status = LocalRunStatus::Queued;
        run.current_step_id.clear();
        run.updated_at = timestamp();
        self.persist_run(run)?;
        Ok(())
    }

    pub(crate) fn record_loop_feedback(
        &self,
        package: &WorkflowPackage,
        run: LocalRun,
        feedback: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        if run.status != LocalRunStatus::Waiting || run.current_step_id.trim().is_empty() {
            return Err("workflow run is not waiting for loop feedback".into());
        }
        let step = package
            .steps
            .iter()
            .find(|step| step.id == run.current_step_id)
            .ok_or("workflow loop step state is missing")?;
        let loop_config = step
            .loop_config
            .as_ref()
            .filter(|loop_config| loop_config.pause_for_feedback)
            .ok_or("workflow run is not waiting for loop feedback")?;
        let feedback = feedback.trim();
        if feedback.is_empty() {
            return Err("workflow loop feedback is required".into());
        }
        if feedback.len() > 8000 {
            return Err("workflow loop feedback exceeds 8000 bytes".into());
        }
        let (question_event_id, iteration) = self
            .pending_loop_question(&run, &step.id)?
            .ok_or("workflow loop has no pending feedback question")?;
        if iteration > loop_config.max_iterations {
            return Err("workflow loop feedback iteration is invalid".into());
        }
        self.append_event(
            &run,
            step,
            RuntimeEventType::QuestionResolved,
            serde_json::json!({
                "question_event_id": question_event_id,
                "loop_id": step.id,
                "iteration": iteration,
                "feedback": feedback,
                "resolved_at": timestamp(),
            }),
        )?;
        Ok(run)
    }

    fn pending_loop_question(
        &self,
        run: &LocalRun,
        loop_id: &str,
    ) -> Result<Option<(String, u32)>, Box<dyn Error>> {
        let mut pending = None;
        for event in self.ledger.list_events(&run.run_id)? {
            if event.step_id != loop_id
                || event.payload.get("loop_id").and_then(Value::as_str) != Some(loop_id)
            {
                continue;
            }
            match event.event_type {
                RuntimeEventType::QuestionRequested => {
                    let iteration = event
                        .payload
                        .get("iteration")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or("workflow feedback question iteration is invalid")?;
                    pending = Some((event.event_id, iteration));
                }
                RuntimeEventType::Progress
                    if event
                        .payload
                        .get("waiting_for_feedback")
                        .and_then(Value::as_bool)
                        == Some(true) =>
                {
                    let iteration = event
                        .payload
                        .get("iteration")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or_default();
                    pending = Some((event.event_id, iteration));
                }
                RuntimeEventType::QuestionResolved => {
                    let question_event_id = event
                        .payload
                        .get("question_event_id")
                        .and_then(Value::as_str);
                    if question_event_id.is_none()
                        || pending.as_ref().is_some_and(|(pending_id, _)| {
                            Some(pending_id.as_str()) == question_event_id
                        })
                    {
                        pending = None;
                    }
                }
                _ => {}
            }
        }
        Ok(pending)
    }

    fn loop_history_from_events(
        &self,
        run: &LocalRun,
        loop_id: &str,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        use std::collections::BTreeMap;

        let mut by_iteration = BTreeMap::new();
        let mut feedback_by_iteration = BTreeMap::new();
        for event in self.ledger.list_events(&run.run_id)? {
            if event.payload.get("loop_id").and_then(Value::as_str) != Some(loop_id) {
                continue;
            }
            let Some(iteration) = event.payload.get("iteration").and_then(Value::as_u64) else {
                continue;
            };
            if event.event_type == RuntimeEventType::Progress {
                if let Some(latest) = event.payload.get("latest") {
                    by_iteration.insert(iteration, latest.clone());
                }
            } else if event.event_type == RuntimeEventType::QuestionResolved {
                if let Some(feedback) = event.payload.get("feedback").and_then(Value::as_str) {
                    feedback_by_iteration.insert(
                        iteration,
                        serde_json::json!({
                            "text": feedback,
                            "resolved_at": event.payload.get("resolved_at").cloned().unwrap_or(Value::Null),
                        }),
                    );
                }
            }
        }
        Ok(by_iteration
            .into_iter()
            .map(|(iteration, latest)| {
                let mut latest = latest;
                if let (Some(object), Some(feedback)) = (
                    latest.as_object_mut(),
                    feedback_by_iteration.get(&iteration),
                ) {
                    object.insert("user_feedback".to_string(), feedback.clone());
                }
                latest
            })
            .collect())
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
    ) -> Result<WorkflowStepOutcome, Box<dyn Error>> {
        let mut history = self.loop_history_from_events(parent_run, &step.id)?;
        let mut artifacts = Vec::new();
        let child_package = WorkflowPackage {
            steps: loop_config.steps.clone(),
            candidate: None,
            execution_policy: "strict".to_string(),
            entrypoints: Vec::new(),
            default_entrypoint: String::new(),
            default_exitpoint: String::new(),
            exits: Vec::new(),
            ..package.clone()
        };
        let start_iteration = u32::try_from(history.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        for iteration in start_iteration..=loop_config.max_iterations {
            let request_id = run_suffix(&format!(
                "{}_loop_{}_{}",
                parent_run.run_id, step.id, iteration
            ));
            let last_user_feedback = history
                .last()
                .and_then(|latest| latest.get("user_feedback"))
                .cloned()
                .unwrap_or(Value::Null);
            let loop_context = serde_json::json!({
                "id": step.id,
                "iteration": iteration,
                "history": history.clone(),
                "last_user_feedback": last_user_feedback,
            });
            let child_run_id = format!("workflow_run_{request_id}");
            let child_run = if let Some(existing) = self.ledger.get_run(&child_run_id)? {
                existing
            } else {
                self.start_with_parent(
                    "local-agent",
                    &child_package,
                    &request_id,
                    workflow_input,
                    &parent_run.run_id,
                )?
            };
            let outcome = if child_run.status.is_terminal() {
                WorkflowRunOutcome {
                    run: child_run,
                    blocked_step_id: String::new(),
                    completed_steps: Vec::new(),
                }
            } else {
                self.run_ready_internal(
                    &child_package,
                    child_run,
                    workflow_input,
                    executor,
                    false,
                    Some(&loop_context),
                )?
            };
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
                return Ok(WorkflowStepOutcome::Execution(WorkflowStepExecution {
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
                }));
            }
            if loop_config.pause_for_feedback {
                return Ok(WorkflowStepOutcome::LoopFeedback { iteration, latest });
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
        self.persist_run(&mut run)?;
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
        if let Err(error) = self.persist_run(&mut run) {
            if let Some(existing) = self.ledger.get_run(&run.run_id)? {
                if existing.status.is_terminal() {
                    return Ok(existing);
                }
            }
            return Err(error);
        }
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

    fn persist_run(&self, run: &mut LocalRun) -> Result<(), Box<dyn Error>> {
        if let Err(error) = self.ledger.save_run(run) {
            if error.to_string() == "terminal local run status cannot be changed" {
                if let Some(existing) = self.ledger.get_run(&run.run_id)? {
                    if existing.status == LocalRunStatus::Canceled {
                        *run = existing;
                        return Ok(());
                    }
                }
            }
            return Err(error);
        }
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

fn requirement_satisfied(requirement: &str, seed_artifacts: &[String], input: &Value) -> bool {
    if seed_artifacts.iter().any(|item| item == requirement) {
        return true;
    }
    let requirement = requirement.trim();
    let facts = input
        .get("facts")
        .or_else(|| input.get("workflow_facts"))
        .and_then(Value::as_object);
    if facts
        .and_then(|facts| facts.get(requirement))
        .is_some_and(|value| value.as_bool().unwrap_or(!value.is_null()))
    {
        return true;
    }
    let aliases = match requirement {
        "project" => &["project", "project_root", "repository_root"][..],
        "workspace" => &["workspace", "workspace_root", "project_root"][..],
        _ => &[requirement][..],
    };
    aliases.iter().any(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
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
    let candidate_id = candidate
        .get("candidate_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let candidate_commit = candidate
        .get("commit_sha")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let candidate_tree = candidate
        .get("tree_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(path) = artifact_path(&artifact.uri) else {
        return Ok(());
    };
    let Ok(instance) = serde_json::from_slice::<Value>(&std::fs::read(path)?) else {
        return Ok(());
    };
    let binding_required = matches!(
        definition.artifact_type.as_str(),
        "wechat_preview" | "wechat_experience_version" | "acceptance_report" | "release_record"
    );
    let artifact_candidate = instance.get("candidate_id").and_then(Value::as_str);
    let artifact_commit = instance.get("commit_sha").and_then(Value::as_str);
    let artifact_tree = instance.get("tree_digest").and_then(Value::as_str);
    if binding_required
        && (artifact_candidate.is_none() || artifact_commit.is_none() || artifact_tree.is_none())
    {
        return Err(format!(
            "workflow artifact {} must bind candidate_id, commit_sha and tree_digest",
            definition.id
        )
        .into());
    }
    if artifact_candidate.is_some_and(|value| value != candidate_id) {
        return Err(format!(
            "workflow artifact {} candidate_id does not match the frozen candidate",
            definition.id
        )
        .into());
    }
    if artifact_commit.is_some_and(|value| value != candidate_commit) {
        return Err(format!(
            "workflow artifact {} commit_sha does not match the frozen candidate",
            definition.id
        )
        .into());
    }
    if artifact_tree.is_some_and(|value| value != candidate_tree) {
        return Err(format!(
            "workflow artifact {} tree_digest does not match the frozen candidate",
            definition.id
        )
        .into());
    }
    if definition.artifact_type == "acceptance_report" {
        validate_acceptance_attestation(&instance)?;
    }
    Ok(())
}

fn validate_acceptance_attestation(instance: &Value) -> Result<(), Box<dyn Error>> {
    if instance.get("passed").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    let tester_id = instance
        .get("tester_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if tester_id.is_empty() {
        return Err("passed acceptance report must include tester_id".into());
    }
    let signature = instance
        .get("evidence_signature")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !signature.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(
            "passed acceptance report must include evidence_signature=sha256:<64 hex chars>".into(),
        );
    }
    let chain = instance
        .get("responsibility_chain")
        .and_then(Value::as_array)
        .filter(|chain| !chain.is_empty())
        .ok_or("passed acceptance report must include responsibility_chain")?;
    for (index, actor) in chain.iter().enumerate() {
        let actor = actor
            .as_object()
            .ok_or_else(|| format!("acceptance responsibility_chain[{index}] must be an object"))?;
        for field in ["actor_id", "role", "action", "occurred_at"] {
            if actor
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(
                    format!("acceptance responsibility_chain[{index}] is missing {field}").into(),
                );
            }
        }
    }
    let evidence_paths = instance
        .get("evidence_paths")
        .and_then(Value::as_array)
        .ok_or("acceptance report evidence_paths must be an array")?;
    let expected = evidence_manifest_signature(evidence_paths)?;
    if !signature.eq_ignore_ascii_case(&expected) {
        return Err("acceptance evidence_signature does not match evidence_paths".into());
    }
    Ok(())
}

fn evidence_manifest_signature(paths: &[Value]) -> Result<String, Box<dyn Error>> {
    let mut entries = Vec::new();
    for value in paths {
        let raw = value
            .as_str()
            .ok_or("acceptance evidence_paths entries must be strings")?
            .trim();
        if raw.is_empty() {
            return Err("acceptance evidence_paths entries cannot be empty".into());
        }
        let path = PathBuf::from(raw);
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("acceptance evidence file is unavailable: {raw}: {error}"))?;
        let digest = Sha256::digest(std::fs::read(&canonical)?);
        // The producer signs the normalized artifact path, while the Agent
        // reads the canonical target to prevent a missing/deleted evidence
        // file from passing verification. Strip Windows' extended prefix so
        // the digest is stable across Go and Rust path APIs.
        let canonical_text = canonical.to_string_lossy().to_string();
        let normalized_path = canonical_text
            .strip_prefix(r"\\?\")
            .unwrap_or(&canonical_text)
            .to_string();
        entries.push(format!("{}|{:x}", normalized_path, digest));
    }
    if entries.is_empty() {
        return Err("acceptance report must include at least one evidence file".into());
    }
    entries.sort();
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(entries.join("\n").as_bytes())
    ))
}

/// A step opts into degradation with `on_failure: "continue"`. The failure then
/// stops the step instead of the run, so a workflow can keep producing its
/// deterministic output when an optional step (for example an AI insight) is
/// unavailable.
fn tolerates_failure(step: &WorkflowStep) -> bool {
    step.on_failure.trim().eq_ignore_ascii_case("continue")
}

fn degraded_failure_message(message: &str) -> String {
    format!("degraded after failure: {}", message.trim())
}

fn step_failure(
    step: &WorkflowStep,
    condition_context: &Value,
    execution: &WorkflowStepExecution,
) -> Result<Option<String>, Box<dyn Error>> {
    let Some(condition) = step.fail_when.as_ref() else {
        return Ok(None);
    };
    let mut context = condition_context.clone();
    if let Some(steps) = context.get_mut("steps").and_then(Value::as_object_mut) {
        steps.insert(step.id.clone(), execution.output.clone());
    }
    if super::evaluate_condition(condition, &context)? {
        return Ok(Some(format!(
            "workflow step {} failed its fail_when condition",
            step.id
        )));
    }
    Ok(None)
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
    if !definition.schema.trim().is_empty() {
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
        if !errors.is_empty() {
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
            } else {
                return Err(message.into());
            }
        }
    }
    validate_artifact_digest(definition, &path, &artifact.sha256)
}

fn validate_artifact_digest(
    definition: &super::WorkflowArtifact,
    path: &std::path::Path,
    declared: &str,
) -> Result<(), Box<dyn Error>> {
    let declared = declared.trim();
    if declared.is_empty() {
        if definition.validation == "strict" {
            return Err(format!(
                "workflow artifact {} must include sha256 for strict validation",
                definition.id
            )
            .into());
        }
        return Ok(());
    }
    if declared.len() != 64 || !declared.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "workflow artifact {} sha256 must be a 64-character hexadecimal digest",
            definition.id
        )
        .into());
    }
    let actual = format!("{:x}", Sha256::digest(std::fs::read(path)?));
    if !actual.eq_ignore_ascii_case(declared) {
        return Err(format!(
            "workflow artifact {} sha256 does not match the file content",
            definition.id
        )
        .into());
    }
    Ok(())
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

pub(crate) fn verify_run(
    package: &WorkflowPackage,
    run: &LocalRun,
) -> Result<WorkflowRunVerification, Box<dyn Error>> {
    package.validate().map_err(std::io::Error::other)?;
    validate_required_artifacts(package, run)?;
    let candidate = package
        .candidate
        .as_ref()
        .map(|policy| {
            let artifact = run
                .artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == policy.artifact_id)
                .ok_or_else(|| {
                    format!(
                        "workflow run is missing candidate artifact {}",
                        policy.artifact_id
                    )
                })?;
            super::read_candidate(&artifact.uri)
        })
        .transpose()?;
    let candidate_id = candidate
        .as_ref()
        .and_then(|candidate| candidate.get("candidate_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let commit_sha = candidate
        .as_ref()
        .and_then(|candidate| candidate.get("commit_sha"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tree_digest = candidate
        .as_ref()
        .and_then(|candidate| candidate.get("tree_digest"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let package_digest = if package.source_root.is_dir() {
        super::store::package_digest(&package.source_root)?
    } else {
        String::new()
    };
    let (signature_key_id, signature_algorithm) = if package.source_root.is_dir() {
        super::store::package_signature_identity(&package.source_root)?.unwrap_or_default()
    } else {
        (String::new(), String::new())
    };
    if package
        .candidate
        .as_ref()
        .is_some_and(|policy| policy.required)
        && (candidate_id.is_empty() || commit_sha.is_empty() || tree_digest.is_empty())
    {
        return Err("workflow candidate identity is incomplete".into());
    }

    let mut artifacts = Vec::new();
    for artifact in &run.artifacts {
        let definition = package
            .artifacts
            .iter()
            .find(|definition| definition.id == artifact.artifact_id)
            .ok_or_else(|| {
                format!(
                    "workflow run contains undeclared artifact {}",
                    artifact.artifact_id
                )
            })?;
        if definition.artifact_type != artifact.artifact_type {
            return Err(format!(
                "workflow artifact {} type does not match package declaration",
                artifact.artifact_id
            )
            .into());
        }
        validate_artifact_contract(package, definition, artifact)?;
        validate_candidate_artifact_binding(definition, artifact, candidate.as_ref())?;
        let candidate_bound = !candidate_id.is_empty()
            && matches!(
                definition.artifact_type.as_str(),
                "wechat_preview"
                    | "wechat_experience_version"
                    | "acceptance_report"
                    | "release_record"
            );
        artifacts.push(WorkflowArtifactVerification {
            artifact_id: artifact.artifact_id.clone(),
            artifact_type: artifact.artifact_type.clone(),
            uri: artifact.uri.clone(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
            schema_validation: definition.validation.clone(),
            candidate_bound,
        });
    }
    Ok(WorkflowRunVerification {
        workflow_id: package.id.clone(),
        run_id: run.run_id.clone(),
        package_digest,
        signature_key_id,
        signature_algorithm,
        candidate_id,
        commit_sha,
        tree_digest,
        artifacts,
    })
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
    // Artifact 以文件路径进入步骤输入：数据走文件，提示词只描述任务。
    if let Some(runtime) = step.runtime.as_ref() {
        if !runtime.input_artifacts.is_empty() {
            let mut artifacts = serde_json::Map::new();
            for artifact_id in &runtime.input_artifacts {
                let artifact_id = artifact_id.trim();
                let artifact = run
                    .artifacts
                    .iter()
                    .find(|candidate| candidate.artifact_id == artifact_id)
                    .ok_or_else(|| {
                        format!(
                            "workflow step {} requires artifact {} before it can run",
                            step.id, artifact_id
                        )
                    })?;
                let path = artifact_path(&artifact.uri).ok_or_else(|| {
                    format!(
                        "workflow step {} cannot resolve artifact {} to a local file",
                        step.id, artifact_id
                    )
                })?;
                artifacts.insert(
                    artifact_id.to_string(),
                    Value::String(path.to_string_lossy().to_string()),
                );
            }
            input.insert("input_artifacts".to_string(), Value::Object(artifacts));
        }
    }
    if let Some(candidate) = candidate.as_ref() {
        input.insert("candidate".to_string(), candidate.clone());
    }
    for (name, value) in super::executor::candidate_freeze_inputs(package, step) {
        input.insert(name.to_string(), value);
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
                .is_some_and(|candidate| {
                    matches!(
                        candidate.status,
                        LocalStepStatus::Succeeded | LocalStepStatus::Skipped
                    )
                })
        });
        if dependencies_succeeded {
            return Ok(Some(step));
        }
    }
    Ok(None)
}

fn select_endpoint<'a>(
    endpoints: &'a [super::WorkflowEndpoint],
    requested: &str,
    label: &str,
) -> Result<&'a super::WorkflowEndpoint, Box<dyn Error>> {
    if !requested.is_empty() {
        return endpoints
            .iter()
            .find(|endpoint| endpoint.id == requested)
            .ok_or_else(|| format!("workflow {label} not found: {requested}").into());
    }
    if endpoints.len() == 1 {
        return Ok(&endpoints[0]);
    }
    Err(format!("workflow {label} selection is required").into())
}

fn transitive_dependencies(
    step_id: &str,
    dependencies: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    fn visit(
        step_id: &str,
        dependencies: &HashMap<String, Vec<String>>,
        result: &mut HashSet<String>,
    ) {
        let Some(items) = dependencies.get(step_id) else {
            return;
        };
        for dependency in items {
            if result.insert(dependency.clone()) {
                visit(dependency, dependencies, result);
            }
        }
    }

    let mut result = HashSet::new();
    visit(step_id, dependencies, &mut result);
    result
}

fn plan_with_digest(
    mut plan: LocalRunExecutionPlan,
) -> Result<LocalRunExecutionPlan, Box<dyn Error>> {
    let source = serde_json::to_vec(&serde_json::json!({
        "workflow_id": &plan.workflow_id,
        "execution_policy": &plan.execution_policy,
        "entrypoint": &plan.entrypoint,
        "exitpoint": &plan.exitpoint,
        "entry_step_id": &plan.entry_step_id,
        "exit_step_id": &plan.exit_step_id,
        "active_step_ids": &plan.active_step_ids,
        "seed_artifacts": &plan.seed_artifacts,
        "assumptions": &plan.assumptions,
    }))?;
    plan.plan_digest = format!("sha256:{:x}", Sha256::digest(source));
    Ok(plan)
}

fn execution_plan_exit_reached(run: &LocalRun) -> bool {
    let Some(plan) = run.execution_plan.as_ref() else {
        return false;
    };
    if run.completion_mode != "partial" {
        return false;
    }
    run.steps
        .iter()
        .find(|step| step.step_id == plan.exit_step_id)
        .is_some_and(|step| {
            matches!(
                step.status,
                LocalStepStatus::Succeeded | LocalStepStatus::Skipped
            )
        })
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
            distribution_targets: Vec::new(),
            name: "Runner test".to_string(),
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: "0.3.47".to_string(),
            local_requirements: serde_json::json!({}),
            optional_providers: Vec::new(),
            capabilities: vec!["test.first".to_string(), "test.second".to_string()],
            dependencies: Default::default(),
            candidate: None,
            execution_policy: "strict".to_string(),
            entrypoints: Vec::new(),
            default_entrypoint: String::new(),
            default_exitpoint: String::new(),
            exits: Vec::new(),
            steps: vec![
                WorkflowStep {
                    id: "STEP-1".to_string(),
                    title: "First".to_string(),
                    kind: "capability".to_string(),
                    capability_id: "test.first".to_string(),
                    runtime: None,
                    loop_config: None,
                    when: None,
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "read_only".to_string(),
                    approval_required: false,
                    on_failure: String::new(),
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
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "read_only".to_string(),
                    approval_required: false,
                    on_failure: String::new(),
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
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "R3".to_string(),
                    approval_required: true,
                    on_failure: String::new(),
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
            connectors: Vec::new(),
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

    #[test]
    fn a_step_can_degrade_instead_of_failing_the_run() {
        struct DegradeExecutor {
            seen: RefCell<Vec<String>>,
        }

        impl WorkflowStepExecutor for DegradeExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                self.seen.borrow_mut().push(step.id.clone());
                if step.id == "STEP-2" {
                    return Err("insight runtime is unavailable".into());
                }
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "ok": true
                })))
            }
        }

        let ledger = ledger("degraded-step");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        // The workflow declares the middle step as an optional enhancement.
        workflow.steps[1].on_failure = "continue".to_string();
        let run = runner
            .start(
                "agent-1",
                &workflow,
                "degrade-request",
                &serde_json::json!({}),
            )
            .unwrap();
        let executor = DegradeExecutor {
            seen: RefCell::new(Vec::new()),
        };
        let blocked = runner
            .run_ready(&workflow, run, &serde_json::json!({}), &executor)
            .unwrap();

        // The tolerated failure stops the step, not the run: the workflow still
        // reaches its approval gate instead of failing at the optional step.
        assert_eq!(blocked.blocked_step_id, "APPROVE");
        assert_eq!(blocked.run.status, LocalRunStatus::Waiting);
        let degraded = blocked
            .run
            .steps
            .iter()
            .find(|item| item.step_id == "STEP-2")
            .unwrap();
        // A tolerated failure keeps the reason visible while unblocking the run.
        assert_eq!(degraded.status, LocalStepStatus::Skipped);
        assert!(degraded.error.starts_with("degraded after failure"));
        assert!(ledger
            .list_events(&blocked.run.run_id)
            .unwrap()
            .iter()
            .any(|event| event.event_type == RuntimeEventType::Error));

        let approved = runner.approve_step(blocked.run, "APPROVE").unwrap();
        let completed = runner
            .run_ready(&workflow, approved, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(completed.run.status, LocalRunStatus::Succeeded);
        assert!(completed.run.error.is_empty());
    }

    #[test]
    fn a_step_failure_still_fails_the_run_by_default() {
        struct FailingExecutor;

        impl WorkflowStepExecutor for FailingExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                if step.id == "STEP-2" {
                    return Err("insight runtime is unavailable".into());
                }
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "ok": true
                })))
            }
        }

        let runner = WorkflowRunner::with_ledger(ledger("default-failure"));
        let workflow = package();
        let run = runner
            .start("agent-1", &workflow, "fail-request", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(&workflow, run, &serde_json::json!({}), &FailingExecutor)
            .unwrap();

        assert_eq!(outcome.run.status, LocalRunStatus::Failed);
        assert_eq!(
            outcome
                .run
                .steps
                .iter()
                .find(|item| item.step_id == "STEP-2")
                .unwrap()
                .status,
            LocalStepStatus::Failed
        );
    }

    #[test]
    fn skipped_dependency_satisfies_downstream_step() {
        let workflow = package();
        let runner = WorkflowRunner::with_ledger(ledger("skipped-dependency"));
        let mut run = runner
            .start(
                "agent-1",
                &workflow,
                "skipped-dependency",
                &serde_json::json!({}),
            )
            .unwrap();
        run.steps
            .iter_mut()
            .find(|step| step.step_id == "STEP-1")
            .unwrap()
            .status = LocalStepStatus::Skipped;

        let next = next_ready_step(&workflow, &run).unwrap().unwrap();
        assert_eq!(next.id, "STEP-2");
    }

    #[test]
    fn segmented_run_executes_only_the_selected_subgraph() {
        let runner = WorkflowRunner::with_ledger(ledger("segmented-plan"));
        let mut package = package();
        package.artifacts = vec![super::super::WorkflowArtifact {
            id: "candidate".to_string(),
            artifact_type: "candidate".to_string(),
            name: "Candidate".to_string(),
            schema: String::new(),
            required: false,
            validation: "advisory".to_string(),
            max_bytes: 1024,
        }];
        package.execution_policy = "segmented".to_string();
        package.entrypoints = vec![super::super::WorkflowEndpoint {
            id: "develop".to_string(),
            at_step: "STEP-2".to_string(),
            label: "Develop".to_string(),
            requires: vec!["candidate".to_string()],
            produces: Vec::new(),
        }];
        package.exits = vec![super::super::WorkflowEndpoint {
            id: "checkpoint".to_string(),
            at_step: "STEP-2".to_string(),
            label: "Checkpoint".to_string(),
            requires: Vec::new(),
            produces: vec!["development_checkpoint".to_string()],
        }];
        let input = serde_json::json!({
            "execution": {
                "entrypoint": "develop",
                "exitpoint": "checkpoint",
                "seed_artifacts": ["candidate"]
            }
        });
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "segmented-plan", &input)
            .unwrap();
        assert_eq!(run.completion_mode, "partial");
        assert_eq!(
            run.execution_plan.as_ref().unwrap().active_step_ids,
            vec!["STEP-2"]
        );
        let outcome = runner.run_ready(&package, run, &input, &executor).unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);
        assert_eq!(outcome.run.completion_mode, "partial");
        assert_eq!(executor.steps.borrow().as_slice(), &["STEP-2"]);
        let step_status = |step_id: &str| {
            outcome
                .run
                .steps
                .iter()
                .find(|step| step.step_id == step_id)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(step_status("STEP-1"), LocalStepStatus::Skipped);
        assert_eq!(step_status("STEP-2"), LocalStepStatus::Succeeded);
        assert_eq!(step_status("APPROVE"), LocalStepStatus::Skipped);
    }

    #[test]
    fn segmented_plan_requires_declared_seed_or_fact_for_entry_requirements() {
        let mut package = package();
        package.artifacts = vec![super::super::WorkflowArtifact {
            id: "candidate".to_string(),
            artifact_type: "candidate".to_string(),
            name: "Candidate".to_string(),
            schema: String::new(),
            required: false,
            validation: "advisory".to_string(),
            max_bytes: 1024,
        }];
        package.execution_policy = "segmented".to_string();
        package.entrypoints = vec![super::super::WorkflowEndpoint {
            id: "develop".to_string(),
            at_step: "STEP-2".to_string(),
            label: "Develop".to_string(),
            requires: vec!["candidate".to_string()],
            produces: Vec::new(),
        }];
        package.exits = vec![super::super::WorkflowEndpoint {
            id: "checkpoint".to_string(),
            at_step: "STEP-2".to_string(),
            label: "Checkpoint".to_string(),
            requires: Vec::new(),
            produces: Vec::new(),
        }];

        let missing = super::WorkflowRunner::build_execution_plan(
            &package,
            &serde_json::json!({
                "execution": {
                    "entrypoint": "develop",
                    "exitpoint": "checkpoint"
                }
            }),
        );
        assert!(missing
            .unwrap_err()
            .to_string()
            .contains("requires seed artifact"));

        let seeded = super::WorkflowRunner::build_execution_plan(
            &package,
            &serde_json::json!({
                "execution": {
                    "entrypoint": "develop",
                    "exitpoint": "checkpoint",
                    "seed_artifacts": ["candidate"]
                }
            }),
        );
        assert!(seeded.is_ok(), "{seeded:?}");
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
    fn runner_preserves_cancellation_that_arrives_during_a_step() {
        struct SlowExecutor;
        impl WorkflowStepExecutor for SlowExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                thread::sleep(Duration::from_millis(300));
                Ok(WorkflowStepExecution::output(Value::Null))
            }
        }

        let runner = WorkflowRunner::with_ledger(ledger("cancel-during-step"));
        let package = package();
        let run = runner
            .start(
                "agent-1",
                &package,
                "request-cancel",
                &serde_json::json!({}),
            )
            .unwrap();
        let run_id = run.run_id.clone();
        let cancel_runner = WorkflowRunner::with_ledger(runner.ledger.clone());
        let cancel_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let current = cancel_runner.ledger.get_run(&run_id).unwrap().unwrap();
            cancel_runner
                .cancel(current, "canceled during workflow step")
                .unwrap();
        });
        let outcome = runner
            .run_ready(&package, run, &serde_json::json!({}), &SlowExecutor)
            .unwrap();
        cancel_handle.join().unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Canceled);
        assert_eq!(outcome.run.error, "canceled during workflow step");
        assert!(outcome.completed_steps.is_empty());
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
                            sha256: format!(
                                "{:x}",
                                Sha256::digest(std::fs::read(&self.artifact_path).unwrap())
                            ),
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
                fail_when: None,
                candidate_action: String::new(),
                input: serde_json::json!({}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                on_failure: String::new(),
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
                fail_when: None,
                candidate_action: String::new(),
                input: serde_json::json!({"fixed": true}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                on_failure: String::new(),
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
    fn strict_artifact_requires_matching_sha256() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-artifact-digest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("record.json");
        std::fs::write(&path, br#"{"value":42}"#).unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "record".to_string(),
            artifact_type: "record".to_string(),
            name: "Record".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 1024,
        };
        let mut workflow = package();
        workflow.source_root = root.clone();

        let missing = LocalRunArtifact {
            artifact_id: "record".to_string(),
            artifact_type: "record".to_string(),
            name: "Record".to_string(),
            uri: path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        assert!(validate_artifact_contract(&workflow, &definition, &missing)
            .unwrap_err()
            .to_string()
            .contains("must include sha256"));

        let mismatched = LocalRunArtifact {
            sha256: "0".repeat(64),
            ..missing
        };
        assert!(
            validate_artifact_contract(&workflow, &definition, &mismatched)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fail_when_turns_successful_output_into_a_failed_step() {
        struct FailedAcceptanceExecutor;
        impl WorkflowStepExecutor for FailedAcceptanceExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                Ok(WorkflowStepExecution::output(serde_json::json!({
                    "passed": false
                })))
            }
        }

        let mut workflow = package();
        workflow.artifacts.clear();
        workflow.steps = vec![WorkflowStep {
            id: "WX-ACCEPTANCE".to_string(),
            title: "Acceptance".to_string(),
            kind: "capability".to_string(),
            capability_id: "test.first".to_string(),
            runtime: None,
            loop_config: None,
            when: None,
            fail_when: Some(super::super::WorkflowCondition {
                operator: "equals".to_string(),
                path: "steps.WX-ACCEPTANCE.passed".to_string(),
                value: serde_json::json!(false),
                conditions: Vec::new(),
            }),
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "sync".to_string(),
            risk_level: "local_write".to_string(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        }];
        let runner = WorkflowRunner::with_ledger(ledger("fail-when"));
        let run = runner
            .start("agent-1", &workflow, "fail-when", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(
                &workflow,
                run,
                &serde_json::json!({}),
                &FailedAcceptanceExecutor,
            )
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Failed);
        assert!(outcome.run.error.contains("fail_when"));
        assert_eq!(outcome.completed_steps.len(), 0);
    }

    #[test]
    fn candidate_binding_rejects_a_different_candidate_id() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-binding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let artifact_path = root.join("acceptance.json");
        std::fs::write(
            &artifact_path,
            br#"{
              "candidate_id":"candidate-other",
              "commit_sha":"commit-1",
              "tree_digest":"tree-1"
            }"#,
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 1024,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            uri: artifact_path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let candidate = serde_json::json!({
            "candidate_id":"candidate-current",
            "commit_sha":"commit-1",
            "tree_digest":"tree-1"
        });
        let error = validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate))
            .unwrap_err();
        assert!(error.to_string().contains("candidate_id"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn passed_acceptance_requires_stable_tester_id() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-attestation-tester-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let evidence_path = root.join("acceptance.png");
        std::fs::write(&evidence_path, b"evidence").unwrap();
        let artifact_path = root.join("acceptance.json");
        std::fs::write(
            &artifact_path,
            serde_json::json!({
                "candidate_id": "candidate-current",
                "commit_sha": "commit-1",
                "tree_digest": "tree-1",
                "passed": true,
                "evidence_paths": [evidence_path.to_string_lossy()]
            })
            .to_string(),
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 4096,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            uri: artifact_path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let candidate = serde_json::json!({
            "candidate_id": "candidate-current",
            "commit_sha": "commit-1",
            "tree_digest": "tree-1"
        });
        let error = validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate))
            .unwrap_err();
        assert!(error.to_string().contains("tester_id"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn passed_acceptance_requires_a_non_empty_responsibility_chain() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-attestation-chain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let artifact_path = root.join("acceptance.json");
        std::fs::write(
            &artifact_path,
            serde_json::json!({
                "candidate_id": "candidate-current",
                "commit_sha": "commit-1",
                "tree_digest": "tree-1",
                "passed": true,
                "tester_id": "tester-1",
                "attested_at": "2026-09-18T10:00:00Z",
                "evidence_signature": format!("sha256:{}", "0".repeat(64)),
                "responsibility_chain": []
            })
            .to_string(),
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 4096,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            uri: artifact_path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let candidate = serde_json::json!({
            "candidate_id": "candidate-current",
            "commit_sha": "commit-1",
            "tree_digest": "tree-1"
        });
        let error = validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate))
            .unwrap_err();
        assert!(error.to_string().contains("responsibility_chain"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn verify_rejects_modified_acceptance_evidence() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-attestation-signature-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let evidence_path = root.join("acceptance.png");
        std::fs::write(&evidence_path, b"original evidence").unwrap();
        let evidence_values = vec![Value::String(evidence_path.to_string_lossy().to_string())];
        let signature = evidence_manifest_signature(&evidence_values).unwrap();
        let artifact_path = root.join("acceptance.json");
        std::fs::write(
            &artifact_path,
            serde_json::json!({
                "candidate_id": "candidate-current",
                "commit_sha": "commit-1",
                "tree_digest": "tree-1",
                "passed": true,
                "tester_id": "tester-1",
                "attested_at": "2026-09-18T10:00:00Z",
                "responsibility_chain": [{
                    "actor_id": "tester-1",
                    "role": "tester",
                    "action": "accepted",
                    "occurred_at": "2026-09-18T10:00:00Z"
                }],
                "evidence_paths": [evidence_path.to_string_lossy()],
                "evidence_signature": signature
            })
            .to_string(),
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 4096,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            uri: artifact_path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let candidate = serde_json::json!({
            "candidate_id": "candidate-current",
            "commit_sha": "commit-1",
            "tree_digest": "tree-1"
        });
        validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate)).unwrap();
        std::fs::write(&evidence_path, b"modified evidence").unwrap();
        let error = validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate))
            .unwrap_err();
        assert!(error.to_string().contains("does not match evidence_paths"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_acceptance_does_not_require_attestation_fields() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-attestation-failed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let artifact_path = root.join("acceptance.json");
        std::fs::write(
            &artifact_path,
            br#"{
              "candidate_id":"candidate-current",
              "commit_sha":"commit-1",
              "tree_digest":"tree-1",
              "passed":false
            }"#,
        )
        .unwrap();
        let definition = super::super::WorkflowArtifact {
            id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            schema: String::new(),
            required: true,
            validation: "strict".to_string(),
            max_bytes: 4096,
        };
        let artifact = LocalRunArtifact {
            artifact_id: "acceptance-report".to_string(),
            artifact_type: "acceptance_report".to_string(),
            name: "Acceptance".to_string(),
            uri: artifact_path.to_string_lossy().to_string(),
            sha256: String::new(),
            size_bytes: 0,
        };
        let candidate = serde_json::json!({
            "candidate_id": "candidate-current",
            "commit_sha": "commit-1",
            "tree_digest": "tree-1"
        });
        validate_candidate_artifact_binding(&definition, &artifact, Some(&candidate)).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn loop_executes_child_runs_until_exit_condition() {
        struct LoopExecutor {
            calls: RefCell<u32>,
            inputs: RefCell<Vec<Value>>,
        }
        impl WorkflowStepExecutor for LoopExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                self.inputs.borrow_mut().push(input.clone());
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
                pause_for_feedback: true,
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
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    risk_level: "local_write".to_string(),
                    approval_required: false,
                    on_failure: String::new(),
                    depends_on: Vec::new(),
                }],
            })),
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "long_running".to_string(),
            risk_level: "local_write".to_string(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        }];
        let run = runner
            .start("agent-1", &workflow, "loop-request", &serde_json::json!({}))
            .unwrap();
        let executor = LoopExecutor {
            calls: RefCell::new(0),
            inputs: RefCell::new(Vec::new()),
        };
        let outcome = runner
            .run_ready(&workflow, run.clone(), &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Waiting);
        let missing_feedback = runner
            .run_ready(
                &workflow,
                outcome.run.clone(),
                &serde_json::json!({}),
                &executor,
            )
            .unwrap_err();
        assert!(missing_feedback
            .to_string()
            .contains("waiting for user feedback"));
        let run = runner
            .record_loop_feedback(&workflow, outcome.run, "请补充边界测试")
            .unwrap();
        let outcome = runner
            .run_ready(&workflow, run, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);
        assert_eq!(outcome.completed_steps, vec!["DEV-LOOP"]);
        let inputs = executor.inputs.borrow();
        assert_eq!(
            inputs[1]["workflow_context"]["loops"]["DEV-LOOP"]["history"][0]["user_feedback"]
                ["text"],
            "请补充边界测试"
        );
        let child_runs = ledger
            .list_runs(20)
            .unwrap()
            .into_iter()
            .filter(|run| run.parent_run_id == outcome.run.run_id)
            .collect::<Vec<_>>();
        assert_eq!(child_runs.len(), 2);
    }
}
