use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::io::{self, BufRead, BufReader, Write};
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
const MAX_MCP_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_MCP_TOOL_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MCP_INPUT_QUEUE_CAPACITY: usize = 128;
const MAX_ACTIVATED_CAPABILITIES: usize = 128;
const MAX_ACTIVATED_SCHEMA_BYTES: usize = 512 * 1024;
const MCP_INSTRUCTIONS: &str = "HiMind Agent MCP companion 使用 stdio 传输，仅启动本地能力网关，不启动本地 HTTP 服务或 Dashboard Worker。因而 system.health 中 local_service_expected=false、local_service_online=false、dashboard_worker_state=not_applicable、dashboard_worker_expected=false、dashboard_worker_online=false、dashboard_worker_reason_code=stdio_companion_gateway_only 在 stdio 下是正常状态，不代表 MCP 或业务接口故障。只有 Connected 模式的本地 Agent 应用服务才托管 Dashboard Worker；判断 Worker 是否异常时先看 dashboard_worker_expected，再看 dashboard_worker_state 和 dashboard_worker_reason_code，不要只看旧版 dashboard_worker_online。Connected 模式下，只有 Dashboard 控制面能力需要 Dashboard 授权；本地插件、Skill、MCP 管理和扩展开发能力仍由 Agent 直接提供。短视频能力 short.video.* 是本地插件能力，创建项目、预览、反馈、Remotion/HyperFrames 渲染和产物导出在 Independent 模式完整可用，不依赖 Dashboard；其中写入和渲染仍遵循 Agent 本机审批策略。默认 tools/list 只暴露通用 Bootstrap 能力；可通过环境变量 HIMIND_MCP_DEFAULT_ACTIVATE 预激活业务能力（逗号分隔 capability ID，MCP 启动即投影，且不受目录 generation 变化影响）；其余能力先使用 capability.catalog.search 搜索目录，再用 capability.catalog.describe 获取具体 Schema，最后调用 capability.catalog.activate 激活当前工作流需要的工具。客户端不支持动态工具刷新时，可继续使用 capability.catalog.invoke 调用已激活能力。调用项目/展项业务能力时，先调用 business.project.list、business.exhibit.list 或 context.resolve，再使用返回的稳定 pid；EX-xxxx 是展示编号，不是路由 ID。组织业务能力是可选 Provider，不是 Agent Core 的运行依赖。";

const BOOTSTRAP_TOOL_IDS: &[&str] = &[
    "capability.catalog.search",
    "capability.catalog.describe",
    "capability.catalog.activate",
    "capability.catalog.invoke",
    "system.health",
];

#[derive(Default)]
struct McpSessionState {
    activated_capabilities: BTreeSet<String>,
    /// Capability IDs pre-activated at MCP startup (from
    /// HIMIND_MCP_DEFAULT_ACTIVATE). Unlike dynamic activations they survive
    /// registry generation changes, so clients that cannot reliably call
    /// capability.catalog.activate (e.g. some VS Code Copilot sessions) still
    /// see the configured business tools on the first tools/list projection.
    default_activated: BTreeSet<String>,
    activation_generation: Option<String>,
    projection_changed: bool,
    /// Internal callers historically received the complete tool list. Keep
    /// that behavior in the direct helper while stdio uses the bounded view.
    legacy_compatibility: bool,
}

impl McpSessionState {
    fn is_exposed(&self, capability_id: &str) -> bool {
        self.legacy_compatibility
            || BOOTSTRAP_TOOL_IDS.contains(&capability_id)
            || self.activated_capabilities.contains(capability_id)
            || self.default_activated.contains(capability_id)
    }

    fn take_projection_changed(&mut self) -> bool {
        std::mem::take(&mut self.projection_changed)
    }

    fn synchronize_generation(&mut self, generation: &str) {
        if self.legacy_compatibility {
            return;
        }
        if self
            .activation_generation
            .as_deref()
            .is_some_and(|bound| bound != generation)
        {
            self.activated_capabilities.clear();
            self.projection_changed = true;
        }
        self.activation_generation = Some(generation.to_string());
    }
}

