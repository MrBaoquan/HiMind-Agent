use reqwest::{blocking::Client, StatusCode};
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use super::types::{AgentResponse, AgentState, Task, TaskCancelStatus};

const TASK_CANCELED_ERROR: &str = "task canceled by user";

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
        let content = fs::read_to_string(state_path)?;
        let state = serde_json::from_str::<AgentState>(&content)?;
        if !state.agent_id.trim().is_empty()
            && !state.credential.trim().is_empty()
            && matches!(
                heartbeat(client, api_base, &state.agent_id, &state.credential),
                Ok(true)
            )
        {
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
        return Err(
            "PROJECT_DASHBOARD_AGENT_ENROLLMENT_TOKEN is required for first enrollment".into(),
        );
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

    let state = AgentState {
        agent_id: response.id,
        credential: response.credential,
        device_id: if response.device_id.trim().is_empty() {
            device_id
        } else {
            response.device_id
        },
    };
    fs::write(state_path, serde_json::to_string_pretty(&state)?)?;
    Ok(state)
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
    use std::time::{SystemTime, UNIX_EPOCH};
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
    let response = client
        .post(format!("{}/api/agent/heartbeat", api_base))
        .json(&json!({
            "agent_id": agent_id,
            "status": "online",
        }))
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

pub fn verify_local_agent_ticket(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    ticket: &str,
    capability: &str,
    credential: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!("{}/api/agent/local-ticket/verify", api_base))
        .json(&json!({ "ticket": ticket, "agent_id": agent_id, "capability": capability }))
        .header("Authorization", agent_authorization(agent_id, credential))
        .send()?
        .error_for_status()?;
    Ok(())
}

fn agent_authorization(agent_id: &str, credential: &str) -> String {
    format!("Agent {agent_id}:{credential}")
}
