use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::capability::service::CapabilityGateway;
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const TOOL_PAGE_SIZE: usize = 128;
const REGISTRY_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: "MCP stdio mode".to_string(),
        local_service_online: false,
        local_service_error: String::new(),
        distribution_update_available: false,
        distribution_update_version: String::new(),
        distribution_update_url: String::new(),
        distribution_update_sha256: String::new(),
        distribution_update_signature: String::new(),
        distribution_update_signature_key_id: String::new(),
        distribution_update_signature_algorithm: String::new(),
    }));
    let gateway = CapabilityGateway::new(options, worker_status);
    let (line_tx, line_rx) = mpsc::channel::<io::Result<String>>();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdout = io::stdout().lock();
    let mut initialized = false;
    let mut last_generation = None::<String>;
    let mut registry_updates = None::<Receiver<String>>;

    loop {
        if initialized {
            if let Some(updates) = registry_updates.as_ref() {
                emit_registry_notifications(updates, &mut stdout, &mut last_generation)?;
            }
        }
        let line = match line_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_message(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": error.to_string() }
                    }),
                )?;
                continue;
            }
        };
        // MCP lifecycle uses a shutdown request followed by an `exit`
        // notification.  The notification has no response, but it must end
        // the stdio process so clients can cleanly restart or upgrade it.
        if request.get("method").and_then(Value::as_str) == Some("exit")
            && request.get("id").is_none()
        {
            break;
        }
        let request_id = request.get("id").cloned();
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            if let Some(id) = request_id {
                write_message(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32600, "message": "invalid JSON-RPC request" }
                    }),
                )?;
            }
            continue;
        }
        if request_id.is_none() {
            if request.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
                initialized = true;
                if registry_updates.is_none() {
                    registry_updates = Some(spawn_registry_watcher(gateway.clone()));
                }
            }
            continue;
        }
        let Some(id) = request_id else {
            continue;
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            write_message(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32600, "message": "MCP method is required" }
                }),
            )?;
            continue;
        };
        let params = request
            .get("params")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let response = match handle_request(&gateway, method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => {
                let message = error.to_string();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": mcp_error_code(method, &message),
                        "message": message
                    }
                })
            }
        };
        write_message(&mut stdout, &response)?;
        if method == "initialize" && response.get("error").is_none() {
            last_generation = response
                .pointer("/result/_meta/himind/registryGeneration")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    Ok(())
}

/// MCP listChanged notifications are emitted from the same writer loop as
/// request responses, so a client never receives interleaved JSON lines. The
/// registry watcher computes generations off the request loop and only sends
/// changes through this channel. The generation intentionally covers tools,
/// prompts and resources together; sending all three notifications keeps the
/// contract correct when a Skill or plugin changes more than one projection.
fn emit_registry_notifications(
    updates: &Receiver<String>,
    stdout: &mut impl Write,
    last_generation: &mut Option<String>,
) -> Result<(), Box<dyn Error>> {
    let mut generation = None;
    while let Ok(next) = updates.try_recv() {
        generation = Some(next);
    }
    let Some(generation) = generation else {
        return Ok(());
    };
    if last_generation.as_deref() == Some(generation.as_str()) {
        return Ok(());
    }
    for method in [
        "notifications/tools/list_changed",
        "notifications/prompts/list_changed",
        "notifications/resources/list_changed",
    ] {
        write_notification(stdout, method, json!({ "registryGeneration": generation }))?;
    }
    *last_generation = Some(generation);
    Ok(())
}

fn spawn_registry_watcher(gateway: CapabilityGateway) -> Receiver<String> {
    spawn_registry_watcher_with_interval(gateway, REGISTRY_POLL_INTERVAL)
}

fn spawn_registry_watcher_with_interval(
    gateway: CapabilityGateway,
    poll_interval: Duration,
) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let mut last_generation = mcp_registry_generation(&gateway).ok();
    thread::spawn(move || loop {
        thread::sleep(poll_interval);
        let Ok(generation) = mcp_registry_generation(&gateway) else {
            continue;
        };
        if last_generation.as_deref() == Some(generation.as_str()) {
            continue;
        }
        if tx.send(generation.clone()).is_err() {
            break;
        }
        last_generation = Some(generation);
    });
    rx
}

