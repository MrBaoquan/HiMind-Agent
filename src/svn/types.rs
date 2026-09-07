use serde::{de::Deserializer, Deserialize, Serialize};

fn deserialize_nullable_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SvnConnectionSummary {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub username: String,
    pub provider: String,
    pub credentials_configured: bool,
    pub status: String,
    pub last_error: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SaveSvnConnectionRequest {
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SvnCheckoutRequest {
    pub project_id: String,
    pub exhibit_id: String,
    #[serde(default)]
    pub repository_url: Option<String>,
    pub target_path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateExhibitRepositoryPathRequest {
    pub project_id: String,
    pub exhibit_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InitializeExhibitRepositoryRequest {
    pub project_id: String,
    pub exhibit_id: String,
    pub engine_type: String,
    pub template_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CloneExhibitRepositoryRequest {
    pub project_id: String,
    pub exhibit_id: String,
    pub source_repository_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportLocalExhibitRequest {
    pub project_id: String,
    pub exhibit_id: String,
    pub source_path: String,
    #[serde(default)]
    pub force_migration: bool,
    #[serde(default)]
    pub ignore_policy: MigrationIgnorePolicy,
    #[serde(default)]
    pub expected_source_fingerprint: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SvnWorkspaceRequest {
    pub target_path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MigrationSourceScanRequest {
    pub target_path: String,
    #[serde(default)]
    pub ignore_policy: MigrationIgnorePolicy,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct MigrationIgnorePolicy {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub root_large_file_threshold_bytes: u64,
    #[serde(default)]
    pub root_archive_patterns: Vec<String>,
    #[serde(default)]
    pub excluded_relative_paths: Vec<String>,
    #[serde(default)]
    pub included_relative_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateRepositoryRequest {
    pub project_id: String,
    #[serde(default)]
    pub project_name: String,
    pub hook_endpoint: String,
    #[serde(default)]
    pub repository_access: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EnsureProjectExhibitsAccessRequest {
    pub project_id: String,
    #[serde(default)]
    pub repository_access: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectAclEntry {
    pub path: String,
    pub username: String,
    pub access: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewProjectAclRequest {
    pub plan_id: String,
    pub project_id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub managed_paths: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub desired_entries: Vec<ProjectAclEntry>,
    #[serde(default)]
    pub repository_access: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApplyProjectAclRequest {
    pub plan_id: String,
    pub project_id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub managed_paths: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub desired_entries: Vec<ProjectAclEntry>,
    pub expected_current_digest: String,
    #[serde(default)]
    pub repository_access: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReconcileProjectAclRequest {
    pub project_id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub managed_paths: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub desired_entries: Vec<ProjectAclEntry>,
    #[serde(default)]
    pub repository_access: String,
}

#[cfg(test)]
mod tests {
    use super::{ApplyProjectAclRequest, PreviewProjectAclRequest, ReconcileProjectAclRequest};

    #[test]
    fn project_acl_requests_accept_null_arrays_as_empty() {
        let preview: PreviewProjectAclRequest = serde_json::from_value(serde_json::json!({
            "plan_id": "plan-1",
            "project_id": "project-1",
            "managed_paths": null,
            "desired_entries": null,
        }))
        .unwrap();
        assert!(preview.managed_paths.is_empty());
        assert!(preview.desired_entries.is_empty());

        let apply: ApplyProjectAclRequest = serde_json::from_value(serde_json::json!({
            "plan_id": "plan-1",
            "project_id": "project-1",
            "managed_paths": null,
            "desired_entries": null,
            "expected_current_digest": "digest",
        }))
        .unwrap();
        assert!(apply.managed_paths.is_empty());
        assert!(apply.desired_entries.is_empty());

        let reconcile: ReconcileProjectAclRequest = serde_json::from_value(serde_json::json!({
            "project_id": "project-1",
            "managed_paths": null,
            "desired_entries": null,
        }))
        .unwrap();
        assert!(reconcile.managed_paths.is_empty());
        assert!(reconcile.desired_entries.is_empty());
    }
}
