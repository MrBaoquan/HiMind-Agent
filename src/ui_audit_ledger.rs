//! 界面审查台账。
//!
//! 为什么要单独一份台账：审查跑完，结论只留在那一次运行的事件流里，
//! 下一次审查既不知道「上次发现过什么」，也不知道「上次那条修了没有」。
//! 打磨因此永远从零开始——这正是「持续优化」变成空话的原因。
//!
//! 台账按稳定 id 记录问题及其状态，跨运行可查。它不复制运行的执行事实
//! （那些在 Local Run Ledger 里），只记录**判断的结论**。

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;

const STORE_FILE: &str = "ui-audit-ledger.json";
const MAX_ISSUES: usize = 2000;

/// 一条被记录的界面问题。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditIssue {
    /// 稳定 id，形如 `a11y-4f2c`。跨运行不变，用于回答「这条修了吗」。
    pub id: String,
    /// 发现它的类别：ia / craft / a11y。
    pub category: String,
    /// 问题标题，一句话。
    pub title: String,
    /// 复现位置（文件路径或页面名）。
    #[serde(default)]
    pub location: String,
    /// 现象与原因。
    #[serde(default)]
    pub detail: String,
    /// open / fixed / wontfix。
    pub status: String,
    /// 首次发现它的运行。
    #[serde(default)]
    pub first_run_id: String,
    /// 最近一次确认它的运行（可能是再次复现，也可能是确认已修）。
    #[serde(default)]
    pub last_run_id: String,
    pub created_at: String,
    pub updated_at: String,
}

fn store_path() -> std::path::PathBuf {
    crate::store::paths::agent_home().join(STORE_FILE)
}

fn now_stamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn load() -> Result<Vec<AuditIssue>, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&path)?;
    // 文件损坏时返回空而不是让调用方崩掉：台账是辅助资料，不该阻断审查。
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

fn save(items: &[AuditIssue]) -> Result<(), Box<dyn Error>> {
    let trimmed = if items.len() > MAX_ISSUES {
        &items[items.len() - MAX_ISSUES..]
    } else {
        items
    };
    let body = serde_json::to_vec_pretty(trimmed)?;
    crate::store::atomic_file::atomic_write(&store_path(), &body)?;
    Ok(())
}

