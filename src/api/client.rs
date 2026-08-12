use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::{blocking::Client, StatusCode};
use rsa::rand_core::{OsRng, RngCore};
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::{
    AgentResponse, AgentRunClaim, AgentState, AgentTaskHistoryItem, RemoteExecutionReport,
    RuntimeInstallationReport, Task, TaskCancelStatus,
};
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};

#[derive(Debug, serde::Deserialize)]
pub struct LocalAgentTicketPrincipal {
    pub user_id: String,
    pub session_id_hash: String,
    pub agent_id: String,
    pub capability: String,
}

const TASK_CANCELED_ERROR: &str = "task canceled by user";
const AGENT_CREDENTIAL_ROTATION_INTERVAL: u64 = 30 * 24 * 60 * 60;

#[derive(Debug)]
pub struct TaskCancelGuard {
    last_checked: Option<Instant>,
    canceled: bool,
}

impl TaskCancelGuard {
    pub fn new() -> Self {
        Self {
            last_checked: None,
            canceled: false,
        }
    }

    pub fn check(
        &mut self,
        client: &Client,
        options: &crate::Options,
        agent_id: &str,
        task_id: &str,
    ) -> Result<(), Box<dyn Error>> {
        if self.canceled {
            return Err(TASK_CANCELED_ERROR.into());
        }
        if let Some(last_checked) = self.last_checked {
            if last_checked.elapsed() < Duration::from_secs(1) {
                return Ok(());
            }
        }
        self.last_checked = Some(Instant::now());
        let state = client
            .get(format!("{}/api/tasks/{}/cancel", options.api_base, task_id))
            .header(
                "Authorization",
                agent_authorization(agent_id, &options.agent_credential()),
            )
            .send()?
            .error_for_status()?
            .json::<TaskCancelStatus>()?;
        if state.cancel_requested || state.status == "canceled" || state.status == "canceling" {
            self.canceled = true;
            return Err(TASK_CANCELED_ERROR.into());
        }
        Ok(())
    }
}

pub fn is_task_canceled_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains(TASK_CANCELED_ERROR)
}

pub fn load_or_register(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    version: &str,
    enrollment_token: &str,
) -> Result<AgentState, Box<dyn Error>> {
    if state_path.exists() {
        let mut state = load_agent_state(state_path)?;
        let current_valid = !state.agent_id.trim().is_empty()
            && !state.credential.trim().is_empty()
            && matches!(
                heartbeat(client, api_base, &state.agent_id, &state.credential),
                Ok(true)
            );
        if !state.credential_pending.trim().is_empty() {
            if current_valid {
                state.credential_pending.clear();
                state.credential_pending_protected.clear();
                save_agent_state(state_path, &state)?;
                return Ok(state);
            }
            if matches!(
                heartbeat(client, api_base, &state.agent_id, &state.credential_pending),
                Ok(true)
            ) {
                state.credential = state.credential_pending.clone();
                state.credential_pending.clear();
                state.credential_pending_protected.clear();
                state.credential_updated_at = unix_now();
                save_agent_state(state_path, &state)?;
                return Ok(state);
            }
        } else if current_valid {
            return Ok(state);
        }
        if !enrollment_token.trim().is_empty() {
            return register_agent(client, api_base, state_path, version, enrollment_token);
        }
        return Err("stored Agent credential is no longer valid; an administrator must authorize a new enrollment".into());
    }

    register_agent(client, api_base, state_path, version, enrollment_token)
}

