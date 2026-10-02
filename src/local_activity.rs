//! 统一活动列表：把「这台电脑已经/正在做的事」收敛成一份只读视图。
//!
//! 数据来源刻意保持分布式，这里只做投影，不新建存储：
//! - 本机工作流运行：`LocalRunLedger` 中 `source = Workflow` 的运行（含步骤、产物、错误）；
//! - 技能运行：`skill-runs/<run_id>/run.json`，定时任务触发的会带 `schedule_id`；
//! - 工作台下发任务：仍由工作台任务历史提供，不在这里复制，避免出现两份事实。
//!
//! 状态统一映射到宿主界面既有的任务状态词表，避免每个来源各说一套。

use crate::agent_core_contracts::{
    InteractionSource, LocalRunStatus, LocalRunStep, LocalStepStatus,
};
use crate::store::local_runs::LocalRunLedger;
use crate::workflow::WorkflowStore;
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::error::Error;

const DEFAULT_LIMIT: usize = 60;

/// 台账与技能运行都用 unix 秒字符串存时间，这里统一转成 ISO 供界面解析。
fn iso_from_epoch(value: &str) -> String {
    let seconds = value.trim().parse::<i64>().unwrap_or_default();
    if seconds <= 0 {
        return String::new();
    }
    Utc.timestamp_opt(seconds, 0)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// 本机运行状态压平到界面既有的任务状态词表。
fn run_status_key(status: &LocalRunStatus) -> &'static str {
    match status {
        LocalRunStatus::Queued => "pending",
        LocalRunStatus::Running => "running",
        // 等待审批/反馈既不是失败也没有结束，归到「需处理」，由人来推进。
        LocalRunStatus::Waiting => "waiting",
        LocalRunStatus::Succeeded => "completed",
        LocalRunStatus::Failed => "failed",
        LocalRunStatus::Canceled => "canceled",
    }
}

fn skill_status_key(status: &str) -> &'static str {
    match status.trim() {
        "running" => "running",
        "succeeded" => "completed",
        "failed" => "failed",
        "canceled" | "cancelled" => "canceled",
        _ => "pending",
    }
}

/// 台账只记步骤状态，没有百分比。这里按「已落定步骤 / 总步骤」推算进度，
/// 正在执行或等待处理的步骤算半格——否则进行中的进度条会长期钉在 0%，
/// 用户看到的是「卡住了」而不是「在跑」。总步数为 0 时返回 None，
/// 界面据此显示不确定态，而不是伪造一个 0%。
fn step_completion(steps: &[LocalRunStep]) -> (Option<u32>, usize, usize) {
    let total = steps.len();
    if total == 0 {
        return (None, 0, 0);
    }
    let mut settled = 0usize;
    let mut in_flight = 0usize;
    for step in steps {
        match step.status {
            LocalStepStatus::Succeeded
            | LocalStepStatus::Failed
            | LocalStepStatus::Canceled
            | LocalStepStatus::Skipped => settled += 1,
            LocalStepStatus::Running | LocalStepStatus::Waiting => in_flight += 1,
            LocalStepStatus::Pending => {}
        }
    }
    // 半格用「双倍刻度」表示，避免浮点数和四舍五入带来的跳动。
    let points = settled * 2 + in_flight;
    let percent = (points * 100) / (total * 2);
    (Some(percent.min(100) as u32), settled, total)
}

