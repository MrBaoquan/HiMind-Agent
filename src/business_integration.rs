use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use std::any::Any;
use std::error::Error;

use crate::approval::remote::ApprovalProof;

/// Stable identifier for a remote business system that contributes
/// organization-scoped capabilities to HiMind Agent.
pub(crate) const BUSINESS_INTEGRATION_PROTOCOL_ID: &str = "himind-agent.business-integration";
pub(crate) const BUSINESS_INTEGRATION_PROTOCOL_VERSION: &str = "1";
pub(crate) const BUSINESS_INTEGRATION_ACCEPT: &str =
    "application/vnd.himind.business-integration+json;v=1";
pub(crate) const BUSINESS_INTEGRATION_PROTOCOL_HEADER: &str =
    "X-HiMind-Business-Integration-Protocol";
pub(crate) const BUSINESS_INTEGRATION_VERSION_HEADER: &str =
    "X-HiMind-Business-Integration-Version";
pub(crate) const BUSINESS_INTEGRATION_PROVIDER_HEADER: &str =
    "X-HiMind-Business-Integration-Provider";
pub(crate) const BUSINESS_CAPABILITY_ID_HEADER: &str = "X-HiMind-Business-Capability-ID";
pub(crate) const BUSINESS_CAPABILITY_VERSION_HEADER: &str = "X-HiMind-Business-Capability-Version";

/// Canonical identity for the first built-in integration. Keeping this value
/// outside the Dashboard adapter lets the Gateway reason about providers
/// without taking a dependency on a Dashboard-specific type.
pub(crate) const DASHBOARD_BUSINESS_PROVIDER_ID: &str = "himind.dashboard";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct BusinessProviderDescriptor {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BusinessCapabilityContract {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub risk_level: String,
    pub http_method: String,
    pub scope: String,
    /// Provider-relative operation route. `dashboard_route` is accepted only
    /// as a wire compatibility alias while Dashboard migrates to this shared
    /// business-integration protocol.
    pub route: String,
    pub input_schema: Value,
    pub execution_mode: String,
    #[serde(default)]
    pub supports_progress: bool,
    #[serde(default)]
    pub supports_cancel: bool,
    pub idempotency: String,
    #[serde(default = "default_retry_policy")]
    pub retry_policy: String,
    #[serde(default = "default_concurrency_policy")]
    pub concurrency: String,
    #[serde(default)]
    pub approval_required: bool,
}

#[derive(Debug, Deserialize)]
struct BusinessCapabilityContractWire {
    id: String,
    version: String,
    name: String,
    description: String,
    risk_level: String,
    http_method: String,
    scope: String,
    route: Option<String>,
    dashboard_route: Option<String>,
    input_schema: Value,
    execution_mode: String,
    #[serde(default)]
    supports_progress: bool,
    #[serde(default)]
    supports_cancel: bool,
    idempotency: String,
    #[serde(default = "default_retry_policy")]
    retry_policy: String,
    #[serde(default = "default_concurrency_policy")]
    concurrency: String,
    #[serde(default)]
    approval_required: bool,
}

impl<'de> Deserialize<'de> for BusinessCapabilityContract {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = BusinessCapabilityContractWire::deserialize(deserializer)?;
        let route = match (wire.route, wire.dashboard_route) {
            (Some(route), Some(legacy)) if route != legacy => {
                return Err(serde::de::Error::custom(
                    "route and dashboard_route must be identical",
                ));
            }
            (Some(route), _) | (_, Some(route)) => route,
            (None, None) => {
                return Err(serde::de::Error::custom("route is required"));
            }
        };
        Ok(Self {
            id: wire.id,
            version: wire.version,
            name: wire.name,
            description: wire.description,
            risk_level: wire.risk_level,
            http_method: wire.http_method,
            scope: wire.scope,
            route,
            input_schema: wire.input_schema,
            execution_mode: wire.execution_mode,
            supports_progress: wire.supports_progress,
            supports_cancel: wire.supports_cancel,
            idempotency: wire.idempotency,
            retry_policy: wire.retry_policy,
            concurrency: wire.concurrency,
            approval_required: wire.approval_required,
        })
    }
}

