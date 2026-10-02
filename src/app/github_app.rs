//! GitHub App 授权：device flow 换 user token，再换短期 installation token。
//!
//! 与手填 PAT 相比，App 形态的权限由组织一次性授予、可按安装撤销，令牌 1 小时自动
//! 过期；agent 只长期保存 user token 与 refresh token（DPAPI 加密），安装令牌缓存到
//! 过期前复用。需要组织先注册 GitHub App 并提供 client_id（client_id 是公开值）。
//!
//! 说明：设备流与安装令牌协议按 GitHub 官方文档实现；未注册 App 时该链路无法在生产
//! 使用，因此默认配置为空并给出明确提示。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::github_credentials::{self, GithubAppRecord};

/// device flow 期间允许的最长等待，避免用户放弃后仍无限轮询。
const MAX_DEVICE_WAIT: Duration = Duration::from_secs(900);
/// 安装令牌提前过期的安全边界。
const INSTALLATION_TOKEN_SKEW: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// 带 user_code 预填的授权页（GitHub 新版设备流都会返回），能少一步手输。
    #[serde(default)]
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AppInstallation {
    pub id: String,
    pub account: String,
    pub account_type: String,
    pub repository_selection: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppUserToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// 等待设备授权的中间状态，供调用方决定是继续轮询还是报错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DevicePollOutcome {
    Pending,
    SlowDown,
    Authorized(AppUserToken),
    Expired,
    Denied,
}

fn oauth_base() -> String {
    std::env::var("HIMIND_GITHUB_OAUTH_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://github.com".to_string())
}

fn api_base() -> String {
    std::env::var("HIMIND_GITHUB_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

/// 配置的 App client_id。未配置时 App 授权不可用，但 PAT 路径保持可用。
pub(crate) fn configured_client_id() -> String {
    std::env::var("HIMIND_GITHUB_APP_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            github_credentials::status()
                .ok()
                .map(|status| status.app_client_id)
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_default()
}

fn client() -> Result<reqwest::blocking::Client, Box<dyn Error>> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("himind-agent")
        .build()?)
}

/// 第一步：申请设备码，用户在浏览器里输入 user_code 完成授权。
pub(crate) fn start_device_flow(client_id: &str) -> Result<DeviceAuthorization, Box<dyn Error>> {
    if client_id.trim().is_empty() {
        return Err(
            "尚未配置 GitHub App client_id。请由组织注册 GitHub App 后设置 HIMIND_GITHUB_APP_CLIENT_ID，或改用个人访问令牌授权。"
                .into(),
        );
    }
    let response = client()?
        .post(format!("{}/login/device/code", oauth_base()))
        .header("Accept", "application/json")
        .form(&[("client_id", client_id.trim()), ("scope", "")])
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(format!(
            "申请 GitHub 设备码失败：GitHub 返回 HTTP {}（{}）",
            status.as_u16(),
            body.trim().chars().take(160).collect::<String>()
        )
        .into());
    }
    let value: Value = serde_json::from_str(&body)?;
    Ok(DeviceAuthorization {
        device_code: value
            .get("device_code")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        user_code: value
            .get("user_code")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        verification_uri: value
            .get("verification_uri")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        verification_uri_complete: value
            .get("verification_uri_complete")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        expires_in: value
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        interval: value
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .max(1),
    })
}

/// 第二步：轮询换取 user token。`authorization_pending` / `slow_down` 由调用方决定
/// 何时再来，其余错误直接返回终态。
pub(crate) fn poll_device_flow(
    client_id: &str,
    device_code: &str,
) -> Result<DevicePollOutcome, Box<dyn Error>> {
    let response = client()?
        .post(format!("{}/login/oauth/access_token", oauth_base()))
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.trim()),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(format!("GitHub 设备授权轮询失败：HTTP {}", status.as_u16()).into());
    }
    let value: Value = serde_json::from_str(&body)?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Ok(match error {
            "authorization_pending" => DevicePollOutcome::Pending,
            "slow_down" => DevicePollOutcome::SlowDown,
            "expired_token" => DevicePollOutcome::Expired,
            "access_denied" => DevicePollOutcome::Denied,
            other => {
                return Err(format!("GitHub 设备授权失败：{other}").into());
            }
        });
    }
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err("GitHub 未返回访问令牌".into());
    }
    Ok(DevicePollOutcome::Authorized(AppUserToken {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        expires_in: value
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(28_800),
    }))
}

