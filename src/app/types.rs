use serde::{Deserialize, Serialize};

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

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectWorkspaceRequest {
    pub path: String,
    pub engine_type: Option<String>,
    pub engine_version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct WorkspaceBuildRequest {
    pub target_path: String,
    pub engine_type: Option<String>,
    pub engine_version: Option<String>,
    /// `native` invokes the detected engine CLI, `script` invokes the
    /// project's explicit .himind build script, and `auto` only selects
    /// native when it is available (it never silently falls back).
    pub provider: Option<String>,
    pub target_platform: Option<String>,
    pub architecture: Option<String>,
    pub configuration: Option<String>,
    pub build_method: Option<String>,
    pub output_path: Option<String>,
    pub clean: Option<bool>,
    /// 工作流里的一次构建要跑完才算一步，所以允许调用方要求阻塞等待结果。
    /// 交互式（AI/界面）调用保持默认不等待，拿到 job_id 后自己看状态。
    pub wait: Option<bool>,
    pub timeout_seconds: Option<u64>,
}
