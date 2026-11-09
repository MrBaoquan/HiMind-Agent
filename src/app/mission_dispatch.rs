//! Mission task dispatch. A Mission task is executed as a one-step
//! WorkflowPackage through the existing WorkflowRunner; the Mission row only
//! records the business task and its LocalRun attempt id.

use serde_json::{json, Value};
use std::error::Error;
use std::sync::Arc;

use crate::capability::service::CapabilityGateway;
use crate::mission::{MissionStore, MissionTaskSpec};
use crate::workflow::{
    execute_runtime_step, WorkflowPackage, WorkflowRunner, WorkflowStep, WorkflowStepExecution,
    WorkflowStepExecutor,
};
use crate::Options;

struct RuntimeStepExecutor<'a> {
    options: &'a Options,
    canceled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl WorkflowStepExecutor for RuntimeStepExecutor<'_> {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        execute_runtime_step(
            package,
            step,
            input,
            Some(self.options),
            &|| Ok((self.canceled)()),
        )
    }
}

#[tauri::command]
pub(crate) async fn dispatch_next_mission_task(
    mission_id: String,
    options: tauri::State<'_, crate::app::commands::AgentState>,
) -> Result<serde_json::Value, String> {
    let gateway = options.capability_gateway.clone();
    let options = options.options.clone();
    let thread_mission_id = mission_id.clone();
    std::thread::Builder::new()
        .name(format!("mission-dispatch-{mission_id}"))
        .spawn(move || {
            if let Err(error) = dispatch_next(&gateway, &options, &thread_mission_id) {
                let message = error.to_string();
                let _ = MissionStore::open_default().and_then(|store| {
                    store.append_event(&thread_mission_id, "mission_failed", "", &message, json!({"error": message}))
                });
                eprintln!("mission dispatch {thread_mission_id} failed: {error}");
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(json!({"accepted": true, "mission_id": mission_id}))
}

pub(crate) fn dispatch_next(
    _gateway: &CapabilityGateway,
    options: &Options,
    mission_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let store = MissionStore::open_default()?;
    let mission = store.get(mission_id)?;
    if let Some(pending) = store.next_verification_task(mission_id)? {
        return run_verification(&store, options, &mission, &pending);
    }
    let Some(task) = store.next_ready_task(mission_id)? else {
        return Ok(json!({"dispatched": false, "reason": "no ready task"}));
    };
    if task.execution.executor != "himind.builtin" {
        return Err(format!(
            "MVP dispatch currently supports himind.builtin, got {}",
            task.execution.executor
        ).into());
    }
    let task_id = task.id.clone();
    let has_required_checks = !task.required_checks.is_empty();
    let has_acceptance = !task.acceptance_criteria.is_empty();
    let package = task_package(&mission.mission_id, &mission.title, &task, &mission.workspace.root)?;
    let runner = WorkflowRunner::open_default()?;
    let request_id = format!("mission_{}_{}", mission.mission_id, task_id);
    let input = json!({
        "workspace_root": mission.workspace.root,
        "mission_id": mission.mission_id,
        "task_id": task_id,
        "task": task,
        "approved_paths": mission.confirmation.as_ref().map(|value| value.approved_paths.clone()).unwrap_or_default(),
        "candidate_acceptance": mission.acceptance_criteria,
    });
    let run = runner.start("local-agent", &package, &request_id, &input)?;
    store.begin_task_attempt(mission_id, &task_id, &run.run_id)?;
    let executor = RuntimeStepExecutor { options, canceled: Arc::new(|| false) };
    let outcome = runner.run_ready(&package, run, &input, &executor)?;
    let run_succeeded = outcome.run.status == crate::agent_core_contracts::LocalRunStatus::Succeeded;
    let updated = if run_succeeded && has_required_checks && has_acceptance {
        store.await_task_verification(
            mission_id,
            &task_id,
            &outcome.run.run_id,
            "执行器已完成，等待独立验证 Agent",
        )?
    } else {
        let reason = if !run_succeeded {
            outcome.run.error.clone()
        } else if !has_required_checks {
            "任务没有声明必需检查，不能进入验收".to_string()
        } else {
            "任务没有声明验收标准，不能进入验收".to_string()
        };
        store.complete_task_attempt(mission_id, &task_id, &outcome.run.run_id, false, &[], &reason)?
    };
    Ok(json!({
        "dispatched": true,
        "task_id": task_id,
        "run_id": outcome.run.run_id,
        "run_status": outcome.run.status,
        "verification_pending": run_succeeded && has_required_checks && has_acceptance,
        "mission": updated,
    }))
}

fn run_verification(
    store: &MissionStore,
    options: &Options,
    mission: &crate::mission::MissionView,
    pending: &crate::mission::MissionTaskView,
) -> Result<Value, Box<dyn Error>> {
    let task = &pending.spec;
    let evidence_refs = vec![format!("local-run:{}", pending.attempt_run_id)];
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let implementation_run = ledger.get_run(&pending.attempt_run_id)?
        .ok_or_else(|| format!("implementation run not found: {}", pending.attempt_run_id))?;
    let implementation_events = ledger.list_events(&pending.attempt_run_id)?;
    let implementation_evidence = implementation_events.iter()
        .filter(|event| matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted))
        .filter_map(|event| event.payload.get("evidence").cloned())
        .collect::<Vec<_>>();
    let implementation_output = implementation_events.iter().rev()
        .find(|event| matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted))
        .map(|event| event.payload.clone())
        .unwrap_or(Value::Null);
    let package = verification_package(mission, task)?;
    let runner = WorkflowRunner::open_default()?;
    let request_id = format!("mission_{}_{}_verify", mission.mission_id, task.id);
    let input = json!({
        "workspace_root": mission.workspace.root,
        "mission_id": mission.mission_id,
        "task_id": task.id,
        "implementation_run_id": pending.attempt_run_id,
        "acceptance_criteria": task.acceptance_criteria,
        "required_checks": task.required_checks,
        "approved_paths": task.write_paths,
        "evidence_refs": evidence_refs,
        "implementation_status": implementation_run.status,
        "implementation_output": implementation_output,
        "implementation_evidence": implementation_evidence,
    });
    let run = runner.start("local-verifier", &package, &request_id, &input)?;
    if run.run_id == pending.attempt_run_id {
        return Err("verification run must differ from implementation run".into());
    }
    store.begin_verification_attempt(&mission.mission_id, &task.id, &run.run_id)?;
    let executor = RuntimeStepExecutor { options, canceled: Arc::new(|| false) };
    let outcome = runner.run_ready(&package, run, &input, &executor)?;
    let run_succeeded = outcome.run.status == crate::agent_core_contracts::LocalRunStatus::Succeeded;
    let verification_output = verification_events_output(&outcome.run.run_id)?;
    let passed = run_succeeded && verification_output.as_ref().is_some_and(|value| {
        value.get("passed").and_then(Value::as_bool) == Some(true)
            && value.get("checks").and_then(Value::as_array).is_some_and(|checks| !checks.is_empty())
    });
    let mut final_evidence = evidence_refs;
    final_evidence.push(format!("local-run:{}", outcome.run.run_id));
    let reason = if passed { String::new() } else if outcome.run.error.trim().is_empty() {
        "验证 Agent 未返回 passed=true 和非空 checks".to_string()
    } else {
        outcome.run.error.clone()
    };
    let updated = store.record_verification_result(
        &mission.mission_id, &task.id, &pending.attempt_run_id,
        &outcome.run.run_id, passed, &final_evidence, &reason,
    )?;
    Ok(json!({
        "dispatched": true,
        "verification": true,
        "task_id": task.id,
        "run_id": outcome.run.run_id,
        "run_status": outcome.run.status,
        "passed": passed,
        "mission": updated,
    }))
}

fn verification_events_output(run_id: &str) -> Result<Option<Value>, Box<dyn Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    Ok(ledger.list_events(run_id)?.into_iter().rev().find_map(|event| {
        if matches!(event.event_type, crate::agent_core_contracts::RuntimeEventType::ToolCompleted) {
            event.payload.get("output").cloned()
        } else {
            None
        }
    }))
}