fn mcp_error_code(method: &str, message: &str) -> i64 {
    let normalized = message.to_ascii_lowercase();
    if normalized.starts_with("unsupported mcp method:") {
        -32601
    } else if (method == "tools/list" && normalized.contains("cursor"))
        || (method == "tools/call" && normalized == "mcp tool name is required")
        || ((method == "resources/read" || method == "prompts/get")
            && normalized.contains("is required"))
    {
        -32602
    } else {
        -32000
    }
}

fn handle_request(
    gateway: &CapabilityGateway,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn Error>> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate_protocol_version(&params),
            "capabilities": {
                "tools": { "listChanged": true },
                "prompts": { "listChanged": true },
                "resources": { "listChanged": true }
            },
            "serverInfo": { "name": "himind-agent", "version": VERSION },
            "_meta": { "himind": { "registryGeneration": mcp_registry_generation(gateway)? } }
        })),
        "ping" => Ok(json!({})),
        "shutdown" => Ok(Value::Null),
        "resources/list" => {
            let facts = mcp_capability_facts(gateway)?;
            let mut result = crate::skill::mcp_resources_json(VERSION, &facts)?;
            result["_meta"] =
                json!({ "himind": { "registryGeneration": mcp_registry_generation(gateway)? } });
            Ok(result)
        }
        // Some clients probe resource templates immediately after
        // initialization even when the server does not expose resources.
        // Returning the empty, well-formed result keeps discovery compatible
        // without pretending that Agent-owned files are MCP resources.
        "resources/templates/list" => Ok(json!({
            "resourceTemplates": [{
                "uriTemplate": "himind://skill/{skill_id}/{path}",
                "name": "Skill 附属资料",
                "description": "读取已就绪 Skill Manifest 声明的附属资料。",
                "mimeType": "text/plain"
            }]
        })),
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("MCP resource URI is required")?;
            let facts = mcp_capability_facts(gateway)?;
            crate::skill::mcp_resource_read(uri, VERSION, &facts)
        }
        "prompts/list" => {
            let facts = mcp_capability_facts(gateway)?;
            let mut result = crate::skill::mcp_prompts_json(VERSION, &facts)?;
            result["_meta"] =
                json!({ "himind": { "registryGeneration": mcp_registry_generation(gateway)? } });
            Ok(result)
        }
        "prompts/get" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or("MCP prompt name is required")?;
            let facts = mcp_capability_facts(gateway)?;
            crate::skill::mcp_prompt_get(name, VERSION, &facts)
        }
        "tools/list" => {
            let context = mcp_invocation_context();
            let all_tools = gateway
                .list_capabilities(&context)?
                .into_iter()
                .map(|capability| {
                    json!({
                        "name": capability.id,
                        "title": capability.name,
                        "description": capability.description,
                        "inputSchema": capability.input_schema,
                        "annotations": mcp_annotations(&capability)
                    })
                })
                .collect::<Vec<_>>();
            let offset = parse_tool_cursor(&params)?;
            if offset > all_tools.len() {
                return Err("invalid tools/list cursor".into());
            }
            let end = offset.saturating_add(TOOL_PAGE_SIZE).min(all_tools.len());
            let mut result = json!({
                "tools": all_tools[offset..end].to_vec(),
                "_meta": { "himind": { "registryGeneration": mcp_registry_generation(gateway)? } }
            });
            if end < all_tools.len() {
                result["nextCursor"] = json!(format!("offset:{end}"));
            }
            Ok(result)
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or("MCP tool name is required")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let context = mcp_invocation_context();
            match gateway.invoke(&context, name, arguments) {
                Ok(result) => Ok(mcp_tool_call_result(result)?),
                Err(error) => Ok(json!({
                    "content": [{ "type": "text", "text": error.to_string() }],
                    "isError": true
                })),
            }
        }
        _ => Err(format!("unsupported MCP method: {method}").into()),
    }
}

