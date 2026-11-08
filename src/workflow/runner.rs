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
    LocalRunParentKind, LocalRunStatus, LocalRunStep, LocalRunUsage, LocalStepStatus, RunPauseKind,
    RuntimeEvent,
    RuntimeEventType, INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION,
    RUNTIME_EVENT_SCHEMA_VERSION,
};
use crate::store::local_runs::LocalRunLedger;

pub(crate) trait WorkflowStepExecutor {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>>;

    /// 能否被多个线程同时调用。
    ///
    /// 委派分支要真并发就必须让执行器跨线程使用。默认返回 `None`——既有执行器
    /// （含测试里的 `RefCell` 实现）不受影响，委派退回顺序执行；只有声明了自己
    /// 可以共享的实现才会真正并行。
    fn as_shared(&self) -> Option<&dyn SharedStepExecutor> {
        None
    }
}

/// 可在线程间共享的执行器。
///
/// 用独立 trait 而不是给 `WorkflowStepExecutor` 加 `Send + Sync` 约束：
/// 那会让所有既有实现（尤其是测试里带 `RefCell` 的）一起编译失败，
/// 而它们本来就不需要并发。
pub(crate) trait SharedStepExecutor: WorkflowStepExecutor + Send + Sync {}

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


/// 一条委派分支的执行结果。
struct DelegateBranchResult {
    status: LocalRunStatus,
    /// 失败描述；成功时为空串。
    failure: String,
    /// 汇总进父步骤输出的分支记录。
    json: Value,
    artifacts: Vec<LocalRunArtifact>,
}

