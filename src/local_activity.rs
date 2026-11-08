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

/// 步骤状态也压平到同一套词表，角色汇总要按它显示「这个角色现在是进行中还是已完成」。
fn step_status_key(status: &LocalStepStatus) -> &'static str {
    match status {
        LocalStepStatus::Pending => "pending",
        LocalStepStatus::Running => "running",
        LocalStepStatus::Waiting => "waiting",
        LocalStepStatus::Succeeded => "completed",
        LocalStepStatus::Failed => "failed",
        LocalStepStatus::Canceled => "canceled",
        LocalStepStatus::Skipped => "skipped",
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

/// 同一角色下有多个步骤时，取「最需要注意」的那个状态。
///
/// 排序按用户注意力而不是时间：失败最需要处理，其次是正在跑/等确认，
/// 然后是已完成。取最大值，所以只要还有一步在跑，角色就不会显示成已完成。
fn role_status_rank(status: &str) -> u8 {
    match status {
        "failed" => 5,
        "waiting" => 4,
        "running" => 3,
        "canceled" => 2,
        "pending" => 1,
        "completed" => 0,
        _ => 0,
    }
}

/// 汇总一条运行里出现过的角色。
///
/// 顺序按首次出现（反映步骤推进顺序，排序会打乱协作链路）；同一角色可能挂在
/// 多个步骤上（例如「测试」下面还有修复循环），状态取最需要注意的那个。
fn role_rollup(steps: &[LocalRunStep]) -> Vec<Value> {
    let mut roles: Vec<Value> = Vec::new();
    for step in steps {
        let id = step.role_id.trim();
        if id.is_empty() {
            continue;
        }
        let status = step_status_key(&step.status);
        match roles.iter_mut().find(|role| role["id"] == id) {
            Some(existing) => {
                let current = existing["status"].as_str().unwrap_or_default();
                if role_status_rank(status) > role_status_rank(current) {
                    existing["status"] = Value::String(status.to_string());
                }
            }
            None => roles.push(json!({ "id": id, "status": status })),
        }
    }
    roles
}

/// 把一个父运行下的子运行汇总成协作计数。
///
/// 委派和重做分开算：两者都写 `parent_run_id`，但「多角色分工」和「返工」是
/// 两种完全不同的事实，合成一个数字会让团队视图误导人。没有子运行时返回 Null，
/// 前端据此整块不渲染，而不是显示一排 0。
fn team_rollup(children: Option<&Vec<ChildFacts>>) -> Value {
    let Some(items) = children else {
        return Value::Null;
    };
    if items.is_empty() {
        return Value::Null;
    }
    let delegated = items.iter().filter(|child| child.delegated).count();
    let running = items.iter().filter(|child| !child.terminal).count();
    json!({
        "delegated": delegated,
        "retries": items.len() - delegated,
        "running": running,
    })
}

/// 一条子运行在父行里需要的全部事实。
///
/// 子运行原本只贡献三个计数；用户点开「2 项委派」时想看到的是「哪两项、
/// 谁在做、卡在哪」，所以这里连同角色、状态、错误一起带出去。
struct ChildFacts {
    run_id: String,
    delegated: bool,
    terminal: bool,
    role_id: String,
    status: String,
    error: String,
    started_at: String,
    updated_at: String,
}

impl ChildFacts {
    fn to_json(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "kind": if self.delegated { "delegate" } else { "retry" },
            "role_id": self.role_id,
            "status": self.status,
            "error": self.error,
            "started_at": self.started_at,
            "updated_at": self.updated_at,
        })
    }
}