pub fn register_agent(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    version: &str,
    enrollment_token: &str,
) -> Result<AgentState, Box<dyn Error>> {
    if enrollment_token.trim().is_empty() {
        return Err("HIMIND_AGENT_ENROLLMENT_TOKEN is required for first enrollment".into());
    }
    let name = env::var("COMPUTERNAME").unwrap_or_else(|_| "windows-agent".to_string());
    let device_id = load_or_create_device_id(state_path)?;
    let response = client
        .post(format!("{}/api/agent/register", api_base))
        .json(&json!({
            "name": name,
            "device_id": device_id,
            "version": version,
            "os": env::consts::OS,
            "enrollment_token": enrollment_token,
        }))
        .send()?
        .error_for_status()?
        .json::<AgentResponse>()?;

    let token_response = if response.refresh_token.trim().is_empty() {
        None
    } else {
        Some(super::oauth::OAuthTokenResponse {
            access_token: response.access_token.clone(),
            token_type: response.token_type.clone(),
            expires_in: response.expires_in,
            refresh_token: response.refresh_token.clone(),
            refresh_token_expires_in: response.refresh_token_expires_in,
            scope: response.scope.clone(),
            user_id: response.user_id.clone(),
            agent_id: response.id.clone(),
        })
    };
    let state = AgentState {
        agent_id: response.id,
        credential: response.credential,
        credential_protected: String::new(),
        credential_pending: String::new(),
        credential_pending_protected: String::new(),
        credential_updated_at: unix_now(),
        device_id: if response.device_id.trim().is_empty() {
            device_id
        } else {
            response.device_id
        },
        access_token: response.access_token,
        access_token_expires_in: response.expires_in,
        access_scope: response.scope,
        user_id: response.user_id,
    };
    save_agent_state(state_path, &state)?;
    if let Some(token) = token_response.as_ref() {
        super::oauth::save_authorization_response(state_path, token)?;
    }
    Ok(state)
}

pub fn load_agent_state(state_path: &Path) -> Result<AgentState, Box<dyn Error>> {
    let content = fs::read_to_string(state_path)?;
    let mut state = serde_json::from_str::<AgentState>(&content)?;
    let missing_device_id = state.device_id.trim().is_empty();
    if missing_device_id {
        state.device_id = load_or_create_device_id(state_path)?;
    }
    if state.credential.trim().is_empty() && !state.credential_protected.trim().is_empty() {
        state.credential = unprotect_secret_for_current_user(&state.credential_protected)?;
    }
    if state.credential_pending.trim().is_empty()
        && !state.credential_pending_protected.trim().is_empty()
    {
        state.credential_pending =
            unprotect_secret_for_current_user(&state.credential_pending_protected)?;
    }
    if state.agent_id.trim().is_empty() || state.credential.trim().is_empty() {
        return Err("stored Agent identity is incomplete".into());
    }
    let needs_migration = missing_device_id
        || state.credential_protected.trim().is_empty()
        || state.credential_updated_at == 0;
    if state.credential_updated_at == 0 {
        state.credential_updated_at = unix_now();
    }
    if needs_migration {
        save_agent_state(state_path, &state)?;
    }
    Ok(state)
}

pub fn save_agent_state(state_path: &Path, state: &AgentState) -> Result<(), Box<dyn Error>> {
    let mut stored = state.clone();
    stored.credential_protected = protect_secret_for_current_user(&state.credential)?;
    stored.credential_pending_protected = if state.credential_pending.trim().is_empty() {
        String::new()
    } else {
        protect_secret_for_current_user(&state.credential_pending)?
    };
    if stored.credential_updated_at == 0 {
        stored.credential_updated_at = unix_now();
    }
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = state_path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&stored)?)?;
    if state_path.exists() {
        fs::remove_file(state_path)?;
    }
    fs::rename(temporary, state_path)?;
    Ok(())
}

pub fn agent_credential_rotation_due(state: &AgentState) -> bool {
    state.credential_updated_at > 0
        && unix_now().saturating_sub(state.credential_updated_at)
            >= AGENT_CREDENTIAL_ROTATION_INTERVAL
}

pub fn rotate_agent_credential(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    state: &AgentState,
) -> Result<AgentState, Box<dyn Error>> {
    let mut random = [0_u8; 48];
    OsRng.fill_bytes(&mut random);
    let next_credential = URL_SAFE_NO_PAD.encode(random);

    let mut staged = state.clone();
    staged.credential_pending = next_credential.clone();
    save_agent_state(state_path, &staged)?;

    #[derive(serde::Deserialize)]
    struct RotationResponse {
        agent_id: String,
        rotated: bool,
    }
    let response = client
        .post(format!("{}/api/agent/credential/rotate", api_base))
        .header(
            "Authorization",
            agent_authorization(&state.agent_id, &state.credential),
        )
        .json(&json!({ "credential": next_credential }))
        .send()?
        .error_for_status()?
        .json::<RotationResponse>()?;
    if !response.rotated || response.agent_id != state.agent_id {
        return Err("Dashboard returned an invalid Agent credential rotation response".into());
    }

    staged.credential = staged.credential_pending.clone();
    staged.credential_pending.clear();
    staged.credential_pending_protected.clear();
    staged.credential_updated_at = unix_now();
    save_agent_state(state_path, &staged)?;
    Ok(staged)
}

