//! 平台级定时任务。
//!
//! 定时是底座原语，不是某个能力的附属功能：一条计划描述“什么时候、对什么目标做什么”，
//! 目前支持的目标类型是 Workflow Run，后续新增目标类型只需扩展 `dispatch` 与校验，
//! 不需要再长一套计划存储、调度线程和 UI。
//!
//! 平台只负责到点启动，不改变被执行者的语义：Workflow 目标走的是与手动启动完全相同的
//! `schedule_workflow_with_gateway`，因此 Ledger、事件、Artifact、审批、`on_failure`
//! 降级全部一致。

use crate::capability::service::CapabilityGateway;
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::store::atomic_file::atomic_write;
use crate::store::paths::agent_home;
use chrono::{Datelike, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const STORE_FILE: &str = "schedules.json";
/// 上一版把计划存在 workflow-schedules.json，且只支持 Workflow 目标；读到就迁移成通用结构。
const LEGACY_STORE_FILE: &str = "workflow-schedules.json";
/// 一条 cron 最多向前搜索一年，足够覆盖闰年和“每 4 年一次”这类表达式。
const MAX_SEARCH_MINUTES: i64 = 366 * 24 * 60;
/// 目前实现的定时目标类型。新增类型要同时补 `validate_target` 与 `dispatch`。
const SUPPORTED_TARGET_KINDS: &[&str] = &["workflow", "skill"];
/// 收尾僵尸运行前的宽限期：避开刚启动、还没取到租约的运行。
const STALE_RUN_GRACE_SECONDS: i64 = 120;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ScheduleExecution {
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub exitpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Schedule {
    pub id: String,
    /// 目标类型，例如 `workflow`。
    #[serde(default = "default_target_kind")]
    pub kind: String,
    /// 目标标识：Workflow 目标是 workflow_id。
    #[serde(default)]
    pub target_id: String,
    /// Workflow 启动预设的来源标识。到点运行时按「预设当前值 + 这里的覆盖项」合并出参数：
    /// 预设里改了工作区，引用它的计划会跟着变，不需要逐条计划再改一遍。
    #[serde(default)]
    pub preset_id: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub execution: ScheduleExecution,
    pub cron: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub last_run_at: String,
    #[serde(default)]
    pub last_run_id: String,
    /// `accepted` | `failed`
    #[serde(default)]
    pub last_status: String,
    #[serde(default)]
    pub last_error: String,
    /// 下一次触发的 epoch 秒；为空表示还没算过。
    #[serde(default)]
    pub next_run_at: String,
}

fn default_target_kind() -> String {
    "workflow".to_string()
}

fn default_enabled() -> bool {
    true
}

/// 旧版（仅 Workflow）计划文件的形状，只用于迁移。
#[derive(Debug, Clone, Deserialize)]
struct LegacySchedule {
    id: String,
    workflow_id: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    entrypoint: String,
    #[serde(default)]
    exitpoint: String,
    cron: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    last_run_at: String,
    #[serde(default)]
    last_run_id: String,
    #[serde(default)]
    last_status: String,
    #[serde(default)]
    last_error: String,
    #[serde(default)]
    next_run_at: String,
}

pub(crate) fn store_path() -> PathBuf {
    agent_home().join(STORE_FILE)
}

fn legacy_store_path() -> PathBuf {
    agent_home().join(LEGACY_STORE_FILE)
}

pub(crate) fn load() -> Result<Vec<Schedule>, Box<dyn Error>> {
    let path = store_path();
    if path.is_file() {
        let body = fs::read_to_string(&path)?;
        // 计划文件损坏时宁可返回空列表也不要让 Agent 起不来；文件本身有 .bak 备份。
        return Ok(serde_json::from_str(&body).unwrap_or_default());
    }
    let legacy = legacy_store_path();
    if !legacy.is_file() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(&legacy)?;
    let migrated = serde_json::from_str::<Vec<LegacySchedule>>(&body)
        .unwrap_or_default()
        .into_iter()
        .map(|item| Schedule {
            id: item.id,
            kind: "workflow".to_string(),
            target_id: item.workflow_id,
            preset_id: String::new(),
            input: item.input,
            execution: ScheduleExecution {
                entrypoint: item.entrypoint,
                exitpoint: item.exitpoint,
            },
            cron: item.cron,
            enabled: item.enabled,
            created_at: item.created_at,
            updated_at: item.updated_at,
            last_run_at: item.last_run_at,
            last_run_id: item.last_run_id,
            last_status: item.last_status,
            last_error: item.last_error,
            next_run_at: item.next_run_at,
        })
        .collect::<Vec<_>>();
    save(&migrated)?;
    Ok(migrated)
}

pub(crate) fn save(items: &[Schedule]) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec_pretty(items)?;
    atomic_write(&store_path(), &body)?;
    Ok(())
}

pub(crate) fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or_default()
}

