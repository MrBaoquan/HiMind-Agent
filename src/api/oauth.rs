use base64::Engine;
use rand::RngCore;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::client::load_agent_state;
use crate::api::types::AgentState;
use crate::store::atomic_file::{self, AtomicFileLock};
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};
use crate::Options;

pub(crate) const PROFILE_SCOPE: &str = "agent.profile";
pub(crate) const BUSINESS_CONTEXT_READ_SCOPE: &str = "business.context.read";
pub(crate) const BUSINESS_PROJECT_READ_SCOPE: &str = "business.project.read";
pub(crate) const BUSINESS_PROJECT_WRITE_SCOPE: &str = "business.project.write";
pub(crate) const BUSINESS_EXHIBIT_READ_SCOPE: &str = "business.exhibit.read";
pub(crate) const BUSINESS_EXHIBIT_WRITE_SCOPE: &str = "business.exhibit.write";
pub(crate) const BUSINESS_PEOPLE_READ_SCOPE: &str = "business.people.read";
pub(crate) const BUSINESS_PEOPLE_WRITE_SCOPE: &str = "business.people.write";
pub(crate) const BUSINESS_REQUIREMENT_READ_SCOPE: &str = "business.requirement.read";
pub(crate) const BUSINESS_REQUIREMENT_WRITE_SCOPE: &str = "business.requirement.write";
pub(crate) const BUSINESS_WORKSPACE_READ_SCOPE: &str = "business.workspace.read";
pub(crate) const BUSINESS_WORKSPACE_WRITE_SCOPE: &str = "business.workspace.write";
pub(crate) const KNOWLEDGE_SEARCH_SCOPE: &str = "knowledge.search";
pub(crate) const CREATIVE_SUBMIT_SCOPE: &str = "distribution.creative.submit";
pub(crate) const RELEASE_MANAGE_SCOPE: &str = "distribution.release.manage";
pub(crate) const AI_CONVERSATION_SCOPE: &str = "ai.conversation.invoke";
pub(crate) const MEDIA_SUBMIT_SCOPE: &str = "ai.media.submit";
pub(crate) const MEDIA_READ_SCOPE: &str = "ai.media.read";
pub(crate) const MEDIA_CANCEL_SCOPE: &str = "ai.media.cancel";
pub(crate) const OPERATION_READ_SCOPE: &str = "operation.read";
pub(crate) const OPERATION_CANCEL_SCOPE: &str = "operation.cancel";
const CLIENT_ID: &str = "himind-agent";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// 轮询在服务端有效期之后继续问的余量（秒），见 wait_for_device_authorization_with_cancel。
const DEVICE_POLL_OVERRUN_SECS: u64 = 30;

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pending_refresh_token_protected: String,
    // 同一份授权文件会被多个进程同时使用：桌面 Agent 一个，每个接入的 AI 客户端
    // 又会各拉起一个 himind-agent-mcp。进程内的 access-token 缓存互不可见，若只落盘
    // refresh token，access token 一到期，这 N 个进程就会各自轮换同一个 refresh
    // token，形成 N 连击（实测 8 进程 = 每 10 分钟 6~8 次链式轮换）。
    // 把最近一次签发的 access token 一并落盘，后来者直接复用，跨进程只保留一次刷新。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    access_token_protected: String,
    #[serde(default)]
    access_expires_at: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    access_scope: String,
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

    let _refresh_lock = lock_authorization_file(&options.state_path)?;
    // 授权文件不存在 = 真的没登录；文件存在却读不出来是另一类故障（损坏、权限、
    // 备份也坏了），把两者压成一句「请先登录」会让这类故障无法排查。
    let stored = read_stored_authorization(&options.state_path).map_err(|error| {
        if authorization_path(&options.state_path).is_file() {
            format!("HiMind 账号授权文件不可用: {error}")
        } else {
            "请先登录 HiMind 账号".to_string()
        }
    })?;
    // 跨进程共享缓存：另一个进程（例如某个 AI 客户端拉起的 MCP 服务）刚刚刷新过，
    // 就直接采用它的 access token。没有这一步，每个进程都会把同一个 refresh token
    // 轮换一次，服务端看到的就是一串毫无必要的链式轮换。
    if let Some(shared) = shared_access_token_from(&stored, required_scope) {
        *cache = Some(shared.clone());
        return Ok(shared);
    }
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
    let next_refresh_token = prepare_refresh_attempt(&options.state_path, &stored)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .post(format!("{}/oauth/token", options.api_base()))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token.as_str()),
            ("next_refresh_token", next_refresh_token.as_str()),
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
    if token.refresh_token != next_refresh_token {
        return Err("Dashboard returned an unexpected refresh token".into());
    }
    save_authorization_response_unlocked(&options.state_path, &token, Some(&stored))?;
    let access = access_from_response(&token);
    if !access.valid_for(required_scope) {
        return Err(format!("Dashboard authorization is missing scope: {required_scope}").into());
    }
    *cache = Some(access.clone());
    Ok(access)
}

