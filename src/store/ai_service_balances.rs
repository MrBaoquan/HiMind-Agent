use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const SNAPSHOT_FILE: &str = "ai-service-balances.json";
const MAX_CACHE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct BalanceCurrency {
    pub currency: String,
    pub available: f64,
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub components: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AISourceBalanceSnapshot {
    pub provider_profile_id: String,
    #[serde(default = "default_snapshot_kind")]
    pub kind: String,
    #[serde(default)]
    pub balances: Vec<BalanceCurrency>,
    #[serde(default)]
    pub plan_name: Option<String>,
    #[serde(default)]
    pub quota_windows: Vec<BalanceQuotaWindow>,
    #[serde(default)]
    pub is_available: Option<bool>,
    /// Unix timestamp in seconds, matching the Agent's other local snapshots.
    pub observed_at: String,
}

fn default_snapshot_kind() -> String {
    "balance".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct BalanceQuotaWindow {
    pub name: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
}

fn snapshot_path() -> PathBuf {
    crate::store::paths::agent_home().join(SNAPSHOT_FILE)
}

fn read_snapshots(path: &PathBuf) -> BTreeMap<String, AISourceBalanceSnapshot> {
    let Ok(metadata) = fs::metadata(path) else {
        return BTreeMap::new();
    };
    if metadata.len() > MAX_CACHE_BYTES {
        return BTreeMap::new();
    }
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Cached balance data is non-critical; a corrupt or missing cache never blocks service listing.
pub(crate) fn list() -> BTreeMap<String, AISourceBalanceSnapshot> {
    read_snapshots(&snapshot_path())
}

pub(crate) fn save(id: &str, snapshot: AISourceBalanceSnapshot) -> Result<(), String> {
    validate_service_id(id)?;
    if snapshot.kind == "balance" && (snapshot.balances.is_empty() || snapshot.balances.len() > 16)
    {
        return Err("余额快照包含的币种数量无效".to_string());
    }
    if snapshot.kind == "usage" && (snapshot.balances.is_empty() || snapshot.balances.len() > 16) {
        return Err("额度快照包含的套餐数量无效".to_string());
    }
    if snapshot.kind == "quota"
        && (snapshot.quota_windows.is_empty() || snapshot.quota_windows.len() > 16)
    {
        return Err("配额快照包含的窗口数量无效".to_string());
    }
    if !matches!(snapshot.kind.as_str(), "balance" | "usage" | "quota") {
        return Err("余额/额度快照类型无效".to_string());
    }
    if snapshot.provider_profile_id.len() > 32
        || snapshot
            .plan_name
            .as_deref()
            .is_some_and(|value| value.len() > 128)
        || snapshot.balances.iter().any(|balance| {
            balance.currency.trim().is_empty()
                || balance.currency.len() > 16
                || balance.components.len() > 16
                || balance.components.keys().any(|key| key.len() > 64)
                || !balance.available.is_finite()
                || balance.total.is_some_and(|value| !value.is_finite())
                || balance.components.values().any(|value| !value.is_finite())
        })
        || snapshot.quota_windows.iter().any(|window| {
            window.name.trim().is_empty()
                || window.name.len() > 32
                || window
                    .resets_at
                    .as_deref()
                    .is_some_and(|value| value.len() > 64)
                || !window.used_percent.is_finite()
                || !window.remaining_percent.is_finite()
                || !(0.0..=100.0).contains(&window.used_percent)
                || !(0.0..=100.0).contains(&window.remaining_percent)
        })
    {
        return Err("余额快照包含无效数值".to_string());
    }

    let path = snapshot_path();
    let _lock =
        crate::store::atomic_file::lock(&path).map_err(|_| "无法锁定余额缓存".to_string())?;
    let mut snapshots = read_snapshots(&path);
    snapshots.insert(id.to_string(), snapshot);
    let bytes =
        serde_json::to_vec_pretty(&snapshots).map_err(|_| "无法序列化余额缓存".to_string())?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err("余额缓存超出允许大小".to_string());
    }
    crate::store::atomic_file::atomic_write(&path, &bytes)
        .map_err(|_| "无法保存余额缓存".to_string())
}

pub(crate) fn remove(id: &str) -> Result<(), String> {
    validate_service_id(id)?;
    let path = snapshot_path();
    let _lock =
        crate::store::atomic_file::lock(&path).map_err(|_| "无法锁定余额缓存".to_string())?;
    let mut snapshots = read_snapshots(&path);
    if snapshots.remove(id).is_some() {
        let bytes =
            serde_json::to_vec_pretty(&snapshots).map_err(|_| "无法序列化余额缓存".to_string())?;
        crate::store::atomic_file::atomic_write(&path, &bytes)
            .map_err(|_| "无法更新余额缓存".to_string())?;
    }
    Ok(())
}

fn validate_service_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("服务 ID 格式无效".to_string());
    }
    Ok(())
}
