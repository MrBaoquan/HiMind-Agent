//! 技能运行：把技能说明（SKILL.md）与任务输入交给一次性 AI 运行，并把结果落盘。
//!
//! 这是平台级事实，不是某个扩展的私事：定时任务把 `kind=skill` 派发到这里，
//! 结果写在 `agent_home/skill-runs/<run_id>/`（`run.json` + `prompt.md` + `result.md`），
//! 因此“定时跑一个技能”跑完之后有明确的产物可以回看、可以定位。

use crate::Options;
use crate::skill::store::SkillStore;
use crate::store::atomic_file::atomic_write;
use crate::store::paths::agent_home;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const RUNS_DIRECTORY: &str = "skill-runs";
const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
const MAX_PREVIEW_CHARS: usize = 1200;
static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn default_tool_policy() -> String {
    "none".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillRunRecord {
    pub run_id: String,
    pub skill_id: String,
    pub skill_name: String,
    pub skill_version: String,
    /// 由哪条定时任务触发；手工发起时为空。
    #[serde(default)]
    pub schedule_id: String,
    pub task: String,
    pub workspace: String,
    /// `running` | `succeeded` | `failed`
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
    #[serde(default)]
    pub duration_seconds: u64,
    pub timeout_seconds: u64,
    /// 实际用到的模型与服务来源（managed / custom / native）。
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub service_source: String,
    #[serde(default)]
    pub endpoint: String,
    /// `none`（默认）或 `default`，与 Runtime 步骤的 `tool_policy` 同口径。
    #[serde(default = "default_tool_policy")]
    pub tools: String,
    #[serde(default)]
    pub output_path: String,
    #[serde(default)]
    pub output_chars: usize,
    #[serde(default)]
    pub output_preview: String,
    #[serde(default)]
    pub error: String,
}

pub(crate) fn runs_root() -> PathBuf {
    agent_home().join(RUNS_DIRECTORY)
}

fn run_directory(run_id: &str) -> PathBuf {
    runs_root().join(run_id)
}

fn record_path(run_id: &str) -> PathBuf {
    run_directory(run_id).join("run.json")
}

pub(crate) fn output_path(run_id: &str) -> PathBuf {
    run_directory(run_id).join("result.md")
}

fn write_record(item: &SkillRunRecord) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec_pretty(item)?;
    atomic_write(&record_path(&item.run_id), &body)?;
    Ok(())
}

fn is_safe_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id.len() <= 128
        // 至少要有一个字母或数字：否则 "." / ".." / "---" 这类会被当成目录跳转。
        && run_id.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

pub(crate) fn get(run_id: &str) -> Result<Option<SkillRunRecord>, Box<dyn Error>> {
    if !is_safe_run_id(run_id.trim()) {
        return Ok(None);
    }
    let path = record_path(run_id.trim());
    if !path.is_file() {
        return Ok(None);
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?).ok())
}

/// 最近的技能运行，按开始时间倒序。
pub(crate) fn list(limit: usize) -> Result<Value, Box<dyn Error>> {
    let root = runs_root();
    let mut items = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(&root)?.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(mut record) = get(&name)? {
                // 自愈：进程退出会带走后台线程，留下永远 running 的记录。
                // 超过超时时间再加一分钟就判定为中断，避免界面上出现假“运行中”。
                if record.status == "running" {
                    let started = record.started_at.parse::<u64>().unwrap_or_default();
                    let stale_after = record.timeout_seconds.saturating_add(60);
                    if started > 0 && timestamp_seconds().saturating_sub(started) > stale_after {
                        record.status = "failed".to_string();
                        record.error = "运行中断：Agent 进程在技能完成前退出".to_string();
                        record.finished_at = timestamp_seconds().to_string();
                        let _ = write_record(&record);
                    }
                }
                items.push(record);
            }
        }
    }
    items.sort_by(|left, right| right.started_at.cmp(&left.started_at));
    let limit = if limit == 0 { 20 } else { limit };
    items.truncate(limit);
    Ok(json!({
        "root": root.to_string_lossy().to_string(),
        "total": items.len(),
        "runs": items,
    }))
}

/// 技能运行的 workspace：优先用输入里的 workspace_root，其次当前工作区，最后 Agent 目录。
fn resolve_workspace(input: &Value) -> Result<String, Box<dyn Error>> {
    let explicit = input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let candidate = explicit
        .or_else(|| crate::extension_projects::current_workspace_path().ok())
        .unwrap_or_else(agent_home);
    let workspace =
        crate::runtime::process::canonical_workspace(candidate.to_string_lossy().as_ref())?;
    Ok(workspace.to_string_lossy().to_string())
}

