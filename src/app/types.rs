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
pub struct RemoteClientConfigureRequest {
    pub vendor: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct ProjectWorkspaceRequest {
    pub path: String,
    pub engine_type: Option<String>,
    pub engine_version: Option<String>,
}
