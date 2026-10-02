//! 扩展分发台账。
//!
//! 按 `(kind, id, version, target)` 记录每个落点的发布结果，是幂等重试的唯一
//! 依据：同一个键重跑只补差，不重复投递；「部分完成」在这里是可枚举的事实，
//! 而不是靠发布器猜。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extension_contracts::DistributionTarget;
use crate::store::{atomic_file, paths};

const STATE_SCHEMA_VERSION: u32 = 1;

pub(crate) const STATUS_PENDING: &str = "pending";
pub(crate) const STATUS_PUBLISHED: &str = "published";
pub(crate) const STATUS_FAILED: &str = "failed";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DistributionStateEntry {
    pub kind: String,
    pub id: String,
    pub version: String,
    pub target: DistributionTarget,
    /// `pending` / `published` / `failed`。
    pub status: String,
    /// GitHub 落点：tag、Release id 与网页地址。
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub release_id: String,
    #[serde(default)]
    pub html_url: String,
    /// 制品资产名与摘要，用于幂等判定与来源核对。
    #[serde(default)]
    pub asset_name: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: u64,
    /// 工作台落点：提交单与发布版本标识。
    #[serde(default)]
    pub submission_id: String,
    #[serde(default)]
    pub release_reference: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub attempts: u32,
    pub updated_at: String,
}

impl DistributionStateEntry {
    pub(crate) fn key(&self) -> String {
        state_key(&self.kind, &self.id, &self.version, self.target)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DistributionStateFile {
    #[serde(default = "schema_version")]
    schema_version: u32,
    #[serde(default)]
    entries: BTreeMap<String, DistributionStateEntry>,
}

impl Default for DistributionStateFile {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

fn schema_version() -> u32 {
    STATE_SCHEMA_VERSION
}

pub(crate) fn state_key(kind: &str, id: &str, version: &str, target: DistributionTarget) -> String {
    format!(
        "{}:{}@{}#{}",
        kind.trim(),
        id.trim(),
        version.trim(),
        target.as_str()
    )
}

pub(crate) fn path() -> PathBuf {
    paths::agent_home().join("data/distribution-state.json")
}

pub(crate) fn load() -> Result<DistributionStateFileView, Box<dyn Error>> {
    load_at(&path())
}

fn load_at(path: &Path) -> Result<DistributionStateFileView, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(DistributionStateFileView::default());
    }
    let raw = std::fs::read(path)?;
    if raw.is_empty() {
        return Ok(DistributionStateFileView::default());
    }
    let file: DistributionStateFile = serde_json::from_slice(&raw)?;
    if file.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 distribution-state schema 版本: {}",
            file.schema_version
        )
        .into());
    }
    Ok(DistributionStateFileView {
        entries: file.entries,
    })
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DistributionStateFileView {
    entries: BTreeMap<String, DistributionStateEntry>,
}

impl DistributionStateFileView {
    pub(crate) fn get(
        &self,
        kind: &str,
        id: &str,
        version: &str,
        target: DistributionTarget,
    ) -> Option<&DistributionStateEntry> {
        self.entries.get(&state_key(kind, id, version, target))
    }

    /// 某个扩展制品在全部版本上的记录，按版本倒序，用于 UI 展示发布历史。
    pub(crate) fn for_asset(&self, kind: &str, id: &str) -> Vec<DistributionStateEntry> {
        let mut items = self
            .entries
            .values()
            .filter(|entry| entry.kind == kind && entry.id == id)
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right
                .version
                .cmp(&left.version)
                .then_with(|| left.target.cmp(&right.target))
        });
        items
    }

    pub(crate) fn all(&self) -> Vec<DistributionStateEntry> {
        self.entries.values().cloned().collect()
    }
}

pub(crate) fn record(
    entry: DistributionStateEntry,
) -> Result<DistributionStateEntry, Box<dyn Error>> {
    record_at(&path(), entry)
}

fn record_at(
    path: &Path,
    mut entry: DistributionStateEntry,
) -> Result<DistributionStateEntry, Box<dyn Error>> {
    if let Some(existing) = load_at(path)?.entries.get(&entry.key()) {
        // 累加尝试次数，方便 UI 说明「重试了几次」。
        entry.attempts = existing.attempts.saturating_add(1);
    } else {
        entry.attempts = entry.attempts.max(1);
    }
    entry.updated_at = unix_timestamp_string();
    let mut file = read_file(path)?;
    let key = entry.key();
    file.entries.insert(key, entry.clone());
    write_file(path, &file)?;
    Ok(entry)
}

