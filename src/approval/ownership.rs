use serde::Serialize;

pub(crate) const APPROVAL_OWNERSHIP_SCHEMA_VERSION: &str = "approval_ownership.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ApprovalOwner {
    pub id: &'static str,
    pub owner: &'static str,
    pub trigger: &'static str,
    pub decision_channel: &'static str,
    pub durable_fact_source: &'static str,
    pub projection_role: &'static str,
    pub independent_mode: &'static str,
    pub secondary_decision_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ApprovalOwnershipMatrix {
    pub schema_version: &'static str,
    pub owners: Vec<ApprovalOwner>,
    pub invariants: Vec<&'static str>,
}

pub(crate) fn matrix() -> ApprovalOwnershipMatrix {
    ApprovalOwnershipMatrix {
        schema_version: APPROVAL_OWNERSHIP_SCHEMA_VERSION,
        owners: vec![
            ApprovalOwner {
                id: "agent_local",
                owner: "agent",
                trigger: "Capability Gateway, Workflow manual step, or ACP permission request",
                decision_channel: "Agent local approval queue",
                durable_fact_source: "Agent approval store and Local Run Ledger",
                projection_role: "Optional read-only Dashboard projection",
                independent_mode: "available",
                secondary_decision_allowed: false,
            },
            ApprovalOwner {
                id: "dashboard_control_plane",
                owner: "dashboard",
                trigger: "Dashboard business mutation or governed distribution action",
                decision_channel: "Dashboard approval decision or Grant",
                durable_fact_source: "Dashboard control plane",
                projection_role: "Authoritative organization fact",
                independent_mode: "unavailable",
                secondary_decision_allowed: false,
            },
            ApprovalOwner {
                id: "dsh_runtime",
                owner: "dsh_runtime",
                trigger: "DSH native approval request",
                decision_channel: "DSH approval.respond bound to rpc_id and approval_id",
                durable_fact_source: "DSH Runtime session and normalized Runtime Event",
                projection_role: "Transport and attribution only",
                independent_mode: "available",
                secondary_decision_allowed: false,
            },
        ],
        invariants: vec![
            "Every approval has exactly one decision owner.",
            "A projection or relay surface cannot create a second decision.",
            "Dashboard unavailability never blocks agent_local or dsh_runtime approvals.",
            "Unknown or ambiguous ownership fails closed.",
        ],
    }
}

pub(crate) fn requires_dashboard_fact(dashboard_enabled: bool, dashboard_provider: bool) -> bool {
    dashboard_enabled && dashboard_provider
}

#[cfg(test)]
mod tests {
    use super::{matrix, requires_dashboard_fact, APPROVAL_OWNERSHIP_SCHEMA_VERSION};

    #[test]
    fn matrix_has_one_decision_owner_for_each_current_surface() {
        let matrix = matrix();
        assert_eq!(matrix.schema_version, APPROVAL_OWNERSHIP_SCHEMA_VERSION);
        assert_eq!(matrix.owners.len(), 3);
        assert!(matrix
            .owners
            .iter()
            .any(|owner| owner.id == "agent_local" && owner.owner == "agent"));
        assert!(matrix
            .owners
            .iter()
            .any(|owner| { owner.id == "dashboard_control_plane" && owner.owner == "dashboard" }));
        assert!(matrix
            .owners
            .iter()
            .any(|owner| owner.id == "dsh_runtime" && owner.owner == "dsh_runtime"));
        assert!(matrix
            .owners
            .iter()
            .all(|owner| !owner.secondary_decision_allowed));
    }

    #[test]
    fn dashboard_fact_sync_is_limited_to_dashboard_providers() {
        assert!(requires_dashboard_fact(true, true));
        assert!(!requires_dashboard_fact(false, true));
        assert!(!requires_dashboard_fact(true, false));
    }
}
