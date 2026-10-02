//! Aggregate enabled user MCP servers into the Agent MCP surface.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::mcp_probe;
use super::mcp_registry::{self, McpServerSpec, McpTransport};
use crate::capability::types::{CapabilityAvailability, CapabilityDescriptor};

const MAX_TOOL_PAGES: usize = 100;
const MAX_TOOLS: usize = 5000;
const MAX_CURSOR_LENGTH: usize = 512;
/// 下游进程重启、升级、换端口都会让调用瞬间失败。DSH 桥用的是 5 次 / 500ms
/// 起步 / 30s 封顶的退避，这里保持一致，避免两套重试手感不一样。
const RECONNECT_MAX_ATTEMPTS: u32 = 5;
const RECONNECT_INITIAL_DELAY_MS: u64 = 500;
const RECONNECT_MAX_DELAY_MS: u64 = 30_000;

/// 一次下游能力扫描的结果。
pub(crate) struct DownstreamCapabilityScan {
    pub(crate) capabilities: Vec<(CapabilityDescriptor, String)>,
    /// 标记为「必须可用」的下游连不上时的原因。非空说明这套工具面是残缺的。
    pub(crate) blocking_failures: Vec<String>,
}

/// 单次调用的失败原因。传输层失败可以重连重试；工具自己返回的错误说明请求已经送到，
/// 再试一次只会让用户多等几秒，所以直接抛给模型。
enum ToolCallFailure {
    Transport(Box<dyn Error>),
    Tool(String),
}

#[derive(Clone)]
pub(crate) struct DownstreamMcpManager {
    state_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, DownstreamSession>>>,
    capability_cache: Arc<Mutex<HashMap<String, CachedTools>>>,
}

struct CachedTools {
    fingerprint: String,
    tools: Vec<Value>,
}

struct DownstreamSession {
    fingerprint: String,
    session: DownstreamTransport,
}

enum DownstreamTransport {
    Stdio(mcp_probe::McpStdioSession),
    StreamableHttp(mcp_probe::McpHttpSession),
}

impl DownstreamTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, Box<dyn Error>> {
        match self {
            Self::Stdio(session) => session.request(method, params),
            Self::StreamableHttp(session) => session.request(method, params),
        }
    }
}

