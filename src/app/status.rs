use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::store::types::LocalWorkerStatus;

pub(crate) fn local_worker_snapshot(status: &Arc<Mutex<LocalWorkerStatus>>) -> Value {
    if let Ok(state) = status.lock() {
        let legacy_error = state.dashboard_worker_error.trim();
        // New runtimes set these fields explicitly. Keep the old inference as
        // a compatibility fallback for an in-memory status produced by an
        // older caller, but never make clients depend on the error wording.
        let dashboard_worker_state = match state.dashboard_worker_state.trim() {
            "online" | "connecting" | "offline" | "not_applicable" | "unknown" => {
                state.dashboard_worker_state.trim()
            }
            _ if legacy_error.eq_ignore_ascii_case("mcp stdio mode") => "not_applicable",
            _ if state.dashboard_worker_online => "online",
            _ if legacy_error.is_empty() => "not_applicable",
            _ if legacy_error.contains("连接") || legacy_error.contains("connecting") => {
                "connecting"
            }
            _ => "offline",
        };
        let dashboard_worker_expected =
            matches!(dashboard_worker_state, "online" | "connecting" | "offline");
        let mcp_transport = match state.worker_transport.trim() {
            "stdio" => "stdio",
            "local_http" => "local_http",
            "tauri" => "tauri",
            "cli" => "cli",
            "internal" => "internal",
            _ if legacy_error.eq_ignore_ascii_case("mcp stdio mode") => "stdio",
            _ => "local_http",
        };
        let dashboard_worker_reason_code = if !state.dashboard_worker_reason_code.trim().is_empty()
        {
            state.dashboard_worker_reason_code.trim()
        } else if mcp_transport == "stdio" {
            "stdio_companion_gateway_only"
        } else if dashboard_worker_state == "not_applicable" {
            "worker_not_managed"
        } else if dashboard_worker_state == "online" {
            "connected_agent_app_worker"
        } else if dashboard_worker_state == "connecting" {
            "connected_agent_app_starting"
        } else if dashboard_worker_state == "offline" {
            "connected_agent_app_worker_error"
        } else {
            "worker_status_unknown"
        };
        let local_service_expected = mcp_transport != "stdio";
        json!({
            "dashboard_worker_online": state.dashboard_worker_online,
            "dashboard_agent_id": if state.dashboard_agent_id.trim().is_empty() { Value::Null } else { Value::String(state.dashboard_agent_id.clone()) },
            "dashboard_worker_error": if legacy_error.is_empty() || dashboard_worker_state == "not_applicable" { Value::Null } else { Value::String(state.dashboard_worker_error.clone()) },
            "dashboard_worker_state": dashboard_worker_state,
            "dashboard_worker_expected": dashboard_worker_expected,
            "dashboard_worker_reason_code": dashboard_worker_reason_code,
            "mcp_transport": mcp_transport,
            "local_service_expected": local_service_expected,
            "local_service_online": state.local_service_online,
            "local_service_error": if state.local_service_error.trim().is_empty() { Value::Null } else { Value::String(state.local_service_error.clone()) },
            "distribution_update_available": state.distribution_update_available,
            "distribution_update_version": state.distribution_update_version,
            "distribution_update_url": state.distribution_update_url,
            "distribution_update_sha256": state.distribution_update_sha256,
            "distribution_update_signature": state.distribution_update_signature,
            "distribution_update_signature_key_id": state.distribution_update_signature_key_id,
            "distribution_update_signature_algorithm": state.distribution_update_signature_algorithm,
        })
    } else {
        json!({
            "dashboard_worker_online": false,
            "dashboard_agent_id": Value::Null,
            "dashboard_worker_error": "worker status unavailable",
            "dashboard_worker_state": "unknown",
            "dashboard_worker_expected": false,
            "dashboard_worker_reason_code": "worker_status_unavailable",
            "mcp_transport": "unknown",
            "local_service_expected": false,
            "local_service_online": false,
            "local_service_error": "worker status unavailable",
            "distribution_update_available": false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::local_worker_snapshot;
    use crate::store::types::LocalWorkerStatus;
    use std::sync::{Arc, Mutex};

    #[test]
    fn stdio_worker_status_is_not_applicable_not_an_error() {
        let snapshot = local_worker_snapshot(&Arc::new(Mutex::new(LocalWorkerStatus {
            dashboard_worker_error: "MCP stdio mode".to_string(),
            ..LocalWorkerStatus::default()
        })));
        assert_eq!(snapshot["dashboard_worker_state"], "not_applicable");
        assert_eq!(snapshot["dashboard_worker_expected"], false);
        assert_eq!(snapshot["mcp_transport"], "stdio");
        assert_eq!(snapshot["local_service_expected"], false);
        assert_eq!(
            snapshot["dashboard_worker_reason_code"],
            "stdio_companion_gateway_only"
        );
        assert!(snapshot["dashboard_worker_error"].is_null());
    }

    #[test]
    fn connecting_worker_status_remains_visible_for_local_app() {
        let snapshot = local_worker_snapshot(&Arc::new(Mutex::new(LocalWorkerStatus {
            dashboard_worker_error: "正在连接 Dashboard 任务 Worker".to_string(),
            ..LocalWorkerStatus::default()
        })));
        assert_eq!(snapshot["dashboard_worker_state"], "connecting");
        assert_eq!(snapshot["dashboard_worker_expected"], true);
        assert_eq!(snapshot["mcp_transport"], "local_http");
        assert_eq!(
            snapshot["dashboard_worker_error"],
            "正在连接 Dashboard 任务 Worker"
        );
        assert_eq!(
            snapshot["dashboard_worker_reason_code"],
            "connected_agent_app_starting"
        );
    }

    #[test]
    fn explicit_worker_state_does_not_parse_error_wording() {
        let snapshot = local_worker_snapshot(&Arc::new(Mutex::new(LocalWorkerStatus {
            dashboard_worker_error: "正在连接 Dashboard 任务 Worker".to_string(),
            dashboard_worker_state: "offline".to_string(),
            dashboard_worker_reason_code: "connected_agent_app_worker_error".to_string(),
            worker_transport: "local_http".to_string(),
            ..LocalWorkerStatus::default()
        })));

        assert_eq!(snapshot["dashboard_worker_state"], "offline");
        assert_eq!(snapshot["dashboard_worker_expected"], true);
        assert_eq!(
            snapshot["dashboard_worker_reason_code"],
            "connected_agent_app_worker_error"
        );
    }
}