pub(crate) fn remove_asset(
    kind: &str,
    id: &str,
    version: &str,
    target: DistributionTarget,
) -> Result<bool, Box<dyn Error>> {
    let path = path();
    let mut file = read_file(&path)?;
    let removed = file
        .entries
        .remove(&state_key(kind, id, version, target))
        .is_some();
    if removed {
        write_file(&path, &file)?;
    }
    Ok(removed)
}

fn read_file(path: &Path) -> Result<DistributionStateFile, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(DistributionStateFile::default());
    }
    let raw = std::fs::read(path)?;
    if raw.is_empty() {
        return Ok(DistributionStateFile::default());
    }
    Ok(serde_json::from_slice(&raw)?)
}

fn write_file(path: &Path, file: &DistributionStateFile) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = atomic_file::lock(path)?;
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(file)?)?;
    Ok(())
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contracts::DistributionTarget;

    fn entry(version: &str, target: DistributionTarget, status: &str) -> DistributionStateEntry {
        DistributionStateEntry {
            kind: "plugin".to_string(),
            id: "com.himind.example".to_string(),
            version: version.to_string(),
            target,
            status: status.to_string(),
            tag: format!("plugin/com.himind.example@{version}"),
            release_id: "1".to_string(),
            html_url: "https://example.invalid".to_string(),
            asset_name: format!("com.himind.example-{version}.hmpkg"),
            sha256: "a".repeat(64),
            size_bytes: 1024,
            submission_id: String::new(),
            release_reference: String::new(),
            channel: "stable".to_string(),
            published_at: "1".to_string(),
            error: String::new(),
            attempts: 0,
            updated_at: String::new(),
        }
    }

    #[test]
    fn state_key_separates_targets() {
        let workbench = state_key(
            "plugin",
            "com.himind.example",
            "1.0.0",
            DistributionTarget::Workbench,
        );
        let github = state_key(
            "plugin",
            "com.himind.example",
            "1.0.0",
            DistributionTarget::Github,
        );
        assert_eq!(workbench, "plugin:com.himind.example@1.0.0#workbench");
        assert_eq!(github, "plugin:com.himind.example@1.0.0#github");
        assert_ne!(workbench, github);
    }

    #[test]
    fn record_roundtrips_and_counts_attempts() {
        let path = std::env::temp_dir().join(format!(
            "himind-distribution-state-{}.json",
            unix_timestamp_string()
        ));
        let first = record_at(
            &path,
            entry("1.0.0", DistributionTarget::Github, STATUS_PENDING),
        )
        .unwrap();
        assert_eq!(first.attempts, 1);
        let second = record_at(
            &path,
            entry("1.0.0", DistributionTarget::Github, STATUS_PUBLISHED),
        )
        .unwrap();
        assert_eq!(second.attempts, 2);
        assert_eq!(second.status, STATUS_PUBLISHED);
        let view = load_at(&path).unwrap();
        assert_eq!(
            view.get(
                "plugin",
                "com.himind.example",
                "1.0.0",
                DistributionTarget::Github
            )
            .map(|item| item.status.clone()),
            Some(STATUS_PUBLISHED.to_string())
        );
        // 工作台与 GitHub 是两条独立记录，互不影响。
        assert!(view
            .get(
                "plugin",
                "com.himind.example",
                "1.0.0",
                DistributionTarget::Workbench
            )
            .is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn for_asset_returns_newest_version_first() {
        let path = std::env::temp_dir().join(format!(
            "himind-distribution-state-order-{}.json",
            unix_timestamp_string()
        ));
        record_at(
            &path,
            entry("1.0.0", DistributionTarget::Github, STATUS_PUBLISHED),
        )
        .unwrap();
        record_at(
            &path,
            entry("1.1.0", DistributionTarget::Github, STATUS_PUBLISHED),
        )
        .unwrap();
        record_at(
            &path,
            entry("1.0.0", DistributionTarget::Workbench, STATUS_PUBLISHED),
        )
        .unwrap();
        let items = load_at(&path)
            .unwrap()
            .for_asset("plugin", "com.himind.example");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].version, "1.1.0");
        let _ = std::fs::remove_file(&path);
    }
}
