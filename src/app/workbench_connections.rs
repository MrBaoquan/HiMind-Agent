//! Workbench connection commands (ADR 0008).
//!
//! [`crate::store::workbenches`] owns the data. This module owns the live side
//! effects a user action must trigger, in the order that keeps identity and
//! address together:
//!
//! * switching flips the store, moves the identity on disk, repoints
//!   [`Options::api_base`], drops the in-memory credential and stops the
//!   sessions that were minted against the previous workbench;
//! * enrolling a new workbench probes it first, registers an Agent identity
//!   against it, and restores the previous connection when registration fails.
//!
//! Errors are plain strings the UI can show, except for one the UI must branch
//! on: a message starting with [`WORKBENCH_BUSY`] means work is in flight and
//! the user has to confirm before the switch happens.

use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

use super::commands::AgentState;
use crate::api::client::register_agent;
use crate::api::types::AgentState as StoredAgentState;
use crate::store::workbenches::{self, WorkbenchConnectionView};

/// Prefix of the error returned when a task is executing and `force` was false.
pub(crate) const WORKBENCH_BUSY: &str = "WORKBENCH_BUSY";

const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkbenchProbe {
    pub api_base: String,
    pub reachable: bool,
    pub status: u16,
    pub message: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkbenchConnectionsSnapshot {
    pub api_base: String,
    pub connections: Vec<WorkbenchConnectionView>,
}

/// The store, creating the first record from the legacy files when this install
/// predates ADR 0008. Reads the store as-is otherwise: listing must not repair.
fn store_of(state: &AgentState) -> Result<workbenches::WorkbenchStore, String> {
    if let Some(store) = workbenches::load(&state.state_path).map_err(|error| error.to_string())? {
        return Ok(store);
    }
    let fallback = state.options.api_base();
    workbenches::ensure(&state.state_path, &fallback).map_err(|error| error.to_string())
}

fn snapshot(state: &AgentState) -> Result<WorkbenchConnectionsSnapshot, String> {
    let store = store_of(state)?;
    Ok(WorkbenchConnectionsSnapshot {
        api_base: state.options.api_base(),
        connections: store.views(),
    })
}

#[tauri::command]
pub(crate) fn list_workbench_connections(
    state: State<'_, AgentState>,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    snapshot(&state)
}

#[tauri::command]
pub(crate) fn add_workbench_connection(
    state: State<'_, AgentState>,
    api_base: String,
    display_name: String,
    purpose: String,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    store_of(&state)?;
    let connection = workbenches::add(&state.state_path, &api_base, &display_name, &purpose)
        .map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已添加工作台 {}", connection.display_name));
    snapshot(&state)
}

#[tauri::command]
pub(crate) fn rename_workbench_connection(
    state: State<'_, AgentState>,
    id: String,
    display_name: String,
    purpose: String,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    workbenches::rename(&state.state_path, &id, &display_name, &purpose)
        .map_err(|error| error.to_string())?;
    snapshot(&state)
}

#[tauri::command]
pub(crate) fn remove_workbench_connection(
    state: State<'_, AgentState>,
    id: String,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    let store = store_of(&state)?;
    let name = store
        .connections()
        .iter()
        .find(|connection| connection.id == id)
        .map(|connection| connection.display_name.clone())
        .unwrap_or_else(|| id.clone());
    workbenches::remove(&state.state_path, &id).map_err(|error| error.to_string())?;
    state
        .approval_manager
        .add_log("info", &format!("已移除工作台 {name}"));
    snapshot(&state)
}

/// Probe a candidate address without touching the store.
///
/// `/api/health` is public on the Dashboard, so this never assumes the address
/// is reachable *and* never depends on a credential from another workbench.
#[tauri::command]
pub(crate) async fn probe_workbench_connection(api_base: String) -> Result<WorkbenchProbe, String> {
    let target = api_base.trim().trim_end_matches('/').to_string();
    if target.is_empty() {
        return Err("请填写工作台地址".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || probe(&target))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn switch_workbench_connection(
    state: State<'_, AgentState>,
    id: String,
    force: bool,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    let store = store_of(&state)?;
    if !store
        .connections()
        .iter()
        .any(|connection| connection.id == id)
    {
        return Err(format!("未知的工作台连接: {id}"));
    }
    if store.active_connection_id() == id {
        return snapshot(&state);
    }
    if !force {
        if let Some((task_id, task_type, _, _)) = state.options.task_execution() {
            return Err(format!(
                "{WORKBENCH_BUSY}: 任务 {task_id}（{task_type}）正在执行，切换工作台会中断它的连接"
            ));
        }
    }
    activate(&state, &id)?;
    snapshot(&state)
}

/// Pair with a workbench: probe, register an Agent identity against it, then
/// keep it active.
///
/// Registration needs an enrollment token that only that workbench can mint,
/// so a device authorization alone can never bootstrap a connection (ADR 0008).
/// Failure restores the previous connection, including its materialised files.
#[tauri::command]
pub(crate) async fn enroll_workbench_connection(
    state: State<'_, AgentState>,
    id: String,
    enrollment_token: String,
) -> Result<WorkbenchConnectionsSnapshot, String> {
    let token = enrollment_token.trim().to_string();
    if token.is_empty() {
        return Err("请填写工作台提供的登记码".to_string());
    }
    let options = state.options.clone();
    let state_path = state.state_path.clone();
    let worker_status = Arc::clone(&state.worker_status);
    let logs = Arc::clone(&state.approval_manager);

    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let store = workbenches::load(&state_path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "工作台连接尚未初始化".to_string())?;
        let connection = store
            .connections()
            .iter()
            .find(|connection| connection.id == id)
            .cloned()
            .ok_or_else(|| format!("未知的工作台连接: {id}"))?;
        let previous_active = store.active_connection_id().to_string();

        let reachability = probe(&connection.api_base)?;
        if !reachability.reachable {
            return Err(reachability.message);
        }

        let active = workbenches::switch(&state_path, &id).map_err(|error| error.to_string())?;
        options.set_api_base(&active.api_base);
        options.adopt_connection();
        set_worker_status(&worker_status, &active, options.mode().dashboard_enabled());

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        let registered: StoredAgentState = match register_agent(
            &client,
            &active.api_base,
            &state_path,
            crate::VERSION,
            &token,
        ) {
            Ok(registered) => registered,
            Err(error) => {
                restore(&state_path, &previous_active, &options);
                set_worker_status(
                    &worker_status,
                    &workbenches::active_connection(&state_path).unwrap_or(active),
                    options.mode().dashboard_enabled(),
                );
                return Err(enrollment_failure(error.as_ref()));
            }
        };

        options.set_agent_credential(&registered.credential);
        crate::api::oauth::cache_registration_access(&options, &registered);
        crate::app::runtime_mode::save(&state_path, crate::app::runtime_mode::AgentMode::Connected)
            .map_err(|error| error.to_string())?;
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        workbenches::capture_active_quiet(&state_path);
        set_worker_status(&worker_status, &active, true);
        logs.add_log(
            "info",
            &format!(
                "已登记工作台 {}（Agent {}）",
                active.display_name, registered.agent_id
            ),
        );
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())??;

    snapshot(&state)
}

/// Make `id` active everywhere: store, address, credential cache and sessions.
fn activate(state: &AgentState, id: &str) -> Result<(), String> {
    let connection =
        workbenches::switch(&state.state_path, id).map_err(|error| error.to_string())?;
    state.options.set_api_base(&connection.api_base);
    state.options.adopt_connection();
    // Sessions and approvals were minted for the previous workbench's account.
    crate::app::ui::stop_builtin_ai_process();
    state
        .approval_manager
        .clear_identity()
        .map_err(|error| error.to_string())?;
    set_worker_status(
        &state.worker_status,
        &connection,
        state.options.mode().dashboard_enabled(),
    );
    state.approval_manager.add_log(
        "info",
        &format!("已切换到工作台 {}", connection.display_name),
    );
    Ok(())
}

/// Enrollment failures are shown inside the HiMind account card, so the raw
/// transport text (`HTTP status client error (401 Unauthorized) for url ...`)
/// is not an acceptable message: it names neither the cause nor the next step.
/// Translate the status the workbench returned into the action the user can take.
fn enrollment_failure(error: &(dyn std::error::Error + 'static)) -> String {
    if let Some(http) = error.downcast_ref::<reqwest::Error>() {
        if let Some(status) = http.status() {
            return match status.as_u16() {
                401 | 403 => "登记码无效或已过期，请在 HiMind 工作台重新生成后重试。".to_string(),
                404 => "这个地址不是可用的 HiMind 工作台，请检查工作台地址。".to_string(),
                409 => "这台设备已在工作台登记过，请在工作台确认设备状态。".to_string(),
                code if code >= 500 => "工作台暂时无法处理登记，请稍后重试。".to_string(),
                code => format!("工作台未受理这次登记（HTTP {code}），请确认登记码后重试。"),
            };
        }
        if http.is_timeout() {
            return "登记超时，请检查网络和工作台地址后重试。".to_string();
        }
        if http.is_connect() {
            return "无法连接该工作台，请检查地址和网络后重试。".to_string();
        }
    }
    format!("登记失败：{error}")
}

/// Put a switch back the way it was when registration fails.
fn restore(state_path: &std::path::Path, previous_active: &str, options: &crate::Options) {
    if previous_active.is_empty() {
        return;
    }
    if workbenches::switch(state_path, previous_active).is_err() {
        return;
    }
    if let Some(previous) = workbenches::active_connection(state_path) {
        options.set_api_base(&previous.api_base);
    }
    options.adopt_connection();
}

fn set_worker_status(
    status: &Arc<std::sync::Mutex<crate::store::types::LocalWorkerStatus>>,
    connection: &workbenches::WorkbenchConnection,
    dashboard_enabled: bool,
) {
    let Ok(mut status) = status.lock() else {
        return;
    };
    status.dashboard_worker_online = false;
    // Never keep showing the previous workbench's agent id.
    status.dashboard_agent_id = connection.identity.agent_id.clone();
    status.dashboard_worker_error.clear();
    if !dashboard_enabled {
        status.dashboard_worker_state = "not_applicable".to_string();
        status.dashboard_worker_reason_code = "worker_not_managed".to_string();
        return;
    }
    if !connection.registered() {
        status.dashboard_worker_state = "offline".to_string();
        status.dashboard_worker_reason_code = "workbench_connection_not_enrolled".to_string();
        status.dashboard_worker_error = "该工作台还没有 Agent 身份，请先完成登记".to_string();
        return;
    }
    status.dashboard_worker_state = "connecting".to_string();
    status.dashboard_worker_reason_code = "connected_agent_app_starting".to_string();
}

fn probe(api_base: &str) -> Result<WorkbenchProbe, String> {
    let api_base = api_base.trim().trim_end_matches('/').to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{api_base}/api/health");
    let unreachable = |message: String| WorkbenchProbe {
        api_base: api_base.clone(),
        reachable: false,
        status: 0,
        message,
        version: String::new(),
    };
    let response = match client.get(&url).send() {
        Ok(response) => response,
        Err(error) => return Ok(unreachable(format!("无法连接工作台：{error}"))),
    };
    let http_status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .unwrap_or(serde_json::Value::Null);
    let version = body
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !http_status.is_success() {
        return Ok(WorkbenchProbe {
            api_base,
            reachable: false,
            status: http_status.as_u16(),
            message: format!("工作台返回 HTTP {}", http_status.as_u16()),
            version: String::new(),
        });
    }
    Ok(WorkbenchProbe {
        api_base,
        reachable: true,
        status: http_status.as_u16(),
        message: if version.is_empty() {
            "工作台可用".to_string()
        } else {
            format!("工作台可用（{version}）")
        },
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::probe;

    #[test]
    fn probe_reports_a_bad_address_as_data_instead_of_failing() {
        // Port 1 is reserved and never served. A bad address must come back as
        // data the UI can render, never as a command error, so the "添加工作台"
        // dialog can explain itself. The status code is deliberately not
        // asserted: a machine with a system proxy answers 502 here, and both
        // outcomes mean the same thing to the caller.
        let result = probe("http://127.0.0.1:1").unwrap();
        assert!(!result.reachable);
        assert!(!result.message.is_empty());
        assert_eq!(result.api_base, "http://127.0.0.1:1");
    }
}