/// 组装一次性提示：技能说明 + 本次任务 + 输入。技能说明原样带入，不做二次总结。
fn build_prompt(item: &SkillRunRecord, skill_markdown: &str, input: &Value) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "你正在执行 HiMind 技能「{}」（id: {}，版本 {}）。\n\n",
        item.skill_name, item.skill_id, item.skill_version
    ));
    prompt.push_str("技能说明（SKILL.md 原文）：\n\n```markdown\n");
    prompt.push_str(skill_markdown.trim());
    prompt.push_str("\n```\n\n本次任务：\n");
    prompt.push_str(item.task.trim());
    prompt.push_str("\n\n本次输入（JSON，可能为空）：\n```json\n");
    prompt.push_str(&serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".to_string()));
    prompt.push_str("\n```\n\n");
    prompt.push_str(
        "要求：严格按技能说明执行本次任务；只输出最终结果（Markdown）；不要输出思考过程；\
         不要询问补充信息；不要编造输入中没有提供的事实。",
    );
    prompt
}

fn preview(output: &str) -> String {
    let trimmed = output.trim();
    let mut preview = trimmed.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    if trimmed.chars().count() > MAX_PREVIEW_CHARS {
        preview.push_str("\n…");
    }
    preview
}

fn timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn execute(options: Options, mut item: SkillRunRecord, prompt: String) {
    let started = Instant::now();
    let result = crate::runtime::deepseek_harness::execute_workflow(
        &options,
        &item.workspace,
        &prompt,
        item.timeout_seconds,
        // 只有显式声明 tools=default 的技能运行才挂载工具。
        item.tools != "default",
        &|| Ok(false),
    );
    item.duration_seconds = started.elapsed().as_secs();
    item.finished_at = timestamp_seconds().to_string();
    match result {
        Ok(output) => {
            // 技能运行也记下实际用到的模型与服务来源（与 Runtime 步骤同一口径）。
            let output = output;
            item.model = output.model.clone();
            item.service_source = output.service_source.to_string();
            item.endpoint = output.endpoint.clone();
            let output = output.text;
            let path = output_path(&item.run_id);
            if let Err(error) = atomic_write(&path, output.as_bytes()) {
                item.status = "failed".to_string();
                item.error = format!("技能结果写入失败：{error}");
            } else {
                item.status = "succeeded".to_string();
                item.output_path = path.to_string_lossy().to_string();
                item.output_chars = output.chars().count();
                item.output_preview = preview(&output);
            }
        }
        Err(error) => {
            item.status = "failed".to_string();
            item.error = error.to_string();
        }
    }
    if let Err(error) = write_record(&item) {
        eprintln!("skill run {} 记录写入失败: {error}", item.run_id);
    }
}

/// 技能运行的工具策略。
///
/// 默认 **不开工具**：定时运行没有人在旁边，流程型技能正文很容易把模型带进
/// 一长串工具调用；先给出确定性的、可复现的结果，需要工具的技能在输入里显式
/// 声明 `"tools": "default"`。这与 Runtime 步骤的 `tool_policy` 是同一套取舍。
fn tool_policy(input: &Value) -> Result<&'static str, Box<dyn Error>> {
    match input
        .get("tools")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("none")
    {
        "" | "none" => Ok("none"),
        "default" => Ok("default"),
        other => Err(format!("技能运行的 tools 只能是 none 或 default，收到：{other}").into()),
    }
}

