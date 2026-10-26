use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::oauth;
use crate::store::workbenches;
use crate::Options;

pub(crate) const WORKBENCH_PROVIDER_ID: &str = "com.himind.workbench.dashboard";

/// One derived view of the optional workbench provider.
///
/// `AgentMode` remains an enable preference for compatibility, but it is not a
/// sufficient availability signal. A provider is available only when there is
/// an active connection, a registered device identity and an unexpired account
/// authorization for that same identity.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ControlPlaneProviderStatus {
    pub provider_id: String,
    pub plugin_id: String,
    pub name: String,
    pub state: String,
    pub enabled_by_user: bool,
    pub configured: bool,
    pub registered: bool,
    pub authorized: bool,
    pub available: bool,
    pub worker_enabled: bool,
    pub reason: String,
    pub message: String,
    pub connection_id: String,
    pub display_name: String,
    pub api_base: String,
    pub agent_id: String,
    pub user_id: String,
    pub user_name: String,
    pub refresh_expires_at: u64,
}

pub(crate) fn status(options: &Options) -> ControlPlaneProviderStatus {
    let enabled_by_user = options.mode().dashboard_enabled();
    let store = workbenches::load(&options.state_path).ok().flatten();
    let active = store
        .as_ref()
        .and_then(workbenches::WorkbenchStore::active_connection);
    let configured = active.is_some();
    let registered = active.is_some_and(workbenches::WorkbenchConnection::registered);
    let agent_id = active
        .map(|connection| connection.identity.agent_id.clone())
        .unwrap_or_default();

    let mut state = "disabled";
    let mut reason = "provider_disabled";
    let mut message = "工作台未启用".to_string();
    let mut authorized = false;
    let mut refresh_expires_at = 0;
    let mut user_id = String::new();
    let mut user_name = String::new();

    if enabled_by_user {
        if !configured {
            state = "unconfigured";
            reason = "provider_not_configured";
            message = "尚未配置工作台连接".to_string();
        } else if !registered {
            state = "unregistered";
            reason = "agent_not_registered";
            message = "工作台连接尚未登记本机 Agent".to_string();
        } else {
            match oauth::authorization_snapshot(&options.state_path) {
                Err(_) => {
                    state = "invalid_authorization";
                    reason = "authorization_invalid";
                    message = "本机工作台授权记录不可用".to_string();
                }
                Ok(None) => {
                    state = "unauthorized";
                    reason = "account_not_authorized";
                    message = "工作台账号尚未授权".to_string();
                }
                Ok(Some(snapshot)) if snapshot.agent_id.trim() != agent_id.trim() => {
                    state = "authorization_mismatch";
                    reason = "authorization_mismatch";
                    message = "工作台授权与当前 Agent 身份不匹配".to_string();
                }
                Ok(Some(snapshot)) if snapshot.refresh_expires_at <= unix_now() => {
                    state = "expired";
                    reason = "authorization_expired";
                    message = "工作台授权已过期，请重新授权".to_string();
                    refresh_expires_at = snapshot.refresh_expires_at;
                    user_id = snapshot.user_id;
                    user_name = snapshot.display_name;
                }
                Ok(Some(snapshot)) => {
                    state = "available";
                    reason = "";
                    message = "工作台已连接".to_string();
                    authorized = true;
                    refresh_expires_at = snapshot.refresh_expires_at;
                    user_id = snapshot.user_id;
                    user_name = snapshot.display_name;
                }
            }
        }
    }

    let available = state == "available";
    if user_name.trim().is_empty() {
        user_name = active
            .map(|connection| connection.identity.user_name.clone())
            .unwrap_or_default();
    }
    let mut result = ControlPlaneProviderStatus {
        provider_id: "himind.dashboard".to_string(),
        plugin_id: WORKBENCH_PROVIDER_ID.to_string(),
        name: "AI 工作台".to_string(),
        state: state.to_string(),
        enabled_by_user,
        configured,
        registered,
        authorized,
        available,
        worker_enabled: available,
        reason: reason.to_string(),
        message,
        connection_id: active
            .map(|connection| connection.id.clone())
            .unwrap_or_default(),
        display_name: active
            .map(|connection| connection.display_name.clone())
            .unwrap_or_default(),
        api_base: active
            .map(|connection| connection.api_base.clone())
            .unwrap_or_else(|| options.api_base()),
        agent_id,
        user_id,
        user_name,
        refresh_expires_at,
    };
    #[cfg(test)]
    match options.provider_availability_override() {
        Some(true) => {
            result.state = "available".to_string();
            result.configured = true;
            result.registered = true;
            result.authorized = true;
            result.available = true;
            result.worker_enabled = true;
            result.reason.clear();
            result.message = "测试夹具：工作台可用".to_string();
        }
        Some(false) if result.enabled_by_user => {
            result.state = "unavailable_for_test".to_string();
            result.available = false;
            result.worker_enabled = false;
            result.reason = "provider_unavailable_for_test".to_string();
            result.message = "测试夹具：工作台不可用".to_string();
        }
        _ => {}
    }
    result
}

