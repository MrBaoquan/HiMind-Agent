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
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let response = match handle_request(&gateway, method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": error.to_string() }
            }),
        };
        write_message(&mut stdout, &response)?;
    }
    Ok(())
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
        "tools/list" => {
            let context = mcp_invocation_context();
            let tools = gateway
                .list_capabilities(&context)?
                .into_iter()
                .map(|capability| {
                    json!({
                        "name": capability.id,
                        "description": capability.description,
                        "inputSchema": capability.input_schema
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "tools": tools }))
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
                Ok(result) => Ok(json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result)? }],
                    "structuredContent": result,
                    "isError": false
                })),
                Err(error) => Ok(json!({
                    "content": [{ "type": "text", "text": error.to_string() }],
                    "isError": true
                })),
            }
        }
        _ => Err(format!("unsupported MCP method: {method}").into()),
    }
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
    use super::{handle_request, negotiate_protocol_version};
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
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "knowledge.search.v1"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "extension.plugin.submission.submit"));
    }
}