fn is_valid_schedule_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// 5 字段 cron：分 时 日 月 周。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronExpression {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    day_of_month_restricted: bool,
    day_of_week_restricted: bool,
}

impl CronExpression {
    pub(crate) fn parse(expression: &str) -> Result<Self, String> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(format!(
                "cron 必须是 5 个字段（分 时 日 月 周），当前是 {} 个：{expression}",
                fields.len()
            ));
        }
        let minutes = parse_field(fields[0], 0, 59)?;
        let hours = parse_field(fields[1], 0, 23)?;
        let days_of_month = parse_field(fields[2], 1, 31)?;
        let months = parse_field(fields[3], 1, 12)?;
        // 周字段允许 0-7，7 与 0 都表示周日。
        let mut days_of_week = parse_field(fields[4], 0, 7)?
            .into_iter()
            .map(|value| value % 7)
            .collect::<Vec<_>>();
        days_of_week.sort_unstable();
        days_of_week.dedup();
        Ok(Self {
            minutes,
            hours,
            days_of_month,
            months,
            days_of_week,
            day_of_month_restricted: fields[2].trim() != "*",
            day_of_week_restricted: fields[4].trim() != "*",
        })
    }

    fn matches(&self, moment: chrono::DateTime<Local>) -> bool {
        if !self.minutes.contains(&moment.minute()) || !self.hours.contains(&moment.hour()) {
            return false;
        }
        if !self.months.contains(&moment.month()) {
            return false;
        }
        let day_of_month = self.days_of_month.contains(&moment.day());
        let day_of_week = self
            .days_of_week
            .contains(&moment.weekday().num_days_from_sunday());
        match (self.day_of_month_restricted, self.day_of_week_restricted) {
            // 两个字段都限定时是“或”语义（与系统 cron 一致）。
            (true, true) => day_of_month || day_of_week,
            (true, false) => day_of_month,
            (false, true) => day_of_week,
            (false, false) => true,
        }
    }

    /// 返回严格大于 `after_epoch` 的下一次触发时间（本地时区）。
    pub(crate) fn next_after(&self, after_epoch: i64) -> Option<i64> {
        let start = Local.timestamp_opt(after_epoch, 0).single()?;
        let mut candidate =
            start.with_second(0)?.with_nanosecond(0)? + chrono::Duration::minutes(1);
        for _ in 0..MAX_SEARCH_MINUTES {
            if self.matches(candidate) {
                return Some(candidate.timestamp());
            }
            candidate += chrono::Duration::minutes(1);
        }
        None
    }
}

fn parse_field(field: &str, minimum: u32, maximum: u32) -> Result<Vec<u32>, String> {
    let mut values = Vec::new();
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("cron 字段 {field} 含空片段"));
        }
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step = step
                    .parse::<u32>()
                    .map_err(|_| format!("cron 步长不合法：{part}"))?;
                if step == 0 {
                    return Err(format!("cron 步长必须大于 0：{part}"));
                }
                (range, step)
            }
            None => (part, 1),
        };
        let (low, high) = if range == "*" {
            (minimum, maximum)
        } else if let Some((low, high)) = range.split_once('-') {
            let low = low
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("cron 取值不合法：{part}"))?;
            let high = high
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("cron 取值不合法：{part}"))?;
            (low, high)
        } else {
            let value = range
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("cron 取值不合法：{part}"))?;
            (value, value)
        };
        if low < minimum || high > maximum || low > high {
            return Err(format!("cron 取值超出范围 {minimum}-{maximum}：{part}"));
        }
        let mut value = low;
        while value <= high {
            values.push(value);
            value += step;
        }
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        return Err(format!("cron 字段 {field} 没有匹配值"));
    }
    Ok(values)
}

fn schedule_next_after(expression: &str, after_epoch: i64) -> Result<i64, String> {
    CronExpression::parse(expression)?
        .next_after(after_epoch)
        .ok_or_else(|| format!("cron 表达式在一年内没有匹配时间：{expression}"))
}