/// Read the last persisted Dashboard identity without attempting a token
/// refresh. Approval outbox records use this when connectivity is temporarily
/// unavailable so they remain bound to the original user and Agent.
pub(crate) fn persisted_authorization_identity(state_path: &Path) -> Option<(String, String)> {
    let stored = read_stored_authorization(state_path).ok()?;
    if stored.refresh_expires_at <= unix_now() {
        return None;
    }
    let agent_id = stored.agent_id.trim();
    let user_id = stored.user_id.trim();
    if agent_id.is_empty() || user_id.is_empty() {
        return None;
    }
    Some((agent_id.to_string(), user_id.to_string()))
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
        pending_refresh_token_protected: String::new(),
        access_token_protected: protect_secret_for_current_user(&response.access_token)?,
        access_expires_at: unix_now().saturating_add(response.expires_in.max(1) as u64),
        access_scope: response.scope.trim().to_string(),
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
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&stored)?)?;
    // The connection record carries its own copy of this file, so the refresh
    // path is captured too: a rotated refresh token must not be lost when the
    // user switches back to this workbench (ADR 0008).
    crate::store::workbenches::capture_active_quiet(state_path);
    Ok(())
}

fn prepare_refresh_attempt(
    state_path: &Path,
    stored: &StoredAgentAuthorization,
) -> Result<String, Box<dyn Error>> {
    if !stored.pending_refresh_token_protected.trim().is_empty() {
        return unprotect_secret_for_current_user(&stored.pending_refresh_token_protected);
    }
    let mut bytes = [0_u8; 48];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let next_refresh_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let mut pending = StoredAgentAuthorization {
        version: stored.version,
        agent_id: stored.agent_id.clone(),
        user_id: stored.user_id.clone(),
        scope: stored.scope.clone(),
        refresh_token_protected: stored.refresh_token_protected.clone(),
        pending_refresh_token_protected: protect_secret_for_current_user(&next_refresh_token)?,
        access_token_protected: stored.access_token_protected.clone(),
        access_expires_at: stored.access_expires_at,
        access_scope: stored.access_scope.clone(),
        refresh_expires_at: stored.refresh_expires_at,
        updated_at: stored.updated_at,
        display_name: stored.display_name.clone(),
        last_verified_at: stored.last_verified_at,
    };
    pending.updated_at = unix_now();
    write_stored_authorization(state_path, &pending)?;
    Ok(next_refresh_token)
}

pub(crate) fn clear_authorization(state_path: &Path) -> Result<(), Box<dyn Error>> {
    let _lock = lock_authorization_file(state_path)?;
    clear_authorization_unlocked(state_path)
}

fn clear_authorization_unlocked(state_path: &Path) -> Result<(), Box<dyn Error>> {
    let path = authorization_path(state_path);
    for candidate in [&path, &atomic_file::backup_path(&path)] {
        if candidate.exists() {
            fs::remove_file(candidate)?;
        }
    }
    let grant_cache = state_path.with_file_name("approval-grants.cache");
    for candidate in [&grant_cache, &atomic_file::backup_path(&grant_cache)] {
        if candidate.exists() {
            fs::remove_file(candidate)?;
        }
    }
    crate::store::workbenches::capture_active_quiet(state_path);
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
        .post(format!("{}/oauth/revoke", options.api_base()))
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
        .post(format!("{}/oauth/device_authorization", options.api_base()))
        .header(
            "Authorization",
            format!("Agent {}:{}", state.agent_id, state.credential),
        )
        .form(&[
            ("client_id", CLIENT_ID),
            // An empty scope asks Dashboard to negotiate every capability the
            // current user may grant. This keeps future catalog scopes a
            // Dashboard-only change while the verification page remains the
            // explicit user-consent boundary.
            ("scope", ""),
            ("agent_id", state.agent_id.as_str()),
            ("device_id", state.device_id.as_str()),
        ])
        .send()?;
    if !response.status().is_success() {
        return Err(parse_oauth_error(response).into());
    }
    Ok(response.json()?)
}