pub(crate) fn available(options: &Options) -> bool {
    status(options).available
}

pub(crate) fn unavailable_error(options: &Options) -> String {
    let provider = status(options);
    serde_json::json!({
        "code": "control_plane_required",
        "provider_id": provider.provider_id,
        "state": provider.state,
        "reason": provider.reason,
        "message": format!("此功能需要已连接的 AI 工作台：{}", provider.message),
    })
    .to_string()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{status, WORKBENCH_PROVIDER_ID};
    use crate::api::oauth::authorization_path;
    use crate::app::runtime_mode::AgentMode;
    use crate::store::workbenches;
    use std::fs;

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "himind-provider-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn options_for(state_path: &std::path::Path, mode: AgentMode) -> crate::Options {
        let mut options = crate::Options::from_env();
        options.state_path = state_path.to_path_buf();
        options.set_mode(mode);
        options.set_provider_availability_override(None);
        options
    }

    #[test]
    fn disabled_and_unconfigured_are_distinct_provider_states() {
        let root = test_root("states");
        let state_path = root.join("data/agent-state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();

        let disabled = status(&options_for(&state_path, AgentMode::Independent));
        assert_eq!(disabled.plugin_id, WORKBENCH_PROVIDER_ID);
        assert_eq!(disabled.state, "disabled");
        assert!(!disabled.available);

        let unconfigured = status(&options_for(&state_path, AgentMode::Connected));
        assert_eq!(unconfigured.state, "unconfigured");
        assert!(!unconfigured.available);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_unauthorized_and_available_are_derived_from_binding() {
        let root = test_root("available");
        let state_path = root.join("data/agent-state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        workbenches::ensure(&state_path, "http://127.0.0.1:18083").unwrap();

        let store_path = workbenches::path_for(&state_path);
        let mut store: serde_json::Value =
            serde_json::from_slice(&fs::read(&store_path).unwrap()).unwrap();
        store["connections"][0]["identity"]["agent_id"] = serde_json::json!("agt-test");
        fs::write(&store_path, serde_json::to_vec_pretty(&store).unwrap()).unwrap();

        let unauthorized = status(&options_for(&state_path, AgentMode::Connected));
        assert_eq!(unauthorized.state, "unauthorized");
        assert!(!unauthorized.authorized);

        let mismatched_authorization = serde_json::json!({
            "version": 1,
            "agent_id": "agt-other",
            "user_id": "usr-test",
            "scope": "agent.profile",
            "refresh_token_protected": "protected",
            "refresh_expires_at": super::unix_now() + 3600,
            "updated_at": super::unix_now()
        });
        fs::write(
            authorization_path(&state_path),
            serde_json::to_vec_pretty(&mismatched_authorization).unwrap(),
        )
        .unwrap();
        let mismatch = status(&options_for(&state_path, AgentMode::Connected));
        assert_eq!(mismatch.state, "authorization_mismatch");
        assert!(!mismatch.available);

        let authorization = serde_json::json!({
            "version": 1,
            "agent_id": "agt-test",
            "user_id": "usr-test",
            "scope": "agent.profile",
            "refresh_token_protected": "protected",
            "refresh_expires_at": super::unix_now() + 3600,
            "updated_at": super::unix_now(),
            "display_name": "测试用户"
        });
        fs::write(
            authorization_path(&state_path),
            serde_json::to_vec_pretty(&authorization).unwrap(),
        )
        .unwrap();

        let available = status(&options_for(&state_path, AgentMode::Connected));
        assert_eq!(available.state, "available");
        assert!(available.authorized);
        assert!(available.available);
        assert!(available.worker_enabled);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expired_authorization_is_unavailable_with_a_stable_reason() {
        let root = test_root("expired");
        let state_path = root.join("data/agent-state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        workbenches::ensure(&state_path, "http://127.0.0.1:18083").unwrap();
        let store_path = workbenches::path_for(&state_path);
        let mut store: serde_json::Value =
            serde_json::from_slice(&fs::read(&store_path).unwrap()).unwrap();
        store["connections"][0]["identity"]["agent_id"] = serde_json::json!("agt-test");
        fs::write(&store_path, serde_json::to_vec_pretty(&store).unwrap()).unwrap();
        let authorization = serde_json::json!({
            "version": 1,
            "agent_id": "agt-test",
            "user_id": "usr-test",
            "scope": "agent.profile",
            "refresh_token_protected": "protected",
            "refresh_expires_at": super::unix_now().saturating_sub(1),
            "updated_at": super::unix_now()
        });
        fs::write(
            authorization_path(&state_path),
            serde_json::to_vec_pretty(&authorization).unwrap(),
        )
        .unwrap();

        let expired = status(&options_for(&state_path, AgentMode::Connected));
        assert_eq!(expired.state, "expired");
        assert_eq!(expired.reason, "authorization_expired");
        assert!(!expired.available);
        assert!(!expired.worker_enabled);
        fs::remove_dir_all(root).ok();
    }
}