/// Parse the `HIMIND_MCP_DEFAULT_ACTIVATE` environment variable into a set of
/// capability IDs to project from the very first `tools/list`. Values may be
/// comma, semicolon or whitespace separated; unknown IDs are ignored so a
/// stale config never breaks MCP startup.
fn default_activation_ids(gateway: &CapabilityGateway) -> BTreeSet<String> {
    let Ok(raw) = std::env::var("HIMIND_MCP_DEFAULT_ACTIVATE") else {
        return BTreeSet::new();
    };
    let known = gateway
        .list_capabilities(&mcp_invocation_context())
        .map(|caps| caps.into_iter().map(|cap| cap.id).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    parse_default_activation_ids(&raw, &known)
}

fn parse_default_activation_ids(raw: &str, known: &BTreeSet<String>) -> BTreeSet<String> {
    if raw.trim().is_empty() {
        return BTreeSet::new();
    }
    raw.split([',', ';', ' '])
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| known.contains(*id))
        .map(str::to_string)
        .collect()
}

pub(crate) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus {
        dashboard_worker_online: false,
        dashboard_agent_id: String::new(),
        dashboard_worker_error: "MCP stdio mode".to_string(),
        dashboard_worker_state: "not_applicable".to_string(),
        dashboard_worker_reason_code: "stdio_companion_gateway_only".to_string(),
        worker_transport: "stdio".to_string(),
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
    let (line_tx, line_rx) = mpsc::sync_channel::<io::Result<String>>(MCP_INPUT_QUEUE_CAPACITY);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match read_bounded_line(&mut reader, MAX_MCP_REQUEST_BYTES) {
                Ok(Some(line)) => {
                    if line_tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = line_tx.send(Err(error));
                    if discard_until_newline(&mut reader).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut stdout = io::stdout().lock();
    let mut initialized = false;
    let mut last_generation = None::<String>;
    let mut registry_updates = None::<Receiver<String>>;
    let mut session = McpSessionState {
        default_activated: default_activation_ids(&gateway),
        ..McpSessionState::default()
    };

    loop {
        if initialized {
            if let Some(updates) = registry_updates.as_ref() {
                emit_registry_notifications(updates, &mut stdout, &mut last_generation)?;
            }
        }
        let line = match line_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => {
                write_message(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32600, "message": error.to_string() }
                    }),
                )?;
                continue;
            }
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
        let response = match handle_request_with_session(&gateway, method, params, &mut session) {
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
        if session.take_projection_changed() {
            write_notification(
                &mut stdout,
                "notifications/tools/list_changed",
                json!({ "registryGeneration": mcp_registry_generation(&gateway)? }),
            )?;
        }
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
    // Preserve the old in-process helper contract for HTTP/Tauri and tests.
    // The real stdio loop uses a session-scoped projection below.
    let mut session = McpSessionState {
        activated_capabilities: gateway
            .list_capabilities(&mcp_invocation_context())?
            .into_iter()
            .map(|capability| capability.id)
            .collect(),
        default_activated: BTreeSet::new(),
        activation_generation: None,
        projection_changed: false,
        legacy_compatibility: true,
    };
    handle_request_with_session(gateway, method, params, &mut session)
}

fn handle_request_with_session(
    gateway: &CapabilityGateway,
    method: &str,
    params: Value,
    session: &mut McpSessionState,
) -> Result<Value, Box<dyn Error>> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate_protocol_version(&params),
            "instructions": MCP_INSTRUCTIONS,
            "capabilities": {
                "tools": { "listChanged": true },
                "prompts": { "listChanged": true },
                "resources": { "listChanged": true }
            },
            "serverInfo": { "name": "himind-agent", "version": VERSION },
            "_meta": { "himind": {
                "registryGeneration": mcp_registry_generation(gateway)?,
                "runtime": gateway.mcp_runtime_metadata()
            } }
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
            let generation = mcp_registry_generation(gateway)?;
            session.synchronize_generation(&generation);
            let mut all_tools = gateway
                .list_capabilities(&context)?
                .into_iter()
                .filter(|capability| session.is_exposed(&capability.id))
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
            // This is a session tool rather than a Gateway capability. It
            // keeps old MCP clients usable after activation without requiring
            // them to understand listChanged notifications.
            if !all_tools
                .iter()
                .any(|tool| tool["name"] == "capability.catalog.invoke")
            {
                all_tools.insert(2.min(all_tools.len()), catalog_invoke_tool());
            }
            if !all_tools
                .iter()
                .any(|tool| tool["name"] == "capability.catalog.activate")
            {
                all_tools.insert(1.min(all_tools.len()), catalog_activation_tool());
            }
            let offset = parse_tool_cursor(&params)?;
            let offset = validate_tool_cursor(&params, &generation, offset)?;
            if offset > all_tools.len() {
                return Err("invalid tools/list cursor".into());
            }
            let page_size = if session.legacy_compatibility {
                all_tools.len().max(TOOL_PAGE_SIZE)
            } else {
                TOOL_PAGE_SIZE
            };
            let end = offset.saturating_add(page_size).min(all_tools.len());
            let mut result = json!({
                "tools": all_tools[offset..end].to_vec(),
                "_meta": { "himind": { "registryGeneration": generation } }
            });
            if end < all_tools.len() {
                result["nextCursor"] = json!(format_tool_cursor(
                    result["_meta"]["himind"]["registryGeneration"]
                        .as_str()
                        .unwrap_or_default(),
                    end
                ));
            }
            Ok(result)
        }
        "tools/call" => {
            let generation = mcp_registry_generation(gateway)?;
            session.synchronize_generation(&generation);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or("MCP tool name is required")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if name == "capability.catalog.activate" {
                return match activate_capabilities(gateway, arguments, session) {
                    Ok(result) => mcp_tool_call_result(result),
                    Err(error) => Ok(mcp_tool_call_error(error.as_ref())),
                };
            }
            if name == "capability.catalog.invoke" {
                return match invoke_activated_capability(gateway, arguments, session) {
                    Ok(result) => mcp_tool_call_result(result),
                    Err(error) => Ok(mcp_tool_call_error(error.as_ref())),
                };
            }
            if !session.is_exposed(name) {
                return Ok(mcp_tool_call_error(&io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("capability is not active in this MCP session: {name}"),
                )));
            }
            let context = mcp_invocation_context();
            match gateway.invoke(&context, name, arguments) {
                Ok(result) => Ok(mcp_tool_call_result(result)?),
                Err(error) => Ok(mcp_tool_call_error(error.as_ref())),
            }
        }
        _ => Err(format!("unsupported MCP method: {method}").into()),
    }
}

