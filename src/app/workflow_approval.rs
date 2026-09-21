use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::agent_core_contracts::{InteractionSource, LocalRun, LocalRunStatus, RuntimeEventType};
use crate::approval::manager::ApprovalManager;
use crate::capability::service::CapabilityGateway;
use crate::store::local_runs::LocalRunLedger;
use crate::workflow::{
    workflow_approval_id, WorkflowPackage, WorkflowRunner, WorkflowStep, WorkflowStore,
};

const BRIDGE_SCAN_INTERVAL: Duration = Duration::from_secs(1);

struct ApprovalContext {
    run_id: String,
    step_id: String,
    approval_id: String,
    capability_id: String,
    risk_level: String,
    title: String,
    description: String,
}

pub(crate) fn start_workflow_approval_bridge(
    gateway: CapabilityGateway,
    approval_manager: Arc<ApprovalManager>,
) {
    let in_flight = Arc::new(Mutex::new(HashSet::<String>::new()));
    let _ = thread::Builder::new()
        .name("himind-workflow-approval-bridge".to_string())
        .spawn(move || loop {
            scan_waiting_workflows(&gateway, &approval_manager, &in_flight);
            thread::sleep(BRIDGE_SCAN_INTERVAL);
        });
}

fn scan_waiting_workflows(
    gateway: &CapabilityGateway,
    approval_manager: &Arc<ApprovalManager>,
    in_flight: &Arc<Mutex<HashSet<String>>>,
) {
    let Ok(ledger) = LocalRunLedger::open_default() else {
        return;
    };
    let Ok(store) = WorkflowStore::open_default() else {
        return;
    };
    let Ok(runs) = ledger.list_runs(500) else {
        return;
    };

    for run in runs {
        if run.source != InteractionSource::Workflow
            || run.status != LocalRunStatus::Waiting
            || run.current_step_id.trim().is_empty()
        {
            continue;
        }
        let Ok(Some(context)) = approval_context(&ledger, &store, &run) else {
            continue;
        };
        let key = format!("{}:{}", run.run_id, run.current_step_id);
        let should_spawn = in_flight
            .lock()
            .map(|mut active| active.insert(key.clone()))
            .unwrap_or(false);
        if !should_spawn {
            continue;
        }

        let gateway = gateway.clone();
        let approval_manager = Arc::clone(approval_manager);
        let in_flight = Arc::clone(in_flight);
        let _ = thread::Builder::new()
            .name(format!("himind-workflow-approval-{}", run.run_id))
            .spawn(move || {
                coordinate_workflow_approval(gateway, approval_manager, context);
                if let Ok(mut active) = in_flight.lock() {
                    active.remove(&key);
                }
            });
    }
}

fn approval_context(
    ledger: &LocalRunLedger,
    store: &WorkflowStore,
    run: &LocalRun,
) -> Result<Option<ApprovalContext>, Box<dyn std::error::Error>> {
    let Some(interaction) = ledger.get_interaction(&run.interaction_id)? else {
        return Ok(None);
    };
    let package = store.load_for_run_interaction(&interaction)?;
    let Some(step) = package
        .steps
        .iter()
        .find(|step| step.id == run.current_step_id)
    else {
        return Ok(None);
    };
    if !step.approval_required || waiting_for_feedback(ledger, run)? {
        return Ok(None);
    }
    Ok(Some(workflow_approval_context(
        &package,
        step,
        run,
        interaction
            .business_context
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    )))
}

fn waiting_for_feedback(
    ledger: &LocalRunLedger,
    run: &LocalRun,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut pending_question: Option<String> = None;
    for event in ledger.list_events(&run.run_id)? {
        if event.step_id != run.current_step_id {
            continue;
        }
        match event.event_type {
            RuntimeEventType::QuestionRequested => {
                pending_question = Some(event.event_id);
            }
            RuntimeEventType::QuestionResolved => {
                let question_id = event
                    .payload
                    .get("question_event_id")
                    .and_then(Value::as_str);
                if question_id.is_none() || question_id == pending_question.as_deref() {
                    pending_question = None;
                }
            }
            RuntimeEventType::Progress
                if event
                    .payload
                    .get("waiting_for_feedback")
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                pending_question = Some(event.event_id);
            }
            _ => {}
        }
    }
    Ok(pending_question.is_some())
}