/// 校验目标类型与参数。未知类型 fail closed，不做“猜一个目标”的兜底。
fn validate_target(kind: &str, target_id: &str) -> Result<(), String> {
    if !SUPPORTED_TARGET_KINDS.contains(&kind) {
        return Err(format!(
            "不支持的定时目标类型：{kind}（当前支持：{}）",
            SUPPORTED_TARGET_KINDS.join(", ")
        ));
    }
    if target_id.trim().is_empty() {
        return Err(format!("定时目标 {kind} 需要 target_id"));
    }
    if kind == "workflow" {
        let installed = crate::workflow::WorkflowStore::open_default()
            .map_err(|error| error.to_string())?
            .list()
            .map_err(|error| error.to_string())?;
        let Some(package) = installed.iter().find(|item| item.package.id == target_id) else {
            return Err(format!("Workflow 尚未安装：{target_id}"));
        };
        if !package.enabled {
            return Err(format!("Workflow 已被禁用，无法建立定时任务：{target_id}"));
        }
    }
    if kind == "skill" {
        // 技能必须在运行前就存在：定时任务不该等到触发那一刻才发现目标不存在。
        let store = crate::skill::store::SkillStore::new();
        let record = store
            .installed_record(target_id)
            .map_err(|error| error.to_string())?;
        if record.is_none() {
            return Err(format!("技能尚未安装：{target_id}"));
        }
    }
    Ok(())
}

fn schedule_json(item: &Schedule) -> Value {
    json!({
        "id": item.id,
        "kind": item.kind,
        "target_id": item.target_id,
        "preset_id": item.preset_id,
        "input": item.input,
        "execution": {
            "entrypoint": item.execution.entrypoint,
            "exitpoint": item.execution.exitpoint,
        },
        "cron": item.cron,
        "enabled": item.enabled,
        "next_run_at": item.next_run_at,
        "last_run_at": item.last_run_at,
        "last_run_id": item.last_run_id,
        "last_status": item.last_status,
        "last_error": item.last_error,
        "created_at": item.created_at,
        "updated_at": item.updated_at,
    })
}

/// 计划引用预设时，身上那些「和预设当前值一样」的键只是建计划当天抄下来的副本。
/// 删掉它不改变下一次运行用的值（本来就一样），却能让计划重新跟随预设：
/// 否则用户改了预设里的工作区，计划还抱着一份旧副本，等于白改。
fn drop_redundant_overrides(
    item: &mut Schedule,
    preset: &crate::workflow::WorkflowRunPreset,
) -> bool {
    let mut changed = false;
    let preset_input = preset.input.as_object();
    if let Some(input) = item.input.as_object_mut() {
        let before = input.len();
        input.retain(
            |key, value| match preset_input.and_then(|base| base.get(key)) {
                // 和预设同值 → 是副本，删掉；预设里没有这个键 → 是这条计划自己的覆盖，留着。
                Some(base_value) => !same_param(value, base_value),
                None => true,
            },
        );
        changed |= input.len() != before;
    }
    for (current, preset_value) in [
        (&mut item.execution.entrypoint, preset.entrypoint.as_str()),
        (&mut item.execution.exitpoint, preset.exitpoint.as_str()),
    ] {
        if !current.is_empty() && current.trim() == preset_value.trim() {
            current.clear();
            changed = true;
        }
    }
    changed
}

/// 值比较抹平「数字/布尔 ↔ 表单往返后的字符串」这类同值不同型，
/// 免得多余副本因为类型不同而留下来。
fn same_param(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Value::Number(_) | Value::Bool(_), Value::String(_))
        | (Value::String(_), Value::Number(_) | Value::Bool(_)) => {
            scalar_text(left) == scalar_text(right)
        }
        _ => false,
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 把所有计划的冗余覆盖项清一遍；返回是否有改动。
/// 读路径只做这一种无损改写，写完文件里留下的才是「真正需要覆盖的东西」。
fn compact_overrides(items: &mut [Schedule]) -> bool {
    let presets = crate::workflow::load_run_presets().unwrap_or_default();
    let mut changed = false;
    for item in items.iter_mut() {
        if item.kind != "workflow" || item.preset_id.trim().is_empty() {
            continue;
        }
        let preset = presets
            .iter()
            .find(|preset| preset.id == item.preset_id && preset.workflow_id == item.target_id);
        if let Some(preset) = preset {
            changed |= drop_redundant_overrides(item, preset);
        }
    }
    changed
}

