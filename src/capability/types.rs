use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapabilityDescriptor {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub risk_level: String,
    pub source: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InvocationSource {
    LocalHttp,
    Tauri,
    DashboardWorker,
    Cli,
    Mcp,
}

impl InvocationSource {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::LocalHttp => "local_http",
            Self::Tauri => "tauri",
            Self::DashboardWorker => "dashboard_worker",
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InvocationContext {
    pub source: InvocationSource,
    pub principal: String,
    pub request_id: String,
}

impl InvocationContext {
    pub(crate) fn new(source: InvocationSource, principal: impl Into<String>) -> Self {
        Self {
            source,
            principal: principal.into(),
            request_id: next_request_id(),
        }
    }

    pub(crate) fn local_http() -> Self {
        Self::new(InvocationSource::LocalHttp, "local-dashboard")
    }

    pub(crate) fn tauri() -> Self {
        Self::new(InvocationSource::Tauri, "local-user")
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CapabilityInvokeRequest {
    pub capability_id: String,
    #[serde(default)]
    pub input: Value,
}

fn next_request_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("inv_{millis}_{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_context_has_stable_source_and_unique_request_id() {
        let first = InvocationContext::local_http();
        let second = InvocationContext::local_http();

        assert_eq!(first.source.as_str(), "local_http");
        assert_eq!(first.principal, "local-dashboard");
        assert_ne!(first.request_id, second.request_id);
    }

    #[test]
    fn exposes_all_planned_invocation_sources() {
        assert_eq!(
            InvocationSource::DashboardWorker.as_str(),
            "dashboard_worker"
        );
        assert_eq!(InvocationSource::Cli.as_str(), "cli");
        assert_eq!(InvocationSource::Mcp.as_str(), "mcp");
    }
}
