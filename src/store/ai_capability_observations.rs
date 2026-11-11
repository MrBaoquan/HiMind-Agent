//! Provider 能力运行时观测。
//!
//! 兼容矩阵是“发货时的事实”，但真实上游会漂移：同一个 Provider 可能今天接受
//! `function_call` 历史，明天对长历史返回泛化 400。网关每次成功执行兼容回退后，
//! 在这里累计一次观测；达到阈值后，后续请求可以据此提前降级，避免每次都先撞
//! 一次 400 再重试。
//!
//! 这里只存结构（Provider / 协议 / 能力名 / 次数 / 时间），不存请求正文或凭据。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const STORE_FILE: &str = "ai-capability-observations.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 128;
/// 至少两次成功回退才认为这是稳定能力缺口，避免一次偶发 400 就永久降级。
pub(crate) const PRE_DEGRADE_THRESHOLD: u64 = 2;
/// 观测只在一段时间内有效；Provider 修复后不应该永久被预降级。
const OBSERVATION_TTL_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CapabilityObservation {
    pub provider: String,
    pub protocol: String,
    pub feature: String,
    pub successes: u64,
    pub last_seen: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ObservationStore {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    observations: BTreeMap<String, CapabilityObservation>,
}

static CACHE: OnceLock<Mutex<Option<ObservationStore>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<ObservationStore>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

fn store_path() -> Result<PathBuf, String> {
    let home = crate::store::paths::agent_home();
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    Ok(home.join(STORE_FILE))
}

fn load_store() -> ObservationStore {
    let Ok(path) = store_path() else {
        return ObservationStore::default();
    };
    let Ok(bytes) = fs::read(path) else {
        return ObservationStore::default();
    };
    let mut store = serde_json::from_slice::<ObservationStore>(&bytes).unwrap_or_default();
    if store.schema_version > SCHEMA_VERSION {
        return ObservationStore::default();
    }
    store.schema_version = SCHEMA_VERSION;
    store
}

fn save_store(store: &ObservationStore) -> Result<(), String> {
    let path = store_path()?;
    let bytes = serde_json::to_vec_pretty(store).map_err(|error| error.to_string())?;
    crate::store::atomic_file::atomic_write(&path, &bytes).map_err(|error| error.to_string())
}

fn observation_key(provider: &str, protocol: &str, feature: &str) -> String {
    format!("{provider}|{protocol}|{feature}")
}

/// 记录一次“兼容回退后上游成功”的观测。调用方只在最终响应成功时调用。
pub(crate) fn record_success(provider: &str, protocol: &str, feature: &str) -> Result<(), String> {
    let mut cache = cache().lock().map_err(|_| "能力观测缓存不可用".to_string())?;
    let store = cache.get_or_insert_with(load_store);
    let key = observation_key(provider, protocol, feature);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let entry = store
        .observations
        .entry(key)
        .or_insert_with(|| CapabilityObservation {
            provider: provider.to_string(),
            protocol: protocol.to_string(),
            feature: feature.to_string(),
            successes: 0,
            last_seen: now.clone(),
        });
    entry.successes = entry.successes.saturating_add(1);
    entry.last_seen = now;
    prune(&mut store.observations);
    save_store(store)
}

/// 已达到预降级阈值的特征名。未知 Provider 的 key 由调用方带上 host，避免
/// 把所有“未知兼容上游”混成同一个能力事实。
pub(crate) fn observed_features(provider: &str, protocol: &str) -> Vec<String> {
    let Ok(mut cache) = cache().lock() else {
        return Vec::new();
    };
    let store = cache.get_or_insert_with(load_store);
    store
        .observations
        .values()
        .filter(|item| {
            item.provider == provider
                && item.protocol == protocol
                && item.successes >= PRE_DEGRADE_THRESHOLD
                && observation_is_recent(&item.last_seen)
        })
        .map(|item| item.feature.clone())
        .collect()
}

fn observation_is_recent(last_seen: &str) -> bool {
    let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(last_seen) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(timestamp.with_timezone(&chrono::Utc));
    age <= chrono::Duration::days(OBSERVATION_TTL_DAYS)
}

/// 状态页/诊断用：按成功次数倒序返回最近的能力观测。
pub(crate) fn list() -> Result<Vec<CapabilityObservation>, String> {
    let mut cache = cache().lock().map_err(|_| "能力观测缓存不可用".to_string())?;
    let store = cache.get_or_insert_with(load_store);
    let mut items = store.observations.values().cloned().collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .successes
            .cmp(&left.successes)
            .then_with(|| right.last_seen.cmp(&left.last_seen))
    });
    items.truncate(50);
    Ok(items)
}

fn prune(observations: &mut BTreeMap<String, CapabilityObservation>) {
    if observations.len() <= MAX_ENTRIES {
        return;
    }
    let mut keys = observations
        .iter()
        .map(|(key, item)| (key.clone(), item.successes, item.last_seen.clone()))
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));
    let remove = observations.len() - MAX_ENTRIES;
    for (key, _, _) in keys.into_iter().take(remove) {
        observations.remove(&key);
    }
}

#[cfg(test)]
pub(crate) fn reset_cache_for_test() {
    if let Ok(mut cache) = cache().lock() {
        *cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn with_isolated_home(run: impl FnOnce()) {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-capability-observations-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        reset_cache_for_test();
        run();
        reset_cache_for_test();
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn observations_need_two_successes_before_predegrade() {
        with_isolated_home(|| {
            record_success("vectide_responses", "openai-responses", "tool_history").unwrap();
            assert!(observed_features("vectide_responses", "openai-responses").is_empty());
            record_success("vectide_responses", "openai-responses", "tool_history").unwrap();
            let features = observed_features("vectide_responses", "openai-responses");
            assert_eq!(features, vec!["tool_history".to_string()]);
            assert_eq!(list().unwrap()[0].successes, 2);
        });
    }

    #[test]
    fn stale_observations_are_ignored() {
        let now = chrono::Utc::now().to_rfc3339();
        assert!(observation_is_recent(&now));
        assert!(!observation_is_recent("2020-01-01T00:00:00Z"));
        assert!(!observation_is_recent("not-a-timestamp"));
    }

    #[test]
    fn observations_are_scoped_by_provider_and_protocol() {
        with_isolated_home(|| {
            record_success("vectide_responses", "openai-responses", "tool_history").unwrap();
            record_success("vectide_responses", "openai-responses", "tool_history").unwrap();
            assert!(observed_features("unknown@example.com", "openai-responses").is_empty());
            assert!(observed_features("vectide_responses", "anthropic").is_empty());
        });
    }
}