fn activate_capabilities(
    gateway: &CapabilityGateway,
    params: Value,
    session: &mut McpSessionState,
) -> Result<Value, Box<dyn Error>> {
    let has_selector = params.get("ids").is_some()
        || params.get("group").is_some()
        || params.get("surface").is_some()
        || params.get("query").is_some();
    if !has_selector {
        return Err("capability activation requires ids, group, surface, or query".into());
    }
    let ids = params
        .get("ids")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let group = params
        .get("group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let surface = params
        .get("surface")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let replace = params
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let available = gateway.list_capabilities(&mcp_invocation_context())?;
    let generation = mcp_registry_generation(gateway)?;
    session.synchronize_generation(&generation);
    let mut matched = available
        .iter()
        .filter(|capability| !BOOTSTRAP_TOOL_IDS.contains(&capability.id.as_str()))
        .filter(|capability| ids.is_empty() || ids.contains(&capability.id))
        .filter(|capability| {
            group
                .as_deref()
                .map_or(true, |value| capability.discovery_group() == value)
        })
        .filter(|capability| {
            surface
                .as_deref()
                .map_or(true, |value| capability.discovery_surface() == value)
        })
        .filter(|capability| {
            query.as_deref().map_or(true, |value| {
                [
                    capability.id.as_str(),
                    capability.name.as_str(),
                    capability.description.as_str(),
                ]
                .iter()
                .any(|field| field.to_ascii_lowercase().contains(value))
            })
        })
        .map(|capability| capability.id.clone())
        .collect::<Vec<_>>();
    matched.sort();
    matched.dedup();
    if matched.is_empty() {
        return Err("capability activation matched no visible capabilities".into());
    }

    let visible_ids = available
        .iter()
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    let previous = session.activated_capabilities.clone();
    let mut next = if replace {
        BTreeSet::new()
    } else {
        previous
            .iter()
            .filter(|capability_id| visible_ids.contains(capability_id.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    next.extend(matched.iter().cloned());
    if next.len() > MAX_ACTIVATED_CAPABILITIES {
        return Err(format!(
            "capability activation exceeds the session limit of {MAX_ACTIVATED_CAPABILITIES}"
        )
        .into());
    }
    let activated_schema_bytes = next.iter().try_fold(0usize, |total, capability_id| {
        let capability = available
            .iter()
            .find(|candidate| candidate.id == *capability_id)
            .ok_or_else(|| format!("capability is no longer visible: {capability_id}"))?;
        let schema_bytes = serde_json::to_vec(&capability.input_schema)
            .map_err(|error| error.to_string())?
            .len();
        total
            .checked_add(schema_bytes)
            .ok_or_else(|| "capability activation schema budget overflow".to_string())
    })?;
    if activated_schema_bytes > MAX_ACTIVATED_SCHEMA_BYTES {
        return Err(format!(
            "capability activation exceeds the schema budget of {MAX_ACTIVATED_SCHEMA_BYTES} bytes"
        )
        .into());
    }

    session.activated_capabilities = next;
    session.projection_changed |= previous != session.activated_capabilities;
    Ok(json!({
        "activated": matched,
        "activatedCount": session.activated_capabilities.len(),
        "limit": MAX_ACTIVATED_CAPABILITIES,
        "activatedSchemaBytes": activated_schema_bytes,
        "schemaByteLimit": MAX_ACTIVATED_SCHEMA_BYTES,
        "registryGeneration": mcp_registry_generation(gateway)?
    }))
}

fn catalog_activation_tool() -> Value {
    json!({
        "name": "capability.catalog.activate",
        "title": "激活能力",
        "description": "按能力 ID、分组、surface 或关键字将能力加入当前 MCP 会话的工具列表。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": MAX_ACTIVATED_CAPABILITIES
                },
                "group": { "type": "string" },
                "surface": { "type": "string" },
                "query": { "type": "string" },
                "replace": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        },
        "annotations": {
            "title": "激活能力",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
            "availability": "local",
            "riskLevel": "read_only",
            "source": "mcp-session",
            "executionMode": "sync",
            "approvalRequired": false,
            "discoveryGroup": "core",
            "discoverySurface": "primary",
            "discoveryRank": 1
        }
    })
}

fn catalog_invoke_tool() -> Value {
    json!({
        "name": "capability.catalog.invoke",
        "title": "调用已激活能力",
        "description": "调用当前 MCP 会话已经激活的能力；适用于不支持动态工具列表刷新的客户端。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "arguments": {}
            },
            "required": ["id"],
            "additionalProperties": false
        },
        "annotations": {
            "title": "调用已激活能力",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": true,
            "availability": "local",
            "riskLevel": "provider_defined",
            "source": "mcp-session",
            "executionMode": "provider_defined",
            "approvalRequired": true,
            "discoveryGroup": "core",
            "discoverySurface": "primary",
            "discoveryRank": 2
        }
    })
}

