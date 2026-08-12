use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;

use crate::api::client::load_agent_state;
use crate::api::types::AgentState;
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};
use crate::Options;

pub(crate) const PROFILE_SCOPE: &str = "agent.profile";
pub(crate) const BUSINESS_CONTEXT_READ_SCOPE: &str = "business.context.read";
pub(crate) const KNOWLEDGE_SEARCH_SCOPE: &str = "knowledge.search";
pub(crate) const CREATIVE_SUBMIT_SCOPE: &str = "distribution.creative.submit";
pub(crate) const RELEASE_MANAGE_SCOPE: &str = "distribution.release.manage";
pub(crate) const AI_CONVERSATION_SCOPE: &str = "ai.conversation.invoke";
const CLIENT_ID: &str = "himind-agent";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone)]
pub(crate) struct AgentAccessToken {
    pub token: String,
    pub expires_at: u64,
    pub scope: String,
    pub user_id: String,
    pub agent_id: String,
}

impl AgentAccessToken {
    fn valid_for(&self, required_scope: &str) -> bool {
        !self.token.trim().is_empty()
            && self.expires_at > unix_now().saturating_add(30)
            && (required_scope.is_empty()
                || self
                    .scope
                    .split_whitespace()
                    .any(|scope| scope == required_scope))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: String,
    pub refresh_token_expires_in: i64,
    pub scope: String,
    pub user_id: String,
    pub agent_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredAgentAuthorization {
    version: u32,
    agent_id: String,
    user_id: String,
    scope: String,
    refresh_token_protected: String,
    refresh_expires_at: u64,
    updated_at: u64,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    last_verified_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentAuthorizationSnapshot {
    pub agent_id: String,
    pub user_id: String,
    pub display_name: String,
    pub scope: String,
    pub refresh_expires_at: u64,
    pub updated_at: u64,
    pub last_verified_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AgentUserInfo {
    pub sub: String,
    pub agent_id: String,
    pub scope: String,
    pub name: String,
    pub active: bool,
    #[serde(default)]
    pub svn_username: String,
    #[serde(default)]
    pub svn_identity_status: String,
    #[serde(default)]
    pub svn_provisioning_status: String,
    #[serde(default)]
    pub svn_provisioning_error: String,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: String,
}

pub(crate) fn platform_access_token(
    options: &Options,
    required_scope: &str,
) -> Result<AgentAccessToken, Box<dyn Error>> {
    let mut cache = options
        .platform_access
        .write()
        .map_err(|_| "Agent OAuth access-token cache is unavailable")?;
    if let Some(token) = cache
        .as_ref()
        .filter(|token| token.valid_for(required_scope))
    {
        return Ok(token.clone());
    }

    let path = authorization_path(&options.state_path);
    let _refresh_lock = lock_authorization_file(&options.state_path)?;
    let stored: StoredAgentAuthorization =
        serde_json::from_slice(&fs::read(&path).map_err(|_| "请先登录 HiMind 账号")?)?;
    if stored.refresh_expires_at <= unix_now() {
        let _ = clear_authorization_unlocked(&options.state_path);
        *cache = None;
        return Err("HiMind 账号授权已过期，请重新登录".into());
    }
    if !required_scope.is_empty()
        && !stored
            .scope
            .split_whitespace()
            .any(|scope| scope == required_scope)
    {
        return Err(format!("Dashboard authorization is missing scope: {required_scope}").into());
    }
    let refresh_token = unprotect_secret_for_current_user(&stored.refresh_token_protected)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .post(format!("{}/oauth/token", options.api_base))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()?;
    let token = match parse_token_response(response) {
        Ok(token) => token,
        Err(error) => {
            let message = error.to_string();
            if authorization_requires_login(&message) {
                let _ = clear_authorization_unlocked(&options.state_path);
                *cache = None;
                return Err("HiMind 账号授权已失效，请重新登录".into());
            }
            return Err(error);
        }
    };
    if token.agent_id != stored.agent_id {
        return Err("Dashboard returned an access token for a different Agent".into());
    }
    save_authorization_response_unlocked(&options.state_path, &token, Some(&stored))?;
    let access = access_from_response(&token);
    if !access.valid_for(required_scope) {
        return Err(format!("Dashboard authorization is missing scope: {required_scope}").into());
    }
    *cache = Some(access.clone());
    Ok(access)
}

pub(crate) fn cache_registration_access(options: &Options, state: &AgentState) {
    if state.access_token.trim().is_empty() {
        return;
    }
    let access = AgentAccessToken {
        token: state.access_token.clone(),
        expires_at: unix_now().saturating_add(state.access_token_expires_in.max(1) as u64),
        scope: state.access_scope.clone(),
        user_id: state.user_id.clone(),
        agent_id: state.agent_id.clone(),
    };
    if let Ok(mut cache) = options.platform_access.write() {
        *cache = Some(access);
    }
}

pub(crate) fn save_authorization_response(
    state_path: &Path,
    response: &OAuthTokenResponse,
) -> Result<(), Box<dyn Error>> {
    let _lock = lock_authorization_file(state_path)?;
    let previous = read_stored_authorization(state_path).ok();
    save_authorization_response_unlocked(state_path, response, previous.as_ref())
}

fn save_authorization_response_unlocked(
    state_path: &Path,
    response: &OAuthTokenResponse,
    previous: Option<&StoredAgentAuthorization>,
) -> Result<(), Box<dyn Error>> {
    if response.refresh_token.trim().is_empty()
        || response.agent_id.trim().is_empty()
        || response.user_id.trim().is_empty()
    {
        return Err("Dashboard returned an incomplete Agent authorization".into());
    }
    let stored = StoredAgentAuthorization {
        version: 1,
        agent_id: response.agent_id.trim().to_string(),
        user_id: response.user_id.trim().to_string(),
        scope: response.scope.trim().to_string(),
        refresh_token_protected: protect_secret_for_current_user(&response.refresh_token)?,
        refresh_expires_at: unix_now()
            .saturating_add(response.refresh_token_expires_in.max(1) as u64),
        updated_at: unix_now(),
        display_name: previous
            .map(|stored| stored.display_name.clone())
            .unwrap_or_default(),
        last_verified_at: previous
            .map(|stored| stored.last_verified_at)
            .unwrap_or_default(),
    };
    let path = authorization_path(state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&stored)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn clear_authorization(state_path: &Path) -> Result<(), Box<dyn Error>> {
    let _lock = lock_authorization_file(state_path)?;
    clear_authorization_unlocked(state_path)
}

fn clear_authorization_unlocked(state_path: &Path) -> Result<(), Box<dyn Error>> {
    let path = authorization_path(state_path);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn revoke_authorization(options: &Options) -> Result<(), Box<dyn Error>> {
    let mut cache = options
        .platform_access
        .write()
        .map_err(|_| "Agent OAuth access-token cache is unavailable")?;
    let _refresh_lock = lock_authorization_file(&options.state_path)?;
    let path = authorization_path(&options.state_path);
    if !path.exists() {
        *cache = None;
        return Ok(());
    }
    let stored: StoredAgentAuthorization = serde_json::from_slice(&fs::read(&path)?)?;
    let refresh_token = unprotect_secret_for_current_user(&stored.refresh_token_protected)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    client
        .post(format!("{}/oauth/revoke", options.api_base))
        .form(&[
            ("client_id", CLIENT_ID),
            ("token", refresh_token.as_str()),
            ("token_type_hint", "refresh_token"),
        ])
        .send()?
        .error_for_status()?;
    clear_authorization_unlocked(&options.state_path)?;
    crate::svn::service::remove_connection()?;
    *cache = None;
    Ok(())
}

pub(crate) fn begin_device_authorization(
    options: &Options,
) -> Result<DeviceAuthorizationResponse, Box<dyn Error>> {
    let state = load_agent_state(&options.state_path)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .post(format!("{}/oauth/device_authorization", options.api_base))
        .header(
            "Authorization",
            format!("Agent {}:{}", state.agent_id, state.credential),
        )
        .form(&[
            ("client_id", CLIENT_ID),
            (
                "scope",
                &format!(
                    "{PROFILE_SCOPE} {BUSINESS_CONTEXT_READ_SCOPE} {KNOWLEDGE_SEARCH_SCOPE} {AI_CONVERSATION_SCOPE} {CREATIVE_SUBMIT_SCOPE} {RELEASE_MANAGE_SCOPE}"
                ),
            ),
            ("agent_id", state.agent_id.as_str()),
            ("device_id", state.device_id.as_str()),
        ])
        .send()?;
    if !response.status().is_success() {
        return Err(parse_oauth_error(response).into());
    }
    Ok(response.json()?)
}

pub(crate) fn wait_for_device_authorization(
    options: &Options,
    authorization: &DeviceAuthorizationResponse,
) -> Result<AgentAccessToken, Box<dyn Error>> {
    wait_for_device_authorization_with_cancel(options, authorization, || false)
}

pub(crate) fn wait_for_device_authorization_with_cancel<F>(
    options: &Options,
    authorization: &DeviceAuthorizationResponse,
    mut is_cancelled: F,
) -> Result<AgentAccessToken, Box<dyn Error>>
where
    F: FnMut() -> bool,
{
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let deadline = unix_now().saturating_add(authorization.expires_in.max(1) as u64);
    let mut interval = authorization.interval.max(1);
    while unix_now() < deadline {
        if is_cancelled() {
            return Err("Agent device authorization canceled".into());
        }
        thread::sleep(Duration::from_secs(interval));
        if is_cancelled() {
            return Err("Agent device authorization canceled".into());
        }
        let response = client
            .post(format!("{}/oauth/token", options.api_base))
            .form(&[
                ("grant_type", DEVICE_GRANT_TYPE),
                ("client_id", CLIENT_ID),
                ("device_code", authorization.device_code.as_str()),
            ])
            .send()?;
        if response.status().is_success() {
            let token: OAuthTokenResponse = response.json()?;
            save_authorization_response(&options.state_path, &token)?;
            let access = access_from_response(&token);
            if let Ok(mut cache) = options.platform_access.write() {
                *cache = Some(access.clone());
            }
            return Ok(access);
        }
        let error = parse_oauth_error(response);
        match error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval = interval.saturating_add(5);
                continue;
            }
            "access_denied" => return Err("Dashboard user denied Agent authorization".into()),
            "expired_token" => return Err("Agent device authorization expired".into()),
            _ => return Err(error.into()),
        }
    }
    Err("Agent device authorization expired".into())
}

pub(crate) fn authorization_snapshot(
    state_path: &Path,
) -> Result<Option<AgentAuthorizationSnapshot>, Box<dyn Error>> {
    let path = authorization_path(state_path);
    if !path.exists() {
        return Ok(None);
    }
    let stored = read_stored_authorization(state_path)?;
    Ok(Some(AgentAuthorizationSnapshot {
        agent_id: stored.agent_id,
        user_id: stored.user_id,
        display_name: stored.display_name,
        scope: stored.scope,
        refresh_expires_at: stored.refresh_expires_at,
        updated_at: stored.updated_at,
        last_verified_at: stored.last_verified_at,
    }))
}

pub(crate) fn fetch_user_info(options: &Options) -> Result<AgentUserInfo, Box<dyn Error>> {
    let access = platform_access_token(options, PROFILE_SCOPE)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .get(format!("{}/api/agent/oauth/userinfo", options.api_base))
        .bearer_auth(&access.token)
        .send()?
        .error_for_status()?;
    let info: AgentUserInfo = response.json()?;
    if info.sub != access.user_id || info.agent_id != access.agent_id {
        return Err("Dashboard returned user info for a different Agent authorization".into());
    }
    save_user_info_snapshot(&options.state_path, &info)?;
    Ok(info)
}

fn save_user_info_snapshot(state_path: &Path, info: &AgentUserInfo) -> Result<(), Box<dyn Error>> {
    let _lock = lock_authorization_file(state_path)?;
    let mut stored = read_stored_authorization(state_path)?;
    if stored.user_id != info.sub || stored.agent_id != info.agent_id {
        return Err("Dashboard user info does not match the stored Agent authorization".into());
    }
    stored.display_name = info.name.trim().to_string();
    stored.scope = info.scope.trim().to_string();
    stored.last_verified_at = unix_now();
    write_stored_authorization(state_path, &stored)
}

fn read_stored_authorization(
    state_path: &Path,
) -> Result<StoredAgentAuthorization, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(authorization_path(
        state_path,
    ))?)?)
}

fn write_stored_authorization(
    state_path: &Path,
    stored: &StoredAgentAuthorization,
) -> Result<(), Box<dyn Error>> {
    let path = authorization_path(state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(stored)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn lock_authorization_file(state_path: &Path) -> Result<AuthorizationFileLock, Box<dyn Error>> {
    let path = authorization_path(state_path).with_extension("lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock_exclusive()?;
    Ok(AuthorizationFileLock(file))
}

struct AuthorizationFileLock(File);

impl Drop for AuthorizationFileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn parse_token_response(response: Response) -> Result<OAuthTokenResponse, Box<dyn Error>> {
    if !response.status().is_success() {
        return Err(parse_oauth_error(response).into());
    }
    let token: OAuthTokenResponse = response.json()?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
        || token.access_token.trim().is_empty()
        || token.refresh_token.trim().is_empty()
    {
        return Err("Dashboard returned an invalid OAuth token response".into());
    }
    Ok(token)
}

fn parse_oauth_error(response: Response) -> String {
    let status = response.status();
    match response.json::<OAuthErrorResponse>() {
        Ok(error) if !error.error.trim().is_empty() => {
            if error.error_description.trim().is_empty() {
                error.error
            } else if matches!(
                error.error.as_str(),
                "authorization_pending" | "slow_down" | "access_denied" | "expired_token"
            ) {
                error.error
            } else {
                format!("{}: {}", error.error, error.error_description)
            }
        }
        _ => format!("Dashboard OAuth request failed: {status}"),
    }
}

fn authorization_requires_login(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("invalid_grant")
        || normalized.contains("invalid_token")
        || normalized.contains("refresh token reuse")
}

fn access_from_response(response: &OAuthTokenResponse) -> AgentAccessToken {
    AgentAccessToken {
        token: response.access_token.clone(),
        expires_at: unix_now().saturating_add(response.expires_in.max(1) as u64),
        scope: response.scope.clone(),
        user_id: response.user_id.clone(),
        agent_id: response.agent_id.clone(),
    }
}

fn authorization_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("agent-user-authorization.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        authorization_requires_login, save_authorization_response, unix_now, AgentAccessToken,
        OAuthTokenResponse,
    };
    use std::fs;

    fn access(expires_at: u64, scope: &str) -> AgentAccessToken {
        AgentAccessToken {
            token: "access-token".to_string(),
            expires_at,
            scope: scope.to_string(),
            user_id: "usr-test".to_string(),
            agent_id: "agt-test".to_string(),
        }
    }

    #[test]
    fn access_token_cache_requires_scope_and_expiry_margin() {
        let usable = access(
            unix_now() + 120,
            "agent.profile distribution.creative.submit",
        );
        assert!(usable.valid_for("agent.profile"));
        assert!(usable.valid_for("distribution.creative.submit"));
        assert!(!usable.valid_for("dashboard.admin"));

        let expiring = access(unix_now() + 20, "agent.profile");
        assert!(!expiring.valid_for("agent.profile"));

        let empty = AgentAccessToken {
            token: String::new(),
            ..usable
        };
        assert!(!empty.valid_for("agent.profile"));
    }

    #[test]
    fn terminal_oauth_errors_require_a_new_login() {
        assert!(authorization_requires_login(
            "invalid_grant: refresh token reuse detected"
        ));
        assert!(authorization_requires_login("invalid_token"));
        assert!(!authorization_requires_login(
            "Dashboard is temporarily unavailable"
        ));
    }

    #[test]
    fn refresh_token_is_only_persisted_as_a_protected_secret() {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-oauth-{}-{}",
            std::process::id(),
            unix_now()
        ));
        fs::create_dir_all(&root).expect("create OAuth test directory");
        let state_path = root.join("agent-state.json");
        let refresh_token = "refresh-token-that-must-never-be-plaintext";
        let response = OAuthTokenResponse {
            access_token: "memory-only-access-token".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: 600,
            refresh_token: refresh_token.to_string(),
            refresh_token_expires_in: 3600,
            scope: "agent.profile".to_string(),
            user_id: "usr-test".to_string(),
            agent_id: "agt-test".to_string(),
        };

        save_authorization_response(&state_path, &response)
            .expect("persist protected refresh token");
        let raw = fs::read_to_string(root.join("agent-user-authorization.json"))
            .expect("read stored authorization");
        assert!(!raw.contains(refresh_token));
        assert!(!raw.contains("memory-only-access-token"));
        assert!(raw.contains("refresh_token_protected"));
        let _ = fs::remove_dir_all(root);
    }
}
