use serde_json::{json, Value};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use crate::capability::service::CapabilityGateway;
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const TOOL_PAGE_SIZE: usize = 128;

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
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line?;
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
    }
    Ok(())
}

fn mcp_error_code(method: &str, message: &str) -> i64 {
    let normalized = message.to_ascii_lowercase();
    if normalized.starts_with("unsupported mcp method:") {
        -32601
    } else if (method == "tools/list" && normalized.contains("cursor"))
        || (method == "tools/call" && normalized == "mcp tool name is required")
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
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "himind-agent", "version": VERSION }
        })),
        "ping" => Ok(json!({})),
        "shutdown" => Ok(Value::Null),
        "resources/list" => Ok(json!({ "resources": [] })),
        // Some clients probe resource templates immediately after
        // initialization even when the server does not expose resources.
        // Returning the empty, well-formed result keeps discovery compatible
        // without pretending that Agent-owned files are MCP resources.
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
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
                "tools": all_tools[offset..end].to_vec()
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
    let idempotent = capability.risk_level == "read_only"
        || [
            "replace",
            "update",
            "bind",
            "attach",
            "register",
            "unregister",
            "sync",
        ]
        .iter()
        .any(|marker| id.contains(marker));
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
        "source": capability.source
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

#[cfg(test)]
mod tests {
    use super::{
        handle_request, mcp_error_code, mcp_tool_call_result, negotiate_protocol_version,
        parse_tool_cursor,
    };
    use crate::capability::service::CapabilityGateway;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn test_gateway() -> CapabilityGateway {
        test_gateway_for_mode(crate::app::runtime_mode::AgentMode::Connected)
    }

    fn test_gateway_for_mode(mode: crate::app::runtime_mode::AgentMode) -> CapabilityGateway {
        let mut options = Options::from_env();
        options.api_base = "http://127.0.0.1:9".to_string();
        options.state_path = std::env::temp_dir().join(format!(
            "himind-agent-mcp-test-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
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
    fn optional_mcp_discovery_lists_are_empty_and_well_formed() {
        let gateway = test_gateway();
        assert_eq!(
            handle_request(&gateway, "resources/list", json!({})).unwrap(),
            json!({ "resources": [] })
        );
        assert_eq!(
            handle_request(&gateway, "resources/templates/list", json!({})).unwrap(),
            json!({ "resourceTemplates": [] })
        );
        assert_eq!(
            handle_request(&gateway, "prompts/list", json!({})).unwrap(),
            json!({ "prompts": [] })
        );
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
    fn connected_mcp_exposes_project_management_capabilities() {
        let result = handle_request(&test_gateway(), "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        for name in [
            "business.project.list",
            "business.project.create",
            "business.exhibit.create",
            "business.project.managers.replace",
            "business.exhibit.crew.replace",
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
        }
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