fn invoke_activated_capability(
    gateway: &CapabilityGateway,
    params: Value,
    session: &McpSessionState,
) -> Result<Value, Box<dyn Error>> {
    let capability_id = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("capability.catalog.invoke requires a non-empty id")?;
    if !session.is_exposed(capability_id) {
        return Err(
            format!("capability is not active in this MCP session: {capability_id}").into(),
        );
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    gateway.invoke(&mcp_invocation_context(), capability_id, arguments)
}

fn mcp_tool_call_error(error: &dyn Error) -> Value {
    let text = error.to_string();
    // Authoring operations expose a JSON diagnostic envelope so external AI
    // clients can branch on stable blocker codes. Legacy errors remain plain
    // text while still using the standard MCP error shape.
    if let Ok(payload) = serde_json::from_str::<Value>(&text) {
        return json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": payload,
            "isError": true
        });
    }
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
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
    let projected = json!({
        "content": &content,
        "structuredContent": &structured,
        "isError": is_error
    });
    if serde_json::to_vec(&projected)
        .map(|bytes| bytes.len() > MAX_MCP_TOOL_RESULT_BYTES)
        .unwrap_or(true)
    {
        return Err(format!("MCP tool result exceeds {MAX_MCP_TOOL_RESULT_BYTES} bytes").into());
    }
    Ok(json!({
        "content": content,
        "structuredContent": structured,
        "isError": is_error
    }))
}

