//! GitHub 分发凭据。
//!
//! 只保存一条当前账号记录：token 以 DPAPI 按当前 Windows 用户加密，落盘内容
//! 与连接器凭据、AI 服务密钥使用同一套保护实现。明文 token 只在进程内存中
//! 短暂存在，不写日志、不进错误信息、不进项目记录。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::PathBuf;

use crate::store::atomic_file;
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};

const STORE_FILE: &str = "github.json";
/// 细粒度 PAT 与经典 PAT 的区分只用于提示用户最小权限，不改变调用方式。
const TOKEN_KIND_FINE_GRAINED: &str = "fine_grained_pat";
const TOKEN_KIND_CLASSIC: &str = "classic_pat";
/// GitHub App 安装授权的凭据形态。
pub(crate) const AUTH_KIND_PAT: &str = "pat";
pub(crate) const AUTH_KIND_APP: &str = "app";

/// GitHub App 授权事实。只在授权流程与令牌刷新时以明文形式存在于内存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubAppRecord {
    pub login: String,
    pub client_id: String,
    pub installation_id: String,
    pub installation_account: String,
    pub user_token: String,
    pub refresh_token: String,
    pub user_token_expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct GithubAccountRecord {
    /// GitHub 登录名，由 `GET /user` 校验得到，不信任用户输入。
    login: String,
    token_kind: String,
    protected_token: String,
    /// 授权时可写的仓库（`owner/repo`）。空集合表示未绑定，发布时会再校验。
    #[serde(default)]
    repositories: Vec<String>,
    updated_at: String,
    /// 凭据形态：`pat`（个人令牌，默认）或 `app`（GitHub App 安装授权）。
    #[serde(default)]
    auth_kind: String,
    #[serde(default)]
    app_client_id: String,
    #[serde(default)]
    installation_id: String,
    /// 安装所在的账号（组织或个人），仅用于展示。
    #[serde(default)]
    installation_account: String,
    #[serde(default)]
    protected_user_token: String,
    #[serde(default)]
    protected_refresh_token: String,
    #[serde(default)]
    user_token_expires_at: String,
    /// 安装令牌是短期的（1 小时），缓存到过期前复用，避免每次发布都多一次往返。
    #[serde(default)]
    protected_installation_token: String,
    #[serde(default)]
    installation_token_expires_at: String,
    /// 安装令牌是否具备 `contents: write`（来自签发响应的 permissions）。
    #[serde(default)]
    installation_contents_write: bool,
    /// App 私钥（PKCS#1 或 PKCS#8 PEM），用于签发 App JWT 换取安装令牌。
    #[serde(default)]
    protected_app_private_key: String,
}

/// 对外暴露的账号状态。永远不包含 token 本身。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubAccountStatus {
    pub authorized: bool,
    pub login: String,
    pub token_kind: String,
    /// `pat` 或 `app`，UI 用它决定展示哪种授权方式。
    pub auth_kind: String,
    pub app_client_id: String,
    pub installation_id: String,
    pub installation_account: String,
    /// 是否已配置 App 私钥（只暴露布尔值，绝不回显密钥内容）。
    pub private_key_configured: bool,
    pub repositories: Vec<String>,
    pub updated_at: String,
    /// 账号记录文件位置，便于用户确认凭据落在哪个档案下。
    pub store_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubAccount {
    pub login: String,
    pub token_kind: String,
    pub repositories: Vec<String>,
}

/// 保存 token。`login` 必须是调用 `GET /user` 校验过的真实登录名，
/// 避免把用户手填的名字当成身份来源。
pub(crate) fn set_account(
    login: &str,
    token: &str,
    token_kind: &str,
    repositories: &[String],
) -> Result<GithubAccountStatus, Box<dyn Error>> {
    if token.trim().is_empty() {
        return Err("GitHub token 不能为空".into());
    }
    if login.trim().is_empty() {
        return Err("GitHub 登录名不能为空".into());
    }
    let token_kind = match token_kind.trim() {
        TOKEN_KIND_CLASSIC => TOKEN_KIND_CLASSIC,
        "" => TOKEN_KIND_FINE_GRAINED,
        TOKEN_KIND_FINE_GRAINED => TOKEN_KIND_FINE_GRAINED,
        other => return Err(format!("GitHub token 类型无效: {other}").into()),
    };
    let mut repositories = repositories
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    repositories.sort();
    repositories.dedup();
    for repository in &repositories {
        if repository.split('/').count() != 2 {
            return Err(format!("GitHub 仓库必须是 owner/repo 形式: {repository}").into());
        }
    }
    // App 私钥与 client_id 属于本机配置，切回个人令牌时保留，避免用户重新导入。
    let previous = read_record()?.unwrap_or_default();
    let record = GithubAccountRecord {
        login: login.trim().to_string(),
        token_kind: token_kind.to_string(),
        protected_token: protect_secret_for_current_user(token.trim())?,
        repositories,
        updated_at: unix_timestamp_string(),
        // 保存个人令牌时切回 PAT 形态，并丢掉 App 形态的授权事实，避免两种凭据混用。
        auth_kind: AUTH_KIND_PAT.to_string(),
        app_client_id: previous.app_client_id,
        installation_id: String::new(),
        installation_account: String::new(),
        protected_user_token: String::new(),
        protected_refresh_token: String::new(),
        user_token_expires_at: String::new(),
        protected_installation_token: String::new(),
        installation_token_expires_at: String::new(),
        installation_contents_write: false,
        protected_app_private_key: previous.protected_app_private_key,
    };
    write_record(&record)?;
    Ok(status_from(Some(record)))
}