/// 授权页地址必须落在当前 Agent 对接的 Dashboard 上。
///
/// 开发环境与生产环境共用同一套 device authorization 协议，用户最终在哪个环境
/// 完成授权完全由这个地址决定。服务端按请求的 Host 生成它（Dashboard 侧的
/// `requestOrigin`），正常情况天然同源；一旦反向代理、`X-Forwarded-Proto` 或
/// `HIMIND_PUBLIC_URL` 配错，它就会返回另一个环境的地址。客户端无条件打开该
/// 地址，会让用户在 A 环境发起对接、却在 B 环境完成授权，而且没有任何提示。
/// 这里统一把地址收敛到当前 `api_base`，并返回一条可记录的说明。
pub(crate) fn align_authorization_urls(
    api_base: &str,
    authorization: &mut DeviceAuthorizationResponse,
) -> Option<String> {
    let target = url::Url::parse(api_base).ok()?;
    let original = authorization.verification_uri_complete.trim().to_string();
    let Ok(returned) = url::Url::parse(&original) else {
        return rebuild_authorization_urls(
            api_base,
            authorization,
            &format!("Dashboard 返回的授权页地址无法解析：{original}"),
        );
    };
    if same_origin(&target, &returned) {
        return None;
    }
    // 只在服务端返回的确实是同一类授权页时才改写；路径结构变了说明服务端换了
    // 设计，直接按本机地址拼接反而会拼出一个不存在的页面。
    if !is_authorization_page_path(returned.path()) {
        return Some(format!(
            "Dashboard 返回的授权页地址 {original} 与当前对接的 {api_base} 不同源，路径也不是授权页，未自动改写，请确认对接的 Dashboard 是否正确"
        ));
    }
    rebuild_authorization_urls(
        api_base,
        authorization,
        &format!("Dashboard 返回的授权页地址 {original} 不属于当前对接的 {api_base}"),
    )
}

/// Authorization pages live at `/oauth/device`, optionally behind a deployment
/// prefix such as `/himind/oauth/device`.
fn is_authorization_page_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path.is_empty() || path.ends_with("/oauth/device")
}

fn rebuild_authorization_urls(
    api_base: &str,
    authorization: &mut DeviceAuthorizationResponse,
    reason: &str,
) -> Option<String> {
    let page = device_page_url(api_base, &authorization.user_code)?;
    authorization.verification_uri = page.clone();
    authorization.verification_uri_complete = page.clone();
    Some(format!("{reason}，已改为在 {page} 完成授权"))
}

fn device_page_url(api_base: &str, user_code: &str) -> Option<String> {
    let mut url = url::Url::parse(api_base).ok()?;
    // api_base 可能带部署前缀（例如 https://host/himind），拼接时要保留。
    let base_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{base_path}/oauth/device"));
    url.set_query(None);
    url.set_fragment(None);
    if !user_code.trim().is_empty() {
        url.query_pairs_mut()
            .append_pair("user_code", user_code.trim());
    }
    Some(url.to_string())
}

fn same_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
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
    // 客户端不要在服务端的有效期上「卡点」放弃：本机时钟偏差、休眠唤醒、或者用户
    // 恰好在最后一秒点确认，都会让一次已经生效的确认白做。多给一个轮询周期以上
    // 的余量继续问，服务端仍然是唯一的裁判——真过期了它会回 expired_token。
    let deadline =
        unix_now().saturating_add(authorization.expires_in.max(1) as u64 + DEVICE_POLL_OVERRUN_SECS);
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
            .post(format!("{}/oauth/token", options.api_base()))
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
            "expired_token" => {
                return Err(format!(
                    "Agent device authorization expired：确认已超时（确认码有效期约 {} 分钟）。请在 HiMind Agent 中重新发起授权。",
                    authorization.expires_in.max(1) / 60
                )
                .into())
            }
            _ => return Err(error.into()),
        }
    }
    Err(
        "Agent device authorization expired：等待确认超时。如果刚刚才在工作台点过确认，请在 HiMind Agent 中重新发起授权。"
            .into(),
    )
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
        .get(format!("{}/api/agent/oauth/userinfo", options.api_base()))
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
    let path = authorization_path(state_path);
    match read_stored_authorization_file(&path) {
        Ok(value) => Ok(value),
        Err(primary_error) => {
            let backup = atomic_file::backup_path(&path);
            let recovered = read_stored_authorization_file(&backup).map_err(|backup_error| {
                format!(
                    "Agent user authorization is unreadable (primary: {primary_error}; backup: {backup_error})"
                )
            })?;
            atomic_file::restore_backup(&path)?;
            eprintln!("Agent user authorization recovered from the last known good backup");
            Ok(recovered)
        }
    }
}

