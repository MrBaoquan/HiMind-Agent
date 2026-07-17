use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: String,
    #[serde(default)]
    pub credential: String,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentResponse {
    pub id: String,
    pub credential: String,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub detail: Option<String>,
    pub payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TaskCancelStatus {
    pub status: String,
    pub cancel_requested: bool,
}