/// 列出计划，并按需要补齐 `next_run_at`（例如计划写入后 Agent 重启过）。
pub(crate) fn list(now: i64) -> Result<Value, Box<dyn Error>> {
    let mut items = load()?;
    let mut changed = false;
    for item in items.iter_mut() {
        if item.next_run_at.is_empty() {
            if let Ok(next) = schedule_next_after(&item.cron, now) {
                item.next_run_at = next.to_string();
                changed = true;
            }
        }
    }
    // 顺手把「抄自预设」的键清掉：这样用户改一次预设，所有引用它的计划都跟上。
    changed |= compact_overrides(&mut items);
    if changed {
        save(&items)?;
    }
    Ok(json!({
        "now": now.to_string(),
        "store_path": store_path().to_string_lossy().to_string(),
        "timezone": Local::now().format("%z").to_string(),
        "target_kinds": SUPPORTED_TARGET_KINDS,
        "schedules": items.iter().map(schedule_json).collect::<Vec<_>>(),
    }))
}

pub(crate) fn set(input: &Value, now: i64) -> Result<Value, Box<dyn Error>> {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("workflow")
        .to_string();
    let target_id = input
        .get("target_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("schedule.set 需要 target_id")?
        .to_string();
    let preset_id = input
        .get("preset_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let cron = input
        .get("cron")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("schedule.set 需要 cron（5 字段，本地时区）")?;
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("schedule-{target_id}").replace(['/', '\\', ':'], "-"));
    if !is_valid_schedule_id(&id) {
        return Err("计划 id 只能包含字母、数字、点、横线和下划线".into());
    }
    validate_target(&kind, &target_id).map_err(|error| -> Box<dyn Error> { error.into() })?;
    if kind == "workflow" && !preset_id.is_empty() {
        let presets = crate::workflow::list_run_presets(&target_id)?;
        let found = presets
            .get("presets")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("id").and_then(Value::as_str) == Some(preset_id.as_str())
                        && item.get("workflow_id").and_then(Value::as_str)
                            == Some(target_id.as_str())
                })
            });
        if !found {
            return Err(format!("Workflow 启动预设不存在或不属于目标：{preset_id}").into());
        }
    }
    let next_run_at = schedule_next_after(cron, now)?;
    let enabled = input
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let run_input = input
        .get("input")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let execution = input
        .get("execution")
        .cloned()
        .map(serde_json::from_value::<ScheduleExecution>)
        .transpose()
        .map_err(|_| "schedule.set 的 execution 结构不合法")?
        .unwrap_or_default();

    let mut items = load()?;
    let mut record = items
        .iter()
        .find(|item| item.id == id)
        .cloned()
        .unwrap_or_else(|| Schedule {
            id: id.clone(),
            kind: kind.clone(),
            target_id: target_id.clone(),
            preset_id: preset_id.clone(),
            input: json!({}),
            execution: ScheduleExecution::default(),
            cron: cron.to_string(),
            enabled: true,
            created_at: now.to_string(),
            updated_at: String::new(),
            last_run_at: String::new(),
            last_run_id: String::new(),
            last_status: String::new(),
            last_error: String::new(),
            next_run_at: String::new(),
        });
    record.kind = kind;
    record.target_id = target_id;
    record.preset_id = preset_id;
    record.cron = cron.to_string();
    record.enabled = enabled;
    record.input = run_input;
    record.execution = execution;
    record.updated_at = now.to_string();
    record.next_run_at = next_run_at.to_string();
    record.last_error.clear();
    items.retain(|item| item.id != record.id);
    items.push(record.clone());
    items.sort_by(|left, right| left.id.cmp(&right.id));
    save(&items)?;
    Ok(json!({
        "saved": true,
        "schedule": schedule_json(&record),
        "store_path": store_path().to_string_lossy().to_string(),
    }))
}

pub(crate) fn delete(id: &str) -> Result<Value, Box<dyn Error>> {
    let id = id.trim();
    if !is_valid_schedule_id(id) {
        return Err("计划 id 不合法".into());
    }
    let mut items = load()?;
    let before = items.len();
    items.retain(|item| item.id != id);
    let removed = before != items.len();
    if removed {
        save(&items)?;
    }
    Ok(json!({ "removed": removed, "id": id }))
}

fn due_at(item: &Schedule, now: i64) -> bool {
    if !item.enabled {
        return false;
    }
    match item.next_run_at.trim().parse::<i64>() {
        // 没有 next_run_at（新写入或旧文件）时按“还没算过”处理，本次补算不触发，
        // 避免 Agent 重启后把所有计划都当成“刚刚到点”。
        Ok(next) => next <= now,
        Err(_) => false,
    }
}

