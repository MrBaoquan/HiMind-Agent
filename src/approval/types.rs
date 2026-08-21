use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Manual,
    AutoApprove,
    AutoDeny,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    RemoteConnect,
    UploadCode,
    UploadPlaceholder,
}

impl RequestType {
    pub fn default_mode(&self) -> ApprovalMode {
        match self {
            // Connecting to a recorded remote endpoint is an explicit user action
            // from the operations workbench; do not block it on a second approval.
            Self::RemoteConnect => ApprovalMode::AutoApprove,
            Self::UploadCode => ApprovalMode::AutoApprove,
            Self::UploadPlaceholder => ApprovalMode::AutoApprove,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub request_type: String,
    pub title: String,
    pub description: String,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub remaining_seconds: u64,
    pub created_at: String,
}

#[derive(Debug)]
pub struct PendingApproval {
    pub request: ApprovalRequest,
    pub respond_tx: mpsc::Sender<bool>,
    pub created: Instant,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSettings {
    pub rules: HashMap<String, String>,
    pub timeout_seconds: u64,
    pub auto_start: bool,
}

#[cfg(test)]
mod tests {
    use super::{ApprovalMode, RequestType};

    #[test]
    fn remote_connect_is_auto_approved_by_default() {
        assert!(matches!(
            RequestType::RemoteConnect.default_mode(),
            ApprovalMode::AutoApprove
        ));
    }
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        let mut rules = HashMap::new();
        rules.insert("remote_connect".to_string(), "auto_approve".to_string());
        rules.insert("upload_code".to_string(), "auto_approve".to_string());
        rules.insert("upload_placeholder".to_string(), "auto_approve".to_string());
        Self {
            rules,
            timeout_seconds: 30,
            auto_start: false,
        }
    }
}