fn load_or_create_device_id(state_path: &Path) -> Result<String, Box<dyn Error>> {
    let device_path = state_path.with_extension("device-id");
    if let Ok(value) = fs::read_to_string(&device_path) {
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
    }
    if state_path.exists() {
        if let Ok(content) = fs::read_to_string(state_path) {
            if let Ok(state) = serde_json::from_str::<AgentState>(&content) {
                if !state.device_id.trim().is_empty() {
                    fs::write(&device_path, state.device_id.trim())?;
                    return Ok(state.device_id);
                }
            }
        }
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let device_id = format!("dev-{}-{:x}", env::consts::OS, nanos);
    fs::write(device_path, &device_id)?;
    Ok(device_id)
}

pub fn heartbeat(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
) -> Result<bool, Box<dyn Error>> {
    heartbeat_with_runtime_installations(client, api_base, agent_id, credential, None, None)
}

pub fn heartbeat_with_runtime_installations(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    runtime_installations: Option<&[RuntimeInstallationReport]>,
    remote_execution: Option<&RemoteExecutionReport>,
) -> Result<bool, Box<dyn Error>> {
    let mut payload = json!({
        "agent_id": agent_id,
        "status": "online",
        "svn_admin_ready": crate::svn::service::svn_admin_ready(),
    });
    if let Some(items) = runtime_installations {
        payload["runtime_installations"] = serde_json::to_value(items)?;
    }
    if let Some(settings) = remote_execution {
        payload["remote_execution"] = serde_json::to_value(settings)?;
    }
    let response = client
        .post(format!("{}/api/agent/heartbeat", api_base))
        .json(&payload)
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND | StatusCode::GONE
    ) {
        return Ok(false);
    }
    response.error_for_status()?;
    Ok(true)
}

pub fn poll_tasks(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
) -> Result<Vec<Task>, Box<dyn Error>> {
    let tasks = client
        .get(format!("{}/api/agent/tasks/poll", api_base))
        .query(&[("agent_id", agent_id), ("wait", "25")])
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?
        .json::<Vec<Task>>()?;
    Ok(tasks)
}

pub fn list_task_history(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    limit: usize,
) -> Result<Vec<AgentTaskHistoryItem>, Box<dyn Error>> {
    let tasks = client
        .get(format!("{}/api/agent/tasks/history", api_base))
        .query(&[("limit", limit.to_string())])
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?
        .json::<Vec<AgentTaskHistoryItem>>()?;
    Ok(tasks)
}

pub fn report_task(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    task_id: &str,
    status: &str,
    progress: i32,
    detail: &str,
    result: Option<Value>,
    error: Option<String>,
    execution_id: &str,
    lease_id: &str,
    credential: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!("{}/api/tasks/{}/report", api_base, task_id))
        .json(&json!({
            "agent_id": agent_id,
            "status": status,
            "progress": progress,
            "detail": detail,
            "result": result.unwrap_or_else(|| json!({})),
            "error": error.unwrap_or_default(),
            "execution_id": execution_id,
            "lease_id": lease_id,
        }))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?;
    Ok(())
}

pub fn renew_task_lease(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    task_id: &str,
    execution_id: &str,
    lease_id: &str,
    credential: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!(
            "{}/api/agent/tasks/{}/lease/renew",
            api_base, task_id
        ))
        .json(&json!({
            "agent_id": agent_id,
            "execution_id": execution_id,
            "lease_id": lease_id,
        }))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?;
    Ok(())
}

pub fn claim_agent_run(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    task_id: &str,
    run_id: &str,
    credential: &str,
) -> Result<AgentRunClaim, Box<dyn Error>> {
    let response = client
        .post(format!("{}/api/agent/runs/{}/claim", api_base, run_id))
        .json(&json!({"agent_id": agent_id, "task_id": task_id}))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "Agent Run 领取失败（HTTP {}）：{}",
            status.as_u16(),
            bounded_response_detail(&detail)
        )
        .into());
    }
    Ok(response.json::<AgentRunClaim>()?)
}