/// 校验目标、读技能说明、建记录并落盘 `prompt.md`。返回 (记录, 提示词)。
fn prepare(
    schedule_id: &str,
    skill_id: &str,
    input: &Value,
) -> Result<(SkillRunRecord, String), Box<dyn Error>> {
    let skill_id = skill_id.trim();
    if skill_id.is_empty() {
        return Err("技能运行需要 skill_id".into());
    }
    let task = input
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("技能运行需要 input.task（本次要做什么）")?
        .to_string();
    let store = SkillStore::new();
    let record = store
        .installed_record(skill_id)?
        .ok_or_else(|| format!("技能尚未安装：{skill_id}"))?;
    let skill_markdown = fs::read_to_string(record.version_root.join("SKILL.md"))
        .map_err(|error| format!("技能说明读取失败：{skill_id}: {error}"))?;
    let workspace = resolve_workspace(input)?;
    let timeout_seconds = input
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let tools = tool_policy(input)?.to_string();
    let run_id = format!(
        "skill_run_{}_{}",
        timestamp_seconds(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let item = SkillRunRecord {
        run_id: run_id.clone(),
        skill_id: record.manifest.id.clone(),
        skill_name: record.manifest.name.clone(),
        skill_version: record.manifest.version.clone(),
        schedule_id: schedule_id.to_string(),
        task,
        workspace,
        status: "running".to_string(),
        started_at: timestamp_seconds().to_string(),
        finished_at: String::new(),
        duration_seconds: 0,
        timeout_seconds,
        tools,
        model: String::new(),
        service_source: String::new(),
        endpoint: String::new(),
        output_path: String::new(),
        output_chars: 0,
        output_preview: String::new(),
        error: String::new(),
    };
    let prompt = build_prompt(&item, &skill_markdown, input);
    fs::create_dir_all(run_directory(&run_id))?;
    atomic_write(&run_directory(&run_id).join("prompt.md"), prompt.as_bytes())?;
    write_record(&item)?;
    Ok((item, prompt))
}

/// 启动一次技能运行。
///
/// 立即返回 `running` 记录，实际模型调用在后台线程中完成并回写 `run.json`；
/// 这样定时任务的一个 tick 不会被一次长技能卡住。
pub(crate) fn start(
    options: &Options,
    schedule_id: &str,
    skill_id: &str,
    input: &Value,
) -> Result<Value, Box<dyn Error>> {
    let (item, prompt) = prepare(schedule_id, skill_id, input)?;
    let run_id = item.run_id.clone();
    let thread_options = options.clone();
    let thread_item = item.clone();
    std::thread::Builder::new()
        .name(format!("skill-run-{run_id}"))
        .spawn(move || execute(thread_options, thread_item, prompt))?;
    Ok(json!({
        "accepted": true,
        "run_id": run_id,
        "run": item,
    }))
}

/// 同步运行一次技能，返回完成后的记录。
///
/// 命令行与脚本用它做验收：一次性进程不会因为先退出而丢掉后台线程，
/// 因此能拿到最终结果；Agent 内部的定时派发仍走 `start`（不阻塞调度 tick）。
pub(crate) fn run_blocking(
    options: &Options,
    schedule_id: &str,
    skill_id: &str,
    input: &Value,
) -> Result<Value, Box<dyn Error>> {
    let (item, prompt) = prepare(schedule_id, skill_id, input)?;
    let run_id = item.run_id.clone();
    execute(options.clone(), item, prompt);
    let record = get(&run_id)?.ok_or_else(|| format!("技能运行记录丢失：{run_id}"))?;
    Ok(json!({
        "run_id": run_id,
        "run": record,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> SkillRunRecord {
        SkillRunRecord {
            run_id: "skill_run_1_1".to_string(),
            skill_id: "com.example.skill".to_string(),
            skill_name: "示例技能".to_string(),
            skill_version: "1.0.0".to_string(),
            schedule_id: String::new(),
            task: "写一份变更摘要".to_string(),
            workspace: "C:\\work".to_string(),
            status: "running".to_string(),
            started_at: "0".to_string(),
            finished_at: String::new(),
            duration_seconds: 0,
            timeout_seconds: 60,
            tools: "none".to_string(),
            model: "deepseek-v4-flash".to_string(),
            service_source: "custom".to_string(),
            endpoint: "https://api.deepseek.com".to_string(),
            output_path: String::new(),
            output_chars: 0,
            output_preview: String::new(),
            error: String::new(),
        }
    }

    #[test]
    fn tool_policy_defaults_to_no_tools() {
        // 默认不开工具：定时运行无人值守，先要可复现的结果。
        assert_eq!(tool_policy(&json!({})).unwrap(), "none");
        assert_eq!(tool_policy(&json!({"tools": "default"})).unwrap(), "default");
        assert_eq!(tool_policy(&json!({"tools": ""})).unwrap(), "none");
        assert!(tool_policy(&json!({"tools": "all"})).is_err());
    }

    #[test]
    fn run_ids_are_path_safe() {
        assert!(is_safe_run_id("skill_run_1789857769_1"));
        assert!(!is_safe_run_id(""));
        assert!(!is_safe_run_id(".."));
        assert!(!is_safe_run_id("a/b"));
        assert!(!is_safe_run_id("a\\b"));
    }

    #[test]
    fn unsafe_run_ids_are_not_resolved_to_records() {
        assert!(get("..").unwrap().is_none());
        assert!(get("").unwrap().is_none());
        assert!(get("a/b").unwrap().is_none());
    }

    #[test]
    fn prompt_embeds_the_skill_markdown_and_task_verbatim() {
        let prompt = build_prompt(&sample_item(), "# 技能正文\n步骤一", &json!({"k": "v"}));
        assert!(prompt.contains("示例技能"));
        assert!(prompt.contains("# 技能正文"));
        assert!(prompt.contains("写一份变更摘要"));
        assert!(prompt.contains("\"k\": \"v\""));
        assert!(prompt.contains("只输出最终结果（Markdown）"));
    }

    #[test]
    fn preview_is_truncated_for_long_output() {
        let short = preview("短结果");
        assert_eq!(short, "短结果");
        let long = preview(&"字".repeat(MAX_PREVIEW_CHARS + 10));
        assert!(long.ends_with('…'));
        assert!(long.chars().count() <= MAX_PREVIEW_CHARS + 2);
    }
}
