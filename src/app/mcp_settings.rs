use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::{atomic_file, credentials};

const SCHEMA_VERSION: u32 = 1;
const RESERVED_SERVER_NAME: &str = "himind-agent";
const LEGACY_RESERVED_SERVER_NAME: &str = "himind";

fn is_reserved_server_name(value: &str) -> bool {
    value.eq_ignore_ascii_case(RESERVED_SERVER_NAME)
        || value.eq_ignore_ascii_case(LEGACY_RESERVED_SERVER_NAME)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct McpServerConfig {
    pub server_name: String,
    #[serde(default)]
    pub display_name: String,
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub tool_call_timeout_ms: u64,
    #[serde(default)]
    pub fail_on_startup_error: bool,
    #[serde(default = "default_reconnect")]
    pub reconnect: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct McpSettingsFile {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}
fn default_timeout() -> u64 {
    30_000
}
fn default_reconnect() -> bool {
    true
}
fn default_enabled() -> bool {
    true
}

pub(crate) fn settings_path(agent_state_path: &Path) -> PathBuf {
    agent_state_path.with_file_name("himind-ai-mcp.json")
}

pub(crate) fn load(agent_state_path: &Path) -> Result<Vec<McpServerConfig>, Box<dyn Error>> {
    let path = settings_path(agent_state_path);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let _lock = atomic_file::lock(&path)?;
    load_unlocked(&path)
}

fn load_unlocked(path: &Path) -> Result<Vec<McpServerConfig>, Box<dyn Error>> {
    let document = serde_json::from_slice::<McpSettingsFile>(&fs::read(path)?)?;
    if document.schema_version != SCHEMA_VERSION {
        return Err(format!("不支持的 MCP 配置版本: {}", document.schema_version).into());
    }
    let mut servers = document.servers;
    for server in &mut servers {
        reveal_secrets(server)?;
    }
    validate_all(&servers)?;
    Ok(servers)
}

fn save_unlocked(
    path: &Path,
    servers: &[McpServerConfig],
) -> Result<Vec<McpServerConfig>, Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut stored_servers = servers.to_vec();
    for server in &mut stored_servers {
        protect_secrets(server)?;
    }
    let document = McpSettingsFile {
        schema_version: SCHEMA_VERSION,
        servers: stored_servers,
    };
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(&document)?)?;
    Ok(servers.to_vec())
}

pub(crate) fn upsert(
    agent_state_path: &Path,
    mut server: McpServerConfig,
) -> Result<McpServerConfig, Box<dyn Error>> {
    normalize(&mut server);
    let path = settings_path(agent_state_path);
    let _lock = atomic_file::lock(&path)?;
    let mut servers = if path.is_file() {
        load_unlocked(&path)?
    } else {
        Vec::new()
    };
    if let Some(existing) = servers
        .iter_mut()
        .find(|item| item.server_name == server.server_name)
    {
        *existing = server.clone();
    } else {
        servers.push(server.clone());
    }
    validate_all(&servers)?;
    save_unlocked(&path, &servers)?;
    Ok(server)
}

pub(crate) fn remove(agent_state_path: &Path, server_name: &str) -> Result<bool, Box<dyn Error>> {
    let server_name = server_name.trim();
    if is_reserved_server_name(server_name) {
        return Err("HiMind 内置 MCP 服务不能删除".into());
    }
    let path = settings_path(agent_state_path);
    let _lock = atomic_file::lock(&path)?;
    let mut servers = if path.is_file() {
        load_unlocked(&path)?
    } else {
        Vec::new()
    };
    let before = servers.len();
    servers.retain(|item| item.server_name != server_name);
    if servers.len() == before {
        return Ok(false);
    }
    save_unlocked(&path, &servers)?;
    Ok(true)
}

pub(crate) fn validate_config(server: &McpServerConfig) -> Result<(), String> {
    validate(server).map_err(|error| error.to_string())
}

fn normalize(server: &mut McpServerConfig) {
    server.server_name = server.server_name.trim().to_string();
    server.display_name = server.display_name.trim().to_string();
    server.transport = server.transport.trim().to_ascii_lowercase();
    server.command = server.command.trim().to_string();
    server.cwd = server.cwd.trim().to_string();
    server.url = server.url.trim().to_string();
    server.args = server
        .args
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if server.display_name.is_empty() {
        server.display_name = server.server_name.clone();
    }
    if server.tool_call_timeout_ms == 0 {
        server.tool_call_timeout_ms = default_timeout();
    }
}