pub(crate) fn status() -> Result<GithubAccountStatus, Box<dyn Error>> {
    Ok(status_from(read_record()?))
}

pub(crate) fn account() -> Result<Option<GithubAccount>, Box<dyn Error>> {
    Ok(read_record()?.and_then(|record| {
        is_authorized(&record).then(|| GithubAccount {
            login: record.login,
            token_kind: record.token_kind,
            repositories: record.repositories,
        })
    }))
}

/// 解析当前有效的分发令牌。App 形态下优先复用未过期的安装令牌，必要时刷新；
/// 调用方（发布、安装、私仓读取）因此不需要关心凭据形态。
pub(crate) fn resolve_token() -> Result<Option<String>, Box<dyn Error>> {
    let Some(record) = read_record()? else {
        return Ok(None);
    };
    if !is_authorized(&record) {
        return Ok(None);
    }
    if record.auth_kind != AUTH_KIND_APP {
        return Ok(Some(unprotect_secret_for_current_user(
            &record.protected_token,
        )?));
    }
    crate::app::github_app::ensure_installation_token()
        .map(Some)
        .map_err(|error| error.into())
}

pub(crate) fn remove() -> Result<bool, Box<dyn Error>> {
    let path = store_path();
    let Some(record) = read_record()? else {
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        return Ok(false);
    };
    // App 私钥与 client_id 是本机配置，不随授权撤销一起删除：撤销的是「谁被授权」，
    // 私钥仍然只属于这台机器，重新授权后无需再导入一次。
    let keep_app_material = record.auth_kind == AUTH_KIND_APP
        && (!record.protected_app_private_key.trim().is_empty()
            || !record.app_client_id.trim().is_empty());
    if keep_app_material {
        write_record(&GithubAccountRecord {
            app_client_id: record.app_client_id,
            protected_app_private_key: record.protected_app_private_key,
            updated_at: unix_timestamp_string(),
            ..GithubAccountRecord::default()
        })?;
    } else if path.is_file() {
        std::fs::remove_file(&path)?;
    }
    Ok(true)
}

/// 读取 GitHub App 授权事实（token 已解密，只在进程内存中短暂存在）。
pub(crate) fn app_state() -> Result<Option<GithubAppRecord>, Box<dyn Error>> {
    let Some(record) = read_record()? else {
        return Ok(None);
    };
    if record.auth_kind != AUTH_KIND_APP {
        return Ok(None);
    }
    let user_token = if record.protected_user_token.trim().is_empty() {
        String::new()
    } else {
        unprotect_secret_for_current_user(&record.protected_user_token)?
    };
    let refresh_token = if record.protected_refresh_token.trim().is_empty() {
        String::new()
    } else {
        unprotect_secret_for_current_user(&record.protected_refresh_token)?
    };
    Ok(Some(GithubAppRecord {
        login: record.login,
        client_id: record.app_client_id,
        installation_id: record.installation_id,
        installation_account: record.installation_account,
        user_token,
        refresh_token,
        user_token_expires_at: record.user_token_expires_at,
    }))
}

/// 保存 GitHub App 授权事实；写入时统一加密，并清掉旧的安装令牌缓存。
pub(crate) fn save_app_state(record: &GithubAppRecord) -> Result<(), Box<dyn Error>> {
    let mut stored = read_record()?.unwrap_or_default();
    stored.auth_kind = AUTH_KIND_APP.to_string();
    stored.login = record.login.clone();
    stored.token_kind = "github_app".to_string();
    stored.app_client_id = record.client_id.clone();
    stored.installation_id = record.installation_id.clone();
    stored.installation_account = record.installation_account.clone();
    stored.protected_user_token = protect_secret_for_current_user(&record.user_token)?;
    stored.protected_refresh_token = if record.refresh_token.trim().is_empty() {
        String::new()
    } else {
        protect_secret_for_current_user(&record.refresh_token)?
    };
    stored.user_token_expires_at = record.user_token_expires_at.clone();
    stored.protected_installation_token = String::new();
    stored.installation_token_expires_at = String::new();
    stored.updated_at = unix_timestamp_string();
    write_record(&stored)
}

