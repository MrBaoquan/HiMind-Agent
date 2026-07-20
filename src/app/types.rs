use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LocalLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentEnrollmentRequest {
    pub enrollment_token: String,
}

#[derive(Debug, Deserialize)]
pub struct BrowserTextCaptureRequest {
    pub source_url: String,
}

#[derive(Debug, Deserialize)]
pub struct EngineeringSyncRequest {
    pub project_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteConnectRequest {
    pub vendor: String,
    pub code: String,
    pub password: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectWorkspaceRequest {
    pub path: String,
    pub engine_type: Option<String>,
    pub engine_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocalAgentUpdateRequest {
    pub download_url: Option<String>,
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub signature: Option<String>,
    pub signature_key_id: Option<String>,
    pub signature_algorithm: Option<String>,
}
