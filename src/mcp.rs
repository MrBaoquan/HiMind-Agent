use serde_json::{json, Value};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use crate::capability::service::CapabilityGateway;
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

pub(crate) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: "MCP stdio mode".to_string(),
        local_service_online: false,
        local_service_error: String::new(),
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
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "project-dashboard-agent", "version": VERSION }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let context = InvocationContext::new(InvocationSource::Mcp, "mcp-client");
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
            let context = InvocationContext::new(InvocationSource::Mcp, "mcp-client");
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

fn write_message(writer: &mut impl Write, value: &Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