fn workflow_approval_context(
    package: &WorkflowPackage,
    step: &WorkflowStep,
    run: &LocalRun,
    input: Value,
) -> ApprovalContext {
    let approval_id = workflow_approval_id(&run.run_id, &step.id);
    let capability_id = if step.capability_id.trim().is_empty() {
        step.id.clone()
    } else {
        step.capability_id.clone()
    };
    let risk_level = if step.risk_level.trim().is_empty() {
        "R3".to_string()
    } else {
        step.risk_level.trim().to_ascii_uppercase()
    };
    let title = truncate_chars(&format!("Workflow: {}", step.title.trim()), 120);
    let project = json_string(&input, &["project_root", "workspace_root"])
        .unwrap_or_else(|| "未记录项目".to_string());
    let app_id = json_string(&input, &["app_id"]).unwrap_or_else(|| "未记录 AppID".to_string());
    let environment =
        json_string(&input, &["environment", "env"]).unwrap_or_else(|| "未记录环境".to_string());
    let commit_sha =
        json_string(&input, &["commit_sha"]).unwrap_or_else(|| "未记录提交".to_string());
    let description = format!(
        "工作流：{} v{}\n步骤：{} ({})\nRun：{}\n项目：{}\nAppID：{}\n环境：{}\n候选提交：{}\n风险：{}",
        package.name,
        package.version,
        step.title,
        step.id,
        run.run_id,
        project,
        app_id,
        environment,
        commit_sha,
        risk_level,
    );

    ApprovalContext {
        run_id: run.run_id.clone(),
        step_id: step.id.clone(),
        approval_id,
        capability_id,
        risk_level,
        title,
        description,
    }
}

fn json_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn coordinate_workflow_approval(
    gateway: CapabilityGateway,
    approval_manager: Arc<ApprovalManager>,
    context: ApprovalContext,
) {
    let run_id = context.run_id.clone();
    let step_id = context.step_id.clone();
    let decision = approval_manager.request_workflow_approval_with_cancel(
        &context.approval_id,
        &context.capability_id,
        &context.risk_level,
        context.title,
        context.description,
        || {
            let ledger = LocalRunLedger::open_default().map_err(|error| error.to_string())?;
            Ok(ledger
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .map(|run| run.status != LocalRunStatus::Waiting || run.current_step_id != step_id)
                .unwrap_or(true))
        },
    );

    match decision {
        Ok(Some(true)) => {
            if let Err(error) =
                approve_and_resume_workflow(gateway, &approval_manager, &run_id, &step_id)
            {
                approval_manager.add_log(
                    "error",
                    &format!("Workflow 审批已批准，但继续执行失败: {error}"),
                );
            }
        }
        Ok(Some(false)) => {
            if let Err(error) = reject_workflow(&run_id, &step_id) {
                approval_manager
                    .add_log("error", &format!("Workflow 审批拒绝结果写入失败: {error}"));
            }
        }
        Ok(None) => {}
        Err(error) => approval_manager.add_log("error", &format!("Workflow 审批协调失败: {error}")),
    }
}

fn approve_and_resume_workflow(
    gateway: CapabilityGateway,
    approval_manager: &ApprovalManager,
    run_id: &str,
    step_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let runner = WorkflowRunner::open_default()?;
    let ledger = LocalRunLedger::open_default()?;
    let Some(run) = ledger.get_run(run_id)? else {
        return Ok(());
    };
    if run.status != LocalRunStatus::Waiting || run.current_step_id != step_id {
        return Ok(());
    }
    let run = runner.approve_step(run, step_id)?;
    crate::app::commands::resume_workflow_with_gateway(gateway, &run.run_id, None)?;
    approval_manager.add_log(
        "info",
        &format!("Workflow 审批已批准并继续: {run_id} / {step_id}"),
    );
    Ok(())
}

fn reject_workflow(run_id: &str, step_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let runner = WorkflowRunner::open_default()?;
    let ledger = LocalRunLedger::open_default()?;
    let Some(run) = ledger.get_run(run_id)? else {
        return Ok(());
    };
    if run.status != LocalRunStatus::Waiting || run.current_step_id != step_id {
        return Ok(());
    }
    runner.reject_step(run, step_id)?;
    Ok(())
}
