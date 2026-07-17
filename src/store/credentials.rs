use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::types::{StoredInnerAdminCredentials, StoredSvnConnection};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct LocalEditorSettings {
    unity_editor_path: String,
}

pub(crate) fn local_unity_editor_settings() -> Result<Value, Box<dyn Error>> {
    let saved = load_local_editor_settings()?.unity_editor_path;
    let (path, source) = if !saved.trim().is_empty() {
        (saved, "agent")
    } else if let Some(path) = unity_editor_environment_path() {
        (path, "environment")
    } else {
        (String::new(), "automatic")
    };
    Ok(json!({
        "unity_editor_path": path,
        "source": source,
        "valid": !path.is_empty() && PathBuf::from(&path).is_file()
    }))
}

pub(crate) fn configured_unity_editor_path() -> Option<String> {
    let settings = load_local_editor_settings().ok()?;
    let path = settings.unity_editor_path.trim();
    (!path.is_empty() && PathBuf::from(path).is_file()).then(|| path.to_string())
}

pub(crate) fn save_local_unity_editor_path(path: &str) -> Result<Value, Box<dyn Error>> {
    let normalized = path.trim();
    if !normalized.is_empty() {
        let editor = PathBuf::from(normalized);
        if !editor.is_file()
            || !editor
                .file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case("Unity.exe"))
                .unwrap_or(false)
        {
            return Err("请选择有效的 Unity.exe".into());
        }
    }
    fs::write(
        editor_settings_path()?,
        serde_json::to_vec(&LocalEditorSettings {
            unity_editor_path: normalized.to_string(),
        })?,
    )?;
    local_unity_editor_settings()
}

pub(crate) fn unity_editor_environment_path() -> Option<String> {
    [
        "unity_art_editor",
        "uniart_ediotr",
        "PROJECT_DASHBOARD_UNITY_EDITOR",
    ]
    .into_iter()
    .find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn local_login_status_value() -> &'static str {
    if stored_inner_admin_account().is_some() {
        "credentials_configured"
    } else {
        "credentials_missing"
    }
}

pub(crate) fn local_login_status_json() -> Value {
    let account = stored_inner_admin_account();
    let configured = account.is_some();
    json!({
        "status": local_login_status_value(),
        "authenticated": configured,
        "label": if configured { "已保存内网登录" } else { "未登录内网账号" },
        "owner": "agent",
        "account": account,
        "secure_storage": if configured { "agent_local_profile" } else { "not_configured" },
        "login_url": format!("{}/admin/personal/software_code", inner_admin_base())
    })
}

pub(crate) fn save_local_inner_admin_credentials(
    username: &str,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    let normalized_username = username.trim();
    if normalized_username.is_empty() {
        return Err("inner admin username is required".into());
    }
    if password.trim().is_empty() {
        return Err("inner admin password is required".into());
    }
    let encrypted_password = protect_secret_for_current_user(password)?;
    let payload = StoredInnerAdminCredentials {
        username: normalized_username.to_string(),
        encrypted_password,
    };
    let path = inner_admin_credentials_path()?;
    fs::write(path, serde_json::to_vec(&payload)?)?;
    Ok(())
}

pub(crate) fn clear_local_inner_admin_credentials() -> Result<(), Box<dyn Error>> {
    let path = inner_admin_credentials_path()?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn list_local_svn_connections() -> Result<Vec<StoredSvnConnection>, Box<dyn Error>> {
    let path = svn_connections_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(crate) fn save_local_svn_connection(
    id: &str,
    name: &str,
    base_url: &str,
    username: &str,
    password: &str,
    provider: &str,
) -> Result<(), Box<dyn Error>> {
    let mut connections = list_local_svn_connections()?;
    let encrypted_password = if password.is_empty() {
        connections
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.encrypted_password.clone())
            .ok_or("SVN password is required for a new connection")?
    } else {
        protect_secret_for_current_user(password)?
    };
    let connection = StoredSvnConnection {
        id: id.to_string(),
        name: name.to_string(),
        base_url: base_url.to_string(),
        username: username.to_string(),
        encrypted_password,
        provider: provider.to_string(),
        status: "configured".to_string(),
        last_error: String::new(),
    };
    if let Some(existing) = connections.iter_mut().find(|item| item.id == id) {
        *existing = connection;
    } else {
        connections.push(connection);
    }
    connections.sort_by(|left, right| left.id.cmp(&right.id));
    fs::write(svn_connections_path()?, serde_json::to_vec(&connections)?)?;
    Ok(())
}

pub(crate) fn update_local_svn_connection_status(
    id: &str,
    status: &str,
    last_error: &str,
) -> Result<(), Box<dyn Error>> {
    let mut connections = list_local_svn_connections()?;
    let connection = connections
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("SVN connection not found: {id}"))?;
    connection.status = status.to_string();
    connection.last_error = last_error.to_string();
    fs::write(svn_connections_path()?, serde_json::to_vec(&connections)?)?;
    Ok(())
}