/// 按目标类型派发。新增目标类型在这里扩展，定时语义（cron/next_run/记录）保持不变。
fn dispatch(
    gateway: CapabilityGateway,
    item: &Schedule,
    run_input: Value,
    context: InvocationContext,
) -> Result<Value, Box<dyn Error>> {
    match item.kind.as_str() {
        "workflow" => crate::app::commands::schedule_workflow_with_gateway(
            gateway,
            &item.target_id,
            run_input,
            context,
        ),
        // 技能目标：跑一次技能运行，结果写在 skill-runs/<run_id>/。
        "skill" => {
            crate::skill_run::start(gateway.options(), &item.id, &item.target_id, &run_input)
        }
        other => Err(format!("不支持的定时目标类型：{other}").into()),
    }
}

/// 同目标是否还有没跑完的运行？
///
/// 计划到点就派发只看时间，不看上一次是否还在跑。对会改工作区的目标来说，上一次
/// 没结束又开一次，等于让两个 Run 同时写同一份 `dist/`、上游锁和缓存，谁覆盖谁
/// 全凭运气。派发前查一次运行台账，有在跑的就把这次到点顺延。
///
/// 只按计划目标（Workflow id）判断，不区分是谁启动的：手动跑的那次同样占着工作区。
fn busy_run_for(workflow_id: &str) -> Result<Option<String>, Box<dyn Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    busy_run_in(&ledger, workflow_id)
}

/// 台账查询本身单独抽出来，测试才能对着一个临时台账验证，不去碰真实 Agent 目录。
fn busy_run_in(
    ledger: &crate::store::local_runs::LocalRunLedger,
    workflow_id: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    for run in ledger.list_runs(100)? {
        if run.status.is_terminal() {
            continue;
        }
        let Some(plan) = run.execution_plan.as_ref() else {
            continue;
        };
        if plan.workflow_id == workflow_id {
            return Ok(Some(run.run_id));
        }
    }
    Ok(None)
}