/// 用 refresh token 续期 user token（GitHub App 的 user token 默认 8 小时）。
pub(crate) fn refresh_user_token(
    client_id: &str,
    refresh_token: &str,
) -> Result<AppUserToken, Box<dyn Error>> {
    let response = client()?
        .post(format!("{}/login/oauth/access_token", oauth_base()))
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.trim()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(format!(
            "刷新 GitHub App 授权失败：HTTP {}（{}）",
            status.as_u16(),
            body.trim().chars().take(160).collect::<String>()
        )
        .into());
    }
    let value: Value = serde_json::from_str(&body)?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err("刷新 GitHub App 授权失败：GitHub 未返回访问令牌，请重新授权。".into());
    }
    Ok(AppUserToken {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or(refresh_token)
            .to_string(),
        expires_in: value
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(28_800),
    })
}

/// 当前用户可用的 App 安装列表，供用户选择发布到哪个组织/账号。
pub(crate) fn list_installations(user_token: &str) -> Result<Vec<AppInstallation>, Box<dyn Error>> {
    let response = client()?
        .get(format!("{}/user/installations", api_base()))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(user_token)
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(format!(
            "读取 GitHub App 安装列表失败：HTTP {}（{}）",
            status.as_u16(),
            body.trim().chars().take(160).collect::<String>()
        )
        .into());
    }
    let value: Value = serde_json::from_str(&body)?;
    let installations = value
        .get("installations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(installations
        .iter()
        .map(|item| AppInstallation {
            id: item
                .get("id")
                .map(|value| value.to_string())
                .unwrap_or_default(),
            account: item
                .get("account")
                .and_then(|account| account.get("login"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            account_type: item
                .get("account")
                .and_then(|account| account.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            repository_selection: item
                .get("repository_selection")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

/// 用 App 私钥签发 JWT（RS256）。
///
/// GitHub 要求 `iat` 不早于 60 秒前、`exp` 不超过 10 分钟；`iss` 用 client ID 或 App ID
/// 均可。这里取 9 分钟有效期，留出时钟偏差余量。
pub(crate) fn create_app_jwt(
    client_id: &str,
    private_key_pem: &str,
) -> Result<String, Box<dyn Error>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::{Pkcs1v15Sign, RsaPrivateKey};
    use sha2::{Digest, Sha256};

    if client_id.trim().is_empty() {
        return Err("缺少 GitHub App client_id，无法签发 App JWT".into());
    }
    // GitHub 下载的私钥默认是 PKCS#1，也支持 PKCS#8，两种都接受。
    let key = RsaPrivateKey::from_pkcs1_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(private_key_pem))
        .map_err(|error| format!("App 私钥格式无效（需要 PKCS#1 或 PKCS#8 PEM）：{error}"))?;
    let now = now_epoch();
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let payload = serde_json::json!({
        "iat": now.saturating_sub(60),
        "exp": now + 540,
        "iss": client_id.trim(),
    });
    let encoded_header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?);
    let encoded_payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?);
    let signing_input = format!("{encoded_header}.{encoded_payload}");
    let digest = Sha256::digest(signing_input.as_bytes());
    let signature = key
        .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map_err(|error| format!("App JWT 签名失败：{error}"))?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// 用 App JWT 换取安装令牌（1 小时有效）。
///
/// 注意：安装令牌只能由 App 身份签发（`POST /app/installations/{id}/access_tokens`）。
/// `POST /user/installations/{id}/access_tokens` 并不存在——用户令牌能列安装、能列
/// 仓库，但签不出安装令牌，这一点由真实 GitHub 探针确认。
pub(crate) fn create_installation_token(
    app_jwt: &str,
    installation_id: &str,
) -> Result<(String, u64, bool), Box<dyn Error>> {
    if installation_id.trim().is_empty() {
        return Err("尚未选择 GitHub App 安装".into());
    }
    let response = client()?
        .post(format!(
            "{}/app/installations/{}/access_tokens",
            api_base(),
            installation_id.trim()
        ))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(app_jwt)
        .body("{}")
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(format!(
            "创建 GitHub 安装令牌失败：HTTP {}（{}）",
            status.as_u16(),
            body.trim().chars().take(160).collect::<String>()
        )
        .into());
    }
    let value: Value = serde_json::from_str(&body)?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if token.is_empty() {
        return Err("GitHub 未返回安装令牌".into());
    }
    let expires_at = value
        .get("expires_at")
        .and_then(Value::as_str)
        .map(parse_rfc3339_seconds)
        .filter(|value| *value > 0)
        .unwrap_or_else(|| now_epoch() + 3_600);
    // 签发响应带 permissions，是判断「这个令牌能不能写 Contents」的唯一可靠来源：
    // `GET /repos/{owner}/{repo}` 的 permissions 只反映登录用户，对安装令牌恒为 false。
    let contents_write = value
        .get("permissions")
        .and_then(|permissions| permissions.get("contents"))
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case("write"))
        .unwrap_or(false);
    Ok((token, expires_at, contents_write))
}

/// 取当前可用的分发令牌：缓存未过期就直接用，否则刷新 user token 后重新签发。
pub(crate) fn ensure_installation_token() -> Result<String, String> {
    let now = now_epoch();
    if let Ok(Some((token, expires_at))) = github_credentials::cached_installation_token() {
        if expires_at > now + INSTALLATION_TOKEN_SKEW && !token.is_empty() {
            return Ok(token);
        }
    }
    let state = github_credentials::app_state()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "GitHub App 授权已失效，请重新授权".to_string())?;
    // 有私钥就走标准的 App 身份：签发 JWT → 换 1 小时的安装令牌。
    if let Some(private_key) =
        github_credentials::app_private_key().map_err(|error| error.to_string())?
    {
        let jwt =
            create_app_jwt(&state.client_id, &private_key).map_err(|error| error.to_string())?;
        let (token, expires_at, contents_write) =
            create_installation_token(&jwt, &state.installation_id)
                .map_err(|error| error.to_string())?;
        github_credentials::cache_installation_token(&token, expires_at, contents_write)
            .map_err(|error| error.to_string())?;
        return Ok(token);
    }
    // 没有私钥时回退到用户令牌：权限是「App 权限 ∩ 用户权限」，仍然比长期 PAT 收敛，
    // 只是身份代表用户而不是 App。设备流已经拿到它，不必再要求用户配置密钥。
    let mut user_token = state.user_token.clone();
    let expires_at = state
        .user_token_expires_at
        .trim()
        .parse::<u64>()
        .unwrap_or_default();
    if user_token.is_empty() || (expires_at > 0 && expires_at <= now + INSTALLATION_TOKEN_SKEW) {
        if state.refresh_token.trim().is_empty() {
            return Err("GitHub App 授权已过期，请重新授权".to_string());
        }
        let refreshed = refresh_user_token(&state.client_id, &state.refresh_token)
            .map_err(|error| error.to_string())?;
        let mut updated = GithubAppRecord {
            user_token: refreshed.access_token.clone(),
            refresh_token: if refreshed.refresh_token.trim().is_empty() {
                state.refresh_token.clone()
            } else {
                refreshed.refresh_token.clone()
            },
            user_token_expires_at: (now + refreshed.expires_in).to_string(),
            ..state.clone()
        };
        user_token = refreshed.access_token;
        github_credentials::save_app_state(&updated).map_err(|error| error.to_string())?;
    }
    Ok(user_token)
}

