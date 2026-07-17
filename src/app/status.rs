use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::store::types::LocalWorkerStatus;

pub(crate) fn local_worker_snapshot(status: &Arc<Mutex<LocalWorkerStatus>>) -> Value {
    if let Ok(state) = status.lock() {
        json!({
            "dashboard_worker_online": state.dashboard_worker_online,
            "dashboard_agent_id": if state.dashboard_agent_id.trim().is_empty() { Value::Null } else { Value::String(state.dashboard_agent_id.clone()) },
            "dashboard_worker_error": if state.dashboard_worker_error.trim().is_empty() { Value::Null } else { Value::String(state.dashboard_worker_error.clone()) },
            "local_service_online": state.local_service_online,
            "local_service_error": if state.local_service_error.trim().is_empty() { Value::Null } else { Value::String(state.local_service_error.clone()) },
        })
    } else {
        json!({
            "dashboard_worker_online": false,
            "dashboard_agent_id": Value::Null,
            "dashboard_worker_error": "worker status unavailable",
            "local_service_online": false,
            "local_service_error": "worker status unavailable",
        })
    }
}