/// 触发所有到点的计划：派发目标、记录结果、推进下一次时间。
pub(crate) fn run_due(gateway: CapabilityGateway, now: i64) -> Result<Value, Box<dyn Error>> {
    let mut items = load()?;
    let mut fired = Vec::new();
    let mut changed = false;
    for item in items.iter_mut() {
        if item.next_run_at.trim().is_empty() {
            if let Ok(next) = schedule_next_after(&item.cron, now) {
                item.next_run_at = next.to_string();
                changed = true;
            }
            continue;
        }
        if !due_at(item, now) {
            continue;
        }
        let mut run_input = item.input.clone();
        if !run_input.is_object() {
            run_input = json!({});
        }
        // 预设是这条计划的参数基线：到点按预设「当前」的那份值跑，计划里只留覆盖项。
        // 用户改一次预设（例如项目换了目录），所有引用它的计划自动跟上，
        // 而不是各自抱着一份建计划当天的陈旧快照。
        let mut preset_entrypoint = String::new();
        let mut preset_exitpoint = String::new();
        let mut skip_reason = String::new();
        if item.kind == "workflow" && !item.preset_id.trim().is_empty() {
            match crate::workflow::find_run_preset(&item.target_id, &item.preset_id) {
                Ok(Some(preset)) => {
                    run_input = crate::workflow::merge_run_preset_input(&preset.input, &run_input);
                    preset_entrypoint = preset.entrypoint.trim().to_string();
                    preset_exitpoint = preset.exitpoint.trim().to_string();
                }
                Ok(None) => {
                    // 预设被删掉、计划自己也没存参数：没有可用的输入，
                    // 与其带着一份缺参数的请求去撞一次运行失败，不如把原因记在计划上。
                    if run_input
                        .as_object()
                        .is_some_and(|object| object.is_empty())
                    {
                        skip_reason = format!(
                            "引用的启动预设已不存在：{}。请重新选择预设，或把这套参数保存成计划自带的参数。",
                            item.preset_id
                        );
                    }
                }
                Err(error) => {
                    skip_reason = format!("读取启动预设失败：{error}");
                }
            }
        }
        if skip_reason.is_empty() {
            // 入口/出口同样以预设为基线，计划里显式选过的才覆盖。
            let entrypoint = if item.execution.entrypoint.trim().is_empty() {
                preset_entrypoint
            } else {
                item.execution.entrypoint.clone()
            };
            let exitpoint = if item.execution.exitpoint.trim().is_empty() {
                preset_exitpoint
            } else {
                item.execution.exitpoint.clone()
            };
            if !entrypoint.is_empty() || !exitpoint.is_empty() {
                let execution = json!({ "entrypoint": entrypoint, "exitpoint": exitpoint });
                if let Some(object) = run_input.as_object_mut() {
                    object.insert("execution".to_string(), execution);
                }
            }
        }
        // 上一次没跑完就先不派发：顺延，不推进 next_run_at，也不记 last_run_at，
        // 等它结束后下一个 tick 立刻补上——是「到点顺延」而不是「跳过这一次」。
        if skip_reason.is_empty() && item.kind == "workflow" {
            match busy_run_for(&item.target_id) {
                Ok(Some(busy_run_id)) => {
                    let note =
                        format!("上一次运行（{busy_run_id}）还没结束，本次到点顺延，跑完立刻补上");
                    // 同一条提示只写一次：每 30 秒 tick 重写一次文件没有意义。
                    if item.last_error != note {
                        item.last_error = note;
                        changed = true;
                    }
                    continue;
                }
                Ok(None) => {}
                // 台账读不出来不该拦住计划本身：记一笔，照常派发。
                Err(error) => eprintln!("scheduler busy-check failed for {}: {error}", item.id),
            }
        }
        let context =
            InvocationContext::new(InvocationSource::Scheduler, format!("schedule:{}", item.id));
        item.last_run_at = now.to_string();
        if !skip_reason.is_empty() {
            item.last_run_id.clear();
            item.last_status = "failed".to_string();
            item.last_error = skip_reason;
        } else {
            let outcome = dispatch(gateway.clone(), item, run_input, context);
            match outcome {
                Ok(value) => {
                    item.last_run_id = value
                        .get("run_id")
                        .and_then(Value::as_str)
                        .or_else(|| value.pointer("/run/run_id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    item.last_status = "accepted".to_string();
                    item.last_error.clear();
                }
                Err(error) => {
                    item.last_run_id.clear();
                    item.last_status = "failed".to_string();
                    item.last_error = error.to_string();
                }
            }
        }
        match schedule_next_after(&item.cron, now) {
            Ok(next) => item.next_run_at = next.to_string(),
            Err(error) => {
                item.next_run_at.clear();
                item.last_status = "failed".to_string();
                item.last_error = error;
            }
        }
        fired.push(schedule_json(item));
        changed = true;
    }
    if changed {
        save(&items)?;
    }
    Ok(json!({
        "now": now.to_string(),
        "fired": fired,
        "store_path": store_path().to_string_lossy().to_string(),
    }))
}

/// 启动 Agent 内置调度线程。
///
/// 每 30 秒检查一次计划文件；没有到点的计划时只做一次文件读取，开销可以忽略。
/// 只有桌面 Agent 会启动它，MCP 伴生进程不重复调度。
pub(crate) fn start_scheduler(gateway: CapabilityGateway) {
    let _ = std::thread::Builder::new()
        .name("himind-scheduler".to_string())
        .spawn(move || loop {
            let now = now_epoch();
            if let Err(error) = run_due(gateway.clone(), now) {
                eprintln!("scheduler tick failed: {error}");
            }
            // 顺手收尾被中断的运行：进程死了就没人续租，界面不该永远显示“执行中”。
            if let Err(error) = abandon_stale_runs() {
                eprintln!("scheduler stale-run sweep failed: {error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
}

/// 把租约已过期的运行标成失败。返回收尾数量，便于日志与验证。
pub(crate) fn abandon_stale_runs() -> Result<usize, Box<dyn Error>> {
    let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
    let abandoned = ledger.abandon_stale_runs(
        "运行中断：执行进程已退出（租约过期，未续租）",
        STALE_RUN_GRACE_SECONDS,
        200,
    )?;
    for run in &abandoned {
        eprintln!("abandoned stale workflow run {}: {}", run.run_id, run.error);
    }
    Ok(abandoned.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core_contracts::{
        InteractionSource, InteractionTransport, LocalRun, LocalRunExecutionPlan, LocalRunStatus,
        LOCAL_RUN_SCHEMA_VERSION,
    };

    #[test]
    fn cron_accepts_standard_field_forms() {
        let every_minute = CronExpression::parse("* * * * *").unwrap();
        assert_eq!(every_minute.minutes.len(), 60);
        let daily_morning = CronExpression::parse("30 9 * * *").unwrap();
        assert_eq!(daily_morning.minutes, vec![30]);
        assert_eq!(daily_morning.hours, vec![9]);
        assert!(!daily_morning.day_of_month_restricted);
        let step = CronExpression::parse("*/15 * * * *").unwrap();
        assert_eq!(step.minutes, vec![0, 15, 30, 45]);
        let range = CronExpression::parse("0 9-11 * * 1-5").unwrap();
        assert_eq!(range.hours, vec![9, 10, 11]);
        assert_eq!(range.days_of_week, vec![1, 2, 3, 4, 5]);
        // 7 与 0 都是周日。
        assert_eq!(
            CronExpression::parse("0 0 * * 7").unwrap().days_of_week,
            vec![0]
        );
    }

    #[test]
    fn cron_rejects_invalid_fields() {
        assert!(CronExpression::parse("* * * *").is_err());
        assert!(CronExpression::parse("60 * * * *").is_err());
        assert!(CronExpression::parse("* 24 * * *").is_err());
        assert!(CronExpression::parse("* * 0 * *").is_err());
        assert!(CronExpression::parse("* * * 13 *").is_err());
        assert!(CronExpression::parse("*/0 * * * *").is_err());
        assert!(CronExpression::parse("5-1 * * * *").is_err());
        assert!(CronExpression::parse("a * * * *").is_err());
    }

    #[test]
    fn next_after_finds_the_next_matching_minute_in_local_time() {
        let expression = CronExpression::parse("30 9 * * *").unwrap();
        let base = 1789812420; // 2026-09-19T10:07:00Z
        let next = expression.next_after(base).unwrap();
        assert!(next > base);
        let moment = Local.timestamp_opt(next, 0).unwrap();
        assert_eq!(moment.hour(), 9);
        assert_eq!(moment.minute(), 30);
        // 严格大于：连续两次调用不会得到同一个时间。
        let following = expression.next_after(next).unwrap();
        assert!(following > next);
    }

    #[test]
    fn day_of_month_and_weekday_use_or_semantics_when_both_restricted() {
        let expression = CronExpression::parse("0 12 1 * 1").unwrap();
        assert!(expression.day_of_month_restricted);
        assert!(expression.day_of_week_restricted);
        let base = 1789812420;
        let next = expression.next_after(base).unwrap();
        let moment = Local.timestamp_opt(next, 0).unwrap();
        assert_eq!(moment.hour(), 12);
        assert!(
            moment.day() == 1 || moment.weekday().num_days_from_sunday() == 1,
            "expected the 1st or a Monday, got {moment}"
        );
    }

    #[test]
    fn unknown_target_kinds_are_rejected_instead_of_guessed() {
        // 未实现的目标类型必须 fail closed，不做兜底猜测。
        assert!(validate_target("http", "https://example.com").is_err());
        assert!(validate_target("shell", "ls").is_err());
        assert!(validate_target("", "anything").is_err());
        assert_eq!(SUPPORTED_TARGET_KINDS, &["workflow", "skill"]);
    }

    fn preset(
        input: Value,
        entrypoint: &str,
        exitpoint: &str,
    ) -> crate::workflow::WorkflowRunPreset {
        crate::workflow::WorkflowRunPreset {
            id: "preset-1".to_string(),
            workflow_id: "wf.demo".to_string(),
            label: "演示".to_string(),
            input,
            entrypoint: entrypoint.to_string(),
            exitpoint: exitpoint.to_string(),
            pinned: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn plan(input: Value, entrypoint: &str, exitpoint: &str) -> Schedule {
        Schedule {
            id: "plan-1".to_string(),
            kind: "workflow".to_string(),
            target_id: "wf.demo".to_string(),
            preset_id: "preset-1".to_string(),
            input,
            execution: ScheduleExecution {
                entrypoint: entrypoint.to_string(),
                exitpoint: exitpoint.to_string(),
            },
            cron: "0 9 * * *".to_string(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
            last_run_at: String::new(),
            last_run_id: String::new(),
            last_status: String::new(),
            last_error: String::new(),
            next_run_at: String::new(),
        }
    }

    #[test]
    fn plan_follows_preset_and_only_keeps_real_overrides() {
        // 建计划那天抄下来的整份副本：工作区、分支都和预设一样。
        let mut item = plan(
            json!({ "workspace_root": "F:/demo", "branch": "main", "topic": "日报" }),
            "run.sh",
            "",
        );
        let base = preset(
            json!({ "workspace_root": "F:/demo", "branch": "main" }),
            "run.sh",
            "",
        );
        assert!(drop_redundant_overrides(&mut item, &base));
        // 与预设同值的键被清掉（跟随预设），只有计划自己的覆盖项留下。
        assert_eq!(item.input, json!({ "topic": "日报" }));
        // 入口和预设一致 → 也清掉，回到跟随预设。
        assert!(item.execution.entrypoint.is_empty());

        // 已清理过一遍：再跑一次不该有改动，避免每次读都重写文件。
        assert!(!drop_redundant_overrides(&mut item, &base));
    }

    #[test]
    fn plan_keeps_values_that_differ_from_the_preset() {
        let mut item = plan(
            json!({ "workspace_root": "F:/other", "branch": "release" }),
            "other.sh",
            "notify.sh",
        );
        let base = preset(
            json!({ "workspace_root": "F:/demo", "branch": "main" }),
            "run.sh",
            "notify.sh",
        );
        assert!(drop_redundant_overrides(&mut item, &base));
        assert_eq!(
            item.input,
            json!({ "workspace_root": "F:/other", "branch": "release" })
        );
        assert_eq!(item.execution.entrypoint, "other.sh");
        // 出口和预设同值 → 清掉。
        assert!(item.execution.exitpoint.is_empty());
    }

    #[test]
    fn override_compaction_tolerates_form_round_tripped_scalars() {
        // 表单往返会把数字/布尔变成字符串，同值不同型也要认成副本，否则清不掉。
        let mut item = plan(json!({ "shards": "4", "dry_run": "true" }), "", "");
        let base = preset(json!({ "shards": 4, "dry_run": true }), "", "");
        assert!(drop_redundant_overrides(&mut item, &base));
        assert_eq!(item.input, json!({}));
    }

    fn busy_ledger() -> crate::store::local_runs::LocalRunLedger {
        let root = std::env::temp_dir().join(format!(
            "himind-scheduler-busy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::store::local_runs::LocalRunLedger::new(
            root.join(crate::store::local_runs::LOCAL_RUN_DB_FILE),
        )
    }

    fn workflow_run(status: LocalRunStatus, run_id: &str, workflow_id: &str) -> LocalRun {
        LocalRun {
            schema_version: LOCAL_RUN_SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            interaction_id: format!("int-{run_id}"),
            parent_run_id: String::new(),
            source: InteractionSource::Cron,
            transport: InteractionTransport::Local,
            status,
            runtime_provider: "himind.builtin".to_string(),
            workspace_ref: "F:/workspace".to_string(),
            current_step_id: String::new(),
            completion_mode: "partial".to_string(),
            execution_plan: Some(LocalRunExecutionPlan {
                workflow_id: workflow_id.to_string(),
                execution_policy: "segmented".to_string(),
                entrypoint: "sync".to_string(),
                exitpoint: "published".to_string(),
                entry_step_id: "step-1".to_string(),
                exit_step_id: "step-2".to_string(),
                active_step_ids: vec!["step-1".to_string(), "step-2".to_string()],
                seed_artifacts: Vec::new(),
                assumptions: Vec::new(),
                plan_digest: "sha256:test".to_string(),
            }),
            steps: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            usage: None,
            error: String::new(),
            created_at: "2026-09-28T00:00:00Z".to_string(),
            updated_at: format!("2026-09-28T00:00:{:02}Z", run_id.len() % 60),
        }
    }

    #[test]
    fn busy_run_only_counts_unfinished_runs_of_the_same_workflow() {
        let ledger = busy_ledger();
        ledger
            .save_run(&workflow_run(
                LocalRunStatus::Running,
                "run-busy",
                "wf.sync",
            ))
            .unwrap();
        ledger
            .save_run(&workflow_run(
                LocalRunStatus::Running,
                "run-other",
                "wf.other",
            ))
            .unwrap();

        // 同一个工作流还没跑完 → 这条计划顺延；别的目标在跑不关它的事。
        assert_eq!(
            busy_run_in(&ledger, "wf.sync").unwrap().as_deref(),
            Some("run-busy")
        );
        assert_eq!(
            busy_run_in(&ledger, "wf.other").unwrap().as_deref(),
            Some("run-other")
        );

        // 终态（成功/失败/取消）不再占着工作区，下一次到点照常派发。
        let mut finished = workflow_run(LocalRunStatus::Succeeded, "run-busy", "wf.sync");
        finished.updated_at = "2026-09-28T00:10:00Z".to_string();
        ledger.save_run(&finished).unwrap();
        assert_eq!(busy_run_in(&ledger, "wf.sync").unwrap(), None);
    }
}
