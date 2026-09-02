//! Real MCP handshake and tool discovery.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use super::mcp_registry::{McpServerSpec, McpTransport};
use crate::runtime::process::configure_hidden_process;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_MCP_HTTP_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_MCP_STDIO_LINE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpProbeResult {
    pub ok: bool,
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
    pub transport: String,
    pub tool_count: usize,
    pub duration_ms: u128,
    pub error_kind: String,
    pub error: String,
}

pub(crate) fn probe(server: &McpServerSpec) -> Result<McpProbeResult, Box<dyn Error>> {
    let started = Instant::now();
    match server.transport {
        McpTransport::Stdio => probe_stdio(server, started),
        McpTransport::StreamableHttp => probe_http(server, started),
    }
}

pub(crate) fn probe_report(server: &McpServerSpec) -> McpProbeResult {
    let started = Instant::now();
    match probe(server) {
        Ok(result) => result,
        Err(error) => {
            let message = error.to_string();
            let (error_kind, detail) = message
                .split_once(':')
                .map(|(kind, detail)| (kind.trim(), detail.trim()))
                .unwrap_or(("probe_failed", message.as_str()));
            McpProbeResult {
                ok: false,
                server_name: String::new(),
                server_version: String::new(),
                protocol_version: String::new(),
                transport: server.transport.as_str().to_string(),
                tool_count: 0,
                duration_ms: started.elapsed().as_millis(),
                error_kind: error_kind.to_string(),
                error: detail.to_string(),
            }
        }
    }
}