pub fn update_agent_run_status(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    run_id: &str,
    claim_token: &str,
    status: &str,
    result: Option<&Value>,
    error: &str,
    credential: &str,
) -> Result<(), Box<dyn Error>> {
    let response = client
        .post(format!("{}/api/agent/runs/{}/status", api_base, run_id))
        .json(&json!({
            "agent_id": agent_id,
            "claim_token": claim_token,
            "status": status,
            "result": result.cloned().unwrap_or(Value::Null),
            "error": error,
        }))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?;
    if !response.status().is_success() {
        let http_status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "Agent Run 状态回报失败（HTTP {}）：{}",
            http_status.as_u16(),
            bounded_response_detail(&detail)
        )
        .into());
    }
    Ok(())
}

pub fn renew_agent_run_lease(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    run_id: &str,
    claim_token: &str,
    credential: &str,
) -> Result<(), Box<dyn Error>> {
    let response = client
        .post(format!(
            "{}/api/agent/runs/{}/lease/renew",
            api_base, run_id
        ))
        .json(&json!({"agent_id": agent_id, "claim_token": claim_token}))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "Agent Run 租约续期失败（HTTP {}）：{}",
            status.as_u16(),
            bounded_response_detail(&detail)
        )
        .into());
    }
    Ok(())
}

fn bounded_response_detail(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "服务未返回错误详情".to_string();
    }
    value.chars().take(1000).collect()
}

pub fn verify_local_agent_ticket(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    ticket: &str,
    capability: &str,
    credential: &str,
) -> Result<LocalAgentTicketPrincipal, Box<dyn Error>> {
    let principal = client
        .post(format!("{}/api/agent/local-ticket/verify", api_base))
        .json(&json!({ "ticket": ticket, "agent_id": agent_id, "capability": capability }))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?
        .json::<LocalAgentTicketPrincipal>()?;
    if principal.user_id.trim().is_empty()
        || principal.session_id_hash.trim().is_empty()
        || principal.agent_id != agent_id
        || principal.capability != capability
    {
        return Err("local Agent ticket principal is invalid".into());
    }
    Ok(principal)
}

fn agent_authorization(agent_id: &str, credential: &str) -> String {
    format!("Agent {agent_id}:{credential}")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{load_agent_state, save_agent_state, unix_now};
    use crate::api::types::AgentState;
    use std::fs;
    use std::path::PathBuf;

    fn test_state_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-agent-{name}-{}-{}.json",
            std::process::id(),
            unix_now()
        ))
    }

    fn test_state(credential: &str) -> AgentState {
        AgentState {
            agent_id: "agt-test".to_string(),
            credential: credential.to_string(),
            credential_protected: String::new(),
            credential_pending: String::new(),
            credential_pending_protected: String::new(),
            credential_updated_at: unix_now(),
            device_id: "device-test".to_string(),
            access_token: String::new(),
            access_token_expires_in: 0,
            access_scope: String::new(),
            user_id: String::new(),
        }
    }

    #[test]
    fn agent_state_never_serializes_plaintext_credentials() {
        let path = test_state_path("protected-state");
        let credential = "device-credential-that-must-never-be-plaintext";
        let pending = "pending-credential-that-must-never-be-plaintext";
        let mut state = test_state(credential);
        state.credential_pending = pending.to_string();

        save_agent_state(&path, &state).expect("save protected Agent state");
        let raw = fs::read_to_string(&path).expect("read Agent state");
        assert!(!raw.contains(credential));
        assert!(!raw.contains(pending));
        assert!(!raw.contains("\"credential\""));
        assert!(raw.contains("credential_protected"));
        assert!(raw.contains("credential_pending_protected"));

        let loaded = load_agent_state(&path).expect("load protected Agent state");
        assert_eq!(loaded.credential, credential);
        assert_eq!(loaded.credential_pending, pending);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_plaintext_agent_state_is_migrated_on_read() {
        let path = test_state_path("legacy-state");
        let credential = "legacy-device-credential-value";
        fs::write(
            &path,
            format!(
                r#"{{"agent_id":"agt-legacy","credential":"{credential}","device_id":"device-legacy"}}"#
            ),
        )
        .expect("write legacy Agent state");

        let loaded = load_agent_state(&path).expect("migrate legacy Agent state");
        assert_eq!(loaded.credential, credential);
        let migrated = fs::read_to_string(&path).expect("read migrated Agent state");
        assert!(!migrated.contains(credential));
        assert!(migrated.contains("credential_protected"));
        assert!(loaded.credential_updated_at > 0);
        let _ = fs::remove_file(path);
    }
}
