use serde::{Deserialize, Serialize};

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
pub(crate) struct SvnWorkspaceRequest {
    pub target_path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateRepositoryRequest {
    pub project_id: String,
    #[serde(default)]
    pub project_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EnsureProjectExhibitsAccessRequest {
    pub project_id: String,
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
    pub managed_paths: Vec<String>,
    pub desired_entries: Vec<ProjectAclEntry>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApplyProjectAclRequest {
    pub plan_id: String,
    pub project_id: String,
    pub managed_paths: Vec<String>,
    pub desired_entries: Vec<ProjectAclEntry>,
    pub expected_current_digest: String,
}
