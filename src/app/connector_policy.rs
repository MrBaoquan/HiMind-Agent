use crate::api::distribution::{connector_policy_bundle, ConnectorPolicyBundle};
use reqwest::blocking::Client;
use serde::Serialize;
use std::collections::HashSet;
use std::error::Error;
use std::time::Duration;

const SCHEMA_VERSION: &str = "connector_policy_bundle.v1";
const MAX_POLICIES: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConnectorPolicySyncReport {
    pub synced_at: String,
    pub generated_at: String,
    pub policy_count: usize,
    pub revoked_count: usize,
    pub restored_count: usize,
}

pub(crate) fn sync(options: &crate::Options) -> Result<ConnectorPolicySyncReport, Box<dyn Error>> {
    let state = crate::api::client::load_agent_state(&options.state_path)?;
    if state.agent_id.trim().is_empty() || state.credential.trim().is_empty() {
        return Err("HiMind 账号尚未授权".into());
    }
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let bundle = connector_policy_bundle(
        &client,
        &options.api_base(),
        &state.agent_id,
        &state.credential,
    )?;
    let report = apply_bundle_at(&bundle, &crate::store::connector_state::store_path())?;
    Ok(report)
}

fn apply_bundle_at(
    bundle: &ConnectorPolicyBundle,
    path: &std::path::Path,
) -> Result<ConnectorPolicySyncReport, Box<dyn Error>> {
    apply_bundle_with_credentials_at(
        bundle,
        path,
        &crate::store::connector_credentials::credential_store_path(),
    )
}

fn apply_bundle_with_credentials_at(
    bundle: &ConnectorPolicyBundle,
    path: &std::path::Path,
    credential_path: &std::path::Path,
) -> Result<ConnectorPolicySyncReport, Box<dyn Error>> {
    validate_bundle(bundle)?;
    let mut revoked_count = 0;
    let mut restored_count = 0;
    for policy in &bundle.policies {
        let before = crate::store::connector_state::status_at(path, &policy.connector_id)?;
        let after = crate::store::connector_state::apply_remote_policy_at(
            path,
            &policy.connector_id,
            policy.revoked,
            &policy.reason,
            policy.revision,
        )?;
        if !before.revoked && after.revoked {
            revoked_count += 1;
        }
        if before.revoked && !after.revoked {
            restored_count += 1;
        }
        if policy.revoked {
            crate::store::connector_credentials::remove_by_connector_at(
                credential_path,
                &policy.connector_id,
            )?;
        }
    }
    Ok(ConnectorPolicySyncReport {
        synced_at: unix_timestamp_string(),
        generated_at: bundle.generated_at.clone(),
        policy_count: bundle.policies.len(),
        revoked_count,
        restored_count,
    })
}

fn validate_bundle(bundle: &ConnectorPolicyBundle) -> Result<(), Box<dyn Error>> {
    if bundle.schema_version != SCHEMA_VERSION {
        return Err("connector policy bundle schema is invalid".into());
    }
    if bundle.policies.len() > MAX_POLICIES {
        return Err("connector policy bundle contains too many policies".into());
    }
    let mut ids = HashSet::new();
    for policy in &bundle.policies {
        let connector_id = policy.connector_id.trim();
        if connector_id.is_empty()
            || connector_id.len() > 200
            || !connector_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || !ids.insert(connector_id.to_string())
            || policy.revision == 0
            || policy.reason.len() > 500
        {
            return Err("connector policy bundle contains an invalid or duplicate policy".into());
        }
    }
    Ok(())
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{apply_bundle_at, apply_bundle_with_credentials_at, ConnectorPolicyBundle};
    use crate::api::distribution::ConnectorPolicy;
    use crate::store::connector_state;

    #[test]
    fn remote_policy_applies_restores_and_respects_local_revocation() {
        let root = std::env::temp_dir().join(format!(
            "himind-connector-policy-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = root.join("state.json");
        let remote = ConnectorPolicyBundle {
            schema_version: "connector_policy_bundle.v1".to_string(),
            policies: vec![ConnectorPolicy {
                connector_id: "wechat-miniprogram".to_string(),
                revoked: true,
                reason: "dashboard policy".to_string(),
                revision: 2,
            }],
            generated_at: "2026-09-16T00:00:00Z".to_string(),
        };
        let report = apply_bundle_at(&remote, &path).unwrap();
        assert_eq!(report.revoked_count, 1);
        assert_eq!(
            connector_state::status_at(&path, "wechat-miniprogram")
                .unwrap()
                .source,
            "dashboard"
        );

        let restore = ConnectorPolicyBundle {
            schema_version: "connector_policy_bundle.v1".to_string(),
            policies: vec![ConnectorPolicy {
                connector_id: "wechat-miniprogram".to_string(),
                revoked: false,
                reason: String::new(),
                revision: 3,
            }],
            generated_at: "2026-09-16T00:01:00Z".to_string(),
        };
        let report = apply_bundle_at(&restore, &path).unwrap();
        assert_eq!(report.restored_count, 1);
        assert!(
            !connector_state::status_at(&path, "wechat-miniprogram")
                .unwrap()
                .revoked
        );

        connector_state::revoke_at(&path, "wechat-miniprogram", "local breach").unwrap();
        apply_bundle_at(&restore, &path).unwrap();
        assert!(
            connector_state::status_at(&path, "wechat-miniprogram")
                .unwrap()
                .revoked,
            "remote non-revocation must not override a local revocation"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remote_revocation_removes_matching_local_credentials() {
        let root = std::env::temp_dir().join(format!(
            "himind-connector-policy-credentials-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let state_path = root.join("connector-state.json");
        let credential_path = root.join("connector-credentials.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &credential_path,
            serde_json::to_vec_pretty(&serde_json::json!([
                {
                    "handle": "wechat-upload-private-key",
                    "connector_id": "wechat-miniprogram",
                    "kind": "file_path",
                    "protected_value": "protected",
                    "updated_at": "1"
                },
                {
                    "handle": "github-token",
                    "connector_id": "github",
                    "kind": "secret",
                    "protected_value": "protected",
                    "updated_at": "1"
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        let bundle = ConnectorPolicyBundle {
            schema_version: "connector_policy_bundle.v1".to_string(),
            policies: vec![ConnectorPolicy {
                connector_id: "wechat-miniprogram".to_string(),
                revoked: true,
                reason: "dashboard revocation".to_string(),
                revision: 2,
            }],
            generated_at: "2026-09-16T00:00:00Z".to_string(),
        };

        apply_bundle_with_credentials_at(&bundle, &state_path, &credential_path).unwrap();

        let records: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(&credential_path).unwrap()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["connector_id"], "github");
        assert!(
            connector_state::status_at(&state_path, "wechat-miniprogram")
                .unwrap()
                .revoked
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