fn read_bounded_line<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(8192.min(limit));
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            };
        }
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(buffer.len());
        if bytes.len().saturating_add(take) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("MCP request exceeds {limit} bytes"),
            ));
        }
        bytes.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if bytes.last() == Some(&b'\n') {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}

fn discard_until_newline<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            return Ok(());
        }
        let length = buffer.len();
        reader.consume(length);
    }
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
    let mut hasher = Sha256::new();
    for capability in &capabilities {
        // Hash the contract fields individually. This preserves changes to
        // a Schema while avoiding construction of one large registry JSON
        // value on every watcher tick.
        let schema = serde_json::to_vec(&capability.input_schema)?;
        for field in [
            capability.id.as_bytes(),
            capability.version.as_bytes(),
            capability.name.as_bytes(),
            capability.description.as_bytes(),
            capability.risk_level.as_bytes(),
            capability.source.as_bytes(),
            capability.contract_source.as_bytes(),
            capability
                .contract_generation
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            capability.execution_mode.as_bytes(),
            capability.idempotency.as_bytes(),
            capability.retry_policy.as_bytes(),
            capability.concurrency.as_bytes(),
            capability
                .required_scope
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            capability
                .dashboard_route
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            schema.as_slice(),
        ] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.update([capability.supports_progress as u8]);
        hasher.update([capability.supports_cancel as u8]);
        hasher.update([capability.approval_required as u8]);
        hasher.update([capability.dashboard_provider as u8]);
        hasher.update(capability.availability.as_str().as_bytes());
    }
    let facts = capabilities
        .iter()
        .map(|descriptor| crate::skill::resolver::CapabilityFact {
            id: descriptor.id.clone(),
            version: descriptor.version.clone(),
            source: descriptor.source.clone(),
        })
        .collect::<Vec<_>>();
    let prompts = crate::skill::mcp_prompts_json(VERSION, &facts)?;
    let resources = crate::skill::mcp_resources_json(VERSION, &facts)?;
    hasher.update(serde_json::to_vec(&prompts)?);
    hasher.update(serde_json::to_vec(&resources)?);
    Ok(format!("sha256:{:x}", hasher.finalize()))
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
        .map(str::to_string)
        .or_else(|| {
            cursor
                .strip_prefix("generation:")
                .and_then(|value| value.rsplit_once("|offset:"))
                .map(|(_, offset)| offset.to_string())
        })
        .ok_or("invalid tools/list cursor")?
        .parse::<usize>()
        .map_err(|_| "invalid tools/list cursor")?;
    Ok(offset)
}

fn format_tool_cursor(generation: &str, offset: usize) -> String {
    format!("generation:{generation}|offset:{offset}")
}