fn read_stored_authorization_file(path: &Path) -> Result<StoredAgentAuthorization, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_stored_authorization(
    state_path: &Path,
    stored: &StoredAgentAuthorization,
) -> Result<(), Box<dyn Error>> {
    let path = authorization_path(state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(stored)?)?;
    crate::store::workbenches::capture_active_quiet(state_path);
    Ok(())
}

fn lock_authorization_file(state_path: &Path) -> Result<AtomicFileLock, Box<dyn Error>> {
    Ok(atomic_file::lock(&authorization_path(state_path))?)
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

/// 复用授权文件里最近一次签发的 access token（跨进程共享缓存）。
///
/// 只有仍然覆盖 `required_scope`、且还没进入 `valid_for` 的 30 秒安全边界时才复用；
/// 否则返回 None，由调用方走正常的 refresh 轮换。
fn shared_access_token_from(
    stored: &StoredAgentAuthorization,
    required_scope: &str,
) -> Option<AgentAccessToken> {
    if stored.access_token_protected.trim().is_empty() || stored.access_expires_at == 0 {
        return None;
    }
    let token = unprotect_secret_for_current_user(&stored.access_token_protected).ok()?;
    let scope = if stored.access_scope.trim().is_empty() {
        stored.scope.clone()
    } else {
        stored.access_scope.clone()
    };
    let access = AgentAccessToken {
        token,
        expires_at: stored.access_expires_at,
        scope,
        user_id: stored.user_id.clone(),
        agent_id: stored.agent_id.clone(),
    };
    access.valid_for(required_scope).then_some(access)
}

/// Resolve the effective Agent user authorization file.
///
/// The canonical location is next to the Agent state file. Installations that
/// predate the per-profile `data` directory stored it one level above, and the
/// approval identity path already tolerates that layout. The OAuth/token path
/// must resolve the same file, otherwise an authorised account looks logged out
/// and connected-mode sessions are refused with "请先登录 HiMind 账号".
pub(crate) fn authorization_path(state_path: &Path) -> PathBuf {
    let canonical = state_path.with_file_name("agent-user-authorization.json");
    if canonical.is_file() {
        return canonical;
    }
    if let Some(home) = state_path
        .parent()
        .filter(|directory| {
            directory
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("data"))
        })
        .and_then(Path::parent)
    {
        let legacy = home.join("agent-user-authorization.json");
        if legacy.is_file() {
            return legacy;
        }
    }
    canonical
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
        align_authorization_urls, authorization_path, authorization_requires_login,
        authorization_snapshot, clear_authorization, prepare_refresh_attempt,
        read_stored_authorization, save_authorization_response, unix_now, AgentAccessToken,
        DeviceAuthorizationResponse, OAuthTokenResponse,
    };
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn authorization(
        verification_uri_complete: &str,
        user_code: &str,
    ) -> DeviceAuthorizationResponse {
        DeviceAuthorizationResponse {
            device_code: "device-code".to_string(),
            user_code: user_code.to_string(),
            verification_uri: "https://himind.andcrane.com/oauth/device".to_string(),
            verification_uri_complete: verification_uri_complete.to_string(),
            expires_in: 600,
            interval: 5,
        }
    }

    #[test]
    fn authorization_urls_on_the_same_origin_are_left_untouched() {
        let mut response = authorization(
            "http://127.0.0.1:18083/oauth/device?user_code=AB12-CD34",
            "AB12-CD34",
        );
        let before = response.verification_uri_complete.clone();
        assert!(align_authorization_urls("http://127.0.0.1:18083", &mut response).is_none());
        assert_eq!(response.verification_uri_complete, before);
    }

    #[test]
    fn authorization_urls_from_another_environment_collapse_to_the_configured_api() {
        let mut response = authorization(
            "https://himind.andcrane.com/oauth/device?user_code=AB12-CD34",
            "AB12-CD34",
        );
        let notice = align_authorization_urls("http://127.0.0.1:18083", &mut response)
            .expect("cross-origin authorization page is realigned");
        assert!(notice.contains("http://127.0.0.1:18083/oauth/device"));
        assert_eq!(
            response.verification_uri,
            "http://127.0.0.1:18083/oauth/device?user_code=AB12-CD34"
        );
        assert_eq!(
            response.verification_uri_complete,
            "http://127.0.0.1:18083/oauth/device?user_code=AB12-CD34"
        );
    }

    #[test]
    fn authorization_hosts_differing_only_by_name_are_treated_as_cross_origin() {
        let mut response = authorization(
            "http://localhost:18083/oauth/device?user_code=AB12-CD34",
            "AB12-CD34",
        );
        assert!(align_authorization_urls("http://127.0.0.1:18083", &mut response).is_some());
        assert_eq!(
            response.verification_uri_complete,
            "http://127.0.0.1:18083/oauth/device?user_code=AB12-CD34"
        );
    }

    #[test]
    fn authorization_page_keeps_the_deployment_prefix_and_omits_an_empty_code() {
        let mut response = authorization("https://other.example/oauth/device", "");
        let notice =
            align_authorization_urls("https://host/himind", &mut response).expect("realign");
        assert!(notice.contains("https://host/himind/oauth/device"));
        assert_eq!(
            response.verification_uri_complete,
            "https://host/himind/oauth/device"
        );
    }

    #[test]
    fn unrelated_cross_origin_pages_are_reported_instead_of_rewritten() {
        let mut response = authorization("https://sso.example/login?next=1", "AB12-CD34");
        let notice = align_authorization_urls("http://127.0.0.1:18083", &mut response)
            .expect("unexpected page is reported");
        assert!(notice.contains("未自动改写"));
        assert_eq!(
            response.verification_uri_complete,
            "https://sso.example/login?next=1"
        );
    }

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
    fn clearing_authorization_removes_primary_and_backup() {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-oauth-clear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let authorization = state_path.with_file_name("agent-user-authorization.json");
        let backup = crate::store::atomic_file::backup_path(&authorization);
        fs::write(&authorization, b"primary").unwrap();
        fs::write(&backup, b"backup").unwrap();

        clear_authorization(&state_path).unwrap();

        assert!(!authorization.exists());
        assert!(!backup.exists());
        fs::remove_dir_all(root).unwrap();
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

    /// 本地伪 Dashboard：只实现 `POST /oauth/token`，把客户端提交的
    /// `next_refresh_token` 原样回显（真实服务端就是这么轮换的），并记录命中次数。
    fn spawn_fake_dashboard() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake dashboard");
        let api_base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let presented = Arc::new(Mutex::new(Vec::new()));
        let counter = Arc::clone(&hits);
        let seen = Arc::clone(&presented);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut body = String::new();
                loop {
                    let mut chunk = [0_u8; 2048];
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                    let Some(head_end) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&request[..head_end]).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or_default();
                    if request.len() < head_end + 4 + length {
                        continue;
                    }
                    body = String::from_utf8_lossy(&request[head_end + 4..]).to_string();
                    break;
                }
                let mut next = String::new();
                let mut used = String::new();
                for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
                    match key.as_ref() {
                        "next_refresh_token" => next = value.to_string(),
                        "refresh_token" => used = value.to_string(),
                        _ => {}
                    }
                }
                let issued = counter.fetch_add(1, Ordering::SeqCst) + 1;
                seen.lock().unwrap().push(used);
                let payload = format!(
                    "{{\"access_token\":\"access-{issued}\",\"token_type\":\"Bearer\",\"expires_in\":600,\"refresh_token\":\"{next}\",\"refresh_token_expires_in\":7776000,\"scope\":\"agent.profile\",\"user_id\":\"usr-test\",\"agent_id\":\"agt-test\"}}"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (api_base, hits, presented)
    }

    /// 一份授权文件被多个进程共享（桌面 Agent + 每个 AI 客户端各一个 MCP 服务）。
    /// 第二个进程必须直接采用第一个进程刚拿到的 access token，而不是把同一个
    /// refresh token 再轮换一次——否则 access token 每过期一次就会产生 N 连击。
    #[test]
    fn a_second_process_reuses_the_shared_access_token_instead_of_rotating_again() {
        let (api_base, hits, presented) = spawn_fake_dashboard();
        let root = std::env::temp_dir().join(format!(
            "himind-agent-oauth-shared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create shared cache test directory");
        let state_path = root.join("agent-state.json");
        // 升级前的授权文件只有 refresh token，可以确定性地逼出一次真实刷新。
        let seeded = serde_json::json!({
            "version": 1,
            "agent_id": "agt-test",
            "user_id": "usr-test",
            "scope": "agent.profile",
            "refresh_token_protected": crate::store::credentials::protect_secret_for_current_user(
                "refresh-token-1",
            )
            .unwrap(),
            "refresh_expires_at": unix_now() + 3600,
            "updated_at": unix_now(),
        });
        fs::write(
            authorization_path(&state_path),
            serde_json::to_vec(&seeded).unwrap(),
        )
        .expect("seed authorization");

        // 每个 `Options` 都是一次进程冷启动：内存里的 access-token 缓存是空的，
        // 两个值之间互不可见。
        let cold_start = || {
            let mut options = crate::Options::from_env();
            options.state_path = state_path.clone();
            options.set_api_base(&api_base);
            options
        };
        let first = cold_start();
        let second = cold_start();

        let from_first = super::platform_access_token(&first, "agent.profile")
            .expect("first process refreshes the rotating token");
        let from_second = super::platform_access_token(&second, "agent.profile")
            .expect("second process reuses the freshly issued access token");

        assert_eq!(hits.load(Ordering::SeqCst), 1, "只有第一个进程需要刷新");
        assert_eq!(from_first.token, from_second.token);
        assert_eq!(presented.lock().unwrap().as_slice(), ["refresh-token-1"]);

        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn refresh_attempt_reuses_persisted_pending_token() {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-oauth-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create OAuth pending test directory");
        let state_path = root.join("agent-state.json");
        let response = OAuthTokenResponse {
            access_token: "memory-only-access-token".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: 600,
            refresh_token: "current-refresh-token".to_string(),
            refresh_token_expires_in: 3600,
            scope: "agent.profile".to_string(),
            user_id: "usr-test".to_string(),
            agent_id: "agt-test".to_string(),
        };
        save_authorization_response(&state_path, &response).expect("save authorization");
        let stored = read_stored_authorization(&state_path).expect("read authorization");
        let first = prepare_refresh_attempt(&state_path, &stored).expect("prepare refresh");
        let pending = read_stored_authorization(&state_path).expect("read pending authorization");
        let retry = prepare_refresh_attempt(&state_path, &pending).expect("resume refresh");
        assert_eq!(first, retry);
        let raw = fs::read_to_string(root.join("agent-user-authorization.json"))
            .expect("read raw authorization");
        assert!(!raw.contains(&first));
        assert!(!pending.pending_refresh_token_protected.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_home_authorization_still_resolves_after_the_data_layout_migration() {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-oauth-legacy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data = root.join("data");
        fs::create_dir_all(&data).expect("create profile data directory");
        let state_path = data.join("agent-state.json");
        let legacy = root.join("agent-user-authorization.json");
        let canonical = data.join("agent-user-authorization.json");
        let stored = |user_id: &str| {
            serde_json::json!({
                "version": 1,
                "agent_id": "agt-legacy",
                "user_id": user_id,
                "scope": "agent.profile ai.conversation.invoke",
                "refresh_token_protected": "protected",
                "refresh_expires_at": 4_000_000_000u64,
                "updated_at": 1,
            })
            .to_string()
        };
        fs::write(&legacy, stored("usr-legacy")).expect("write legacy authorization");

        assert_eq!(authorization_path(&state_path), legacy);
        assert!(authorization_snapshot(&state_path)
            .expect("read authorization snapshot")
            .is_some_and(|snapshot| snapshot.user_id == "usr-legacy"));

        fs::write(&canonical, stored("usr-canonical")).expect("write canonical authorization");
        assert_eq!(authorization_path(&state_path), canonical);
        assert!(authorization_snapshot(&state_path)
            .expect("read authorization snapshot")
            .is_some_and(|snapshot| snapshot.user_id == "usr-canonical"));

        fs::remove_file(&canonical).expect("remove canonical authorization");
        assert_eq!(authorization_path(&state_path), legacy);
        fs::remove_dir_all(root).ok();
    }
}
