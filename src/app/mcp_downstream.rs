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

#[derive(Clone)]
pub(crate) struct DownstreamMcpManager {
    state_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, DownstreamSession>>>,
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
        }
    }

    pub(crate) fn list_capabilities(
        &self,
    ) -> Result<Vec<(CapabilityDescriptor, String)>, Box<dyn Error>> {
        // DSH receives user MCP rows through its native configuration overlay.
        // Do not expose the same rows a second time through the Agent bridge.
        if is_native_dsh_client() {
            return Ok(Vec::new());
        }
        let servers = mcp_registry::list(&self.state_path)?;
        let mut result = Vec::new();
        let mut ids = HashSet::new();
        for server in servers.into_iter().filter(|server| server.enabled) {
            let tools = match self.tools_for(&server) {
                Ok(tools) => tools,
                Err(_) => continue,
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
                result.push((
                    CapabilityDescriptor {
                        id,
                        version: "mcp-1.0.0".to_string(),
                        name: format!("{} / {}", server.display_name, original_name),
                        description,
                        risk_level: "mcp_downstream".to_string(),
                        source: format!("mcp:{}", server.stable_id),
                        availability: match server.transport {
                            McpTransport::Stdio => CapabilityAvailability::Local,
                            McpTransport::StreamableHttp => CapabilityAvailability::NetworkService,
                        },
                        input_schema,
                    },
                    original_name.to_string(),
                ));
            }
        }
        Ok(result)
    }

    pub(crate) fn invoke(
        &self,
        capability_id: &str,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        if is_native_dsh_client() {
            return Err("下游 MCP 已由 HiMind AI 原生配置管理".into());
        }
        let servers = mcp_registry::list(&self.state_path)?;
        let mut ids = HashSet::new();
        for server in servers.into_iter().filter(|server| server.enabled) {
            let tools = self.tools_for(&server)?;
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
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "downstream MCP session lock poisoned")?;
        let entry = sessions
            .get_mut(&server.stable_id)
            .ok_or("downstream MCP session is not connected")?;
        let response = entry.session.request(
            "tools/call",
            json!({ "name": tool_name, "arguments": input }),
        );
        match response {
            Ok(response) => {
                if let Some(error) = response.get("error") {
                    sessions.remove(&server.stable_id);
                    return Err(format!("downstream_tool_failed: {error}").into());
                }
                Ok(response.get("result").cloned().unwrap_or_else(|| json!({})))
            }
            Err(error) => {
                sessions.remove(&server.stable_id);
                Err(error)
            }
        }
    }

    fn tools_for(&self, server: &McpServerSpec) -> Result<Vec<Value>, Box<dyn Error>> {
        let fingerprint = format!("{server:?}");
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
        sessions.insert(
            server.stable_id.clone(),
            DownstreamSession {
                fingerprint,
                session,
            },
        );
        Ok(tools)
    }
}

fn is_native_dsh_client() -> bool {
    std::env::var("HIMIND_AI_CLIENT_ID")
        .map(|value| value.eq_ignore_ascii_case("himind-ai"))
        .unwrap_or(false)
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
    use super::{parse_tools_page, safe_segment};
    use serde_json::json;

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