pub(crate) fn probe_stdio_command(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<McpProbeResult, Box<dyn Error>> {
    let server = McpServerSpec {
        stable_id: "probe-target".to_string(),
        display_name: "probe-target".to_string(),
        transport: McpTransport::Stdio,
        command: command.to_string(),
        args: args.to_vec(),
        env: env.clone(),
        cwd: cwd.unwrap_or_default().to_string(),
        url: String::new(),
        headers: BTreeMap::new(),
        tool_call_timeout_ms: timeout.as_millis().clamp(1, u64::MAX as u128) as u64,
        fail_on_startup_error: false,
        reconnect: false,
        enabled: true,
        scope: Default::default(),
        source: "probe".to_string(),
    };
    probe(&server)
}

/// Execute one request against a stdio MCP server. A fresh process is used
/// for each request; this keeps failure isolation deterministic while the
/// registry is still a lightweight local configuration store.
pub(crate) fn request_stdio(
    server: &McpServerSpec,
    method: &str,
    params: Value,
) -> Result<(Value, Value), Box<dyn Error>> {
    let mut session = McpStdioSession::connect(server)?;
    let initialize = session.initialize.clone();
    let response = session.request(method, params)?;
    Ok((initialize, response))
}

/// A long-lived stdio MCP session. The Agent owns the child process and
/// restarts it when the registry fingerprint changes or a request fails.
pub(crate) struct McpStdioSession {
    // Kept solely for process ownership; ChildGuard terminates the server when
    // the session is dropped after a failed request or configuration change.
    #[allow(dead_code)]
    child: ChildGuard,
    stdin: ChildStdin,
    receiver: Receiver<Result<Value, String>>,
    next_id: u64,
    timeout: Duration,
    initialize: Value,
}

/// A long-lived Streamable HTTP MCP session. The server may assign a session
/// id during initialize; all subsequent requests reuse it until a failure
/// causes the downstream manager to discard this object.
pub(crate) struct McpHttpSession {
    client: reqwest::blocking::Client,
    server: McpServerSpec,
    session_id: Option<String>,
    next_id: u64,
    initialize: Value,
    protocol_version: String,
}

impl McpStdioSession {
    pub(crate) fn connect(server: &McpServerSpec) -> Result<Self, Box<dyn Error>> {
        let mut last_error = None;
        for protocol_version in SUPPORTED_PROTOCOL_VERSIONS {
            match Self::connect_with_protocol(server, protocol_version) {
                Ok(session) => return Ok(session),
                Err(error) if is_protocol_version_error(&error.to_string()) => {
                    last_error = Some(error.to_string());
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| "invalid_handshake: no supported MCP protocol version".to_string())
            .into())
    }

    fn connect_with_protocol(
        server: &McpServerSpec,
        protocol_version: &str,
    ) -> Result<Self, Box<dyn Error>> {
        if server.command.trim().is_empty() {
            return Err("command_not_found: MCP stdio command is empty".into());
        }
        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if !server.cwd.trim().is_empty() {
            command.current_dir(&server.cwd);
        }
        for (key, value) in &server.env {
            command.env(key, value);
        }
        configure_hidden_process(&mut command);
        let mut child = ChildGuard(command.spawn().map_err(|error| {
            format!(
                "command_not_found: failed to start '{}': {error}",
                server.command
            )
        })?);
        let mut stdin = child
            .0
            .stdin
            .take()
            .ok_or("process_exit: MCP stdin unavailable")?;
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or("process_exit: MCP stdout unavailable")?;
        let receiver = spawn_json_reader(stdout);
        write_request(
            &mut stdin,
            1,
            "initialize",
            json!({
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": { "name": "himind-agent-downstream", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        let startup_timeout = Duration::from_millis(server.tool_call_timeout_ms.clamp(1, 15_000));
        let initialize = wait_for_response(&receiver, 1, startup_timeout)?;
        if let Some(error) = initialize.get("error") {
            if is_unsupported_protocol_response(&initialize) {
                return Err("invalid_handshake: unsupported protocol version".into());
            }
            return Err(format!("invalid_handshake: {error}").into());
        }
        // MCP requires the initialized notification after the initialize
        // response. Sending it earlier works with permissive servers but
        // violates the protocol ordering and breaks strict implementations.
        write_notification(&mut stdin, "notifications/initialized", json!({}))?;
        Ok(Self {
            child,
            stdin,
            receiver,
            next_id: 2,
            timeout: Duration::from_millis(server.tool_call_timeout_ms.max(1)),
            initialize: initialize
                .get("result")
                .cloned()
                .unwrap_or_else(|| json!({})),
        })
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value, Box<dyn Error>> {
        self.request_with_timeout(method, params, self.timeout)
    }

    pub(crate) fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Box<dyn Error>> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        write_request(&mut self.stdin, id, method, params)?;
        let response = wait_for_response(&self.receiver, id, timeout)?;
        Ok(response)
    }
}

impl McpHttpSession {
    pub(crate) fn connect(server: &McpServerSpec) -> Result<Self, Box<dyn Error>> {
        let mut last_error = None;
        for protocol_version in SUPPORTED_PROTOCOL_VERSIONS {
            match Self::connect_with_protocol(server, protocol_version) {
                Ok(session) => return Ok(session),
                Err(error) if is_protocol_version_error(&error.to_string()) => {
                    last_error = Some(error.to_string());
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| "invalid_handshake: no supported MCP protocol version".to_string())
            .into())
    }

    fn connect_with_protocol(
        server: &McpServerSpec,
        protocol_version: &str,
    ) -> Result<Self, Box<dyn Error>> {
        if server.url.trim().is_empty() {
            return Err("invalid_handshake: MCP HTTP URL is empty".into());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(server.tool_call_timeout_ms.max(1)))
            .build()
            .map_err(|error| format!("connection_failed: {error}"))?;
        let (initialize, headers) = post_http(
            &client,
            server,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": protocol_version,
                    "capabilities": {},
                    "clientInfo": { "name": "himind-agent-downstream", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            None,
            protocol_version,
        )?;
        if let Some(error) = initialize.get("error") {
            if is_unsupported_protocol_response(&initialize) {
                return Err("invalid_handshake: unsupported protocol version".into());
            }
            return Err(format!("invalid_handshake: {error}").into());
        }
        let session_id = headers.get("mcp-session-id").cloned();
        post_http_notification(
            &client,
            server,
            "notifications/initialized",
            json!({}),
            session_id.as_deref(),
            protocol_version,
        )?;
        Ok(Self {
            client,
            server: server.clone(),
            session_id,
            next_id: 2,
            initialize: initialize
                .get("result")
                .cloned()
                .unwrap_or_else(|| json!({})),
            protocol_version: negotiated_protocol_version(&initialize, protocol_version),
        })
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value, Box<dyn Error>> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let (response, headers) = post_http(
            &self.client,
            &self.server,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            self.session_id.as_deref(),
            &self.protocol_version,
        )?;
        if let Some(session_id) = headers.get("mcp-session-id") {
            self.session_id = Some(session_id.clone());
        }
        Ok(response)
    }
}

fn probe_stdio(server: &McpServerSpec, started: Instant) -> Result<McpProbeResult, Box<dyn Error>> {
    let (initialize, tools) = request_stdio(server, "tools/list", json!({}))?;
    if let Some(error) = initialize.get("error") {
        return Err(format!("invalid_handshake: {}", error).into());
    }
    if let Some(error) = tools.get("error") {
        return Err(format!("tools_list_failed: {}", error).into());
    }
    let result = initialize.get("result").cloned().unwrap_or_default();
    let tool_count = tools
        .get("result")
        .and_then(|value| value.get("tools"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    Ok(McpProbeResult {
        ok: true,
        server_name: result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        server_version: result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol_version: result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        transport: "stdio".to_string(),
        tool_count,
        duration_ms: started.elapsed().as_millis(),
        error_kind: String::new(),
        error: String::new(),
    })
}

fn probe_http(server: &McpServerSpec, started: Instant) -> Result<McpProbeResult, Box<dyn Error>> {
    let mut session = McpHttpSession::connect(server)?;
    let tools = session.request("tools/list", json!({}))?;
    if let Some(error) = tools.get("error") {
        return Err(format!("tools_list_failed: {}", error).into());
    }
    let init_result = session.initialize;
    Ok(McpProbeResult {
        ok: true,
        server_name: init_result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        server_version: init_result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol_version: init_result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        transport: "streamable-http".to_string(),
        tool_count: tools
            .get("result")
            .and_then(|value| value.get("tools"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
        duration_ms: started.elapsed().as_millis(),
        error_kind: String::new(),
        error: String::new(),
    })
}

fn post_http(
    client: &reqwest::blocking::Client,
    server: &McpServerSpec,
    request: &Value,
    session_id: Option<&str>,
    protocol_version: &str,
) -> Result<(Value, BTreeMap<String, String>), Box<dyn Error>> {
    let mut builder = client
        .post(&server.url)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", protocol_version)
        .json(request);
    for (key, value) in &server.headers {
        builder = builder.header(key, value);
    }
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        builder = builder.header("Mcp-Session-Id", session_id);
    }
    let response = builder
        .send()
        .map_err(|error| format!("connection_failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("connection_failed: {error}"))?;
    let headers = response
        .headers()
        .iter()
        .filter_map(|(key, value)| {
            Some((
                key.as_str().to_ascii_lowercase(),
                value.to_str().ok()?.to_string(),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let body = read_limited_response(response, MAX_MCP_HTTP_RESPONSE_BYTES)?;
    let value = serde_json::from_str::<Value>(&body).or_else(|_| parse_sse_json(&body))?;
    Ok((value, headers))
}

fn read_limited_response(
    response: reqwest::blocking::Response,
    limit: usize,
) -> Result<String, Box<dyn Error>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("MCP response exceeds {limit} bytes").into());
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length as usize)
            .unwrap_or(8192)
            .min(limit),
    );
    response
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("MCP response exceeds {limit} bytes").into());
    }
    String::from_utf8(bytes).map_err(|error| format!("MCP response is not UTF-8: {error}").into())
}

fn post_http_notification(
    client: &reqwest::blocking::Client,
    server: &McpServerSpec,
    method: &str,
    params: Value,
    session_id: Option<&str>,
    protocol_version: &str,
) -> Result<(), Box<dyn Error>> {
    let mut builder = client
        .post(&server.url)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", protocol_version)
        .json(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    for (key, value) in &server.headers {
        builder = builder.header(key, value);
    }
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        builder = builder.header("Mcp-Session-Id", session_id);
    }
    builder
        .send()
        .map_err(|error| format!("connection_failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("connection_failed: {error}"))?;
    Ok(())
}

fn parse_sse_json(body: &str) -> Result<Value, Box<dyn Error>> {
    for line in body.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                return Ok(value);
            }
        }
    }
    Err("invalid_handshake: MCP HTTP response is not JSON or SSE".into())
}

fn negotiated_protocol_version(initialize: &Value, requested: &str) -> String {
    initialize
        .pointer("/result/protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(requested)
        .to_string()
}

fn is_protocol_version_error(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    message.contains("protocol")
        && (message.contains("version")
            || message.contains("unsupported")
            || message.contains("not support"))
}

fn is_unsupported_protocol_response(response: &Value) -> bool {
    let Some(error) = response.get("error") else {
        return false;
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let code = error.get("code").and_then(Value::as_i64);
    (code == Some(-32600) || code == Some(-32602))
        && (message.contains("protocol")
            || message.contains("version")
            || message.contains("unsupported")
            || message.contains("not support"))
}

fn write_request(
    stdin: &mut impl Write,
    id: u64,
    method: &str,
    params: Value,
) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(
        &mut *stdin,
        &json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }),
    )?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn write_notification(
    stdin: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(
        &mut *stdin,
        &json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }),
    )?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn spawn_json_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> Receiver<Result<Value, String>> {
    let (sender, receiver) = mpsc::sync_channel(256);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_bounded_line(&mut reader, MAX_MCP_STDIO_LINE_BYTES) {
                Ok(Some(line)) if !line.trim().is_empty() => {
                    let value = serde_json::from_str::<Value>(&line)
                        .map_err(|error| format!("invalid_handshake: {error}"));
                    if sender.send(value).is_err() {
                        break;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(format!("process_exit: {error}")));
                    break;
                }
            }
        }
    });
    receiver
}

fn read_bounded_line<R: BufRead>(reader: &mut R, limit: usize) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(8192.min(limit));
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            };
        }
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(buffer.len());
        if bytes.len().saturating_add(take) > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("MCP stdio message exceeds {limit} bytes"),
            ));
        }
        bytes.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if take > 0 && bytes.last() == Some(&b'\n') {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    }
}