fn validate_tool_cursor(
    params: &Value,
    generation: &str,
    legacy_offset: usize,
) -> Result<usize, Box<dyn Error>> {
    let Some(cursor) = params.get("cursor").and_then(Value::as_str) else {
        return Ok(legacy_offset);
    };
    if let Some((cursor_generation, offset)) = cursor
        .strip_prefix("generation:")
        .and_then(|value| value.rsplit_once("|offset:"))
    {
        if cursor_generation != generation {
            return Err("tools/list cursor is stale; request the first page again".into());
        }
        return offset
            .parse::<usize>()
            .map_err(|_| "invalid tools/list cursor".into());
    }
    // Accept old offset cursors for one compatibility cycle.
    Ok(legacy_offset)
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
        emit_registry_notifications, handle_request, handle_request_with_session, mcp_error_code,
        mcp_registry_generation, mcp_tool_call_error, mcp_tool_call_result,
        negotiate_protocol_version, parse_default_activation_ids, parse_tool_cursor,
        spawn_registry_watcher_with_interval, McpSessionState, MCP_INSTRUCTIONS,
    };
    use crate::api::oauth::AgentAccessToken;
    use crate::business_integration::{BusinessCapabilityContract, BusinessCatalogSnapshot};
    use crate::capability::service::CapabilityGateway;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use serde_json::json;
    use std::collections::BTreeSet;
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
                dashboard_worker_state: "not_applicable".to_string(),
                dashboard_worker_reason_code: "stdio_companion_gateway_only".to_string(),
                worker_transport: "stdio".to_string(),
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
    fn initialize_instructions_identify_short_video_as_independent_local_workflow() {
        assert!(MCP_INSTRUCTIONS.contains("short.video.*"));
        assert!(MCP_INSTRUCTIONS.contains("Independent 模式完整可用"));
        assert!(MCP_INSTRUCTIONS.contains("不依赖 Dashboard"));
        assert!(MCP_INSTRUCTIONS.contains("Agent 本机审批策略"));
    }

    #[test]
    fn session_projection_keeps_bootstrap_small_and_requires_activation() {
        let gateway = test_gateway();
        let mut session = McpSessionState::default();
        let listed =
            handle_request_with_session(&gateway, "tools/list", json!({}), &mut session).unwrap();
        let tools = listed["tools"].as_array().unwrap();
        assert!(
            tools.len() <= 6,
            "bootstrap projection grew unexpectedly: {tools:?}"
        );
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "capability.catalog.search"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "capability.catalog.describe"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "capability.catalog.activate"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "capability.catalog.invoke"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"] == "business.project.list"));
        assert!(!tools.iter().any(|tool| tool["name"] == "ai.client.import"));

        let blocked = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({ "name": "ai.client.import", "arguments": {} }),
            &mut session,
        )
        .unwrap();
        assert_eq!(blocked["isError"], true);
        assert!(blocked["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not active"));

        let activated = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.activate",
                "arguments": { "ids": ["ai.client.list"] }
            }),
            &mut session,
        )
        .unwrap();
        assert_eq!(activated["isError"], false);
        assert_eq!(
            activated["structuredContent"]["activated"],
            json!(["ai.client.list"])
        );

        let listed =
            handle_request_with_session(&gateway, "tools/list", json!({}), &mut session).unwrap();
        assert!(listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "ai.client.list"));

        let called = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.invoke",
                "arguments": { "id": "ai.client.list", "arguments": {} }
            }),
            &mut session,
        )
        .unwrap();
        assert_eq!(
            called["isError"], false,
            "dynamic capability call failed: {called}"
        );
    }

    #[test]
    fn default_projection_exposes_preactivated_abilities() {
        let gateway = test_gateway();
        let mut session = McpSessionState {
            default_activated: ["ai.client.list".to_string()].into_iter().collect(),
            ..McpSessionState::default()
        };
        let listed =
            handle_request_with_session(&gateway, "tools/list", json!({}), &mut session).unwrap();
        let names = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"ai.client.list"),
            "preactivated capability must be projected: {names:?}"
        );
        assert!(
            !names.contains(&"business.project.list"),
            "unlisted capabilities must stay hidden: {names:?}"
        );
    }

    #[test]
    fn default_activation_survives_registry_generation_change() {
        let gateway = test_gateway();
        let mut session = McpSessionState {
            default_activated: ["ai.client.list".to_string()].into_iter().collect(),
            ..McpSessionState::default()
        };
        session.synchronize_generation("sha256:one");
        gateway.replace_business_catalog_for_test(BusinessCatalogSnapshot::dashboard(
            "session-generation-two".into(),
            Vec::new(),
        ));
        session.synchronize_generation(&mcp_registry_generation(&gateway).unwrap());
        assert!(session.default_activated.contains("ai.client.list"));
        assert!(session.activated_capabilities.is_empty());
        let listed =
            handle_request_with_session(&gateway, "tools/list", json!({}), &mut session).unwrap();
        assert!(listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "ai.client.list"));
    }

    #[test]
    fn parse_default_activation_filters_unknown_ids() {
        let known = ["ai.client.list", "business.project.list"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let parsed = parse_default_activation_ids(
            "business.project.list, ai.client.list , no.such.capability",
            &known,
        );
        assert_eq!(
            parsed,
            ["ai.client.list", "business.project.list"]
                .into_iter()
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        );
        assert!(parse_default_activation_ids("   ", &known).is_empty());
        assert!(parse_default_activation_ids("nothing.known,also.missing", &known).is_empty());
    }

    #[test]
    fn catalog_search_is_lightweight_and_describe_returns_schema() {
        let gateway = test_gateway();
        let mut session = McpSessionState::default();
        let search = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.search",
                "arguments": { "query": "ai.client", "limit": 5 }
            }),
            &mut session,
        )
        .unwrap();
        assert_eq!(search["isError"], false);
        let item = &search["structuredContent"]["items"][0];
        assert!(item["schemaBytes"].as_u64().unwrap() > 0);
        assert!(item.get("inputSchema").is_none());

        let described = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.describe",
                "arguments": { "id": "ai.client.import" }
            }),
            &mut session,
        )
        .unwrap();
        assert_eq!(described["isError"], false);
        assert!(described["structuredContent"]["inputSchema"]["properties"]["target"].is_object());
    }

    #[test]
    fn registry_change_invalidates_old_session_activation_before_invoke() {
        let gateway = test_gateway();
        let mut session = McpSessionState::default();
        let listed =
            handle_request_with_session(&gateway, "tools/list", json!({}), &mut session).unwrap();
        let generation = listed["_meta"]["himind"]["registryGeneration"]
            .as_str()
            .unwrap()
            .to_string();
        let activated = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.activate",
                "arguments": { "ids": ["ai.client.list"] }
            }),
            &mut session,
        )
        .unwrap();
        assert_eq!(activated["isError"], false);
        assert!(session.activated_capabilities.contains("ai.client.list"));

        gateway.replace_business_catalog_for_test(BusinessCatalogSnapshot::dashboard(
            "session-generation-two".into(),
            Vec::new(),
        ));
        let changed = handle_request_with_session(
            &gateway,
            "tools/call",
            json!({
                "name": "capability.catalog.invoke",
                "arguments": { "id": "ai.client.list", "arguments": {} }
            }),
            &mut session,
        )
        .unwrap();
        assert_ne!(
            mcp_registry_generation(&gateway).unwrap(),
            generation,
            "the provider catalog change must produce a new MCP generation"
        );
        assert_eq!(changed["isError"], true);
        assert!(changed["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not active"));
        assert!(session.activated_capabilities.is_empty());
    }

    #[test]
    fn session_activation_is_invalidated_when_registry_generation_changes() {
        let mut session = McpSessionState {
            activated_capabilities: ["plugin.example.run".to_string()].into_iter().collect(),
            default_activated: ["business.exhibit.list".to_string()].into_iter().collect(),
            activation_generation: Some("sha256:one".to_string()),
            projection_changed: false,
            legacy_compatibility: false,
        };
        session.synchronize_generation("sha256:two");
        assert!(session.activated_capabilities.is_empty());
        assert!(session.default_activated.contains("business.exhibit.list"));
        assert!(session.take_projection_changed());
        assert_eq!(session.activation_generation.as_deref(), Some("sha256:two"));
    }

    #[test]
    fn initialize_explains_stdio_worker_boundary_and_exhibit_id_rule() {
        let result = handle_request(
            &test_gateway(),
            "initialize",
            json!({ "protocolVersion": "2025-11-25" }),
        )
        .unwrap();
        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("dashboard_worker_state=not_applicable"));
        assert!(instructions.contains("business.exhibit.list"));
        assert!(instructions.contains("pid"));
        assert!(instructions.contains("EX-xxxx"));
        assert_eq!(result["_meta"]["himind"]["runtime"]["schemaVersion"], 1);
        assert_eq!(result["_meta"]["himind"]["runtime"]["transport"], "stdio");
        assert_eq!(
            result["_meta"]["himind"]["runtime"]["dashboardWorkerExpected"],
            false
        );
        assert_eq!(
            result["_meta"]["himind"]["runtime"]["dashboardWorkerReasonCode"],
            "stdio_companion_gateway_only"
        );
        assert_eq!(result["_meta"]["himind"]["runtime"]["mode"], "connected");
        assert_eq!(
            result["_meta"]["himind"]["runtime"]["controlPlane"]["enabled"],
            true
        );
    }

    #[test]
    fn initialize_reports_independent_mode_control_plane_state() {
        let result = handle_request(
            &test_gateway_for_mode(crate::app::runtime_mode::AgentMode::Independent),
            "initialize",
            json!({ "protocolVersion": "2025-11-25" }),
        )
        .unwrap();
        let runtime = &result["_meta"]["himind"]["runtime"];
        assert_eq!(runtime["mode"], "independent");
        assert_eq!(runtime["dashboardEnabled"], false);
        assert_eq!(runtime["controlPlane"]["enabled"], false);
        assert_eq!(runtime["dashboardWorkerExpected"], false);
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
    fn business_integration_catalog_change_updates_mcp_discovery_call_and_notifications() {
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

        gateway.replace_business_catalog_for_test(BusinessCatalogSnapshot::dashboard(
            "dashboard-generation-two".into(),
            vec![BusinessCapabilityContract {
                id: "business.example.lookup".into(),
                version: "1.1.0".into(),
                name: "查询示例".into(),
                description: "通过动态 Dashboard catalog 查询示例。".into(),
                risk_level: "read_only".into(),
                http_method: "GET".into(),
                scope: "business.example.read".into(),
                route: "/api/integrations/ai/business/examples/{example_id}".into(),
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
        ));

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
        assert_eq!(
            called["isError"], false,
            "dynamic capability call failed: {called}"
        );
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
    fn structured_authoring_errors_survive_mcp_tool_projection() {
        let error = crate::extension_authoring::blocked_error(
            "plugin",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_unbound",
                "workspace",
                "当前 AI 工作区仍是 Agent 主目录",
                "调用 extension.workspace.bind",
                true,
            )],
            Vec::new(),
            vec!["调用 extension.workspace.bind 后重试".to_string()],
        );
        let response = mcp_tool_call_error(error.as_ref());
        assert_eq!(response["isError"], true);
        assert_eq!(response["structuredContent"]["state"], "blocked");
        assert_eq!(
            response["structuredContent"]["blockers"][0]["code"],
            "extension_workspace_unbound"
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
    fn tools_list_exposes_exhibit_route_id_contract() {
        let result = handle_request(&test_gateway(), "tools/list", json!({})).unwrap();
        let tools = result["tools"].as_array().unwrap();
        let list = tools
            .iter()
            .find(|tool| tool["name"] == "business.exhibit.list")
            .expect("business.exhibit.list must be discoverable");
        assert!(list["description"].as_str().unwrap().contains("pid"));
        let get = tools
            .iter()
            .find(|tool| tool["name"] == "business.exhibit.get")
            .expect("business.exhibit.get must be discoverable");
        assert!(
            get["inputSchema"]["properties"]["exhibit_id"]["description"]
                .as_str()
                .unwrap()
                .contains("EX-0021")
        );
    }

    #[test]
    fn exhibit_display_number_is_rejected_before_dashboard_authentication() {
        let result = handle_request(
            &test_gateway(),
            "tools/call",
            json!({
                "name": "business.exhibit.get",
                "arguments": { "exhibit_id": "EX-0021" }
            }),
        )
        .unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["code"],
            "EXHIBIT_ROUTE_ID_REQUIRED"
        );
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("pid"));
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
            // AI 连接域由 Agent 本机自管：写入/移除本机客户端配置与服务状态，
            // UI 层确认即可，不再进入 capability 审批流、也不创建 Dashboard 审批。
            assert_eq!(
                tools.iter().find(|tool| tool["name"] == name).unwrap()["annotations"]
                    ["approvalRequired"],
                false,
                "{name} must not require capability-level approval"
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
            false
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
        assert!(!tools.iter().any(|tool| tool["name"] == "workspace.current"));
        for name in [
            "extension.authoring.preflight",
            "extension.workspace.bind",
            "extension.workspace.clear",
            "extension.revision.create",
        ] {
            assert!(
                tools.iter().any(|tool| tool["name"] == name),
                "missing local authoring capability: {name}"
            );
        }
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