fn task_package(
    mission_id: &str,
    mission_title: &str,
    task: &MissionTaskSpec,
    workspace_root: &str,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let prompt = format!(
        "你是专家团队的{}。只执行下面这一项任务，不扩大范围。\n任务：{}\n目标：{}\n允许写入：{}\n验收：{}\n必须检查：{}\n完成后输出 JSON：{{\"passed\":true|false,\"summary\":\"...\",\"evidence\":[\"...\"]}}。自报 passed 只是送验建议，最终由宿主检查。",
        task.execution.role_id,
        task.title,
        task.objective,
        task.write_paths.join(", "),
        task.acceptance_criteria.join("；"),
        task.required_checks.join("；"),
    );
    let value = json!({
        "schema_version": "workflow_package.v1",
        "id": format!("com.himind.mission.{}", mission_id),
        "version": "1.0.0",
        "name": mission_title,
        "description": "Mission task attempt",
        "min_agent_version": "0.3.90",
        "supported_runtimes": ["himind.builtin"],
        "steps": [{
            "id": task.id,
            "title": task.title,
            "kind": "runtime",
            "role_id": task.execution.role_id,
            "risk_level": if task.write_paths.is_empty() { "read_only" } else { "local_write" },
            "execution_mode": "long_running",
            "runtime": {
                "provider": task.execution.executor,
                "prompt": prompt,
                "workspace_path": workspace_root,
                "model": task.execution.model,
                "tool_policy": "default",
                "timeout_seconds": 900
            }
        }],
        "artifacts": [],
        "ui": { "mode": "standard" }
    });
    Ok(serde_json::from_value(value)?)
}

fn verification_package(
    mission: &crate::mission::MissionView,
    task: &MissionTaskSpec,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let prompt = format!(
        "你是独立验证 Agent，不得修改代码，不得相信开发 Agent 自报通过。检查任务 {} 的实际改动、验收标准和必需检查。只读审查，必须运行能运行的检查。输出 JSON：{{\"passed\":true|false,\"verified_task_ids\":[\"{}\"],\"checks\":[{{\"command\":\"...\",\"exit_code\":0,\"detail\":\"...\"}}],\"reasons\":[]}}。没有任何实际检查证据时 passed 必须为 false。",
        task.id, task.id
    );
    let value = json!({
        "schema_version": "workflow_package.v1",
        "id": format!("com.himind.mission.verify.{}", mission.mission_id),
        "version": "1.0.0",
        "name": "Mission verification",
        "description": "Independent verification attempt",
        "min_agent_version": "0.3.90",
        "supported_runtimes": ["himind.builtin"],
        "steps": [{
            "id": "VERIFY",
            "title": "独立验证",
            "kind": "runtime",
            "role_id": "verifier",
            "risk_level": "read_only",
            "execution_mode": "long_running",
            "runtime": {
                "provider": task.execution.executor,
                "prompt": prompt,
                "workspace_path": mission.workspace.root,
                "model": task.execution.model,
                "tool_policy": "default",
                "timeout_seconds": 900
            }
        }],
        "artifacts": [],
        "ui": { "mode": "standard" }
    });
    Ok(serde_json::from_value(value)?)
}
