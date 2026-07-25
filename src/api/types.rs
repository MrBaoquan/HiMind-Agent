use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Deserialize)]
pub struct TaskCancelStatus {
    pub status: String,
    pub cancel_requested: bool,
}
