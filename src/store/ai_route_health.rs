//! 聚合路由的短期健康状态。
//!
//! 同一个 exposed model 可以配置多条上游路由。某条路由出现 429/5xx 后，
//! 在短 TTL 内把它排到同模型路由的后面，让下一次请求优先走健康路由。
//! 这是进程内的快速失败切换，不持久化：重启后重新探测。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 失败路由的冷却时间。太长会让恢复后的路由迟迟不参与；太短则等于没有切换。
const FAILURE_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, Default)]
struct RouteHealth {
    failures: u32,
    unhealthy_until_ms: u64,
}

static HEALTH: OnceLock<Mutex<HashMap<String, RouteHealth>>> = OnceLock::new();

fn health() -> &'static Mutex<HashMap<String, RouteHealth>> {
    HEALTH.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub(crate) fn route_key(base_url: &str, upstream_model: &str) -> String {
    format!(
        "{}|{}",
        base_url.trim().trim_end_matches('/'),
        upstream_model.trim()
    )
}

pub(crate) fn mark_failure(base_url: &str, upstream_model: &str) -> u32 {
    mark_failure_at(route_key(base_url, upstream_model), now_ms())
}

pub(crate) fn mark_success(base_url: &str, upstream_model: &str) {
    if let Ok(mut health) = health().lock() {
        health.remove(&route_key(base_url, upstream_model));
    }
}

pub(crate) fn is_healthy(base_url: &str, upstream_model: &str) -> bool {
    is_healthy_at(route_key(base_url, upstream_model), now_ms())
}

fn mark_failure_at(key: String, now_ms: u64) -> u32 {
    let Ok(mut health) = health().lock() else {
        return 0;
    };
    let entry = health.entry(key).or_default();
    entry.failures = entry.failures.saturating_add(1);
    entry.unhealthy_until_ms = now_ms.saturating_add(FAILURE_TTL_MS);
    entry.failures
}

fn is_healthy_at(key: String, now_ms: u64) -> bool {
    let Ok(mut health) = health().lock() else {
        return true;
    };
    match health.get(&key) {
        Some(entry) if entry.unhealthy_until_ms > now_ms => false,
        Some(_) => {
            health.remove(&key);
            true
        }
        None => true,
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(mut health) = health().lock() {
        health.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_cools_down_then_recovers() {
        reset_for_test();
        let key = route_key("https://example.com/v1", "model-a");
        mark_failure_at(key.clone(), 1_000);
        assert!(!is_healthy_at(key.clone(), 1_000));
        assert!(!is_healthy_at(key.clone(), 1_000 + FAILURE_TTL_MS - 1));
        assert!(is_healthy_at(key, 1_000 + FAILURE_TTL_MS));
    }

    #[test]
    fn failure_count_increments_before_recovery() {
        let key = route_key("https://failure-count.example/v1", "model-a");
        assert_eq!(mark_failure_at(key.clone(), 1_000), 1);
        assert_eq!(mark_failure_at(key, 1_000), 2);
    }

    #[test]
    fn success_clears_failure_immediately() {
        reset_for_test();
        let base = "https://example.com/v1";
        let model = "model-a";
        mark_failure(base, model);
        assert!(!is_healthy(base, model));
        mark_success(base, model);
        assert!(is_healthy(base, model));
    }
}