fn workflow_names() -> HashMap<String, String> {
    let Ok(store) = WorkflowStore::open_default() else {
        return HashMap::new();
    };
    store
        .list()
        .map(|items| {
            items
                .into_iter()
                .map(|item| (item.package.id.clone(), item.package.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// 本机工作流运行，投影自本地运行台账。
fn ledger_items(limit: usize) -> Result<Vec<Value>, Box<dyn Error>> {
    let ledger = LocalRunLedger::open_default()?;
    let names = workflow_names();
    let mut items = Vec::new();
    for run in ledger.list_runs(limit)? {
        if run.source != InteractionSource::Workflow {
            continue;
        }
        let interaction = ledger.get_interaction(&run.interaction_id)?;
        let workflow = interaction
            .as_ref()
            .and_then(|interaction| interaction.business_context.get("workflow"));
        let workflow_id = workflow
            .and_then(|workflow| workflow.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let workflow_version = workflow
            .and_then(|workflow| workflow.get("version"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let current_step = run
            .steps
            .iter()
            .find(|step| step.step_id == run.current_step_id)
            .or_else(|| run.steps.last());
        let step_title = current_step
            .map(|step| step.title.trim().to_string())
            .unwrap_or_default();
        let capability = current_step
            .map(|step| step.capability_id.trim().to_string())
            .unwrap_or_default();
        let workflow_name = names.get(&workflow_id).cloned().unwrap_or_default();
        let title = if !workflow_name.trim().is_empty() {
            workflow_name
        } else if !workflow_id.is_empty() {
            workflow_id
        } else if !step_title.is_empty() {
            step_title.clone()
        } else if !capability.is_empty() {
            capability.clone()
        } else {
            "本机工作流".to_string()
        };
        let subtitle = if workflow_version.is_empty() {
            "工作流".to_string()
        } else {
            format!("工作流 · v{workflow_version}")
        };
        let (progress, step_done, step_total) = step_completion(&run.steps);
        items.push(json!({
            "id": run.run_id,
            "source": "workflow",
            "title": title,
            "subtitle": subtitle,
            "status": run_status_key(&run.status),
            "progress": progress,
            "step_done": step_done,
            "step_total": step_total,
            "detail": if run.error.trim().is_empty() { step_title } else { String::new() },
            "error": run.error,
            "created_at": iso_from_epoch(&run.created_at),
            "started_at": iso_from_epoch(&run.created_at),
            "finished_at": if run.status.is_terminal() { iso_from_epoch(&run.updated_at) } else { String::new() },
            "updated_at": iso_from_epoch(&run.updated_at),
            "artifact_count": run.artifacts.len(),
            "workflow_run_id": run.run_id,
        }));
    }
    Ok(items)
}

/// 技能运行，投影自技能运行目录；定时任务触发的单独标注来源。
fn skill_items(limit: usize) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut items = Vec::new();
    for record in crate::skill_run::recent(limit)? {
        let title = if record.skill_name.trim().is_empty() {
            record.skill_id.clone()
        } else {
            record.skill_name.trim().to_string()
        };
        let subtitle = if record.skill_version.trim().is_empty() {
            "技能".to_string()
        } else {
            format!("技能 · v{}", record.skill_version.trim())
        };
        let finished = if record.status == "running" {
            String::new()
        } else {
            iso_from_epoch(&record.finished_at)
        };
        items.push(json!({
            "id": record.run_id,
            "source": if record.schedule_id.trim().is_empty() { "skill" } else { "schedule" },
            "title": title,
            "subtitle": subtitle,
            "status": skill_status_key(&record.status),
            // 技能运行没有步骤指标，进度返回 null：界面显示「进行中」，
            // 而不是把未知当成 0% 挂在进度条上。
            "progress": Value::Null,
            "detail": record.model.trim().to_string(),
            "error": record.error,
            "created_at": iso_from_epoch(&record.started_at),
            "started_at": iso_from_epoch(&record.started_at),
            "finished_at": finished.clone(),
            "updated_at": if finished.trim().is_empty() { iso_from_epoch(&record.started_at) } else { finished },
            "artifact_count": if record.output_path.trim().is_empty() { 0 } else { 1 },
            "workflow_run_id": "",
        }));
    }
    Ok(items)
}

/// 合并本机各类运行，按更新时间倒序，供统一活动页直接渲染。
pub(crate) fn list(limit: Option<usize>) -> Result<Vec<Value>, Box<dyn Error>> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 200);
    let mut runs = ledger_items(limit)?;
    runs.extend(skill_items(limit)?);
    runs.sort_by(|left, right| {
        let left_time = left
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_time = right
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        right_time.cmp(left_time)
    });
    runs.truncate(limit);
    Ok(runs)
}
