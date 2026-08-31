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
    /// Destructive local or business action. Never auto-approvable.
    DestructiveAction,
    FilesystemDelete,
    BusinessProjectDelete,
    BusinessExhibitDelete,
}

impl RequestType {
    pub fn default_mode(&self) -> ApprovalMode {
        match self {
            // Connecting to a recorded remote endpoint is an explicit user action
            // from the operations workbench; do not block it on a second approval.
            Self::RemoteConnect => ApprovalMode::AutoApprove,
            Self::UploadCode => ApprovalMode::AutoApprove,
            Self::UploadPlaceholder => ApprovalMode::AutoApprove,
            Self::DestructiveAction
            | Self::FilesystemDelete
            | Self::BusinessProjectDelete
            | Self::BusinessExhibitDelete => ApprovalMode::Manual,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::RemoteConnect => "remote_connect",
            Self::UploadCode => "upload_code",
            Self::UploadPlaceholder => "upload_placeholder",
            Self::DestructiveAction => "destructive_action",
            Self::FilesystemDelete => "filesystem.delete",
            Self::BusinessProjectDelete => "business.project.delete",
            Self::BusinessExhibitDelete => "business.exhibit.delete",
        }
    }

    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::DestructiveAction
                | Self::FilesystemDelete
                | Self::BusinessProjectDelete
                | Self::BusinessExhibitDelete
        )
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
    #[serde(default)]
    pub created_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalFactStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalFact {
    pub schema_version: u32,
    pub id: String,
    pub request_type: String,
    pub title: String,
    pub description: String,
    pub status: ApprovalFactStatus,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    #[serde(default)]
    pub resolved_at_unix: u64,
    #[serde(default)]
    pub resolution_reason: String,
    #[serde(default)]
    pub owner_instance_id: String,
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
    /// Baseline interaction policy. Explicit capability rules continue to
    /// take precedence, while the profile supplies the default for requests
    /// that do not have a rule.
    #[serde(default = "default_approval_profile")]
    pub profile: String,
    /// Controls whether pending requests open the always-on-top popup.
    #[serde(default = "default_notification_mode")]
    pub notification_mode: String,
    /// Dashboard identity that last confirmed the local approval posture.
    /// Empty values are valid for independent/local-only Agent mode.
    #[serde(default)]
    pub owner_user_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub binding_updated_at: u64,
    /// Unix timestamp of the last explicit R3 self-risk acknowledgement.
    #[serde(default)]
    pub risk_acknowledged_at: u64,
}

pub fn default_approval_profile() -> String {
    "balanced".to_string()
}

pub fn default_notification_mode() -> String {
    "popup".to_string()
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
            profile: default_approval_profile(),
            notification_mode: default_notification_mode(),
            owner_user_id: String::new(),
            agent_id: String::new(),
            binding_updated_at: 0,
            risk_acknowledged_at: 0,
        }
    }
}
