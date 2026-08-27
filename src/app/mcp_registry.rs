//! Unified MCP server registry.
//!
//! The first version of HiMind AI stored personal MCP servers in
//! `himind-ai-mcp.json`.  That file is still the on-disk compatibility
//! format, but all consumers should use this module so that DSH, the local
//! MCP bridge, the UI, and future client adapters share one model.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use super::mcp_settings;
pub(crate) use super::mcp_settings::McpServerConfig;

pub(crate) const AGENT_SERVER_ID: &str = "himind-agent";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum McpTransport {
    Stdio,
    StreamableHttp,
}

impl McpTransport {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable-http",
        }
    }
}

impl From<&str> for McpTransport {
    fn from(value: &str) -> Self {
        if value.eq_ignore_ascii_case("streamable-http")
            || value.eq_ignore_ascii_case("streamable_http")
            || value.eq_ignore_ascii_case("http")
        {
            Self::StreamableHttp
        } else {
            Self::Stdio
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpScope {
    User,
    Workspace,
    Organization,
}

impl Default for McpScope {
    fn default() -> Self {
        Self::User
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpServerSpec {
    pub stable_id: String,
    pub display_name: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Values are kept in memory only. The persisted compatibility file uses
    /// the Agent credential store to protect these values with DPAPI.
    #[serde(default, skip_serializing)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub tool_call_timeout_ms: u64,
    #[serde(default)]
    pub fail_on_startup_error: bool,
    #[serde(default = "default_reconnect")]
    pub reconnect: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub scope: McpScope,
    #[serde(default)]
    pub source: String,
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

impl McpServerSpec {
    pub(crate) fn from_config(config: McpServerConfig) -> Self {
        Self {
            stable_id: config.server_name,
            display_name: config.display_name,
            transport: McpTransport::from(config.transport.as_str()),
            command: config.command,
            args: config.args,
            env: config.env,
            cwd: config.cwd,
            url: config.url,
            headers: config.headers,
            tool_call_timeout_ms: config.tool_call_timeout_ms,
            fail_on_startup_error: config.fail_on_startup_error,
            reconnect: config.reconnect,
            enabled: config.enabled,
            scope: McpScope::User,
            source: "user".to_string(),
        }
    }

    pub(crate) fn into_config(self) -> McpServerConfig {
        McpServerConfig {
            server_name: self.stable_id,
            display_name: self.display_name,
            transport: self.transport.as_str().to_string(),
            command: self.command,
            args: self.args,
            env: self.env,
            cwd: self.cwd,
            url: self.url,
            headers: self.headers,
            tool_call_timeout_ms: self.tool_call_timeout_ms,
            fail_on_startup_error: self.fail_on_startup_error,
            reconnect: self.reconnect,
            enabled: self.enabled,
        }
    }

    pub(crate) fn public_json(&self) -> Value {
        json!({
            "stable_id": self.stable_id,
            "display_name": self.display_name,
            "transport": self.transport.as_str(),
            "command": self.command,
            "args": redacted_args(&self.args),
            "cwd": self.cwd,
            "url": public_url(&self.url),
            "tool_call_timeout_ms": self.tool_call_timeout_ms,
            "fail_on_startup_error": self.fail_on_startup_error,
            "reconnect": self.reconnect,
            "enabled": self.enabled,
            "scope": self.scope,
            "source": self.source,
            "env_keys": self.env.keys().collect::<Vec<_>>(),
            "header_keys": self.headers.keys().collect::<Vec<_>>(),
        })
    }
}

fn redacted_args(args: &[String]) -> Vec<String> {
    let mut redact_next = false;
    args.iter()
        .map(|argument| {
            let lower = argument.to_ascii_lowercase();
            if redact_next {
                redact_next = false;
                return "[REDACTED]".to_string();
            }
            if lower.contains("token")
                || lower.contains("api-key")
                || lower.contains("apikey")
                || lower.contains("password")
                || lower.contains("secret")
            {
                if !argument.contains('=') {
                    redact_next = true;
                    return argument.clone();
                }
                let key = argument.split('=').next().unwrap_or(argument);
                return format!("{key}=[REDACTED]");
            }
            argument.clone()
        })
        .collect()
}

fn public_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    if url.username() != "" {
        let _ = url.set_username("");
    }
    let _ = url.set_password(None);
    if url.query().is_some() {
        url.set_query(Some("[REDACTED]"));
    }
    url.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpRegistrationAction {
    Create,
    Update,
    Remove,
    Noop,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpRegistrationPlan {
    pub target_id: String,
    pub action: McpRegistrationAction,
    pub write_required: bool,
    pub backup_required: bool,
    pub restart_required: bool,
    pub configured_server_id: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpRegistrySnapshot {
    pub schema_version: u32,
    pub servers: Vec<Value>,
}

pub(crate) fn list(agent_state_path: &Path) -> Result<Vec<McpServerSpec>, Box<dyn Error>> {
    Ok(mcp_settings::load(agent_state_path)?
        .into_iter()
        .map(McpServerSpec::from_config)
        .collect())
}

/// Return the wire-compatible configuration model for the native HiMind AI
/// editor. Callers outside this module should use this facade instead of
/// reaching into the persistence implementation in `mcp_settings`.
pub(crate) fn list_configs(
    agent_state_path: &Path,
) -> Result<Vec<McpServerConfig>, Box<dyn Error>> {
    Ok(list(agent_state_path)?
        .into_iter()
        .map(McpServerSpec::into_config)
        .collect())
}

pub(crate) fn get(
    agent_state_path: &Path,
    stable_id: &str,
) -> Result<Option<McpServerSpec>, Box<dyn Error>> {
    let stable_id = stable_id.trim();
    Ok(list(agent_state_path)?
        .into_iter()
        .find(|server| server.stable_id == stable_id))
}

pub(crate) fn upsert(
    agent_state_path: &Path,
    server: McpServerSpec,
) -> Result<McpServerSpec, Box<dyn Error>> {
    let config = mcp_settings::upsert(agent_state_path, server.into_config())?;
    Ok(McpServerSpec::from_config(config))
}

pub(crate) fn upsert_config(
    agent_state_path: &Path,
    server: McpServerConfig,
) -> Result<McpServerConfig, Box<dyn Error>> {
    Ok(upsert(agent_state_path, McpServerSpec::from_config(server))?.into_config())
}

pub(crate) fn remove(agent_state_path: &Path, stable_id: &str) -> Result<bool, Box<dyn Error>> {
    Ok(mcp_settings::remove(agent_state_path, stable_id)?)
}

pub(crate) fn remove_config(
    agent_state_path: &Path,
    stable_id: &str,
) -> Result<bool, Box<dyn Error>> {
    remove(agent_state_path, stable_id)
}

pub(crate) fn settings_path(agent_state_path: &Path) -> std::path::PathBuf {
    mcp_settings::settings_path(agent_state_path)
}

pub(crate) fn validate_config(server: &McpServerConfig) -> Result<(), String> {
    validate(&McpServerSpec::from_config(server.clone()))
}

/// Return a JSON-safe snapshot for UI, CLI and MCP callers. Secret values are
/// intentionally represented only by their key names.
pub(crate) fn public_snapshot(
    agent_state_path: &Path,
) -> Result<McpRegistrySnapshot, Box<dyn Error>> {
    let servers = list(agent_state_path)?
        .iter()
        .map(McpServerSpec::public_json)
        .collect();
    Ok(McpRegistrySnapshot {
        schema_version: 1,
        servers,
    })
}

pub(crate) fn inspect(agent_state_path: &Path, stable_id: &str) -> Result<Value, Box<dyn Error>> {
    get(agent_state_path, stable_id)?
        .map(|server| server.public_json())
        .ok_or_else(|| format!("MCP server not found: {stable_id}").into())
}

pub(crate) fn validate(server: &McpServerSpec) -> Result<(), String> {
    mcp_settings::validate_config(&server.clone().into_config())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    fn state_path() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-mcp-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root.join("agent-state.json")
    }

    fn spec() -> McpServerSpec {
        McpServerSpec {
            stable_id: "example-tools".into(),
            display_name: "Example".into(),
            transport: McpTransport::Stdio,
            command: "node".into(),
            args: vec!["server.js".into()],
            env: BTreeMap::from([(String::from("TOKEN"), String::from("secret"))]),
            cwd: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            tool_call_timeout_ms: 30_000,
            fail_on_startup_error: false,
            reconnect: true,
            enabled: true,
            scope: McpScope::User,
            source: "user".into(),
        }
    }

    #[test]
    fn registry_round_trip_keeps_legacy_file_compatible() {
        let path = state_path();
        let stored = upsert(&path, spec()).unwrap();
        assert_eq!(stored.stable_id, "example-tools");
        let loaded = get(&path, "example-tools").unwrap().unwrap();
        assert_eq!(loaded.env.get("TOKEN"), Some(&"secret".to_string()));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn public_snapshot_never_contains_secret_values() {
        let path = state_path();
        upsert(&path, spec()).unwrap();
        let snapshot = serde_json::to_string(&public_snapshot(&path).unwrap()).unwrap();
        assert!(snapshot.contains("TOKEN"));
        assert!(!snapshot.contains("secret"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn public_snapshot_redacts_secret_like_arguments() {
        let mut item = spec();
        item.args = vec!["--token".into(), "very-secret".into(), "--mode=fast".into()];
        let public = item.public_json().to_string();
        assert!(!public.contains("very-secret"));
        assert!(public.contains("[REDACTED]"));
    }
}