pub(crate) fn default_retry_policy() -> String {
    "never".to_string()
}

pub(crate) fn default_concurrency_policy() -> String {
    "keyed".to_string()
}

#[derive(Debug, Clone)]
pub(crate) struct BusinessCatalogSnapshot {
    pub provider: BusinessProviderDescriptor,
    pub protocol: String,
    pub protocol_version: String,
    pub generation: String,
    pub items: Vec<BusinessCapabilityContract>,
}

impl BusinessCatalogSnapshot {
    #[cfg(test)]
    pub(crate) fn dashboard(generation: String, items: Vec<BusinessCapabilityContract>) -> Self {
        Self {
            provider: BusinessProviderDescriptor {
                id: DASHBOARD_BUSINESS_PROVIDER_ID.to_string(),
                kind: "control_plane".to_string(),
            },
            protocol: BUSINESS_INTEGRATION_PROTOCOL_ID.to_string(),
            protocol_version: BUSINESS_INTEGRATION_PROTOCOL_VERSION.to_string(),
            generation,
            items,
        }
    }
}

/// The only interface the Capability Gateway needs for remote business
/// systems. Providers authenticate and transport requests; the Gateway keeps
/// schema validation, policy, approval and local auditing in one place.
pub(crate) trait BusinessIntegrationProvider: Send + Sync {
    #[cfg(test)]
    fn as_any(&self) -> &dyn Any;

    fn provider_id(&self) -> &str;

    fn protocol_id(&self) -> &str {
        BUSINESS_INTEGRATION_PROTOCOL_ID
    }

    fn protocol_version(&self) -> &str {
        BUSINESS_INTEGRATION_PROTOCOL_VERSION
    }

    fn catalog_snapshot(&self) -> Option<BusinessCatalogSnapshot>;

    fn invoke(
        &self,
        contract: &BusinessCapabilityContract,
        input: Value,
        request_id: &str,
        proof: Option<&ApprovalProof>,
    ) -> Result<Value, Box<dyn Error>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_the_legacy_dashboard_route_as_a_wire_alias() {
        let contract: BusinessCapabilityContract = serde_json::from_value(json!({
            "id": "business.example.read",
            "version": "1.0.0",
            "name": "Example",
            "description": "Example capability",
            "risk_level": "read_only",
            "http_method": "GET",
            "scope": "business.example.read",
            "dashboard_route": "/api/integrations/ai/business/examples",
            "input_schema": {"type": "object", "properties": {}, "additionalProperties": false},
            "execution_mode": "sync",
            "idempotency": "safe"
        }))
        .unwrap();

        assert_eq!(contract.route, "/api/integrations/ai/business/examples");
    }

    #[test]
    fn rejects_drifting_standard_and_legacy_routes() {
        let result = serde_json::from_value::<BusinessCapabilityContract>(json!({
            "id": "business.example.read",
            "version": "1.0.0",
            "name": "Example",
            "description": "Example capability",
            "risk_level": "read_only",
            "http_method": "GET",
            "scope": "business.example.read",
            "route": "/api/integrations/ai/business/examples",
            "dashboard_route": "/api/integrations/ai/business/other",
            "input_schema": {"type": "object", "properties": {}, "additionalProperties": false},
            "execution_mode": "sync",
            "idempotency": "safe"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn dashboard_snapshot_has_the_standard_protocol_identity() {
        let snapshot = BusinessCatalogSnapshot::dashboard("generation-1".into(), Vec::new());
        assert_eq!(snapshot.provider.id, DASHBOARD_BUSINESS_PROVIDER_ID);
        assert_eq!(snapshot.protocol, BUSINESS_INTEGRATION_PROTOCOL_ID);
        assert_eq!(
            snapshot.protocol_version,
            BUSINESS_INTEGRATION_PROTOCOL_VERSION
        );
    }
}
