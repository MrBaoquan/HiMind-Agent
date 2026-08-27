//! Shared MCP target contract used by every client adapter and presentation layer.

use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpTargetDescriptor {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub detected: bool,
    pub detection_message: String,
    pub config_path: String,
    pub config_directory: String,
    pub config_format: String,
    pub state: String,
    pub supported_transports: Vec<String>,
    pub supports_auto_configure: bool,
    pub supports_skills: bool,
    pub skill_client_id: String,
    pub skill_client_name: String,
    pub restart_required: bool,
    pub manual_snippet: String,
    pub config_preview: String,
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpTargetOperationResult {
    pub target: McpTargetDescriptor,
    pub changed: bool,
    pub backup_path: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpTargetBatchFailure {
    pub target_id: String,
    pub target_name: String,
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpTargetBatchResult {
    pub results: Vec<McpTargetOperationResult>,
    pub failures: Vec<McpTargetBatchFailure>,
    pub skipped_target_ids: Vec<String>,
}

pub(crate) fn manual_snippet(command: &str, args: &[String], target_id: &str) -> String {
    let servers_key = if target_id == "vscode" {
        "servers"
    } else {
        "mcpServers"
    };
    let mut server = json!({
        "command": command,
        "args": args,
        "env": {
            "HIMIND_AI_CLIENT_ID": target_id,
            "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
        }
    });
    if target_id == "vscode" {
        server["type"] = json!("stdio");
    }
    let payload = json!({
        servers_key: {
            super::mcp_registry::AGENT_SERVER_ID: server
        }
    });
    serde_json::to_string_pretty(&json!({
        "target": target_id,
        "configuration": payload
    }))
    .unwrap_or_default()
}