impl DelegateBranchResult {
    fn failed(branch: &super::WorkflowDelegateBranch, failure: String) -> Self {
        Self {
            status: LocalRunStatus::Failed,
            failure,
            json: serde_json::json!({
                "branch_id": branch.id,
                "role_id": branch.role_id,
                "run_id": String::new(),
                "status": LocalRunStatus::Failed,
                "output": Value::Null,
            }),
            artifacts: Vec::new(),
        }
    }
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
        if !ledger.acquire_run_lease(
            run_id,
            &owner,
            crate::store::local_runs::RUN_LEASE_TTL_SECONDS,
        )? {
            return Err(format!("workflow run is leased by another process: {run_id}").into());
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread_ledger = ledger.clone();
        let thread_run_id = run_id.to_string();
        let thread_owner = owner.clone();
        let thread = thread::spawn(move || {
            let mut last_error = String::new();
            loop {
                match stop_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let renewal = thread_ledger.renew_run_lease(
                            &thread_run_id,
                            &thread_owner,
                            crate::store::local_runs::RUN_LEASE_TTL_SECONDS,
                        );
                        let error = match renewal {
                            Ok(true) => String::new(),
                            Ok(false) => "lease row is no longer owned".to_string(),
                            Err(error) => error.to_string(),
                        };
                        if !error.is_empty() && error != last_error {
                            crate::app::crash::record_event(
                                "warn",
                                &format!(
                                    "workflow lease renewal failed for {thread_run_id}: {error}"
                                ),
                            );
                            eprintln!("workflow lease renewal failed for {thread_run_id}: {error}");
                            last_error = error;
                        } else if error.is_empty() {
                            last_error.clear();
                        }
                    }
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

    /// 起一个从属运行。默认按「重做」记账：loop 迭代是现有唯一的子运行来源，
    /// 角色委派必须显式用 `start_delegated` 声明，否则会被算进返工率。
    pub(crate) fn start_with_parent(
        &self,
        agent_id: &str,
        package: &WorkflowPackage,
        request_id: &str,
        input: &Value,
        parent_run_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        self.start_with_parent_kind(
            agent_id,
            package,
            request_id,
            input,
            parent_run_id,
            LocalRunParentKind::Retry,
        )
    }

    /// 起一个委派给另一个角色的子运行：`parent_kind = delegate`，统计上算分工。
    pub(crate) fn start_delegated(
        &self,
        agent_id: &str,
        package: &WorkflowPackage,
        request_id: &str,
        input: &Value,
        parent_run_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        self.start_with_parent_kind(
            agent_id,
            package,
            request_id,
            input,
            parent_run_id,
            LocalRunParentKind::Delegate,
        )
    }

    fn start_with_parent_kind(
        &self,
        agent_id: &str,
        package: &WorkflowPackage,
        request_id: &str,
        input: &Value,
        parent_run_id: &str,
        parent_kind: LocalRunParentKind,
    ) -> Result<LocalRun, Box<dyn Error>> {
        // 运行期覆盖先落到包上：它只影响本次运行，且只允许收紧风险边界。
        // 必须在 build_execution_plan 之前应用，否则计划会按未覆盖的图算出来。
        let patched = super::apply_execution_overrides(package, input)?;
        let package = &patched;
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
            // 顶层运行没有父子语义：写 unspecified，避免空父运行却被标成「返工」。
            parent_kind: if parent_run_id.trim().is_empty() {
                LocalRunParentKind::Unspecified
            } else {
                parent_kind
            },
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
            pause_kind: crate::agent_core_contracts::RunPauseKind::None,
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
                    role_id: step.role_id.trim().to_string(),
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
        // 恢复运行也必须重放同一份覆盖：输入随 interaction 存过，恢复时后端拿不到
        // 原始调用参数，只能以存的 input 为准。否则「我批准过的这次多一层闸门」
        // 会在恢复后消失。
        let patched = super::apply_execution_overrides(package, workflow_input)?;
        let package = &patched;
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
                // 用户暂停：在步骤边界停住，不再推进。内存在 run 与台账可能不一致
                // （暂停是另一个调用写进去的），所以以台账为准。
                if current.pause_kind.is_user_pause() {
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
                run.pause_kind = super::super::agent_core_contracts::RunPauseKind::None;
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
                run.pause_kind = super::super::agent_core_contracts::RunPauseKind::Approval;
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

            // 步骤输入在成功分支之外的脱敏快照：结构化证据要记录「这一步看到了什么」。
            // 只保留键名与长度，不落具体值——输入里可能有凭据句柄与业务数据。
            let mut step_input_snapshot = Value::Null;
            let result = if let Some(delegate) = step.delegate.as_ref() {
                self.execute_delegate_step(
                    package,
                    &run,
                    workflow_input,
                    executor,
                    step,
                    delegate,
                    &|| {
                        self.ledger
                            .get_run(&run.run_id)
                            .map(|current| {
                                current.is_some_and(|c| c.status == LocalRunStatus::Canceled)
                            })
                            .unwrap_or(false)
                    },
                )
            } else if let Some(loop_config) = step.loop_config.as_ref() {
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
                step_input_snapshot = summarize_step_input(&step_input);
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
                    run.pause_kind = super::super::agent_core_contracts::RunPauseKind::Feedback;
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
                            // 结构化证据：回答「谁依据什么做了什么判断」，
                            // 不依赖各 Runtime 互不相同的原始对话格式。
                            "evidence": serde_json::json!({
                                "role_id": step.role_id,
                                "capability_id": step.capability_id,
                                "runtime_provider": run_step_provider(&run.run_id, step, &self.ledger),
                                "inputs": step_input_snapshot,
                                "artifact_ids": run.artifacts.iter().map(|artifact| &artifact.artifact_id).collect::<Vec<_>>(),
                            }),
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

    /// 把一个步骤拆成多条并行子运行，各跑一条分支。
    ///
    /// 与 loop 的区别在语义和统计：
    /// - loop 反复执行**同一组步骤**，子运行是「重做」，计入 `rework_runs`；
    /// - delegate 把工作分给**不同角色**，子运行是「分工」，计入 `delegated_runs`。
    ///
    /// 每个分支起一条独立子运行（`start_delegated`），父运行收集各分支结果后继续推进。
    /// 只要达到 `min_succeeded` 就算这一步成功；未达标时整步失败并带上各分支的错误。
    fn execute_delegate_step(
        &self,
        package: &WorkflowPackage,
        parent_run: &LocalRun,
        workflow_input: &Value,
        executor: &dyn WorkflowStepExecutor,
        step: &WorkflowStep,
        delegate: &super::WorkflowDelegate,
        is_canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkflowStepOutcome, Box<dyn Error>> {
        // 执行器若能跨线程共享就真并发跑，否则退回顺序执行。
        // 「声明式地可以并发」而不是「一律并发」：测试里的执行器带 RefCell，并发会编译不过。
        match executor.as_shared() {
            Some(shared) => self.execute_delegate_branches_parallel(
                shared,
                parent_run,
                package,
                step,
                delegate,
                workflow_input,
                is_canceled,
            ),
            None => self.execute_delegate_branches_sequential(
                executor,
                parent_run,
                package,
                step,
                delegate,
                workflow_input,
                is_canceled,
            ),
        }
    }

    /// 顺序执行各分支：既有执行器（含测试）走这条路径。
    #[allow(clippy::too_many_arguments)]
    fn execute_delegate_branches_sequential(
        &self,
        executor: &dyn WorkflowStepExecutor,
        parent_run: &LocalRun,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        delegate: &super::WorkflowDelegate,
        workflow_input: &Value,
        _is_canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkflowStepOutcome, Box<dyn Error>> {
        let mut results = Vec::new();
        let mut artifacts = Vec::new();
        let mut failures = Vec::new();
        let mut succeeded = 0u32;

        for branch in &delegate.branches {
            let (child_package, child_run) =
                self.prepare_delegate_branch(package, parent_run, step, branch, workflow_input)?;
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
                    None,
                )?
            };
            let collected = self.collect_delegate_branch(branch, outcome.run)?;
            for artifact in &collected.artifacts {
                merge_artifact(&mut artifacts, artifact.clone());
            }
            if collected.status == LocalRunStatus::Succeeded {
                succeeded = succeeded.saturating_add(1);
            } else {
                failures.push(collected.failure.clone());
            }
            results.push(collected.json);
        }
        self.finish_delegate_step(parent_run, step, delegate, results, artifacts, succeeded, failures)
    }

    /// 真并发执行各分支。
    ///
    /// 每条分支在独立线程里跑自己的子运行；线程之间不共享可变状态——
    /// 子运行的状态都写在台账（SQLite）里，靠它同步，而不是靠内存里的锁。
    #[allow(clippy::too_many_arguments)]
    fn execute_delegate_branches_parallel(
        &self,
        executor: &dyn SharedStepExecutor,
        parent_run: &LocalRun,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        delegate: &super::WorkflowDelegate,
        workflow_input: &Value,
        is_canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkflowStepOutcome, Box<dyn Error>> {
        // 先把子运行都起好，再一起跑：这样界面上能同时看到所有分支处于「进行中」，
        // 而不是一条一条冒出来。
        let mut prepared = Vec::new();
        for branch in &delegate.branches {
            prepared.push((
                branch,
                self.prepare_delegate_branch(package, parent_run, step, branch, workflow_input)?,
            ));
        }

        let collected = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (branch, (child_package, child_run)) in prepared {
                let runner = self.clone_for_thread();
                let shared = executor;
                let input = workflow_input.clone();
                handles.push((
                    branch,
                    scope.spawn(move || {
                        // 线程之间只传 String：Box<dyn Error> 不是 Send，
                        // 跨线程搬运会编译失败，也没必要。
                        if is_canceled() {
                            return Err(format!(
                                "delegate branch {} cancelled before start",
                                branch.id
                            ));
                        }
                        let outcome = if child_run.status.is_terminal() {
                            WorkflowRunOutcome {
                                run: child_run,
                                blocked_step_id: String::new(),
                                completed_steps: Vec::new(),
                            }
                        } else {
                            runner
                                .run_ready_internal(
                                    &child_package,
                                    child_run,
                                    &input,
                                    shared,
                                    false,
                                    None,
                                )
                                .map_err(|error| error.to_string())?
                        };
                        runner
                            .collect_delegate_branch(branch, outcome.run)
                            .map_err(|error| error.to_string())
                    }),
                ));
            }
            let mut collected = Vec::new();
            for (branch, handle) in handles {
                match handle.join() {
                    Ok(Ok(item)) => collected.push(item),
                    Ok(Err(error)) => collected.push(DelegateBranchResult::failed(
                        branch,
                        format!("{}: {error}", branch.id),
                    )),
                    Err(_) => collected.push(DelegateBranchResult::failed(
                        branch,
                        format!("{}: branch thread panicked", branch.id),
                    )),
                }
            }
            Ok::<_, Box<dyn Error>>(collected)
        })?;

        let mut results = Vec::new();
        let mut artifacts = Vec::new();
        let mut failures = Vec::new();
        let mut succeeded = 0u32;
        for item in collected {
            if item.status == LocalRunStatus::Succeeded {
                succeeded = succeeded.saturating_add(1);
            } else {
                failures.push(item.failure.clone());
            }
            for artifact in &item.artifacts {
                merge_artifact(&mut artifacts, artifact.clone());
            }
            results.push(item.json);
        }
        self.finish_delegate_step(parent_run, step, delegate, results, artifacts, succeeded, failures)
    }

    /// 线程里要用 runner，但 `&self` 不能跨线程；台账本身是「路径 + 超时」的句柄，克隆即可。
    fn clone_for_thread(&self) -> Self {
        Self {
            ledger: self.ledger.clone(),
        }
    }

    /// 起一条委派子运行（幂等：同一分支重跑时复用已有 run）。
    fn prepare_delegate_branch(
        &self,
        package: &WorkflowPackage,
        parent_run: &LocalRun,
        step: &WorkflowStep,
        branch: &super::WorkflowDelegateBranch,
        workflow_input: &Value,
    ) -> Result<(WorkflowPackage, LocalRun), Box<dyn Error>> {
        // 父运行 id 已带 `workflow_run_` 前缀，直接拼会得到
        // `workflow_run_workflow_run_…`。剥掉一层再拼，子运行 id 才可读且可定位。
        let parent_suffix = parent_run
            .run_id
            .strip_prefix("workflow_run_")
            .unwrap_or(&parent_run.run_id);
        let request_id = run_suffix(&format!(
            "{parent_suffix}_delegate_{}_{}",
            step.id, branch.id
        ));
        let child_run_id = format!("workflow_run_{request_id}");
        // 分支的 runtime 必须自带：子运行要能独立执行，不继承父步骤配置。
        let runtime = branch
            .runtime
            .as_ref()
            .ok_or_else(|| format!("delegate branch {} is missing runtime", branch.id))?;
        let branch_step = WorkflowStep {
            id: branch.id.clone(),
            title: branch.title.clone(),
            kind: "runtime".to_string(),
            capability_id: String::new(),
            runtime: Some(runtime.clone()),
            delegate: None,
            loop_config: None,
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: branch.input.clone(),
            execution_mode: "long_running".to_string(),
            risk_level: "local_write".to_string(),
            role_id: branch.role_id.clone(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        };
        let child_package = WorkflowPackage {
            steps: vec![branch_step],
            candidate: None,
            execution_policy: "strict".to_string(),
            entrypoints: Vec::new(),
            default_entrypoint: String::new(),
            default_exitpoint: String::new(),
            exits: Vec::new(),
            ..package.clone()
        };
        let child_run = match self.ledger.get_run(&child_run_id)? {
            Some(existing) => existing,
            None => self.start_delegated(
                "local-agent",
                &child_package,
                &request_id,
                workflow_input,
                &parent_run.run_id,
            )?,
        };
        Ok((child_package, child_run))
    }

    /// 收一条分支的结果：成功时取它的步骤输出，失败时带上错误。
    fn collect_delegate_branch(
        &self,
        branch: &super::WorkflowDelegateBranch,
        run: LocalRun,
    ) -> Result<DelegateBranchResult, Box<dyn Error>> {
        let status = run.status.clone();
        let output = if status == LocalRunStatus::Succeeded {
            delegate_branch_output(&run, &self.ledger)?
        } else {
            Value::Null
        };
        let failure = if status == LocalRunStatus::Succeeded {
            String::new()
        } else if run.error.trim().is_empty() {
            format!("{}: {status:?}", branch.id)
        } else {
            format!("{}: {}", branch.id, run.error)
        };
        Ok(DelegateBranchResult {
            status: status.clone(),
            failure,
            json: serde_json::json!({
                "branch_id": branch.id,
                "role_id": branch.role_id,
                "run_id": run.run_id,
                "status": status,
                "output": output,
            }),
            artifacts: run.artifacts,
        })
    }

    /// 校验分支结果并把整步收成一次执行产出。
    #[allow(clippy::too_many_arguments)]
    fn finish_delegate_step(
        &self,
        parent_run: &LocalRun,
        step: &WorkflowStep,
        delegate: &super::WorkflowDelegate,
        results: Vec<Value>,
        artifacts: Vec<LocalRunArtifact>,
        succeeded: u32,
        failures: Vec<String>,
    ) -> Result<WorkflowStepOutcome, Box<dyn Error>> {
        let required = if delegate.min_succeeded == 0 {
            delegate.branches.len() as u32
        } else {
            delegate.min_succeeded
        };
        self.append_event(
            parent_run,
            step,
            RuntimeEventType::Progress,
            serde_json::json!({
                "delegate_id": step.id,
                "branches": delegate.branches.len(),
                "succeeded": succeeded,
                "required": required,
            }),
        )?;
        if succeeded < required {
            return Err(format!(
                "workflow delegate {} needs {} successful branch(es) but got {}: {}",
                step.id,
                required,
                succeeded,
                failures.join("; ")
            )
            .into());
        }
        Ok(WorkflowStepOutcome::Execution(WorkflowStepExecution {
            output: serde_json::json!({
                "delegate_id": step.id,
                "branches": results,
                "succeeded": succeeded,
            }),
            artifacts: artifacts
                .into_iter()
                .map(local_artifact_to_output)
                .collect(),
            usage: None,
        }))
    }

    /// 用户主动暂停。
    ///
    /// 语义边界：只在**步骤边界**生效，不打断正在执行的步骤——打断需要 kill
    /// Runtime 进程，成本高且中途状态不可控。所以暂停只是把运行标成
    /// `Waiting + pause_kind=User`，下一个步骤开始前由 `run_ready` 提前返回。
    pub(crate) fn pause(&self, mut run: LocalRun) -> Result<LocalRun, Box<dyn Error>> {
        if run.status.is_terminal() {
            return Err(format!("workflow run already finished: {}", run.run_id).into());
        }
        if run.status == LocalRunStatus::Waiting && !run.pause_kind.is_user_pause() {
            return Err(format!(
                "workflow run is waiting for {:?}, not paused by user: {}",
                run.pause_kind, run.run_id
            )
            .into());
        }
        run.status = LocalRunStatus::Waiting;
        run.pause_kind = RunPauseKind::User;
        run.updated_at = timestamp();
        self.persist_run(&mut run)?;
        self.append_run_event(
            &run,
            RuntimeEventType::Progress,
            serde_json::json!({ "paused_by": "user" }),
        )?;
        Ok(run)
    }

    /// 恢复一个被用户暂停的运行。
    ///
    /// 只接受 `pause_kind = User`：等审批、等反馈、等外部信号的运行各有自己的
    /// 恢复入口，用 resume 绕过它们等于跳过了本该发生的确认。
    pub(crate) fn resume_from_pause(&self, mut run: LocalRun) -> Result<LocalRun, Box<dyn Error>> {
        if run.status != LocalRunStatus::Waiting || !run.pause_kind.is_user_pause() {
            return Err(format!(
                "workflow run is not paused by user: {}",
                run.run_id
            )
            .into());
        }
        run.status = LocalRunStatus::Queued;
        run.pause_kind = RunPauseKind::None;
        run.updated_at = timestamp();
        self.persist_run(&mut run)?;
        self.append_run_event(
            &run,
            RuntimeEventType::Progress,
            serde_json::json!({ "resumed_by": "user" }),
        )?;
        Ok(run)
    }

    /// 不带步骤的运行级事件（暂停/恢复这类不归属任何步骤的动作）。
    fn append_run_event(
        &self,
        run: &LocalRun,
        event_type: RuntimeEventType,
        payload: Value,
    ) -> Result<(), Box<dyn Error>> {
        let sequence = self.ledger.next_runtime_sequence(&run.run_id)?;
        self.ledger.append_event(&RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("{}:run:{sequence}", run.run_id),
            run_id: run.run_id.clone(),
            step_id: String::new(),
            capability_id: String::new(),
            sequence,
            occurred_at: timestamp(),
            provider: run.runtime_provider.clone(),
            event_type,
            payload,
        })?;
        Ok(())
    }

    /// 重置一个步骤，让它和它的下游重新执行。
    ///
    /// 只重置目标步骤 + 沿 `depends_on` 反向传播到的下游：
    /// - 已经成功、且与目标无依赖关系的分支不动（重跑它会浪费已完成的工作）；
    /// - 目标步骤的审批记录一并清除（改了输入就该重新确认）；
    /// - 已终结的运行不能被重试，必须先重新入队（否则台账里会出现
    ///   「成功过又被改成排队」这种自相矛盾的记录）。
    pub(crate) fn retry_step(
        &self,
        mut run: LocalRun,
        step_id: &str,
    ) -> Result<LocalRun, Box<dyn Error>> {
        let step_id = step_id.trim();
        if step_id.is_empty() {
            return Err("retry_step requires a step id".into());
        }
        if !run.steps.iter().any(|step| step.step_id == step_id) {
            return Err(format!("workflow run has no step: {step_id}").into());
        }
        // 运行还在跑的时候重置步骤会和执行线程打架：让用户先暂停或取消。
        if run.status == LocalRunStatus::Running {
            return Err(format!(
                "cannot retry a step while the run is executing: {}",
                run.run_id
            )
            .into());
        }

        // 用包的依赖图算「需要重置的集合」。
        //
        // 包读不到时（已被卸载/版本已换）保守退化：重置目标步骤**及其之后的所有步骤**。
        // 不能只重置目标——那会留下「输入已变、下游却还是旧状态」的不一致组合，
        // 下游会拿着过期的上游输出继续跑。多重置几步是可接受的代价，不一致不是。
        let (targets, degraded) = match self.load_package_for_run(&run) {
            Ok(package) => (downstream_chain(&package.steps, step_id), false),
            Err(_) => {
                let mut from_target = std::collections::BTreeSet::new();
                let mut seen_target = false;
                for step in &run.steps {
                    if step.step_id == step_id {
                        seen_target = true;
                    }
                    if seen_target {
                        from_target.insert(step.step_id.clone());
                    }
                }
                (from_target, true)
            }
        };

        let mut reset = 0u32;
        for step in run.steps.iter_mut() {
            if !targets.contains(&step.step_id) {
                continue;
            }
            step.status = LocalStepStatus::Pending;
            step.started_at = String::new();
            step.finished_at = String::new();
            step.error = String::new();
            step.attempt = 0;
            reset += 1;
        }
        // 目标步骤与下游的审批记录作废：输入变了，之前的批准不再适用。
        run.approvals
            .retain(|approval| !targets.iter().any(|id| approval.approval_id.ends_with(id)));
        run.error.clear();
        run.pause_kind = RunPauseKind::None;
        run.status = LocalRunStatus::Queued;
        run.current_step_id.clear();
        run.updated_at = timestamp();
        self.persist_run(&mut run)?;
        self.append_run_event(
            &run,
            RuntimeEventType::Progress,
            serde_json::json!({
                "retry_from": step_id,
                "reset_steps": targets.iter().cloned().collect::<Vec<_>>(),
                "reset_count": reset,
                // 包已不可读时用的是保守策略：重置目标之后的所有步骤。
                "degraded": degraded,
            }),
        )?;
        Ok(run)
    }

    /// 读回运行对应的包；包缺失或已卸载时返回错误，由调用方退化处理。
    fn load_package_for_run(&self, run: &LocalRun) -> Result<WorkflowPackage, Box<dyn Error>> {
        let interaction = self
            .ledger
            .get_interaction(&run.interaction_id)?
            .ok_or("workflow run interaction is missing")?;
        crate::workflow::WorkflowStore::open_default()?.load_for_run_interaction(&interaction)
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

/// 步骤输入的脱敏快照。
///
/// 目的不是完整留档，而是回答「这一步看到了什么」。凭据句柄、业务数据这类值
/// 不能落盘，所以只记键名、类型和规模——足以判断「它有没有拿到上游产物」，
/// 又不会把敏感内容写进台账。
fn summarize_step_input(input: &Value) -> Value {
    fn shape(value: &Value, depth: usize) -> Value {
        // 三层足够回答「这一步拿到了什么结构」；再深就与具体业务强耦合了。
        if depth > 3 {
            return Value::String("<nested>".to_string());
        }
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), shape(value, depth + 1)))
                    .collect(),
            ),
            Value::Array(items) => serde_json::json!({ "array_len": items.len() }),
            Value::String(text) => serde_json::json!({ "string_len": text.chars().count() }),
            Value::Number(number) => serde_json::json!({ "number": number.is_i64() }),
            Value::Bool(_) => serde_json::json!("boolean"),
            Value::Null => Value::Null,
        }
    }
    let mut summarized = shape(input, 0);
    // workflow_context 里的 steps 是上游输出，体积最大且已在别处留痕，只报键名。
    if let Some(context) = summarized
        .get_mut("workflow_context")
        .and_then(Value::as_object_mut)
    {
        if let Some(steps) = context.get_mut("steps").and_then(Value::as_object_mut) {
            for (_, value) in steps.iter_mut() {
                *value = Value::String("<upstream_output>".to_string());
            }
        }
    }
    summarized
}

/// 步骤实际用的 Runtime：执行器可能回填（例如 auto 解析成 codex），优先取回填值。
fn run_step_provider(run_id: &str, step: &WorkflowStep, ledger: &LocalRunLedger) -> String {
    ledger
        .get_run(run_id)
        .ok()
        .flatten()
        .and_then(|run| {
            run.steps
                .iter()
                .find(|item| item.step_id == step.id)
                .map(|item| item.runtime_provider.clone())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            step.runtime
                .as_ref()
                .map(|runtime| runtime.provider.clone())
                .unwrap_or_default()
        })
}

/// 从台账里读出某条运行的结构化证据（按步骤归拢）。
///
/// 证据写在 `tool_completed` 事件的 payload 里；这个函数把它抽成
/// 「step_id → evidence」的映射，供运行详情与审查台账直接消费。
pub(crate) fn collect_step_evidence(
    run_id: &str,
    ledger: &LocalRunLedger,
) -> Result<serde_json::Map<String, Value>, Box<dyn Error>> {
    let mut collected = serde_json::Map::new();
    for event in ledger.list_events(run_id)? {
        if event.event_type != RuntimeEventType::ToolCompleted {
            continue;
        }
        if let Some(evidence) = event.payload.get("evidence") {
            collected.insert(event.step_id, evidence.clone());
        }
    }
    Ok(collected)
}


/// 目标步骤 + 所有依赖它的下游（传递闭包）。
///
/// 依赖方向是「下游 `depends_on` 上游」，所以要反着找：谁依赖了 target。
fn downstream_chain(
    steps: &[WorkflowStep],
    target: &str,
) -> std::collections::BTreeSet<String> {
    fn collect(steps: &[WorkflowStep], out: &mut std::collections::BTreeMap<String, Vec<String>>) {
        for step in steps {
            out.entry(step.id.clone())
                .or_default()
                .extend(step.depends_on.iter().cloned());
            if let Some(loop_config) = step.loop_config.as_ref() {
                collect(&loop_config.steps, out);
            }
        }
    }
    let mut deps = std::collections::BTreeMap::new();
    collect(steps, &mut deps);

    let mut result = std::collections::BTreeSet::new();
    result.insert(target.to_string());
    // 反复扫到不再增长：图很小（步骤数 < 100），传递闭包直接迭代即可。
    loop {
        let before = result.len();
        for (step_id, dependencies) in &deps {
            if result.contains(step_id) {
                continue;
            }
            if dependencies.iter().any(|dependency| result.contains(dependency)) {
                result.insert(step_id.clone());
            }
        }
        if result.len() == before {
            break;
        }
    }
    result
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

/// 汇总一条被委派子运行的产出。
///
/// 与 loop 的 `loop_iteration_output` 同一口径：只取 ToolCompleted 事件里的输出，
/// 不把子运行的完整事件流塞进父步骤输出。
fn delegate_branch_output(run: &LocalRun, ledger: &LocalRunLedger) -> Result<Value, Box<dyn Error>> {
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
            input.insert(
                name.clone(),
                super::executor::resolve_workflow_input_templates(value, workflow_input),
            );
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
            icon: String::new(),
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
            roles: Default::default(),
            capability_alternatives: Default::default(),
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
                    delegate: None,
                    when: None,
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    role_id: String::new(),
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
                    delegate: None,
                    when: None,
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    role_id: String::new(),
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
                    delegate: None,
                    when: None,
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    role_id: String::new(),
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
    fn run_steps_carry_the_declared_role() {
        let runner = WorkflowRunner::with_ledger(ledger("role-projection"));
        let mut package = package();
        package.steps[0].role_id = "software.implementer".to_string();
        package.steps[1].role_id = "software.verifier".to_string();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "request-role", &serde_json::json!({}))
            .unwrap();

        let role_of = |step_id: &str| {
            run.steps
                .iter()
                .find(|step| step.step_id == step_id)
                .map(|step| step.role_id.as_str())
                .unwrap_or("")
        };
        // 角色从工作流声明投影到运行台账；没有声明角色的步骤保持空串，
        // 界面据此决定不渲染徽标，而不是显示一个假角色。
        assert_eq!(role_of("STEP-1"), "software.implementer");
        assert_eq!(role_of("STEP-2"), "software.verifier");
        assert_eq!(role_of("APPROVE"), "");

        let finished = runner
            .run_ready(&package, run, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(finished.blocked_step_id, "APPROVE");
    }

    #[test]
    fn delegated_child_run_is_not_counted_as_rework() {
        let runner = WorkflowRunner::with_ledger(ledger("delegate-kind"));
        let package = package();
        let parent = runner
            .start("agent-1", &package, "request-parent", &serde_json::json!({}))
            .unwrap();
        let delegated = runner
            .start_delegated(
                "agent-1",
                &package,
                "request-child",
                &serde_json::json!({}),
                &parent.run_id,
            )
            .unwrap();
        let retried = runner
            .start_with_parent(
                "agent-1",
                &package,
                "request-retry",
                &serde_json::json!({}),
                &parent.run_id,
            )
            .unwrap();

        assert_eq!(parent.parent_kind, LocalRunParentKind::Unspecified);
        assert!(delegated.parent_kind.is_delegation());
        // 沿用旧入口建子运行的时间语义是「重做」，不能被新加的委派语义悄悄改掉。
        assert_eq!(retried.parent_kind, LocalRunParentKind::Retry);
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
                delegate: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: serde_json::json!({}),
                execution_mode: "sync".to_string(),
                role_id: String::new(),
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
                delegate: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: serde_json::json!({"fixed": true}),
                execution_mode: "sync".to_string(),
                role_id: String::new(),
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
            delegate: None,
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
            role_id: String::new(),
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

    /// 输入快照只留形状，不留值。
    #[test]
    fn step_input_snapshot_keeps_shape_without_values() {
        let input = serde_json::json!({
            "workspace_root": "F:/secret-project",
            "api_key_handle": "cred-123",
            "viewports": ["1024", "768"],
            "workflow_context": {
                "steps": { "IMPLEMENT": { "diff": "very long output" } },
                "input": { "note": "x" }
            }
        });
        let snapshot = summarize_step_input(&input);

        // 字符串只报长度，不落内容。
        assert_eq!(snapshot["workspace_root"]["string_len"], 17);
        assert_eq!(snapshot["api_key_handle"]["string_len"], 8);
        // 数组只报元素个数。
        assert_eq!(snapshot["viewports"]["array_len"], 2);
        // 上游输出被替换成占位符：体积最大且已在别处留痕。
        assert_eq!(
            snapshot["workflow_context"]["steps"]["IMPLEMENT"],
            serde_json::json!("<upstream_output>")
        );
        // 其余结构保留键名，便于回答「这一步拿到了什么」。
        assert_eq!(snapshot["workflow_context"]["input"]["note"]["string_len"], 1);

        let text = snapshot.to_string();
        assert!(!text.contains("secret-project"), "不得泄漏路径值");
        assert!(!text.contains("cred-123"), "不得泄漏凭据句柄值");
        assert!(!text.contains("very long output"), "不得把上游输出再存一遍");
    }

    /// 证据落盘后能按步骤读回来。
    #[test]
    fn step_evidence_is_recorded_and_collectable() {
        let ledger = ledger("evidence-collect");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        workflow.candidate = None;
        workflow.artifacts.clear();
        workflow.steps[0].role_id = "software.analyst".to_string();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &workflow, "evidence-run", &serde_json::json!({ "workspace_root": "F:/x" }))
            .unwrap();
        let outcome = runner
            .run_ready(&workflow, run, &serde_json::json!({ "workspace_root": "F:/x" }), &executor)
            .unwrap();
        assert_eq!(outcome.blocked_step_id, "APPROVE");

        let evidence = collect_step_evidence(&outcome.run.run_id, &ledger).unwrap();
        let first = evidence.get("STEP-1").expect("已完成的步骤必须有证据");
        assert_eq!(first["role_id"], "software.analyst");
        assert_eq!(first["capability_id"], "test.first");
        assert!(first["inputs"].is_object(), "证据必须带输入形状");
        // 未完成的步骤不应伪造证据。
        assert!(!evidence.contains_key("APPROVE"), "等待中的步骤没有完成事件");
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
            delegate: None,
            loop_config: Some(Box::new(super::super::WorkflowLoop {
                max_iterations: 4,
                max_iterations_limit: 0,
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
                    delegate: None,
                    when: None,
                    fail_when: None,
                    candidate_action: String::new(),
                    input: serde_json::json!({}),
                    execution_mode: "sync".to_string(),
                    role_id: String::new(),
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
            role_id: String::new(),
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
    fn delegate_step(package: &WorkflowPackage, branch_count: usize, min_succeeded: u32) -> WorkflowStep {
        let branches = (0..branch_count)
            .map(|i| super::super::WorkflowDelegateBranch {
                id: format!("BRANCH-{i}"),
                title: format!("Branch {i}"),
                role_id: format!("software.role{i}"),
                runtime: Some(serde_json::from_value(serde_json::json!({
                    "provider": "himind.fixture",
                    "prompt": "do the branch work"
                })).unwrap()),
                input: serde_json::json!({}),
            })
            .collect();
        WorkflowStep {
            id: "FANOUT".to_string(),
            title: "Fan out".to_string(),
            kind: "delegate".to_string(),
            capability_id: String::new(),
            runtime: None,
            delegate: Some(Box::new(super::super::WorkflowDelegate {
                branches,
                min_succeeded,
            })),
            loop_config: None,
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "long_running".to_string(),
            risk_level: "local_write".to_string(),
            role_id: String::new(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        }
    }

    #[test]
    fn delegate_step_creates_one_child_run_per_branch() {
        let ledger = ledger("delegate-fanout");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        workflow.candidate = None;
        workflow.artifacts.clear();
        workflow.supported_runtimes = vec!["himind.fixture".to_string()];
        workflow.steps = vec![delegate_step(&workflow, 2, 2)];

        struct OkExecutor;
        impl WorkflowStepExecutor for OkExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                Ok(WorkflowStepExecution::output(serde_json::json!({"ok": true})))
            }
        }

        let run = runner
            .start("agent-1", &workflow, "delegate-run", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(&workflow, run, &serde_json::json!({}), &OkExecutor)
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);

        // 两个分支各起一条独立子运行，都挂在父运行下且标为委派。
        let all = ledger.list_runs(50).unwrap();
        let children: Vec<_> = all
            .iter()
            .filter(|r| r.parent_run_id == outcome.run.run_id)
            .collect();
        assert_eq!(children.len(), 2, "each branch must produce one child run");
        for child in &children {
            assert_eq!(child.parent_kind, LocalRunParentKind::Delegate);
            assert_eq!(child.status, LocalRunStatus::Succeeded);
        }
        // 子运行的角色来自分支自己的 role_id，不继承父步骤。
        let roles: Vec<&str> = children
            .iter()
            .flat_map(|r| r.steps.iter().map(|s| s.role_id.as_str()))
            .collect();
        assert!(roles.contains(&"software.role0"));
        assert!(roles.contains(&"software.role1"));
    }

    /// 并发验证：两个分支的执行区间必须重叠，而不是一前一后。
    /// 用两个挡板让分支互相等待——顺序执行会死锁，并发执行能通过。
    #[test]
    fn delegate_branches_run_concurrently() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        struct BarrierExecutor {
            barrier: Barrier,
            started: AtomicUsize,
        }
        // 只有标记为 shared 的执行器才会走并发路径。
        impl super::super::SharedStepExecutor for BarrierExecutor {}
        impl WorkflowStepExecutor for BarrierExecutor {
            fn as_shared(&self) -> Option<&dyn super::super::SharedStepExecutor> {
                Some(self)
            }
            fn execute(
                &self,
                _package: &WorkflowPackage,
                _step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                self.started.fetch_add(1, Ordering::SeqCst);
                // 两个分支都到达这里才放行：串行执行会在此永久阻塞（测试超时即失败）。
                self.barrier.wait();
                Ok(WorkflowStepExecution::output(serde_json::json!({"ok": true})))
            }
        }

        let ledger = ledger("delegate-concurrent");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        workflow.candidate = None;
        workflow.artifacts.clear();
        workflow.supported_runtimes = vec!["himind.fixture".to_string()];
        workflow.steps = vec![delegate_step(&workflow, 2, 2)];

        let executor = BarrierExecutor {
            barrier: Barrier::new(2),
            started: AtomicUsize::new(0),
        };
        let run = runner
            .start("agent-1", &workflow, "delegate-concurrent", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(&workflow, run, &serde_json::json!({}), &executor)
            .unwrap();

        assert_eq!(outcome.run.status, LocalRunStatus::Succeeded);
        // 两个分支都真正进入过执行器 —— 说明它们是并发运行的。
        assert_eq!(executor.started.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_resets_target_and_downstream_only() {
        let mut pkg: WorkflowPackage = package();
        // STEP-1 → STEP-2 → APPROVE 是一条链；再造一条与它们无关的旁支。
        pkg.steps.push(WorkflowStep {
            id: "SIDE".to_string(),
            title: "Side".to_string(),
            kind: "capability".to_string(),
            capability_id: "test.first".to_string(),
            runtime: None,
            delegate: None,
            loop_config: None,
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "sync".to_string(),
            role_id: String::new(),
            risk_level: "read_only".to_string(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        });

        let chain = downstream_chain(&pkg.steps, "STEP-2");
        // STEP-2 与它的下游 APPROVE 要被重置。
        assert!(chain.contains("STEP-2"));
        assert!(chain.contains("APPROVE"));
        // 上游 STEP-1 已经成功，不该重跑。
        assert!(!chain.contains("STEP-1"), "重试不得回滚上游");
        // 无关旁支不动。
        assert!(!chain.contains("SIDE"), "无关分支不得被牵连");
    }

    #[test]
    fn retry_step_clears_approvals_and_requeues() {
        let ledger = ledger("retry-step");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let package = package();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "retry-run", &serde_json::json!({}))
            .unwrap();
        let blocked = runner
            .run_ready(&package, run, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(blocked.blocked_step_id, "APPROVE");

        let retried = runner.retry_step(blocked.run.clone(), "STEP-2").unwrap();
        assert_eq!(retried.status, LocalRunStatus::Queued);
        assert_eq!(retried.pause_kind, RunPauseKind::None);
        let step2 = retried.steps.iter().find(|s| s.step_id == "STEP-2").unwrap();
        assert_eq!(step2.status, LocalStepStatus::Pending);
        assert_eq!(step2.attempt, 0, "重试要把尝试次数归零");
        assert!(step2.started_at.is_empty());
        // 下游 APPROVE 也重置，且它的审批记录作废。
        let approve = retried.steps.iter().find(|s| s.step_id == "APPROVE").unwrap();
        assert_eq!(approve.status, LocalStepStatus::Pending);
        assert!(
            retried.approvals.iter().all(|a| !a.approval_id.ends_with("APPROVE")),
            "重试下游必须作废对应的审批"
        );
        // 上游 STEP-1 保持成功。
        let step1 = retried.steps.iter().find(|s| s.step_id == "STEP-1").unwrap();
        assert_eq!(step1.status, LocalStepStatus::Succeeded);
    }

    #[test]
    fn retry_rejects_running_and_unknown_steps() {
        let ledger = ledger("retry-guard");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let package = package();
        let mut run = runner
            .start("agent-1", &package, "retry-guard", &serde_json::json!({}))
            .unwrap();
        run.status = LocalRunStatus::Running;
        assert!(runner.retry_step(run.clone(), "STEP-1").is_err(), "运行中不得重试单步");
        run.status = LocalRunStatus::Queued;
        assert!(runner.retry_step(run, "NOPE").is_err(), "未知步骤必须报错");
    }

    #[test]
    fn paused_run_stops_advancing_until_resumed() {
        let ledger = ledger("pause-resume");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let package = package();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "pause-run", &serde_json::json!({}))
            .unwrap();

        // 暂停后推进：必须原地不动，且不执行任何步骤。
        let paused = runner.pause(run).unwrap();
        assert_eq!(paused.status, LocalRunStatus::Waiting);
        assert_eq!(paused.pause_kind, RunPauseKind::User);
        let blocked = runner
            .run_ready(&package, paused.clone(), &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(blocked.run.status, LocalRunStatus::Waiting);
        assert!(executor.steps.borrow().is_empty(), "暂停期间不得执行任何步骤");

        // 恢复后从下一个步骤继续，行为与从未暂停一致。
        let resumed = runner.resume_from_pause(blocked.run).unwrap();
        assert_eq!(resumed.status, LocalRunStatus::Queued);
        assert_eq!(resumed.pause_kind, RunPauseKind::None);
        let outcome = runner
            .run_ready(&package, resumed, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(outcome.blocked_step_id, "APPROVE");
        assert_eq!(executor.steps.borrow().as_slice(), &["STEP-1", "STEP-2"]);
    }

    #[test]
    fn resume_does_not_bypass_approval_or_feedback_waits() {
        let ledger = ledger("pause-guard");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let package = package();
        let executor = RecordingExecutor::default();
        let run = runner
            .start("agent-1", &package, "guard-run", &serde_json::json!({}))
            .unwrap();
        let blocked = runner
            .run_ready(&package, run, &serde_json::json!({}), &executor)
            .unwrap();
        assert_eq!(blocked.blocked_step_id, "APPROVE");
        assert_eq!(
            blocked.run.pause_kind,
            RunPauseKind::Approval,
            "等审批的运行必须标注 pause_kind=Approval"
        );
        // 用 resume 绕过审批必须被拒绝：那等于跳过了本该发生的确认。
        let error = runner.resume_from_pause(blocked.run.clone()).unwrap_err();
        assert!(error.to_string().contains("not paused by user"), "实际：{error}");
    }

    #[test]
    fn delegate_step_fails_when_too_few_branches_succeed() {
        let ledger = ledger("delegate-fail");
        let runner = WorkflowRunner::with_ledger(ledger.clone());
        let mut workflow = package();
        workflow.candidate = None;
        workflow.artifacts.clear();
        workflow.supported_runtimes = vec!["himind.fixture".to_string()];
        workflow.steps = vec![delegate_step(&workflow, 2, 2)];

        // 第一个分支成功、第二个失败：要求两个都成功时整步失败。
        struct HalfExecutor {
            calls: std::cell::RefCell<u32>,
        }
        impl WorkflowStepExecutor for HalfExecutor {
            fn execute(
                &self,
                _package: &WorkflowPackage,
                step: &WorkflowStep,
                _input: &Value,
            ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
                let mut calls = self.calls.borrow_mut();
                *calls += 1;
                if step.id == "BRANCH-1" {
                    return Err("branch failed".into());
                }
                Ok(WorkflowStepExecution::output(Value::Null))
            }
        }

        let run = runner
            .start("agent-1", &workflow, "delegate-fail", &serde_json::json!({}))
            .unwrap();
        let outcome = runner
            .run_ready(
                &workflow,
                run,
                &serde_json::json!({}),
                &HalfExecutor { calls: std::cell::RefCell::new(0) },
            )
            .unwrap();
        assert_eq!(outcome.run.status, LocalRunStatus::Failed);
        assert!(outcome.run.error.contains("needs 2 successful branch"));
    }

}
