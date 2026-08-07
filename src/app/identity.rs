use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::client::load_agent_state;
use crate::api::oauth;
use crate::app::system::open_url;
use crate::approval::manager::ApprovalManager;
use crate::Options;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DashboardIdentityStatus {
    pub state: String,
    pub authorized: bool,
    pub online_verified: bool,
    pub dashboard_base: String,
    pub user_name: String,
    pub user_id: String,
    pub agent_id: String,
    pub scopes: Vec<String>,
    pub refresh_expires_at: u64,
    pub last_verified_at: u64,
    pub svn_username: String,
    pub svn_provisioning_status: String,
    pub svn_provisioning_error: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DashboardAuthorizationProgress {
    pub state: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_at: u64,
    pub error: String,
    pub user_name: String,
    pub user_id: String,
}

impl Default for DashboardAuthorizationProgress {
    fn default() -> Self {
        Self {
            state: "idle".to_string(),
            user_code: String::new(),
            verification_uri: String::new(),
            verification_uri_complete: String::new(),
            expires_at: 0,
            error: String::new(),
            user_name: String::new(),
            user_id: String::new(),
        }
    }
}

#[derive(Default)]
pub(crate) struct DashboardAuthorizationFlow {
    generation: u64,
    progress: DashboardAuthorizationProgress,
}

pub(crate) fn identity_status(options: &Options) -> DashboardIdentityStatus {
    let agent_id = load_agent_state(&options.state_path)
        .map(|state| state.agent_id)
        .unwrap_or_default();
    let snapshot = match oauth::authorization_snapshot(&options.state_path) {
        Ok(value) => value,
        Err(error) => {
            return DashboardIdentityStatus {
                state: "invalid_local_authorization".to_string(),
                authorized: false,
                online_verified: false,
                dashboard_base: options.api_base.clone(),
                user_name: String::new(),
                user_id: String::new(),
                agent_id,
                scopes: Vec::new(),
                refresh_expires_at: 0,
                last_verified_at: 0,
                svn_username: String::new(),
                svn_provisioning_status: String::new(),
                svn_provisioning_error: String::new(),
                error: error.to_string(),
            }
        }
    };

    let Some(snapshot) = snapshot else {
        return DashboardIdentityStatus {
            state: if agent_id.is_empty() {
                "not_enrolled".to_string()
            } else {
                "not_authorized".to_string()
            },
            authorized: false,
            online_verified: false,
            dashboard_base: options.api_base.clone(),
            user_name: String::new(),
            user_id: String::new(),
            agent_id,
            scopes: Vec::new(),
            refresh_expires_at: 0,
            last_verified_at: 0,
            svn_username: String::new(),
            svn_provisioning_status: String::new(),
            svn_provisioning_error: String::new(),
            error: String::new(),
        };
    };

    if snapshot.refresh_expires_at <= unix_now() {
        return status_from_snapshot(
            options,
            snapshot,
            "expired",
            false,
            false,
            "Dashboard 授权已过期，请重新授权".to_string(),
        );
    }

    match oauth::fetch_user_info(options) {
        Ok(info) => {
            let _ = ensure_svn_credentials_for_identity(&info);
            DashboardIdentityStatus {
                state: if info.active {
                    "authorized"
                } else {
                    "disabled"
                }
                .to_string(),
                authorized: info.active,
                online_verified: true,
                dashboard_base: options.api_base.clone(),
                user_name: info.name,
                user_id: info.sub,
                agent_id: info.agent_id,
                scopes: split_scopes(&info.scope),
                refresh_expires_at: snapshot.refresh_expires_at,
                last_verified_at: unix_now(),
                svn_username: info.svn_username,
                svn_provisioning_status: info.svn_provisioning_status,
                svn_provisioning_error: info.svn_provisioning_error,
                error: if info.active {
                    String::new()
                } else {
                    "Dashboard 用户已停用".to_string()
                },
            }
        }
        Err(error) => {
            let message = error.to_string();
            let normalized = message.to_ascii_lowercase();
            let state = if normalized.contains("401")
                || normalized.contains("invalid_grant")
                || normalized.contains("invalid_token")
                || normalized.contains("different agent")
            {
                "requires_login"
            } else if normalized.contains("missing scope") {
                "insufficient_scope"
            } else {
                "dashboard_unavailable"
            };
            let authorized = state == "dashboard_unavailable";
            status_from_snapshot(options, snapshot, state, authorized, false, message)
        }
    }
}

pub(crate) fn authorization_progress(
    flow: &Arc<Mutex<DashboardAuthorizationFlow>>,
) -> DashboardAuthorizationProgress {
    flow.lock()
        .map(|state| state.progress.clone())
        .unwrap_or_else(|_| DashboardAuthorizationProgress {
            state: "failed".to_string(),
            error: "Dashboard 授权状态不可用".to_string(),
            ..DashboardAuthorizationProgress::default()
        })
}

pub(crate) fn start_authorization(
    options: Options,
    flow: Arc<Mutex<DashboardAuthorizationFlow>>,
    logs: Arc<ApprovalManager>,
) -> Result<DashboardAuthorizationProgress, String> {
    let generation = {
        let mut state = flow
            .lock()
            .map_err(|_| "Dashboard 授权状态不可用".to_string())?;
        if matches!(state.progress.state.as_str(), "starting" | "pending")
            && (state.progress.expires_at == 0 || state.progress.expires_at > unix_now())
        {
            return Ok(state.progress.clone());
        }
        state.generation = state.generation.saturating_add(1);
        state.progress = DashboardAuthorizationProgress {
            state: "starting".to_string(),
            ..DashboardAuthorizationProgress::default()
        };
        state.generation
    };

    let flow_for_thread = Arc::clone(&flow);
    thread::spawn(move || {
        let authorization = match oauth::begin_device_authorization(&options) {
            Ok(value) => value,
            Err(error) => {
                finish_with_error(&flow_for_thread, generation, "failed", &error.to_string());
                logs.add_log("error", &format!("Dashboard 授权启动失败: {error}"));
                return;
            }
        };
        {
            let Ok(mut state) = flow_for_thread.lock() else {
                return;
            };
            if state.generation != generation || state.progress.state == "canceled" {
                return;
            }
            state.progress = DashboardAuthorizationProgress {
                state: "pending".to_string(),
                user_code: authorization.user_code.clone(),
                verification_uri: authorization.verification_uri.clone(),
                verification_uri_complete: authorization.verification_uri_complete.clone(),
                expires_at: unix_now().saturating_add(authorization.expires_in.max(1) as u64),
                error: String::new(),
                user_name: String::new(),
                user_id: String::new(),
            };
        }
        let _ = open_url(&authorization.verification_uri_complete);
        logs.add_log("info", "已在浏览器中打开 Dashboard 账号授权页");

        let result =
            oauth::wait_for_device_authorization_with_cancel(&options, &authorization, || {
                flow_for_thread
                    .lock()
                    .map(|state| {
                        state.generation != generation || state.progress.state == "canceled"
                    })
                    .unwrap_or(true)
            });
        match result {
            Ok(access) => {
                let info = oauth::fetch_user_info(&options).ok();
                let svn_result = info.as_ref().map(ensure_svn_credentials_for_identity);
                let Ok(mut state) = flow_for_thread.lock() else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.progress = DashboardAuthorizationProgress {
                    state: "authorized".to_string(),
                    user_name: info
                        .as_ref()
                        .map(|value| value.name.clone())
                        .unwrap_or_default(),
                    user_id: access.user_id,
                    ..state.progress.clone()
                };
                drop(state);
                logs.add_log("info", "Dashboard 账号授权成功");
                match svn_result {
                    Some(Ok(true)) => logs.add_log("info", "已按 HiMind 姓名配置 SVN 账号"),
                    Some(Err(error)) => logs.add_log(
                        "error",
                        &format!("自动配置 SVN 账号失败，可在设置中重试: {error}"),
                    ),
                    _ => {}
                }
            }
            Err(error) => {
                let message = error.to_string();
                if message.contains("canceled") {
                    return;
                }
                let state = if message.contains("denied") {
                    "denied"
                } else if message.contains("expired") {
                    "expired"
                } else {
                    "failed"
                };
                finish_with_error(&flow_for_thread, generation, state, &message);
                logs.add_log("error", &format!("Dashboard 账号授权失败: {message}"));
            }
        }
    });

    Ok(authorization_progress(&flow))
}

fn ensure_svn_credentials_for_identity(
    info: &oauth::AgentUserInfo,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !info.active
        || info.svn_identity_status == "disabled"
        || info.svn_identity_status == "ambiguous"
        || info.svn_provisioning_status != "ready"
    {
        return Ok(false);
    }
    let username = if info.svn_username.trim().is_empty() {
        crate::svn::service::default_svn_username(&info.name)?
    } else {
        info.svn_username.trim().to_string()
    };
    crate::svn::service::ensure_default_svn_credentials(&username)
}

pub(crate) fn sync_svn_credentials(options: &Options) -> Result<bool, Box<dyn std::error::Error>> {
    if oauth::authorization_snapshot(&options.state_path)?.is_none() {
        return Ok(false);
    }
    let info = oauth::fetch_user_info(options)?;
    ensure_svn_credentials_for_identity(&info)
}

pub(crate) fn cancel_authorization(
    flow: &Arc<Mutex<DashboardAuthorizationFlow>>,
) -> Result<DashboardAuthorizationProgress, String> {
    let mut state = flow
        .lock()
        .map_err(|_| "Dashboard 授权状态不可用".to_string())?;
    state.progress.state = "canceled".to_string();
    state.progress.error.clear();
    Ok(state.progress.clone())
}

fn finish_with_error(
    flow: &Arc<Mutex<DashboardAuthorizationFlow>>,
    generation: u64,
    progress_state: &str,
    error: &str,
) {
    if let Ok(mut state) = flow.lock() {
        if state.generation == generation {
            state.progress.state = progress_state.to_string();
            state.progress.error = error.to_string();
        }
    }
}

fn status_from_snapshot(
    options: &Options,
    snapshot: oauth::AgentAuthorizationSnapshot,
    state: &str,
    authorized: bool,
    online_verified: bool,
    error: String,
) -> DashboardIdentityStatus {
    DashboardIdentityStatus {
        state: state.to_string(),
        authorized,
        online_verified,
        dashboard_base: options.api_base.clone(),
        user_name: snapshot.display_name,
        user_id: snapshot.user_id,
        agent_id: snapshot.agent_id,
        scopes: split_scopes(&snapshot.scope),
        refresh_expires_at: snapshot.refresh_expires_at,
        last_verified_at: snapshot.last_verified_at,
        svn_username: String::new(),
        svn_provisioning_status: String::new(),
        svn_provisioning_error: String::new(),
        error,
    }
}

fn split_scopes(scope: &str) -> Vec<String> {
    scope
        .split_whitespace()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