/// 绑定安装：把选定的安装写入凭据记录，后续发布即使用该安装的令牌。
pub(crate) fn select_installation(installation: &AppInstallation) -> Result<(), Box<dyn Error>> {
    let state = github_credentials::app_state()?.ok_or("GitHub App 授权已失效，请重新授权")?;
    github_credentials::save_app_state(&GithubAppRecord {
        installation_id: installation.id.clone(),
        installation_account: installation.account.clone(),
        ..state
    })
}

/// 设备流整体预算，供调用方判断是否继续轮询。
pub(crate) fn device_flow_budget() -> Duration {
    MAX_DEVICE_WAIT
}

pub(crate) fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

/// 解析 `2026-01-01T00:00:00Z` 这类时间戳为 epoch 秒。
fn parse_rfc3339_seconds(value: &str) -> u64 {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return 0;
    }
    let number = |range: std::ops::Range<usize>| -> i64 {
        value
            .get(range)
            .and_then(|part| part.parse::<i64>().ok())
            .unwrap_or_default()
    };
    let year = number(0..4);
    let month = number(5..7);
    let day = number(8..10);
    let hour = number(11..13);
    let minute = number(14..16);
    let second = number(17..19);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return 0;
    }
    // 以 1970-01-01 为基准做民用历法换算，避免为一个字段引入日期库。
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    seconds.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installation_expiry_timestamps() {
        assert_eq!(parse_rfc3339_seconds("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_rfc3339_seconds("2026-01-01T00:00:00Z"), 1_767_225_600);
        assert_eq!(parse_rfc3339_seconds("2024-02-29T12:00:00Z"), 1_709_208_000);
        // 非法输入返回 0，调用方会退回到「1 小时后过期」的保守默认值。
        assert_eq!(parse_rfc3339_seconds(""), 0);
        assert_eq!(parse_rfc3339_seconds("not-a-date"), 0);
    }

    #[test]
    fn missing_client_id_blocks_app_flow_with_actionable_message() {
        let error = start_device_flow("").unwrap_err().to_string();
        assert!(error.contains("client_id"));
        assert!(error.contains("个人访问令牌"));
    }

    #[test]
    fn app_jwt_is_rs256_signed_and_verifiable() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use rsa::pkcs8::{EncodePrivateKey, LineEnding};
        use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
        use sha2::{Digest, Sha256};

        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();

        let jwt = create_app_jwt("Iv23liTestClientId", &pem).unwrap();
        let parts = jwt.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3, "JWT 必须是 header.payload.signature");
        let header: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        let payload: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(payload["iss"], "Iv23liTestClientId");
        let issued = payload["iat"].as_u64().unwrap();
        let expires = payload["exp"].as_u64().unwrap();
        // GitHub 要求 iat 不早于 60 秒前、有效期不超过 10 分钟。
        assert!(issued + 60 <= now_epoch() + 1);
        assert!(expires > issued);
        assert!(expires - issued <= 600);

        // 用公钥验签，等于 GitHub 侧会做的事。
        let public = RsaPublicKey::from(&key);
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let digest = Sha256::digest(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        public
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature)
            .expect("JWT 签名必须能被公钥验证");
    }

    #[test]
    fn app_jwt_accepts_pkcs1_keys_and_rejects_garbage() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::LineEnding;
        use rsa::RsaPrivateKey;

        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        // GitHub 下载的私钥默认就是 PKCS#1。
        let pkcs1 = key.to_pkcs1_pem(LineEnding::LF).unwrap().to_string();
        assert!(create_app_jwt("client", &pkcs1).is_ok());
        let error = create_app_jwt("client", "not a pem")
            .unwrap_err()
            .to_string();
        assert!(error.contains("App 私钥格式无效"));
    }
}
