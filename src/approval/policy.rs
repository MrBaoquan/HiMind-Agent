use super::types::RequestType;
use sha2::{Digest, Sha256};

pub(crate) const APPROVAL_POLICY_VERSION: &str = "approval-policy-v1";

pub(crate) fn approval_generation(contract_generation: Option<&str>) -> i64 {
    let Some(generation) = contract_generation
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return 1;
    };
    let digest = Sha256::digest(generation.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    // Keep the derived generation exact when it crosses JSON/TypeScript
    // boundaries. Values above 2^53-1 cannot be represented losslessly by a
    // JavaScript number.
    (u64::from_be_bytes(bytes) & ((1u64 << 53) - 1)).max(1) as i64
}

pub(crate) fn args_digest(input: &serde_json::Value) -> Result<String, serde_json::Error> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(input)?)))
}

/// Returns the stable approval class for capabilities whose execution can
/// remove data or sever a business relationship. Unknown capabilities are not
/// treated as destructive solely because their display metadata says write.
pub(crate) fn destructive_request_type(capability_id: &str) -> Option<RequestType> {
    let id = capability_id.trim().to_ascii_lowercase();
    match id.as_str() {
        "filesystem.delete" | "workspace.delete" => Some(RequestType::FilesystemDelete),
        "business.project.delete" | "project.delete" => Some(RequestType::BusinessProjectDelete),
        "business.exhibit.delete" | "exhibit.delete" => Some(RequestType::BusinessExhibitDelete),
        // Full replacement and detach operations can remove existing business
        // relationships even though they are exposed as network writes.
        "business.project.managers.replace"
        | "business.project.owners.replace"
        | "business.exhibit.crew.replace"
        | "business.exhibit.crew.remove"
        | "business.project.exhibit.detach" => Some(RequestType::DestructiveAction),
        "mcp.server.remove"
        | "mcp.registration.remove"
        | "mcp.registration.remove_all"
        | "ai.client.remove"
        | "ai.service.custom.remove" => Some(RequestType::DestructiveAction),
        _ if id.ends_with(".delete") || id.ends_with(".delete_all") => {
            Some(RequestType::DestructiveAction)
        }
        _ => None,
    }
}

pub(crate) fn is_destructive_capability(capability_id: &str) -> bool {
    destructive_request_type(capability_id).is_some()
}

/// R3 is the minimum tier for deleting user or business data. The only R4
/// operation in this family is an unsafe target, which is rejected by the
/// filesystem handler before any approval request is shown.
pub(crate) fn effective_risk_level(capability_id: &str, declared: &str) -> &'static str {
    let capability_id = capability_id.trim().to_ascii_lowercase();
    if is_destructive_capability(&capability_id)
        || matches!(
            capability_id.as_str(),
            "software.distribution.release.publish" | "extension.review.decide"
        )
    {
        "R3"
    } else {
        match declared.trim().to_ascii_uppercase().as_str() {
            "READ_ONLY" => "R1",
            "NETWORK_WRITE" | "LOCAL_WRITE" | "LOCAL_ACTION" => "R2",
            "ADMIN_ACTION" => "R3",
            "R1" => "R1",
            "R2" => "R2",
            "R3" => "R3",
            "R4" => "R4",
            // An approval-required capability with an unknown risk vocabulary
            // must not become eligible for ordinary wildcard delegation.
            _ => "R3",
        }
    }
}

pub(crate) fn risk_rank(value: &str) -> u8 {
    match value.trim().to_ascii_uppercase().as_str() {
        "R1" => 1,
        "R2" => 2,
        "R3" => 3,
        "R4" => 4,
        _ => 4,
    }
}

pub(crate) fn target_description(capability_id: &str, input: &serde_json::Value) -> String {
    let key = if capability_id.contains("project") {
        "project_id"
    } else if capability_id.contains("exhibit") {
        "exhibit_id"
    } else if input
        .get("target")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        "target"
    } else if input
        .get("id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        "id"
    } else {
        "path"
    };
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("未提供目标")
        .chars()
        .take(500)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_and_filesystem_deletes_are_r3() {
        assert_eq!(effective_risk_level("business.project.delete", "R1"), "R3");
        assert_eq!(effective_risk_level("filesystem.delete", "read_only"), "R3");
    }

    #[test]
    fn ordinary_writes_are_not_promoted_to_destructive() {
        assert!(!is_destructive_capability("business.project.update"));
        assert_eq!(
            effective_risk_level("business.project.update", "network_write"),
            "R2"
        );
    }

    #[test]
    fn relationship_replacements_and_detach_are_r3() {
        for capability_id in [
            "business.project.managers.replace",
            "business.project.owners.replace",
            "business.exhibit.crew.replace",
            "business.exhibit.crew.remove",
            "business.project.exhibit.detach",
        ] {
            assert_eq!(effective_risk_level(capability_id, "network_write"), "R3");
            assert!(is_destructive_capability(capability_id));
        }
    }

    #[test]
    fn publish_and_admin_decisions_are_r3() {
        assert_eq!(
            effective_risk_level("software.distribution.release.publish", "network_write"),
            "R3"
        );
        assert_eq!(
            effective_risk_level("extension.review.decide", "admin_action"),
            "R3"
        );
    }

    #[test]
    fn unknown_risk_vocabulary_fails_closed_at_r3() {
        assert_eq!(effective_risk_level("third.party.write", "mystery"), "R3");
    }

    #[test]
    fn approval_generation_is_stable_and_changes_with_contract() {
        assert_eq!(approval_generation(None), 1);
        assert_eq!(
            approval_generation(Some("generation-a")),
            approval_generation(Some("generation-a"))
        );
        assert_ne!(
            approval_generation(Some("generation-a")),
            approval_generation(Some("generation-b"))
        );
        assert!(approval_generation(Some("generation-a")) <= (1i64 << 53) - 1);
    }

    #[test]
    fn legacy_catalog_risk_aliases_map_to_unified_tiers() {
        assert_eq!(effective_risk_level("context.resolve", "read_only"), "R1");
        assert_eq!(
            effective_risk_level("project.update", "network_write"),
            "R2"
        );
    }

    #[test]
    fn target_description_prefers_explicit_target_or_id() {
        assert_eq!(
            target_description("ai.client.import", &serde_json::json!({"target":"codex"})),
            "codex"
        );
        assert_eq!(
            target_description(
                "extension.review.decide",
                &serde_json::json!({"id":"skill-1"})
            ),
            "skill-1"
        );
    }
}
