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
    /// Runtime implementation owner. This remains stable when a remote
    /// provider overlays a newer public contract onto a compiled handler.
    pub source: String,
    /// Machine-readable source of the public schema, scope and execution
    /// contract exposed through MCP and the local capability API.
    pub contract_source: String,
    /// Provider generation that supplied the current public contract. Local
    /// and fallback contracts do not carry a provider generation.
    pub contract_generation: Option<String>,
    pub availability: CapabilityAvailability,
    /// Execution contract consumed by MCP clients and the local UI.  This is
    /// intentionally descriptive: the Gateway remains the only executor.
    pub execution_mode: String,
    pub supports_progress: bool,
    pub supports_cancel: bool,
    pub idempotency: String,
    pub retry_policy: String,
    pub concurrency: String,
    pub approval_required: bool,
    pub dashboard_provider: bool,
    pub required_scope: Option<String>,
    pub dashboard_route: Option<String>,
    pub input_schema: Value,
}

/// Describes which runtime boundary a capability needs. A capability can be
/// network-backed without being owned by the organization control plane; only
/// `ControlPlane` is hidden when the Agent is running as an individual tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityAvailability {
    Local,
    NetworkService,
    ControlPlane,
}

impl CapabilityAvailability {
    pub(crate) fn available_without_control_plane(self) -> bool {
        !matches!(self, Self::ControlPlane)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvocationSource {
    LocalHttp,
    Tauri,
    DashboardWorker,
    Cli,
    Mcp,
}

/// The protocol/client that reached the Gateway is independent from the
/// transport used to carry it.  Keeping this dimension explicit prevents a
/// future HTTP MCP endpoint from being reported as a stdio companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvocationTransport {
    LocalHttp,
    Stdio,
    Tauri,
    Cli,
    Internal,
}

impl InvocationTransport {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LocalHttp => "local_http",
            Self::Stdio => "stdio",
            Self::Tauri => "tauri",
            Self::Cli => "cli",
            Self::Internal => "internal",
        }
    }
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
    pub transport: InvocationTransport,
    pub principal: String,
    pub session_id_hash: String,
    pub request_id: String,
}

impl InvocationContext {
    pub(crate) fn new(source: InvocationSource, principal: impl Into<String>) -> Self {
        let transport = match source {
            InvocationSource::LocalHttp => InvocationTransport::LocalHttp,
            InvocationSource::Tauri => InvocationTransport::Tauri,
            InvocationSource::DashboardWorker => InvocationTransport::Internal,
            InvocationSource::Cli => InvocationTransport::Cli,
            InvocationSource::Mcp => InvocationTransport::Stdio,
        };
        Self::with_transport(source, transport, principal)
    }

    pub(crate) fn with_transport(
        source: InvocationSource,
        transport: InvocationTransport,
        principal: impl Into<String>,
    ) -> Self {
        Self {
            source,
            transport,
            principal: principal.into(),
            session_id_hash: String::new(),
            request_id: next_request_id(),
        }
    }

    pub(crate) fn dashboard_user(user_id: &str, session_id_hash: &str) -> Self {
        let mut context = Self::with_transport(
            InvocationSource::LocalHttp,
            InvocationTransport::LocalHttp,
            format!("dashboard-user:{}", user_id.trim()),
        );
        context.session_id_hash = session_id_hash.trim().to_string();
        context
    }

    pub(crate) fn local_http() -> Self {
        Self::new(InvocationSource::LocalHttp, "local-agent")
    }

    pub(crate) fn tauri() -> Self {
        Self::new(InvocationSource::Tauri, "local-user")
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CapabilityInvokeRequest {
    pub capability_id: String,
    #[serde(default)]
    pub ticket: String,
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
        assert_eq!(first.principal, "local-agent");
        assert_ne!(first.request_id, second.request_id);
    }

    #[test]
    fn dashboard_user_context_carries_verified_principal() {
        let context = InvocationContext::dashboard_user("usr_123", "session_hash");

        assert_eq!(context.source, InvocationSource::LocalHttp);
        assert_eq!(context.transport, InvocationTransport::LocalHttp);
        assert_eq!(context.principal, "dashboard-user:usr_123");
        assert_eq!(context.session_id_hash, "session_hash");
    }

    #[test]
    fn mcp_context_defaults_to_stdio_transport() {
        let context = InvocationContext::new(InvocationSource::Mcp, "test-client");

        assert_eq!(context.source, InvocationSource::Mcp);
        assert_eq!(context.transport, InvocationTransport::Stdio);
    }

    #[test]
    fn mcp_source_can_be_reused_over_another_transport() {
        let context = InvocationContext::with_transport(
            InvocationSource::Mcp,
            InvocationTransport::LocalHttp,
            "test-client",
        );

        assert_eq!(context.source, InvocationSource::Mcp);
        assert_eq!(context.transport, InvocationTransport::LocalHttp);
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