/// 本机工作流运行，投影自本地运行台账。
fn ledger_items(limit: usize) -> Result<Vec<Value>, Box<dyn Error>> {
    let ledger = LocalRunLedger::open_default()?;
    let names = workflow_names();
    let mut items = Vec::new();
    let runs = ledger.list_runs(limit)?;
    // 一次扫出父子关系：每个父运行下挂了几个子运行，以及子运行是什么语义。
    // 界面要能回答「这一步是重做还是换了个人做」，逐条再查库会变成 N+1。
    // 用拥有的 run_id 副本做键，后面才能按值消费 runs。
    let mut children: HashMap<String, Vec<ChildFacts>> = HashMap::new();
    for run in &runs {
        let parent = run.parent_run_id.trim();
        if parent.is_empty() {
            continue;
        }
        // 子运行的角色取当前步骤（没有就取最后一步），与父行同一口径。
        let step = run
            .steps
            .iter()
            .find(|step| step.step_id == run.current_step_id)
            .or_else(|| run.steps.last());
        children
            .entry(parent.to_string())
            .or_default()
            .push(ChildFacts {
                run_id: run.run_id.clone(),
                delegated: run.parent_kind.is_delegation(),
                terminal: run.status.is_terminal(),
                role_id: step
                    .map(|step| step.role_id.trim().to_string())
                    .unwrap_or_default(),
                status: run_status_key(&run.status).to_string(),
                error: run.error.clone(),
                started_at: iso_from_epoch(&run.created_at),
                updated_at: iso_from_epoch(&run.updated_at),
            });
    }
    for run in runs {
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
        // 角色取自当前步骤：任务在推进，执行的「谁」也跟着换。
        let role_id = current_step
            .map(|step| step.role_id.trim().to_string())
            .unwrap_or_default();
        // 整条运行里出现过的角色，按首次出现顺序去重：界面用它画「这个任务由哪几种角色协作」。
        let roles = role_rollup(&run.steps);
        // 顶层运行才带团队信息；子运行本身在列表里不出角色团队，避免同一批人重复计数。
        let team = if run.parent_run_id.trim().is_empty() {
            team_rollup(children.get(&run.run_id))
        } else {
            Value::Null
        };
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
            // 行内审批需要知道停在哪一步：approve/reject 都按 step_id 定位。
            "current_step_id": run.current_step_id,
            // 失败步骤用于行内重试：只给第一个失败步骤，重试它会连带重置下游。
            "failed_step_id": run
                .steps
                .iter()
                .find(|step| step.status == LocalStepStatus::Failed)
                .map(|step| step.step_id.clone())
                .unwrap_or_default(),
            "pause_kind": match run.pause_kind {
                crate::agent_core_contracts::RunPauseKind::User => "user",
                crate::agent_core_contracts::RunPauseKind::Approval => "approval",
                crate::agent_core_contracts::RunPauseKind::Feedback => "feedback",
                crate::agent_core_contracts::RunPauseKind::External => "external",
                crate::agent_core_contracts::RunPauseKind::None => "",
            },
            "role_id": role_id,
            "roles": roles,
            "parent_run_id": run.parent_run_id,
            "parent_kind": if run.parent_run_id.trim().is_empty() {
                ""
            } else if run.parent_kind.is_delegation() {
                "delegate"
            } else {
                "retry"
            },
            "team": team,
            // 子运行明细：父行展开时直接渲染，不再为每条子运行单独查询。
            "children": if run.parent_run_id.trim().is_empty() {
                children
                    .get(&run.run_id)
                    .map(|items| items.iter().map(ChildFacts::to_json).collect::<Vec<_>>())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
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
            "current_step_id": "",
            "failed_step_id": "",
            "pause_kind": "",
            // 技能运行没有角色和父子关系：字段保持在，值为空，前端据此不渲染协作区。
            "role_id": "",
            "roles": Vec::<Value>::new(),
            "parent_run_id": "",
            "parent_kind": "",
            "team": Value::Null,
            "children": Vec::<Value>::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn child(delegated: bool, terminal: bool) -> ChildFacts {
        ChildFacts {
            run_id: format!("run-{}", if delegated { "d" } else { "r" }),
            delegated,
            terminal,
            role_id: String::new(),
            status: if terminal { "completed" } else { "running" }.to_string(),
            error: String::new(),
            started_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn no_children_means_no_team_block() {
        // 没有子运行就别给界面一个「0 委派 · 0 重做」的空壳。
        assert_eq!(team_rollup(None), Value::Null);
        assert_eq!(team_rollup(Some(&Vec::new())), Value::Null);
    }

    #[test]
    fn delegation_and_rework_are_reported_separately() {
        let items = vec![
            child(true, true),
            child(true, false),
            child(false, true),
            child(false, true),
        ];
        let rollup = team_rollup(Some(&items));
        assert_eq!(rollup["delegated"], 2);
        assert_eq!(rollup["retries"], 2);
        // 只有未终结的子运行算「进行中」。
        assert_eq!(rollup["running"], 1);
    }

    #[test]
    fn child_facts_expose_what_the_parent_row_shows_on_expand() {
        let mut delegated = child(true, false);
        delegated.role_id = "software.reviewer".to_string();
        delegated.error = String::new();
        let facts = delegated.to_json();
        // 展开子运行要能回答「谁在做、什么状态、有没有错」，只给计数不够。
        assert_eq!(facts["run_id"], "run-d");
        assert_eq!(facts["kind"], "delegate");
        assert_eq!(facts["role_id"], "software.reviewer");
        assert_eq!(facts["status"], "running");

        let retry = child(false, true);
        assert_eq!(retry.to_json()["kind"], "retry");
    }

    fn step(id: &str, role: &str, status: LocalStepStatus) -> LocalRunStep {
        LocalRunStep {
            step_id: id.to_string(),
            title: id.to_string(),
            status,
            capability_id: String::new(),
            runtime_provider: String::new(),
            role_id: role.to_string(),
            attempt: 0,
            started_at: String::new(),
            finished_at: String::new(),
            error: String::new(),
        }
    }

    #[test]
    fn roles_keep_first_seen_order_and_skip_unassigned_steps() {
        let roles = role_rollup(&[
            step("A", "software.analyst", LocalStepStatus::Succeeded),
            step("B", "", LocalStepStatus::Succeeded),
            step("C", "software.implementer", LocalStepStatus::Running),
            step("D", "software.analyst", LocalStepStatus::Succeeded),
        ]);
        // 顺序反映步骤推进顺序；重复角色合并成一条；没有角色的步骤不产出条目。
        let ids: Vec<&str> = roles.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["software.analyst", "software.implementer"]);
    }

    #[test]
    fn role_status_takes_the_most_attention_demanding_step() {
        // 一个角色的两步：一步已完成、一步在跑 —— 角色必须显示进行中，
        // 否则用户会看到「测试已完成」而实际测试还在跑。
        let roles = role_rollup(&[
            step("A", "software.verifier", LocalStepStatus::Succeeded),
            step("B", "software.verifier", LocalStepStatus::Running),
        ]);
        assert_eq!(roles[0]["status"], "running");

        // 有失败就报失败，哪怕同时还有在跑的步骤。
        let roles = role_rollup(&[
            step("A", "software.verifier", LocalStepStatus::Running),
            step("B", "software.verifier", LocalStepStatus::Failed),
        ]);
        assert_eq!(roles[0]["status"], "failed");
    }
}