/// 由类别 + 位置 + 标题派生稳定 id。
///
/// 不用随机值或自增：同一条问题在不同运行里要能对上，全靠这个派生。
/// 位置与标题都参与，所以「同一处但问题变了」会被视为新问题——这是对的，
/// 因为修法不同。
fn issue_id(category: &str, location: &str, title: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(category.trim().as_bytes());
    hasher.update(b"|");
    hasher.update(location.trim().as_bytes());
    hasher.update(b"|");
    hasher.update(title.trim().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(3).map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{hex}", category.trim())
}

/// 登记一批新发现的问题。
///
/// 已存在（同 id）的不重复插入，但会刷新 `last_run_id` 与 `updated_at`——
/// 再次复现也是一条值得记录的事实。
pub(crate) fn record_findings(
    run_id: &str,
    findings: &[Value],
) -> Result<Value, Box<dyn Error>> {
    let mut items = load()?;
    let mut added = 0u32;
    let mut refreshed = 0u32;
    for finding in findings {
        let category = finding
            .get("category")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("misc");
        let title = finding
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(title) = title else {
            // 没有标题的问题无法追踪，直接跳过而不是造一个空条目。
            continue;
        };
        let location = finding
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let id = issue_id(category, location, title);
        let stamp = now_stamp();
        if let Some(existing) = items.iter_mut().find(|item| item.id == id) {
            existing.last_run_id = run_id.to_string();
            existing.updated_at = stamp;
            // 已标 wontfix 的不因为再次出现就翻成 open：那是否决过的决定。
            refreshed += 1;
            continue;
        }
        items.push(AuditIssue {
            id,
            category: category.to_string(),
            title: title.to_string(),
            location: location.to_string(),
            detail: finding
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string(),
            status: "open".to_string(),
            first_run_id: run_id.to_string(),
            last_run_id: run_id.to_string(),
            created_at: stamp.clone(),
            updated_at: stamp,
        });
        added += 1;
    }
    save(&items)?;
    Ok(json!({ "added": added, "refreshed": refreshed, "total": items.len() }))
}

/// 把一批问题标记为已修。
///
/// 只接受显式给出的 id 列表：不根据「这轮没再报」自动关闭——
/// 审查可能因为范围缩小而漏报，那不是修好了。
pub(crate) fn mark_fixed(run_id: &str, ids: &[String]) -> Result<Value, Box<dyn Error>> {
    let mut items = load()?;
    let stamp = now_stamp();
    let mut updated = 0u32;
    let mut missing = Vec::new();
    for id in ids {
        match items.iter_mut().find(|item| item.id == id.trim()) {
            Some(issue) => {
                issue.status = "fixed".to_string();
                issue.last_run_id = run_id.to_string();
                issue.updated_at = stamp.clone();
                updated += 1;
            }
            None => missing.push(id.clone()),
        }
    }
    save(&items)?;
    Ok(json!({ "updated": updated, "missing": missing }))
}

/// 台账汇总：按状态与类别计数，附带仍待处理的问题。
///
/// `open` 保留完整条目（那是下一步要做的），`fixed` 只计数——
/// 历史问题翻出来只会淹没有用的信息。
pub(crate) fn summary() -> Result<Value, Box<dyn Error>> {
    let items = load()?;
    let mut by_status: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_category: BTreeMap<String, u32> = BTreeMap::new();
    let mut open_items = Vec::new();
    for issue in &items {
        *by_status.entry(issue.status.clone()).or_default() += 1;
        if issue.status == "open" {
            *by_category.entry(issue.category.clone()).or_default() += 1;
            open_items.push(json!({
                "id": issue.id,
                "category": issue.category,
                "title": issue.title,
                "location": issue.location,
                "first_run_id": issue.first_run_id,
                "last_run_id": issue.last_run_id,
                "updated_at": issue.updated_at,
            }));
        }
    }
    Ok(json!({
        "store_path": store_path().to_string_lossy().to_string(),
        "total": items.len(),
        "by_status": by_status,
        "open_by_category": by_category,
        "open": open_items,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_id_is_stable_across_runs_and_sensitive_to_content() {
        let a = issue_id("a11y", "settings.tsx", "对比度不足");
        let b = issue_id("a11y", "settings.tsx", "对比度不足");
        assert_eq!(a, b, "同一条问题在不同运行里必须得到同一个 id");

        // 类别、位置、标题任一变，都视为另一条问题：修法不同。
        assert_ne!(a, issue_id("craft", "settings.tsx", "对比度不足"));
        assert_ne!(a, issue_id("a11y", "other.tsx", "对比度不足"));
        assert_ne!(a, issue_id("a11y", "settings.tsx", "命中区过小"));
        assert!(a.starts_with("a11y-"));
    }

    /// 往返：同一条问题在两轮里各报一次，只新增一条；标已修后不再出现在 open 里。
    #[test]
    fn recording_is_idempotent_and_marking_fixed_moves_it_out_of_open() {
        // 用临时 agent home，避免污染真实台账。
        let temp = std::env::temp_dir().join(format!(
            "himind-audit-ledger-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        // SAFETY: 测试是单线程环境变量写入，且每个用例用独立目录。
        unsafe { std::env::set_var("HIMIND_AGENT_HOME", &temp) };

        let finding = json!({
            "category": "a11y",
            "title": "窄窗下页签对比度不足",
            "location": "frontend/src/pages/SettingsPage.tsx",
            "detail": "400px 下页签文字与背景对比度 2.9:1"
        });
        let first = record_findings("run-a", std::slice::from_ref(&finding)).unwrap();
        assert_eq!(first["added"], 1);
        assert_eq!(first["total"], 1);

        // 第二轮再报同一条：不新增，只刷新最近确认。
        let second = record_findings("run-b", std::slice::from_ref(&finding)).unwrap();
        assert_eq!(second["added"], 0, "同一条问题不得重复插入");
        assert_eq!(second["refreshed"], 1);
        assert_eq!(second["total"], 1);

        let first_summary = summary().unwrap();
        assert_eq!(first_summary["by_status"]["open"], 1);
        assert_eq!(first_summary["open"].as_array().unwrap().len(), 1);
        let id = first_summary["open"][0]["id"].as_str().unwrap().to_string();

        // 标已修之后不再出现在 open 清单里，但总数仍保留（历史要可查）。
        let marked = mark_fixed("run-c", &[id]).unwrap();
        assert_eq!(marked["updated"], 1);
        let after = summary().unwrap();
        assert_eq!(after["by_status"]["fixed"], 1);
        assert_eq!(after["open"].as_array().unwrap().len(), 0);

        // 标一个不存在的 id：如实报告 missing，不静默成功。
        let missing = mark_fixed("run-d", &["nope".to_string()]).unwrap();
        assert_eq!(missing["updated"], 0);
        assert_eq!(missing["missing"].as_array().unwrap().len(), 1);

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn findings_without_title_are_skipped() {
        // 空标题无法追踪，跳过而不是造空条目。
        let findings = vec![
            json!({ "category": "ia", "location": "x" }),
            json!({ "category": "ia", "title": "   " }),
        ];
        let ids: Vec<String> = findings
            .iter()
            .filter_map(|finding| {
                finding
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|title| issue_id("ia", "x", title))
            })
            .collect();
        assert!(ids.is_empty());
    }
}