fn wait_for_response(
    receiver: &Receiver<Result<Value, String>>,
    id: u64,
    timeout: Duration,
) -> Result<Value, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("startup_timeout: MCP response timed out".into());
        }
        let message = receiver
            .recv_timeout(remaining)
            .map_err(|error| format!("startup_timeout: {error}"))?
            .map_err(std::io::Error::other)?;
        if message.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(message);
        }
    }
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_protocol_version_error, is_unsupported_protocol_response, negotiated_protocol_version,
        parse_sse_json,
    };
    use serde_json::json;

    #[test]
    fn parses_streamable_http_sse_data() {
        assert_eq!(
            parse_sse_json("event: message\ndata: {\"jsonrpc\":\"2.0\"}\n").unwrap(),
            json!({"jsonrpc":"2.0"})
        );
    }

    #[test]
    fn accepts_server_protocol_version_and_falls_back_for_legacy_servers() {
        assert_eq!(
            negotiated_protocol_version(
                &json!({ "result": { "protocolVersion": "2025-06-18" } }),
                "2025-11-25"
            ),
            "2025-06-18"
        );
        assert_eq!(
            negotiated_protocol_version(&json!({ "result": {} }), "2024-11-05"),
            "2024-11-05"
        );
    }

    #[test]
    fn only_protocol_version_errors_trigger_retry() {
        assert!(is_protocol_version_error(
            "invalid_handshake: unsupported protocol version"
        ));
        assert!(!is_protocol_version_error(
            "connection_failed: permission denied"
        ));
        assert!(is_unsupported_protocol_response(&json!({
            "error": { "code": -32602, "message": "Unsupported protocol version" }
        })));
    }
}
