use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeInstallationReport {
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub status: String,
    #[serde(default)]
    pub capabilities: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteExecutionReport {
    pub enabled: bool,
    pub access_mode: String,
    pub default_provider: String,
}

impl From<crate::app::remote_execution::RemoteExecutionSettings> for RemoteExecutionReport {
    fn from(value: crate::app::remote_execution::RemoteExecutionSettings) -> Self {
        Self {
            enabled: value.enabled,
            access_mode: value.access_mode,
            default_provider: value.default_provider,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: String,
    #[serde(default, skip_serializing)]
    pub credential: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_protected: String,
    #[serde(default, skip_serializing)]
    pub credential_pending: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_pending_protected: String,
    #[serde(default)]
    pub credential_updated_at: u64,
    #[serde(default)]
    pub device_id: String,
    #[serde(skip)]
    pub access_token: String,
    #[serde(skip)]
    pub access_token_expires_in: i64,
    #[serde(skip)]
    pub access_scope: String,
    #[serde(skip)]
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentResponse {
    pub id: String,
    pub credential: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub refresh_token_expires_in: i64,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub detail: Option<String>,
    pub payload: Option<Value>,
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub lease_id: String,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentTaskHistoryItem {
    pub id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub status: String,
    pub progress: i32,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct TaskCancelStatus {
    pub status: String,
    pub cancel_requested: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub instruction: String,
    pub status: String,
    pub created_by_user_id: String,
    pub runtime_provider: String,
    #[serde(default)]
    pub access_mode: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Deserialize)]
pub struct AgentRunClaim {
    pub run: AgentRun,
    pub claim_token: String,
    pub workspace_path: String,
    pub ai_model: String,
    #[serde(default)]
    pub access_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRunArtifactResponse {
    pub run_id: String,
    pub file_object_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: String,
    pub content_type: String,
    pub file_size: i64,
    pub sha256: String,
    pub scan_status: String,
}