fn mcp_tool_call_result(result: Value) -> Result<Value, Box<dyn Error>> {
    // A projected downstream MCP result may already contain standard
    // `content`, `structuredContent`, and `isError` fields. Preserve those
    // fields instead of stringifying the entire result a second time. Built-in
    // Gateway capabilities continue to receive a text representation plus the
    // complete value under `structuredContent`.
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = result
        .get("content")
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| {
            json!([{
                "type": "text",
                "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            }])
        });
    let structured = result
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| result.clone());
    Ok(json!({
        "content": content,
        "structuredContent": structured,
        "isError": is_error
    }))
}

fn mcp_capability_facts(
    gateway: &CapabilityGateway,
) -> Result<Vec<crate::skill::resolver::CapabilityFact>, Box<dyn Error>> {
    Ok(gateway
        .list_capabilities(&mcp_invocation_context())?
        .into_iter()
        .map(|descriptor| crate::skill::resolver::CapabilityFact {
            id: descriptor.id,
            version: descriptor.version,
            source: descriptor.source,
        })
        .collect())
}

fn mcp_registry_generation(gateway: &CapabilityGateway) -> Result<String, Box<dyn Error>> {
    let context = mcp_invocation_context();
    let capabilities = gateway.list_capabilities(&context)?;
    let facts = capabilities
        .iter()
        .map(|descriptor| crate::skill::resolver::CapabilityFact {
            id: descriptor.id.clone(),
            version: descriptor.version.clone(),
            source: descriptor.source.clone(),
        })
        .collect::<Vec<_>>();
    let snapshot = json!({
        "capabilities": capabilities,
        "prompts": crate::skill::mcp_prompts_json(VERSION, &facts)?,
        "resources": crate::skill::mcp_resources_json(VERSION, &facts)?,
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&snapshot)?)
    ))
}

fn parse_tool_cursor(params: &Value) -> Result<usize, Box<dyn Error>> {
    let Some(cursor) = params.get("cursor") else {
        return Ok(0);
    };
    let cursor = cursor
        .as_str()
        .ok_or("tools/list cursor must be a string")?;
    let offset = cursor
        .strip_prefix("offset:")
        .ok_or("invalid tools/list cursor")?
        .parse::<usize>()
        .map_err(|_| "invalid tools/list cursor")?;
    Ok(offset)
}

fn mcp_annotations(capability: &crate::capability::types::CapabilityDescriptor) -> Value {
    let id = capability.id.to_ascii_lowercase();
    let destructive = ["delete", "remove", "cancel", "detach", "revoke", "disable"]
        .iter()
        .any(|marker| id.contains(marker));
    let idempotent = matches!(capability.idempotency.as_str(), "safe" | "conditional");
    json!({
        // Standard MCP ToolAnnotations used by clients for confirmation and
        // retry UX. HiMind-specific fields remain alongside them so clients
        // can render mode/source/version without another discovery request.
        "title": capability.name,
        "readOnlyHint": capability.risk_level == "read_only",
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": capability.availability != crate::capability::types::CapabilityAvailability::Local,
        "version": capability.version,
        "availability": capability.availability,
        "riskLevel": capability.risk_level,
        "source": capability.source,
        "contractSource": capability.contract_source,
        "contractGeneration": capability.contract_generation,
        "executionMode": capability.execution_mode,
        "supportsProgress": capability.supports_progress,
        "supportsCancel": capability.supports_cancel,
        "idempotency": capability.idempotency,
        "retryPolicy": capability.retry_policy,
        "concurrency": capability.concurrency,
        "approvalRequired": capability.approval_required,
        "requiredScope": capability.required_scope,
        "dashboardRoute": capability.dashboard_route
    })
}

fn negotiate_protocol_version(params: &Value) -> &'static str {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|version| *version == requested)
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

