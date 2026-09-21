//! 工作流启动预设。
//!
//! 同一个工作流常常要针对多个工作区反复启动，差别只有 `workspace_root` 之类的少量参数。
//! 预设把这套参数存下来：启动时选预设 → 只改工作区 → 跑。预设只是**参数模板**，
//! 不改变运行语义，也不参与调度（定时是 scheduler 的职责）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const STORE_FILE: &str = "workflow-run-presets.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WorkflowRunPreset {
    pub id: String,
    pub workflow_id: String,
    pub label: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub exitpoint: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

pub(crate) fn store_path() -> PathBuf {
    crate::store::paths::agent_home().join(STORE_FILE)
}

pub(crate) fn load() -> Result<Vec<WorkflowRunPreset>, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(&path)?;
    // 文件损坏时返回空列表，文件本身有 .bak 备份。
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

pub(crate) fn save(items: &[WorkflowRunPreset]) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec_pretty(items)?;
    crate::store::atomic_file::atomic_write(&store_path(), &body)?;
    Ok(())
}

fn is_valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// 列出预设；带 `workflow_id` 时只返回该工作流的预设。
pub(crate) fn list(workflow_id: &str) -> Result<Value, Box<dyn Error>> {
    let filter = workflow_id.trim();
    let mut items = load()?;
    if !filter.is_empty() {
        items.retain(|item| item.workflow_id == filter);
    }
    items.sort_by(|left, right| {
        left.workflow_id
            .cmp(&right.workflow_id)
            .then_with(|| left.label.cmp(&right.label))
    });
    Ok(json!({
        "store_path": store_path().to_string_lossy().to_string(),
        "total": items.len(),
        "presets": items,
    }))
}

pub(crate) fn set(input: &Value, now: i64) -> Result<Value, Box<dyn Error>> {
    let workflow_id = input
        .get("workflow_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("workflow.preset.set 需要 workflow_id")?
        .to_string();
    let installed = crate::workflow::WorkflowStore::open_default()?.list()?;
    if !installed.iter().any(|item| item.package.id == workflow_id) {
        return Err(format!("Workflow 尚未安装：{workflow_id}").into());
    }
    let preset_input = input
        .get("input")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_label(&workflow_id, &preset_input));
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{workflow_id}").replace(['/', '\\', ':'], "-"));
    if !is_valid_id(&id) {
        return Err("预设 id 只能包含字母、数字、点、横线和下划线".into());
    }
    let mut items = load()?;
    let mut record = items
        .iter()
        .find(|item| item.id == id)
        .cloned()
        .unwrap_or_else(|| WorkflowRunPreset {
            id: id.clone(),
            workflow_id: workflow_id.clone(),
            label: label.clone(),
            input: json!({}),
            entrypoint: String::new(),
            exitpoint: String::new(),
            created_at: now.to_string(),
            updated_at: String::new(),
        });
    record.workflow_id = workflow_id;
    record.label = label;
    record.input = preset_input;
    record.entrypoint = input
        .get("entrypoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    record.exitpoint = input
        .get("exitpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    record.updated_at = now.to_string();
    items.retain(|item| item.id != record.id);
    items.push(record.clone());
    save(&items)?;
    Ok(json!({
        "saved": true,
        "preset": record,
        "store_path": store_path().to_string_lossy().to_string(),
    }))
}

pub(crate) fn delete(id: &str) -> Result<Value, Box<dyn Error>> {
    let id = id.trim();
    if !is_valid_id(id) {
        return Err("预设 id 不合法".into());
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

/// 默认名称：显式给了工作区就用目录名，否则用“默认参数”。
fn default_label(workflow_id: &str, input: &Value) -> String {
    let workspace = input
        .get("workspace_root")
        .or_else(|| input.get("project_root"))
        .and_then(Value::as_str)
        .map(|value| value.trim().replace('\\', "/"))
        .filter(|value| !value.is_empty());
    match workspace {
        Some(path) => {
            let leaf = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
            if leaf.is_empty() {
                workflow_id.to_string()
            } else {
                leaf
            }
        }
        None => format!("{workflow_id} 默认参数"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_path_safe() {
        assert!(is_valid_id("tech-radar"));
        assert!(is_valid_id("com.himind.workflow.tech-radar"));
        assert!(!is_valid_id(".."));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id(""));
    }

    #[test]
    fn default_label_prefers_the_workspace_leaf() {
        assert_eq!(
            default_label("com.himind.workflow.tech-radar", &json!({"workspace_root": "F:\\\\WebProjects\\\\项目看板"})),
            "项目看板"
        );
        assert_eq!(
            default_label("com.himind.workflow.tech-radar", &json!({})),
            "com.himind.workflow.tech-radar 默认参数"
        );
    }
}