/// 读取缓存的安装令牌与过期时间（epoch 秒）。
pub(crate) fn cached_installation_token() -> Result<Option<(String, u64)>, Box<dyn Error>> {
    let Some(record) = read_record()? else {
        return Ok(None);
    };
    if record.protected_installation_token.trim().is_empty() {
        return Ok(None);
    }
    let expires_at = record
        .installation_token_expires_at
        .trim()
        .parse::<u64>()
        .unwrap_or_default();
    Ok(Some((
        unprotect_secret_for_current_user(&record.protected_installation_token)?,
        expires_at,
    )))
}

/// 保存 App 私钥（PEM 原文，DPAPI 加密落盘），并清掉旧的安装令牌缓存。
pub(crate) fn save_app_private_key(pem: &str) -> Result<(), Box<dyn Error>> {
    if pem.trim().is_empty() {
        return Err("App 私钥不能为空".into());
    }
    let mut stored = read_record()?.ok_or("GitHub App 尚未授权，请先完成设备流授权")?;
    stored.protected_app_private_key = protect_secret_for_current_user(pem.trim())?;
    stored.protected_installation_token = String::new();
    stored.installation_token_expires_at = String::new();
    stored.updated_at = unix_timestamp_string();
    write_record(&stored)
}

/// 读取 App 私钥原文，仅签发 JWT 时使用。
pub(crate) fn app_private_key() -> Result<Option<String>, Box<dyn Error>> {
    let Some(record) = read_record()? else {
        return Ok(None);
    };
    if record.protected_app_private_key.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(unprotect_secret_for_current_user(
        &record.protected_app_private_key,
    )?))
}

/// 缓存已签发的安装令牌，避免每次调用都重新换取。
pub(crate) fn cache_installation_token(
    token: &str,
    expires_at_epoch: u64,
    contents_write: bool,
) -> Result<(), Box<dyn Error>> {
    let mut stored = read_record()?.ok_or("GitHub 账号尚未授权")?;
    stored.protected_installation_token = protect_secret_for_current_user(token)?;
    stored.installation_token_expires_at = expires_at_epoch.to_string();
    stored.installation_contents_write = contents_write;
    write_record(&stored)
}

/// 当前凭据的写能力：`Some(true/false)` 表示已缓存的安装令牌自带 `contents: write`
/// 事实（来自签发响应），`None` 表示需要按个人令牌的方式探测仓库权限。
pub(crate) fn cached_contents_write() -> Result<Option<bool>, Box<dyn Error>> {
    let Some(record) = read_record()? else {
        return Ok(None);
    };
    if record.protected_installation_token.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(record.installation_contents_write))
}

pub(crate) fn store_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("github")
        .join(STORE_FILE)
}

fn write_record(record: &GithubAccountRecord) -> Result<(), Box<dyn Error>> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(record)?)?;
    Ok(())
}

fn read_record() -> Result<Option<GithubAccountRecord>, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read(&path)?;
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&raw)?))
}

fn status_from(record: Option<GithubAccountRecord>) -> GithubAccountStatus {
    let store_path = store_path();
    match record {
        Some(record) => {
            // 撤销授权后文件里可能只剩 App 私钥与 client_id（本机配置，与授权无关）。
            // 这种记录不算「已授权」，但要把可复用的配置回给 UI。
            let authorized = is_authorized(&record);
            let auth_kind = if record.auth_kind.trim().is_empty() {
                AUTH_KIND_PAT.to_string()
            } else {
                record.auth_kind
            };
            GithubAccountStatus {
                authorized,
                login: if authorized {
                    record.login
                } else {
                    String::new()
                },
                token_kind: if authorized {
                    record.token_kind
                } else {
                    String::new()
                },
                auth_kind,
                app_client_id: record.app_client_id,
                installation_id: if authorized {
                    record.installation_id
                } else {
                    String::new()
                },
                installation_account: if authorized {
                    record.installation_account
                } else {
                    String::new()
                },
                private_key_configured: !record.protected_app_private_key.trim().is_empty(),
                repositories: record.repositories,
                updated_at: record.updated_at,
                store_path: store_path.to_string_lossy().to_string(),
            }
        }
        None => GithubAccountStatus {
            authorized: false,
            login: String::new(),
            token_kind: String::new(),
            auth_kind: String::new(),
            app_client_id: String::new(),
            installation_id: String::new(),
            installation_account: String::new(),
            private_key_configured: false,
            repositories: Vec::new(),
            updated_at: String::new(),
            store_path: store_path.to_string_lossy().to_string(),
        },
    }
}

/// 记录里是否存在可用于分发的凭据。App 形态看 user token，PAT 形态看个人令牌。
fn is_authorized(record: &GithubAccountRecord) -> bool {
    if record.auth_kind == AUTH_KIND_APP {
        !record.protected_user_token.trim().is_empty()
    } else {
        !record.protected_token.trim().is_empty()
    }
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}