fn mcp_invocation_context() -> InvocationContext {
    let client_id = std::env::var("HIMIND_AI_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "mcp-client".to_string());
    InvocationContext::new(InvocationSource::Mcp, format!("ai-client:{client_id}"))
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_notification(
    writer: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<(), Box<dyn Error>> {
    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        emit_registry_notifications, handle_request, mcp_error_code, mcp_registry_generation,
        mcp_tool_call_result, negotiate_protocol_version, parse_tool_cursor,
        spawn_registry_watcher_with_interval,
    };
    use crate::api::oauth::AgentAccessToken;
    use crate::capability::dashboard_catalog::{
        DashboardCapabilityContract, DashboardCatalogSnapshot,
    };
    use crate::capability::service::CapabilityGateway;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static TEST_GATEWAY_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_gateway() -> CapabilityGateway {
        test_gateway_for_mode(crate::app::runtime_mode::AgentMode::Connected)
    }

    fn test_gateway_for_mode(mode: crate::app::runtime_mode::AgentMode) -> CapabilityGateway {
        let mut options = Options::from_env();
        options.api_base = "http://127.0.0.1:9".to_string();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let sequence = TEST_GATEWAY_COUNTER.fetch_add(1, Ordering::Relaxed);
        options.state_path = std::env::temp_dir().join(format!(
            "himind-agent-mcp-test-{}-{nonce}-{sequence}.json",
            std::process::id()
        ));
        options.effective_mode = mode;
        CapabilityGateway::new(
            options,
            Arc::new(Mutex::new(LocalWorkerStatus {
                dashboard_worker_online: false,
                dashboard_agent_id: String::new(),
                dashboard_worker_error: String::new(),
                local_service_online: false,
                local_service_error: String::new(),
                distribution_update_available: false,
                distribution_update_version: String::new(),
                distribution_update_url: String::new(),
                distribution_update_sha256: String::new(),
                distribution_update_signature: String::new(),
                distribution_update_signature_key_id: String::new(),
                distribution_update_signature_algorithm: String::new(),
            })),
        )
    }

    #[test]
    fn initialize_uses_a_supported_requested_protocol_version() {
        assert_eq!(
            negotiate_protocol_version(&json!({ "protocolVersion": "2024-11-05" })),
            "2024-11-05"
        );
    }

    #[test]
    fn initialize_falls_back_to_the_latest_supported_protocol_version() {
        assert_eq!(
            negotiate_protocol_version(&json!({ "protocolVersion": "future-version" })),
            "2025-11-25"
        );
    }

    #[test]
    fn tools_list_cursor_is_strict_and_forward_only() {
        assert_eq!(parse_tool_cursor(&json!({})).unwrap(), 0);
        assert_eq!(
            parse_tool_cursor(&json!({ "cursor": "offset:128" })).unwrap(),
            128
        );
        assert!(parse_tool_cursor(&json!({ "cursor": "128" })).is_err());
        assert!(parse_tool_cursor(&json!({ "cursor": 128 })).is_err());
    }

    #[test]
    fn protocol_errors_use_standard_json_rpc_codes() {
        assert_eq!(
            mcp_error_code("resources/read", "unsupported MCP method: resources/read"),
            -32601
        );
        assert_eq!(
            mcp_error_code("tools/list", "invalid tools/list cursor"),
            -32602
        );
        assert_eq!(
            mcp_error_code("tools/call", "MCP tool name is required"),
            -32602
        );
        assert_eq!(
            mcp_error_code("tools/call", "capability not found: example"),
            -32000
        );
    }

    #[test]
    fn registry_notifications_are_emitted_once_per_generation() {
        let (tx, rx) = mpsc::channel();
        tx.send("sha256:generation-1".to_string()).unwrap();
        let mut output = Vec::new();
        let mut last_generation = None;
        emit_registry_notifications(&rx, &mut output, &mut last_generation).unwrap();
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 3);

        output.clear();
        emit_registry_notifications(&rx, &mut output, &mut last_generation).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn dashboard_catalog_change_updates_mcp_discovery_call_and_notifications() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut options = Options::from_env();
        options.api_base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        options.effective_mode = crate::app::runtime_mode::AgentMode::Connected;
        options.state_path = std::env::temp_dir().join(format!(
            "himind-mcp-catalog-change-{}-{}.json",
            std::process::id(),
            TEST_GATEWAY_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        *options.platform_access.write().unwrap() = Some(AgentAccessToken {
            token: "test-access-token".into(),
            expires_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_add(3600),
            scope: "business.example.read".into(),
            user_id: "user-test".into(),
            agent_id: "agent-test".into(),
        });
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let initial_generation = mcp_registry_generation(&gateway).unwrap();
        let updates =
            spawn_registry_watcher_with_interval(gateway.clone(), Duration::from_millis(10));

        gateway.replace_dashboard_catalog_for_test(DashboardCatalogSnapshot {
            generation: "dashboard-generation-two".into(),
            items: vec![DashboardCapabilityContract {
                id: "business.example.lookup".into(),
                version: "1.1.0".into(),
                name: "查询示例".into(),
                description: "通过动态 Dashboard catalog 查询示例。".into(),
                risk_level: "read_only".into(),
                http_method: "GET".into(),
                scope: "business.example.read".into(),
                dashboard_route: "/api/integrations/ai/business/examples/{example_id}".into(),
                input_schema: json!({
                    "type":"object",
                    "properties":{
                        "example_id":{"type":"string"},
                        "q":{"type":"string"}
                    },
                    "required":["example_id"],
                    "additionalProperties":false
                }),
                execution_mode: "sync".into(),
                supports_progress: false,
                supports_cancel: false,
                idempotency: "safe".into(),
                retry_policy: "safe".into(),
                concurrency: "parallel".into(),
                approval_required: false,
            }],
        });

        let changed_generation = updates
            .recv_timeout(Duration::from_secs(1))
            .expect("catalog change must update the MCP registry generation");
        assert_ne!(changed_generation, initial_generation);
        assert_eq!(
            changed_generation,
            mcp_registry_generation(&gateway).unwrap()
        );

        let listed = handle_request(&gateway, "tools/list", json!({})).unwrap();
        let tool = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "business.example.lookup")
            .expect("dynamic catalog capability must be discoverable");
        assert_eq!(tool["annotations"]["contractSource"], "dashboard:catalog");
        assert_eq!(
            tool["annotations"]["contractGeneration"],
            "dashboard-generation-two"
        );
        assert_eq!(
            tool["annotations"]["dashboardRoute"],
            "/api/integrations/ai/business/examples/{example_id}"
        );

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "request ended before HTTP headers");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request
                .starts_with("GET /api/integrations/ai/business/examples/one?q=hello HTTP/1.1"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-access-token"));
            let body = br#"{"item":{"id":"one","name":"hello"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });
        let called = handle_request(
            &gateway,
            "tools/call",
            json!({
                "name":"business.example.lookup",
                "arguments":{"example_id":"one","q":"hello"}
            }),
        )
        .unwrap();
        assert_eq!(called["isError"], false);
        assert_eq!(called["structuredContent"]["item"]["id"], "one");
        server.join().unwrap();

        let (tx, rx) = mpsc::channel();
        tx.send(changed_generation.clone()).unwrap();
        let mut output = Vec::new();
        let mut last_generation = Some(initial_generation);
        emit_registry_notifications(&rx, &mut output, &mut last_generation).unwrap();
        let notifications = String::from_utf8(output).unwrap();
        assert!(notifications.contains("notifications/tools/list_changed"));
        assert!(notifications.contains(&changed_generation));
    }

    #[test]
    fn optional_mcp_discovery_lists_are_empty_and_well_formed() {
        let gateway = test_gateway();
        let resources = handle_request(&gateway, "resources/list", json!({})).unwrap();
        assert!(resources["resources"].is_array());
        assert!(resources["_meta"]["himind"]["registryGeneration"]
            .as_str()
            .is_some());
        assert_eq!(
            handle_request(&gateway, "resources/templates/list", json!({})).unwrap(),
            json!({
                "resourceTemplates": [{
                    "uriTemplate": "himind://skill/{skill_id}/{path}",
                    "name": "Skill 附属资料",
                    "description": "读取已就绪 Skill Manifest 声明的附属资料。",
                    "mimeType": "text/plain"
                }]
            })
        );
        let prompts = handle_request(&gateway, "prompts/list", json!({})).unwrap();
        assert!(prompts["prompts"].is_array());
    }

    #[test]
    fn downstream_tool_results_keep_standard_content_and_error_fields() {
        let response = mcp_tool_call_result(json!({
            "content": [{ "type": "text", "text": "downstream failed" }],
            "isError": true
        }))
        .unwrap();
        assert_eq!(response["isError"], true);
        assert_eq!(response["content"][0]["text"], "downstream failed");
    }

    #[test]
    fn tools_list_exposes_the_builtin_knowledge_search_capability() {
        let result = handle_request(&test_gateway(), "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        let knowledge = tools
            .iter()
            .find(|tool| tool["name"] == "knowledge.search.v1")
            .expect("knowledge.search.v1 must be discoverable through MCP");
        assert_eq!(knowledge["inputSchema"]["required"], json!(["query"]));
        assert!(knowledge["description"]
            .as_str()
            .unwrap()
            .contains("不调用 HiMind 模型"));
    }

    #[test]
    fn tools_list_exposes_ai_client_capabilities() {
        let result = handle_request(&test_gateway(), "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        for name in [
            "ai.client.list",
            "ai.client.status",
            "ai.client.import",
            "ai.client.remove",
            "ai.client.import.plan",
            "ai.client.remove.plan",
            "ai.service.list",
            "ai.service.custom.upsert",
            "ai.service.custom.remove",
            "ai.service.custom.list_models",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(tool["annotations"]["source"], "builtin");
        }
        for name in ["operation.get", "operation.cancel"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(tool["annotations"]["availability"], "control_plane");
            assert!(tool["annotations"]["requiredScope"].as_str().is_some());
        }
        for name in [
            "ai.client.import.plan",
            "ai.client.remove.plan",
            "ai.service.list",
            "ai.service.custom.list_models",
        ] {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert_eq!(tool["annotations"]["riskLevel"], "read_only");
            assert_eq!(tool["annotations"]["approvalRequired"], false);
        }
        for name in [
            "ai.client.import",
            "ai.client.remove",
            "ai.service.custom.upsert",
            "ai.service.custom.remove",
        ] {
            assert_eq!(
                tools.iter().find(|tool| tool["name"] == name).unwrap()["annotations"]
                    ["approvalRequired"],
                true,
                "{name} must require approval for MCP calls"
            );
        }
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool["name"] == "ai.client.import")
                .unwrap()["annotations"]["riskLevel"],
            "local_write"
        );
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool["name"] == "ai.client.import")
                .unwrap()["annotations"]["approvalRequired"],
            true
        );
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool["name"] == "ai.client.remove")
                .unwrap()["annotations"]["destructiveHint"],
            true
        );
    }

    #[test]
    fn connected_mcp_exposes_project_management_capabilities() {
        let result = handle_request(&test_gateway(), "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        for name in [
            "business.project.list",
            "business.project.create",
            "business.exhibit.create",
            "business.project.managers.replace",
            "business.exhibit.crew.replace",
            "business.exhibit.crew.append",
            "business.exhibit.crew.remove",
            "business.project.exhibit.attach",
            "business.exhibit.workspace.checkout",
            "business.people.search",
            "business.requirement.list",
            "business.requirement.create",
            "business.requirement.assignment.update",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(tool["annotations"]["availability"], "control_plane");
            assert!(
                tool["annotations"]["requiredScope"].as_str().is_some(),
                "{name} must expose its Dashboard scope"
            );
        }
        let checkout = tools
            .iter()
            .find(|tool| tool["name"] == "business.exhibit.workspace.checkout")
            .unwrap();
        assert_eq!(checkout["annotations"]["executionMode"], "long_running");
        assert_eq!(checkout["annotations"]["supportsProgress"], true);
        assert_eq!(checkout["annotations"]["supportsCancel"], true);
        let crew_remove = tools
            .iter()
            .find(|tool| tool["name"] == "business.exhibit.crew.remove")
            .unwrap();
        assert_eq!(crew_remove["annotations"]["riskLevel"], "R3");
        assert_eq!(crew_remove["annotations"]["approvalRequired"], true);
        assert_eq!(crew_remove["annotations"]["idempotency"], "conditional");
    }

    #[test]
    fn tools_call_routes_knowledge_search_through_the_gateway() {
        let result = handle_request(
            &test_gateway(),
            "tools/call",
            json!({
                "name": "knowledge.search.v1",
                "arguments": { "query": "知识平台架构" }
            }),
        )
        .unwrap();
        assert_eq!(result["isError"], true);
        let message = result["content"][0]["text"].as_str().unwrap();
        assert!(!message.contains("capability not found"), "{message}");
    }

    #[test]
    fn independent_mcp_exposes_local_authoring_and_hides_control_plane() {
        let gateway = test_gateway_for_mode(crate::app::runtime_mode::AgentMode::Independent);
        let result = handle_request(&gateway, "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "extension.plugin.candidate.save"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "extension.skill.candidate.test"));
        assert!(tools.iter().any(|tool| tool["name"] == "extension.test"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "extension.workspace.current"));
        assert!(tools.iter().any(|tool| tool["name"] == "plugin.list"));
        assert!(tools.iter().any(|tool| tool["name"] == "mcp.server.list"));
        assert!(tools.iter().any(|tool| tool["name"] == "mcp.server.upsert"));
        assert!(tools.iter().any(|tool| tool["name"] == "mcp.server.remove"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "mcp.registration.apply_all"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "knowledge.search.v1"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "extension.plugin.submission.submit"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "business.project.create"));
    }
}