fn protect_secrets(server: &mut McpServerConfig) -> Result<(), Box<dyn Error>> {
    for value in server.env.values_mut().chain(server.headers.values_mut()) {
        if !value.is_empty() && !value.starts_with("dpapi:") {
            *value = credentials::protect_secret_for_current_user(value)?;
        }
    }
    Ok(())
}

fn reveal_secrets(server: &mut McpServerConfig) -> Result<(), Box<dyn Error>> {
    for value in server.env.values_mut().chain(server.headers.values_mut()) {
        if value.starts_with("dpapi:") {
            *value = credentials::unprotect_secret_for_current_user(value)?;
        }
    }
    Ok(())
}

fn validate_all(servers: &[McpServerConfig]) -> Result<(), Box<dyn Error>> {
    let mut names = std::collections::HashSet::new();
    for server in servers {
        validate(server)?;
        if !names.insert(server.server_name.to_ascii_lowercase()) {
            return Err(format!("MCP 服务名称重复: {}", server.server_name).into());
        }
    }
    Ok(())
}

fn validate(server: &McpServerConfig) -> Result<(), Box<dyn Error>> {
    let name = server.server_name.trim();
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("MCP 服务名称只能包含字母、数字、下划线和短横线，长度不超过 32 个字符".into());
    }
    if is_reserved_server_name(name) {
        return Err("himind-agent 是 HiMind 内置 MCP 服务名称，请换一个名称".into());
    }
    if !matches!(server.transport.as_str(), "stdio" | "streamable-http") {
        return Err("MCP 传输方式必须是 stdio 或 Streamable HTTP".into());
    }
    if server.transport == "stdio" {
        if server.command.trim().is_empty() {
            return Err("stdio MCP 服务需要填写启动命令".into());
        }
    } else {
        let url = server.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Streamable HTTP MCP 服务需要填写 http:// 或 https:// 地址".into());
        }
    }
    validate_map_keys(&server.env, "环境变量")?;
    validate_map_keys(&server.headers, "请求头")?;
    if server.tool_call_timeout_ms > 10 * 60 * 1000 {
        return Err("工具调用超时不能超过 10 分钟".into());
    }
    Ok(())
}

fn validate_map_keys(values: &BTreeMap<String, String>, label: &str) -> Result<(), Box<dyn Error>> {
    if values
        .keys()
        .any(|key| key.trim().is_empty() || key.contains(['=', '\0', '\r', '\n']))
    {
        return Err(format!("{label}名称无效").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-mcp-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root.join("agent-state.json")
    }

    fn stdio(name: &str) -> McpServerConfig {
        McpServerConfig {
            server_name: name.to_string(),
            display_name: "测试服务".to_string(),
            transport: "stdio".to_string(),
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            env: BTreeMap::new(),
            cwd: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            tool_call_timeout_ms: 30_000,
            fail_on_startup_error: false,
            reconnect: true,
            enabled: true,
        }
    }

    #[test]
    fn round_trips_and_rejects_reserved_name() {
        let path = state_path("roundtrip");
        let item = stdio("personal-tools");
        upsert(&path, item.clone()).unwrap();
        assert_eq!(load(&path).unwrap(), vec![item]);
        let stored = fs::read_to_string(settings_path(&path)).unwrap();
        assert!(stored.contains("\"schema_version\": 1"));
        assert!(upsert(&path, stdio("himind")).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validates_http_transport() {
        let mut item = stdio("remote");
        item.transport = "streamable-http".to_string();
        item.command.clear();
        assert!(validate_config(&item).is_err());
        item.url = "https://example.test/mcp".to_string();
        assert!(validate_config(&item).is_ok());
    }

    #[test]
    fn protects_environment_and_header_values_at_rest() {
        let path = state_path("secrets");
        let mut item = stdio("secure-tools");
        item.env
            .insert("API_KEY".to_string(), "secret-value".to_string());
        upsert(&path, item.clone()).unwrap();

        let stored = fs::read_to_string(settings_path(&path)).unwrap();
        assert!(!stored.contains("secret-value"));
        assert!(stored.contains("dpapi:v1:"));
        assert_eq!(load(&path).unwrap(), vec![item]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
