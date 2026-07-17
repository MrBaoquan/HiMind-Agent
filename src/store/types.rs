use serde::{Deserialize, Serialize};

#[derive(Debug, Default)]
pub struct LocalWorkerStatus {
    pub dashboard_worker_online: bool,
    pub dashboard_agent_id: String,
    pub dashboard_worker_error: String,
    pub local_service_online: bool,
    pub local_service_error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredInnerAdminCredentials {
    pub username: String,
    pub encrypted_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSvnConnection {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub username: String,
    pub encrypted_password: String,
    pub provider: String,
    #[serde(default = "default_svn_connection_status")]
    pub status: String,
    #[serde(default)]
    pub last_error: String,
}

fn default_svn_connection_status() -> String {
    "configured".to_string()
}

#[cfg(test)]
mod tests {
    use super::StoredSvnConnection;

    #[test]
    fn legacy_connection_defaults_to_configured() {
        let value: StoredSvnConnection = serde_json::from_str(
            r#"{"id":"company-svn","name":"公司 SVN","base_url":"http://svn.andcrane.com/repo","username":"user","encrypted_password":"secret","provider":"svn"}"#,
        )
        .unwrap();
        assert_eq!(value.status, "configured");
        assert!(value.last_error.is_empty());
    }
}