pub(crate) fn remove_local_svn_connection(id: &str) -> Result<bool, Box<dyn Error>> {
    let mut connections = list_local_svn_connections()?;
    let previous_len = connections.len();
    connections.retain(|item| item.id != id);
    if connections.len() == previous_len {
        return Ok(false);
    }
    fs::write(svn_connections_path()?, serde_json::to_vec(&connections)?)?;
    Ok(true)
}

pub(crate) fn load_local_svn_connection_secret(
    id: &str,
) -> Result<(StoredSvnConnection, String), Box<dyn Error>> {
    let connection = list_local_svn_connections()?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("SVN connection not found: {id}"))?;
    let password = unprotect_secret_for_current_user(&connection.encrypted_password)?;
    Ok((connection, password))
}

pub(crate) fn load_inner_admin_credentials(
    allow_env_fallback: bool,
) -> Result<(String, String), Box<dyn Error>> {
    if allow_env_fallback {
        if let (Ok(username), Ok(password)) = (
            env::var("INNER_ADMIN_USERNAME").or_else(|_| env::var("ANDA_USERNAME")),
            env::var("INNER_ADMIN_PASSWORD").or_else(|_| env::var("ANDA_PASSWORD")),
        ) {
            if !username.trim().is_empty() && !password.trim().is_empty() {
                return Ok((username, password));
            }
        }
    }
    load_local_inner_admin_credentials()
}

fn inner_admin_base() -> String {
    env::var("INNER_ADMIN_BASE_URL")
        .unwrap_or_else(|_| "http://172.16.0.197:8086".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn inner_admin_credentials_path() -> Result<PathBuf, Box<dyn Error>> {
    let local_app_data = env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(local_app_data).join("project-dashboard-agent");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("inner-admin-credentials.json"))
}

fn svn_connections_path() -> Result<PathBuf, Box<dyn Error>> {
    let local_app_data = env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(local_app_data).join("project-dashboard-agent");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("svn-connections.json"))
}

fn editor_settings_path() -> Result<PathBuf, Box<dyn Error>> {
    let local_app_data = env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(local_app_data).join("project-dashboard-agent");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("editor-settings.json"))
}

fn load_local_editor_settings() -> Result<LocalEditorSettings, Box<dyn Error>> {
    let path = editor_settings_path()?;
    if !path.exists() {
        return Ok(LocalEditorSettings {
            unity_editor_path: String::new(),
        });
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn load_local_inner_admin_credentials() -> Result<(String, String), Box<dyn Error>> {
    let path = inner_admin_credentials_path()?;
    if !path.exists() {
        return Err(
            "请先在 Dashboard 的本地 Agent 应用区域登录内网账号，再执行同步或上传任务".into(),
        );
    }
    let stored: StoredInnerAdminCredentials = serde_json::from_slice(&fs::read(&path)?)?;
    if stored.username.trim().is_empty() {
        return Err("本地 Agent 已保存的内网账号无效，请重新登录".into());
    }
    let password = unprotect_secret_for_current_user(&stored.encrypted_password)?;
    if password.trim().is_empty() {
        return Err("本地 Agent 已保存的内网密码无效，请重新登录".into());
    }
    Ok((stored.username.trim().to_string(), password))
}

fn stored_inner_admin_account() -> Option<String> {
    let path = inner_admin_credentials_path().ok()?;
    let content = fs::read(&path).ok()?;
    let stored: StoredInnerAdminCredentials = serde_json::from_slice(&content).ok()?;
    let username = stored.username.trim();
    if username.is_empty() {
        None
    } else {
        Some(username.to_string())
    }
}

fn protect_secret_for_current_user(secret: &str) -> Result<String, Box<dyn Error>> {
    run_powershell_script(
        r#"$plain = [Console]::In.ReadToEnd(); $secure = ConvertTo-SecureString $plain -AsPlainText -Force; ConvertFrom-SecureString $secure"#,
        secret,
    )
}

fn unprotect_secret_for_current_user(secret: &str) -> Result<String, Box<dyn Error>> {
    run_powershell_script(
        r#"$encrypted = [Console]::In.ReadToEnd(); $secure = ConvertTo-SecureString $encrypted; $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure); try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }"#,
        secret,
    )
}

fn run_powershell_script(script: &str, stdin_payload: &str) -> Result<String, Box<dyn Error>> {
    let mut last_error = String::new();
    for shell in ["pwsh", "powershell"] {
        let mut child = match Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_payload.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    if last_error.is_empty() {
        last_error = "PowerShell unavailable".to_string();
    }
    Err(format!("failed to access local credential store: {last_error}").into())
}