impl DownstreamMcpManager {
    pub(crate) fn new(state_path: &Path) -> Self {
        Self {
            state_path: state_path.to_path_buf(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            capability_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn list_capabilities(&self) -> Result<DownstreamCapabilityScan, Box<dyn Error>> {
        let servers = mcp_registry::list(&self.state_path)?;
        let mut capabilities = Vec::new();
        let mut blocking_failures = Vec::new();
        let mut ids = HashSet::new();
        for server in servers.into_iter().filter(|server| server.enabled) {
            let tools = match self.tools_for(&server) {
                Ok(tools) => tools,
                Err(error) => {
                    // 默认情况下下游工具是可选的，连不上就少一套工具。
                    // 但用户显式要求「必须可用」时不能再装作没事：少一套工具
                    // 会让模型拿着残缺的能力面继续回答，比直接报错更难排查。
                    if server.fail_on_startup_error {
                        blocking_failures.push(format!(
                            "downstream_mcp_required_unavailable: {} ({error})",
                            server.display_name
                        ));
                    }
                    continue;
                }
            };
            for tool in tools {
                let original_name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or("tool");
                let id = unique_tool_id(&server.stable_id, original_name, &mut ids);
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("下游 MCP 工具")
                    .to_string();
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" }));
                capabilities.push((
                    CapabilityDescriptor {
                        id,
                        version: "mcp-1.0.0".to_string(),
                        name: format!("{} / {}", server.display_name, original_name),
                        description,
                        risk_level: "mcp_downstream".to_string(),
                        source: format!("mcp:{}", server.stable_id),
                        contract_source: format!("mcp:{}:discovery", server.stable_id),
                        contract_generation: None,
                        availability: match server.transport {
                            McpTransport::Stdio => CapabilityAvailability::Local,
                            McpTransport::StreamableHttp => CapabilityAvailability::NetworkService,
                        },
                        execution_mode: "provider_defined".to_string(),
                        supports_progress: false,
                        supports_cancel: false,
                        idempotency: "provider_defined".to_string(),
                        retry_policy: "provider_defined".to_string(),
                        concurrency: "provider_defined".to_string(),
                        // Third-party tools have no trusted HiMind risk
                        // contract. They remain manual-only until an explicit
                        // governed descriptor can prove a lower risk tier.
                        approval_required: true,
                        dashboard_provider: false,
                        required_scope: None,
                        dashboard_route: None,
                        input_schema,
                    },
                    original_name.to_string(),
                ));
            }
        }
        Ok(DownstreamCapabilityScan {
            capabilities,
            blocking_failures,
        })
    }

    pub(crate) fn invoke(
        &self,
        capability_id: &str,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        let servers = mcp_registry::list(&self.state_path)?;
        let mut ids = HashSet::new();
        for server in servers.into_iter().filter(|server| server.enabled) {
            let tools = match self.tools_for(&server) {
                Ok(tools) => tools,
                // 别的下游掉线不该挡住这次调用；只有「必须可用」的连接才升级成错误。
                Err(error) => {
                    if server.fail_on_startup_error {
                        return Err(error);
                    }
                    continue;
                }
            };
            for tool in tools {
                let original_name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or("tool");
                let id = unique_tool_id(&server.stable_id, original_name, &mut ids);
                if id != capability_id {
                    continue;
                }
                return self.invoke_tool(&server, original_name, input);
            }
        }
        Err(format!("downstream MCP tool not found: {capability_id}").into())
    }

    fn invoke_tool(
        &self,
        server: &McpServerSpec,
        tool_name: &str,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        // 「断开后自动重连」开着时按退避重试，关掉就是一次失败一次报错。
        let attempts = reconnect_attempts(server.reconnect);
        let mut delay_ms = RECONNECT_INITIAL_DELAY_MS;
        let mut last_error: Option<Box<dyn Error>> = None;
        for attempt in 1..=attempts {
            match self.call_tool_once(server, tool_name, &input) {
                Ok(result) => return Ok(result),
                // 工具已经收到请求并给出了错误，重试没有意义。
                Err(ToolCallFailure::Tool(message)) => return Err(message.into()),
                Err(ToolCallFailure::Transport(error)) => {
                    last_error = Some(error);
                    self.drop_session(&server.stable_id)?;
                    if attempt == attempts {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = next_retry_delay(delay_ms);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| "downstream MCP call failed".into()))
    }

    fn call_tool_once(
        &self,
        server: &McpServerSpec,
        tool_name: &str,
        input: &Value,
    ) -> Result<Value, ToolCallFailure> {
        // 会话可能是上次失败时被丢掉的，先补一次连接，避免第一次调用必然失败。
        self.tools_for(server).map_err(ToolCallFailure::Transport)?;
        let response = {
            let mut sessions = self.sessions.lock().map_err(|_| {
                ToolCallFailure::Transport("downstream MCP session lock poisoned".into())
            })?;
            let entry = sessions.get_mut(&server.stable_id).ok_or_else(|| {
                ToolCallFailure::Transport("downstream MCP session is not connected".into())
            })?;
            entry.session.request(
                "tools/call",
                json!({ "name": tool_name, "arguments": input.clone() }),
            )
        }
        .map_err(ToolCallFailure::Transport)?;
        if let Some(error) = response.get("error") {
            return Err(ToolCallFailure::Tool(format!(
                "downstream_tool_failed: {error}"
            )));
        }
        Ok(response.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    fn drop_session(&self, stable_id: &str) -> Result<(), Box<dyn Error>> {
        self.sessions
            .lock()
            .map_err(|_| "downstream MCP session lock poisoned")?
            .remove(stable_id);
        self.remove_cached_tools(stable_id)
    }

    fn tools_for(&self, server: &McpServerSpec) -> Result<Vec<Value>, Box<dyn Error>> {
        let fingerprint = format!("{server:?}");
        {
            let cache = self
                .capability_cache
                .lock()
                .map_err(|_| "downstream MCP capability cache lock poisoned")?;
            if let Some(entry) = cache.get(&server.stable_id) {
                if entry.fingerprint == fingerprint {
                    return Ok(entry.tools.clone());
                }
            }
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "downstream MCP session lock poisoned")?;
        if let Some(entry) = sessions.get_mut(&server.stable_id) {
            if entry.fingerprint == fingerprint {
                match list_tools(&mut entry.session) {
                    Ok(tools) => return Ok(tools),
                    Err(error) => {
                        sessions.remove(&server.stable_id);
                        self.remove_cached_tools(&server.stable_id)?;
                        return Err(error);
                    }
                }
            }
            sessions.remove(&server.stable_id);
        }
        let mut session = match server.transport {
            McpTransport::Stdio => {
                DownstreamTransport::Stdio(mcp_probe::McpStdioSession::connect(server)?)
            }
            McpTransport::StreamableHttp => {
                DownstreamTransport::StreamableHttp(mcp_probe::McpHttpSession::connect(server)?)
            }
        };
        let tools = list_tools(&mut session)?;
        self.capability_cache
            .lock()
            .map_err(|_| "downstream MCP capability cache lock poisoned")?
            .insert(
                server.stable_id.clone(),
                CachedTools {
                    fingerprint: fingerprint.clone(),
                    tools: tools.clone(),
                },
            );
        sessions.insert(
            server.stable_id.clone(),
            DownstreamSession {
                fingerprint,
                session,
            },
        );
        Ok(tools)
    }

    fn remove_cached_tools(&self, stable_id: &str) -> Result<(), Box<dyn Error>> {
        self.capability_cache
            .lock()
            .map_err(|_| "downstream MCP capability cache lock poisoned")?
            .remove(stable_id);
        Ok(())
    }
}

fn list_tools(session: &mut DownstreamTransport) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut tools = Vec::new();
    let mut cursor = None::<String>;
    let mut seen_cursors = HashSet::new();
    for _ in 0..MAX_TOOL_PAGES {
        let params = cursor
            .as_ref()
            .map(|value| json!({ "cursor": value }))
            .unwrap_or_else(|| json!({}));
        let response = session.request("tools/list", params)?;
        let (page_tools, next_cursor) = parse_tools_page(&response)?;
        if tools.len().saturating_add(page_tools.len()) > MAX_TOOLS {
            return Err(format!(
                "tools_list_failed: downstream MCP returned more than {MAX_TOOLS} tools"
            )
            .into());
        }
        tools.extend(page_tools);
        let Some(next_cursor) = next_cursor else {
            return Ok(tools);
        };
        if next_cursor.is_empty() || next_cursor.len() > MAX_CURSOR_LENGTH {
            return Err(
                "tools_list_failed: downstream MCP returned an invalid pagination cursor".into(),
            );
        }
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err("tools_list_failed: downstream MCP pagination cursor repeated".into());
        }
        cursor = Some(next_cursor);
    }
    Err(format!("tools_list_failed: downstream MCP exceeded {MAX_TOOL_PAGES} tool pages").into())
}

fn parse_tools_page(response: &Value) -> Result<(Vec<Value>, Option<String>), Box<dyn Error>> {
    if let Some(error) = response.get("error") {
        return Err(format!("tools_list_failed: {error}").into());
    }
    let tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            Box::<dyn Error>::from("tools_list_failed: MCP response did not contain result.tools")
        })?;
    let next_cursor = response
        .pointer("/result/nextCursor")
        .or_else(|| response.pointer("/result/next_cursor"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((tools, next_cursor))
}

fn unique_tool_id(server_id: &str, tool_name: &str, ids: &mut HashSet<String>) -> String {
    let base = format!(
        "mcp.{}.{}",
        safe_segment(server_id),
        safe_segment(tool_name)
    );
    if ids.insert(base.clone()) {
        return base;
    }
    let mut index = 2;
    loop {
        let candidate = format!("{base}_{index}");
        if ids.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

/// 断线后最多重试几次。没开重连就只试一次，失败直接报给调用方。
fn reconnect_attempts(reconnect: bool) -> u32 {
    if reconnect {
        RECONNECT_MAX_ATTEMPTS
    } else {
        1
    }
}

fn next_retry_delay(current_ms: u64) -> u64 {
    (current_ms * 2).min(RECONNECT_MAX_DELAY_MS)
}

fn safe_segment(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() {
        output.push_str("tool");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        next_retry_delay, parse_tools_page, reconnect_attempts, safe_segment, DownstreamMcpManager,
    };
    use crate::app::mcp_registry::McpServerConfig;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn downstream_server(
        name: &str,
        fail_on_startup_error: bool,
        reconnect: bool,
    ) -> McpServerConfig {
        McpServerConfig {
            server_name: name.to_string(),
            display_name: name.to_string(),
            transport: "stdio".to_string(),
            // 一个不存在的可执行文件名：连接必然失败，而且失败得很快。
            command: "himind-agent-missing-binary-for-test".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            tool_call_timeout_ms: 30_000,
            fail_on_startup_error,
            reconnect,
            enabled: true,
        }
    }

    fn temp_state(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-downstream-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root.join("agent-state.json")
    }

    #[test]
    fn reconnect_policy_matches_the_declared_bridge_defaults() {
        assert_eq!(reconnect_attempts(true), 5);
        assert_eq!(reconnect_attempts(false), 1);
        assert_eq!(next_retry_delay(500), 1_000);
        assert_eq!(next_retry_delay(16_000), 30_000);
        assert_eq!(next_retry_delay(30_000), 30_000);
    }

    #[test]
    fn required_downstream_failures_are_reported_instead_of_disappearing() {
        let state = temp_state("required");
        crate::app::mcp_settings::upsert(&state, downstream_server("required-tools", true, false))
            .unwrap();
        let scan = DownstreamMcpManager::new(&state)
            .list_capabilities()
            .unwrap();
        assert!(scan.capabilities.is_empty());
        assert_eq!(scan.blocking_failures.len(), 1);
        assert!(scan.blocking_failures[0].starts_with("downstream_mcp_required_unavailable"));
        assert!(scan.blocking_failures[0].contains("required-tools"));

        // 同一个连接改成可选后，缺一套工具只是缺工具，不算错误。
        crate::app::mcp_settings::upsert(&state, downstream_server("required-tools", false, false))
            .unwrap();
        let scan = DownstreamMcpManager::new(&state)
            .list_capabilities()
            .unwrap();
        assert!(scan.capabilities.is_empty());
        assert!(scan.blocking_failures.is_empty());
        let _ = std::fs::remove_dir_all(state.parent().unwrap());
    }

    #[test]
    fn tool_segments_are_stable_and_safe() {
        assert_eq!(safe_segment("hello world"), "hello_world");
        assert_eq!(safe_segment("工具"), "__");
        assert_eq!(safe_segment(""), "tool");
    }

    #[test]
    fn parses_tool_page_cursor_in_both_mcp_spellings() {
        let (tools, cursor) = parse_tools_page(&json!({
            "result": { "tools": [{ "name": "one" }], "nextCursor": "next" }
        }))
        .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(cursor.as_deref(), Some("next"));

        let (_, cursor) = parse_tools_page(&json!({
            "result": { "tools": [], "next_cursor": "legacy" }
        }))
        .unwrap();
        assert_eq!(cursor.as_deref(), Some("legacy"));
    }

    #[test]
    fn rejects_tool_page_without_tools() {
        let error = parse_tools_page(&json!({ "result": {} })).unwrap_err();
        assert!(error.to_string().contains("result.tools"));
    }
}
