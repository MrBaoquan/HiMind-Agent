use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::error::Error;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::acp_sessions::{self, StoredAcpSession, StoredAcpTurn};
use crate::Options;

const ACP_PROTOCOL_VERSION: i64 = 1;
const ACP_RUNTIME_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const ACP_SESSION_PROMPT_LIMIT: usize = 128 * 1024;
const ACP_SESSION_PAGE_SIZE: usize = 50;

/// ACP 入站侧的验收夹具：用固定答复替代真实运行时，只用于本地验收。
/// 该后门必须在发布构建中失效，否则任何进程都能靠一个环境变量绕过真实推理。
fn acp_fixture_enabled() -> bool {
    cfg!(debug_assertions) && std::env::var("HIMIND_ACP_FIXTURE").as_deref() == Ok("1")
}

#[derive(Clone)]
struct AcpTurn {
    user: String,
    assistant: String,
}

struct AcpSession {
    cwd: String,
    ai_client_id: String,
    created_at: u64,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    history: Arc<Mutex<Vec<AcpTurn>>>,
}

pub(crate) fn run(options: &Options) -> Result<(), Box<dyn Error>> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let sessions = Arc::new(Mutex::new(HashMap::<String, AcpSession>::new()));
    let mut prompt_threads = Vec::<JoinHandle<()>>::new();
    let mut initialized = false;
    let mut negotiated_ai_client_id = String::new();
    let mut line = String::new();

    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(message) => {
                    let method = message
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let id = message.get("id").cloned();
                    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                    match method.as_str() {
                        "initialize" => {
                            initialized = true;
                            negotiated_ai_client_id = acp_client_id_from_initialize(&params);
                            if let Some(id) = id {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {
                                            "protocolVersion": ACP_PROTOCOL_VERSION,
                                            "agentCapabilities": {
                                                "loadSession": true,
                                                "promptCapabilities": {
                                                    "image": false,
                                                    "audio": false,
                                                    "embeddedContext": false
                                                },
                                                "sessionCapabilities": {
                                                    "list": {},
                                                    "delete": {},
                                                    "resume": {},
                                                    "close": {}
                                                }
                                            },
                                            "agentInfo": {
                                                "name": "HiMind Agent",
                                                "version": crate::VERSION
                                            }
                                        }
                                    }),
                                )?;
                            }
                        }
                        "session/new" => {
                            if !initialized {
                                write_error(&writer, id, -32002, "initialize is required")?;
                                line.clear();
                                continue;
                            }
                            let cwd = match resolve_session_cwd(params.get("cwd")) {
                                Ok(cwd) => cwd,
                                Err(error) => {
                                    write_error(&writer, id, -32602, &error.to_string())?;
                                    line.clear();
                                    continue;
                                }
                            };
                            let session_id =
                                format!("acp_{}_{}", std::process::id(), rand::random::<u64>());
                            let stored = StoredAcpSession::new(session_id.clone(), cwd.clone());
                            if let Err(error) = acp_sessions::save(&stored) {
                                write_error(&writer, id, -32000, &error.to_string())?;
                                line.clear();
                                continue;
                            }
                            sessions.lock().expect("ACP sessions lock").insert(
                                session_id.clone(),
                                AcpSession {
                                    cwd,
                                    ai_client_id: negotiated_ai_client_id.clone(),
                                    created_at: stored.created_at,
                                    cancel: Arc::new(AtomicBool::new(false)),
                                    active: Arc::new(AtomicBool::new(false)),
                                    history: Arc::new(Mutex::new(Vec::new())),
                                },
                            );
                            if let Some(id) = id {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {"sessionId": session_id}
                                    }),
                                )?;
                            }
                        }
                        "session/list" => {
                            if !initialized {
                                write_error(&writer, id, -32002, "initialize is required")?;
                                line.clear();
                                continue;
                            }
                            match list_sessions_page(&params) {
                                Ok(result) => {
                                    if let Some(id) = id {
                                        write_message(
                                            &writer,
                                            json!({
                                                "jsonrpc": "2.0",
                                                "id": id,
                                                "result": result
                                            }),
                                        )?;
                                    }
                                }
                                Err(error) => {
                                    write_error(&writer, id, -32602, &error.to_string())?;
                                }
                            }
                        }
                        "session/delete" => {
                            if !initialized {
                                write_error(&writer, id, -32002, "initialize is required")?;
                                line.clear();
                                continue;
                            }
                            let session_id = params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .unwrap_or_default();
                            let active = sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .get(session_id)
                                .is_some_and(|session| session.active.load(Ordering::SeqCst));
                            if active {
                                write_error(
                                    &writer,
                                    id,
                                    -32002,
                                    "ACP active session must be closed before deletion",
                                )?;
                                line.clear();
                                continue;
                            }
                            sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .remove(session_id);
                            if let Err(error) = acp_sessions::delete(session_id) {
                                write_error(&writer, id, -32000, &error.to_string())?;
                                line.clear();
                                continue;
                            }
                            if let Some(id) = id {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {}
                                    }),
                                )?;
                            }
                        }
                        method if matches!(method, "session/load" | "session/resume") => {
                            if !initialized {
                                write_error(&writer, id, -32002, "initialize is required")?;
                                line.clear();
                                continue;
                            }
                            let loaded = match load_persisted_session(&params) {
                                Ok(loaded) => loaded,
                                Err(error) => {
                                    write_error(&writer, id, -32002, &error.to_string())?;
                                    line.clear();
                                    continue;
                                }
                            };
                            let existing_active = sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .get(&loaded.session_id)
                                .is_some_and(|session| session.active.load(Ordering::SeqCst));
                            if existing_active {
                                write_error(
                                    &writer,
                                    id,
                                    -32002,
                                    "ACP session already has an active prompt",
                                )?;
                                line.clear();
                                continue;
                            }
                            let history = Arc::new(Mutex::new(loaded.history.clone()));
                            sessions.lock().expect("ACP sessions lock").insert(
                                loaded.session_id.clone(),
                                AcpSession {
                                    cwd: loaded.cwd.clone(),
                                    ai_client_id: negotiated_ai_client_id.clone(),
                                    created_at: loaded.created_at,
                                    cancel: Arc::new(AtomicBool::new(false)),
                                    active: Arc::new(AtomicBool::new(false)),
                                    history,
                                },
                            );
                            if method == "session/load" {
                                replay_session_history(
                                    &writer,
                                    &loaded.session_id,
                                    &loaded.history,
                                )?;
                            }
                            if let Some(id) = id {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": if method == "session/load" {
                                            Value::Null
                                        } else {
                                            json!({})
                                        }
                                    }),
                                )?;
                            }
                        }
                        "session/close" => {
                            let session_id = params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let removed = sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .remove(session_id);
                            let Some(session) = removed else {
                                write_error(&writer, id, -32602, "unknown ACP session")?;
                                line.clear();
                                continue;
                            };
                            if session.active.load(Ordering::SeqCst) {
                                session.cancel.store(true, Ordering::SeqCst);
                            }
                            if let Some(id) = id {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {}
                                    }),
                                )?;
                            }
                        }
                        "session/prompt" => {
                            let Some(id) = id else {
                                write_error(
                                    &writer,
                                    None,
                                    -32600,
                                    "session/prompt requires an id",
                                )?;
                                line.clear();
                                continue;
                            };
                            let session_id = params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let session = sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .get(&session_id)
                                .map(|session| {
                                    (
                                        session.cwd.clone(),
                                        session.created_at,
                                        Arc::clone(&session.cancel),
                                        Arc::clone(&session.active),
                                        Arc::clone(&session.history),
                                    )
                                });
                            let Some((cwd, created_at, cancel, active, history)) = session else {
                                write_error(&writer, Some(id), -32602, "unknown ACP session")?;
                                line.clear();
                                continue;
                            };
                            if active.load(Ordering::SeqCst) {
                                write_error(
                                    &writer,
                                    Some(id),
                                    -32002,
                                    "ACP session already has an active prompt",
                                )?;
                                line.clear();
                                continue;
                            }
                            if cancel.swap(false, Ordering::SeqCst) && active.load(Ordering::SeqCst)
                            {
                                write_message(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {"stopReason": "cancelled"}
                                    }),
                                )?;
                                line.clear();
                                continue;
                            }
                            let prompt = match prompt_text(&params) {
                                Ok(prompt) => prompt,
                                Err(error) => {
                                    write_error(&writer, Some(id), -32602, &error.to_string())?;
                                    line.clear();
                                    continue;
                                }
                            };
                            let history_snapshot =
                                history.lock().expect("ACP session history lock").clone();
                            let runtime_prompt =
                                match render_prompt_with_history(&history_snapshot, &prompt) {
                                    Ok(prompt) => prompt,
                                    Err(error) => {
                                        write_error(&writer, Some(id), -32000, &error.to_string())?;
                                        line.clear();
                                        continue;
                                    }
                                };
                            active.store(true, Ordering::SeqCst);
                            let writer_for_prompt = Arc::clone(&writer);
                            let options_for_prompt = options.clone();
                            let session_id_for_prompt = session_id.clone();
                            let ai_client_id_for_prompt = sessions
                                .lock()
                                .expect("ACP sessions lock")
                                .get(&session_id_for_prompt)
                                .map(|session| session.ai_client_id.clone())
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| "acp:client".to_string());
                            let history_for_prompt = Arc::clone(&history);
                            prompt_threads.push(thread::spawn(move || {
                                let canceled = Arc::clone(&cancel);
                                let active_for_prompt = Arc::clone(&active);
                                let ledger =
                                    match crate::store::local_runs::LocalRunLedger::open_default() {
                                        Ok(ledger) => ledger,
                                        Err(error) => {
                                            let _ = write_error(
                                                &writer_for_prompt,
                                                Some(id),
                                                -32000,
                                                &error.to_string(),
                                            );
                                            active_for_prompt.store(false, Ordering::SeqCst);
                                            return;
                                        }
                                    };
                                let recorder =
                                    crate::agent_core_service::AgentCoreRunRecorder::with_ledger(
                                        ledger,
                                    );
                                let (agent_id, device_id) =
                                    crate::agent_core_service::current_agent_attribution(
                                        &options_for_prompt.state_path,
                                    );
                                let mut context = crate::capability::types::InvocationContext::new(
                                    crate::capability::types::InvocationSource::Acp,
                                    format!("acp-session:{session_id_for_prompt}"),
                                )
                                .with_ai_client_id(ai_client_id_for_prompt)
                                .with_device_id(device_id)
                                .with_workspace_ref(cwd.clone());
                                context.request_id = format!(
                                    "acp_{}_{}",
                                    session_id_for_prompt,
                                    rand::random::<u64>()
                                );
                                let mut run =
                                    match recorder.begin(&agent_id, &context, "acp.prompt") {
                                        Ok(run) => run,
                                        Err(error) => {
                                            let _ = write_error(
                                                &writer_for_prompt,
                                                Some(id),
                                                -32000,
                                                &error.to_string(),
                                            );
                                            active_for_prompt.store(false, Ordering::SeqCst);
                                            return;
                                        }
                                    };
                                let provider = if acp_fixture_enabled() {
                                    "himind.fixture"
                                } else {
                                    "himind.builtin"
                                };
                                run.runtime_provider = provider.to_string();
                                if let Some(step) = run.steps.first_mut() {
                                    step.runtime_provider = provider.to_string();
                                }
                                let output =
                                    if acp_fixture_enabled() {
                                        let delay_ms = std::env::var("HIMIND_ACP_FIXTURE_DELAY_MS")
                                            .ok()
                                            .and_then(|value| value.parse::<u64>().ok())
                                            .unwrap_or(0);
                                        let mut waited = 0;
                                        let mut was_canceled = false;
                                        while waited < delay_ms {
                                            if canceled.load(Ordering::SeqCst) {
                                                was_canceled = true;
                                                break;
                                            }
                                            let step = delay_ms.saturating_sub(waited).min(50);
                                            thread::sleep(std::time::Duration::from_millis(step));
                                            waited = waited.saturating_add(step);
                                        }
                                        if was_canceled {
                                            Err("ACP prompt canceled".into())
                                        } else {
                                            Ok(
                                                crate::runtime::deepseek_harness::WorkflowRuntimeOutcome {
                                                    text: format!("ACP fixture: {runtime_prompt}"),
                                                    model: String::new(),
                                                    service_source: "fixture",
                                                    endpoint: String::new(),
                                                },
                                            )
                                        }
                                    } else {
                                        crate::runtime::deepseek_harness::execute_workflow(
                                            &options_for_prompt,
                                            &cwd,
                                            &runtime_prompt,
                                            ACP_RUNTIME_TIMEOUT_SECONDS,
                                            false,
                                            &|| Ok(canceled.load(Ordering::SeqCst)),
                                        )
                                    };
                                let (result, stop_reason) = match output {
                                    Ok(output) => {
                                        let output = output.text;
                                        if let Err(error) = persist_session_turn(
                                            &session_id_for_prompt,
                                            &cwd,
                                            created_at,
                                            &history_for_prompt,
                                            &prompt,
                                            &output,
                                        ) {
                                            let _ = recorder.fail(run, &error.to_string());
                                            let _ = write_error(
                                                &writer_for_prompt,
                                                Some(id),
                                                -32000,
                                                &error.to_string(),
                                            );
                                            active_for_prompt.store(false, Ordering::SeqCst);
                                            return;
                                        }
                                        let summary = json!({
                                            "session_id": session_id_for_prompt,
                                            "prompt_sha256": text_sha256(&prompt),
                                            "prompt_chars": prompt.chars().count(),
                                            "context_turns": history_snapshot.len(),
                                            "output_sha256": text_sha256(&output),
                                            "output_chars": output.chars().count(),
                                        });
                                        if let Err(error) = recorder.complete(run, &summary) {
                                            let _ = write_error(
                                                &writer_for_prompt,
                                                Some(id),
                                                -32000,
                                                &error.to_string(),
                                            );
                                            active_for_prompt.store(false, Ordering::SeqCst);
                                            return;
                                        }
                                        (Some(output), "end_turn")
                                    }
                                    Err(error)
                                        if canceled.load(Ordering::SeqCst)
                                            || error.to_string().contains("cancel") =>
                                    {
                                        if let Err(ledger_error) =
                                            recorder.cancel(run, "ACP prompt canceled")
                                        {
                                            let _ = write_error(
                                                &writer_for_prompt,
                                                Some(id),
                                                -32000,
                                                &ledger_error.to_string(),
                                            );
                                            active_for_prompt.store(false, Ordering::SeqCst);
                                            return;
                                        }
                                        (None, "cancelled")
                                    }
                                    Err(error) => {
                                        let _ = recorder.fail(run, &error.to_string());
                                        let _ = write_error(
                                            &writer_for_prompt,
                                            Some(id),
                                            -32000,
                                            &error.to_string(),
                                        );
                                        active_for_prompt.store(false, Ordering::SeqCst);
                                        return;
                                    }
                                };
                                if let Some(output) = result {
                                    let _ = write_message(
                                        &writer_for_prompt,
                                        json!({
                                            "jsonrpc": "2.0",
                                            "method": "session/update",
                                            "params": {
                                                "sessionId": session_id_for_prompt,
                                                "update": {
                                                    "sessionUpdate": "agent_message_chunk",
                                                    "content": {
                                                        "type": "text",
                                                        "text": output
                                                    }
                                                }
                                            }
                                        }),
                                    );
                                }
                                let _ = write_message(
                                    &writer_for_prompt,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {"stopReason": stop_reason}
                                    }),
                                );
                                active_for_prompt.store(false, Ordering::SeqCst);
                            }));
                        }
                        "session/cancel" => {
                            if let Some(session_id) =
                                params.get("sessionId").and_then(Value::as_str)
                            {
                                if let Some(session) =
                                    sessions.lock().expect("ACP sessions lock").get(session_id)
                                {
                                    if session.active.load(Ordering::SeqCst) {
                                        session.cancel.store(true, Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                        _ => {
                            if let Some(id) = id {
                                write_error(&writer, Some(id), -32601, "method not found")?;
                            }
                        }
                    }
                }
                Err(error) => {
                    write_error(
                        &writer,
                        None,
                        -32700,
                        &format!("invalid JSON-RPC message: {error}"),
                    )?;
                }
            }
        }
        line.clear();
    }
    for session in sessions.lock().expect("ACP sessions lock").values() {
        if session.active.load(Ordering::SeqCst) {
            session.cancel.store(true, Ordering::SeqCst);
        }
    }
    for thread in prompt_threads {
        let _ = thread.join();
    }
    Ok(())
}

fn prompt_text(params: &Value) -> Result<String, Box<dyn Error>> {
    let blocks = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or("ACP prompt must be an array")?;
    let mut prompt = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        prompt.push(text.trim().to_string());
                    }
                }
            }
            Some("resource_link") => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    prompt.push(format!("Resource: {}", uri.trim()));
                }
            }
            Some("resource") => {
                if let Some(uri) = block
                    .get("resource")
                    .and_then(|resource| resource.get("uri"))
                    .and_then(Value::as_str)
                {
                    prompt.push(format!("Resource: {}", uri.trim()));
                }
            }
            Some("image") | Some("audio") => {
                return Err("ACP image/audio prompts are not supported".into());
            }
            _ => {}
        }
    }
    let prompt = prompt.join("\n\n");
    if prompt.is_empty() {
        return Err("ACP prompt does not contain supported text content".into());
    }
    Ok(prompt)
}

fn acp_client_id_from_initialize(params: &Value) -> String {
    let client_info = params.get("clientInfo").unwrap_or(&Value::Null);
    let name = client_info
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = client_info
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let client_id = crate::agent_core_contracts::external_ai_client_id("acp", name, version);
    if client_id.is_empty() {
        "acp:client".to_string()
    } else {
        client_id
    }
}

fn list_sessions_page(params: &Value) -> Result<Value, Box<dyn Error>> {
    let cwd_filter = match params.get("cwd") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Some(canonical_session_cwd(Path::new(value.trim()))?)
        }
        Some(Value::String(_)) => return Err("ACP session list cwd is empty".into()),
        Some(_) => return Err("ACP session list cwd must be a string".into()),
    };
    let offset = match params.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(value)) => decode_session_cursor(value)?,
        Some(_) => return Err("ACP session list cursor must be a string".into()),
    };
    let sessions = acp_sessions::list()?
        .into_iter()
        .filter(|session| cwd_filter.as_ref().is_none_or(|cwd| session.cwd == *cwd))
        .collect::<Vec<_>>();
    if offset > sessions.len() {
        return Err("ACP session list cursor is invalid".into());
    }
    let next_offset = offset.saturating_add(ACP_SESSION_PAGE_SIZE);
    let items = sessions
        .iter()
        .skip(offset)
        .take(ACP_SESSION_PAGE_SIZE)
        .map(session_info)
        .collect::<Vec<_>>();
    Ok(json!({
        "sessions": items,
        "nextCursor": if next_offset < sessions.len() {
            Value::String(encode_session_cursor(next_offset))
        } else {
            Value::Null
        }
    }))
}

fn session_info(session: &StoredAcpSession) -> Value {
    let title = session
        .turns
        .first()
        .map(|turn| crate::runtime::process::summarize_output(&turn.user, 120))
        .filter(|title| !title.is_empty());
    let updated_at = Utc
        .timestamp_opt(session.updated_at as i64, 0)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true));
    json!({
        "sessionId": session.session_id,
        "cwd": session.cwd,
        "title": title,
        "updatedAt": updated_at,
        "_meta": {
            "messageCount": session.turns.len().saturating_mul(2)
        }
    })
}

fn encode_session_cursor(offset: usize) -> String {
    STANDARD.encode(format!("offset:{offset}"))
}

fn decode_session_cursor(cursor: &str) -> Result<usize, Box<dyn Error>> {
    let decoded = STANDARD
        .decode(cursor.trim())
        .map_err(|_| "ACP session list cursor is invalid")?;
    let decoded =
        std::str::from_utf8(&decoded).map_err(|_| "ACP session list cursor is invalid")?;
    let value = decoded
        .strip_prefix("offset:")
        .ok_or("ACP session list cursor is invalid")?;
    value
        .parse::<usize>()
        .map_err(|_| "ACP session list cursor is invalid".into())
}

fn render_prompt_with_history(history: &[AcpTurn], prompt: &str) -> Result<String, Box<dyn Error>> {
    if history.is_empty() {
        return Ok(prompt.to_string());
    }
    let mut rendered =
        String::from("Continue the existing ACP session. Previous conversation:\n\n");
    for turn in history {
        rendered.push_str("User:\n");
        rendered.push_str(&turn.user);
        rendered.push_str("\n\nAssistant:\n");
        rendered.push_str(&turn.assistant);
        rendered.push_str("\n\n");
    }
    rendered.push_str("Current user:\n");
    rendered.push_str(prompt);
    if rendered.chars().count() > ACP_SESSION_PROMPT_LIMIT {
        return Err("ACP session history exceeds the Runtime prompt limit".into());
    }
    Ok(rendered)
}

struct LoadedAcpSession {
    session_id: String,
    cwd: String,
    created_at: u64,
    history: Vec<AcpTurn>,
}

fn resolve_session_cwd(value: Option<&Value>) -> Result<String, Box<dyn Error>> {
    let path = match value {
        Some(Value::String(value)) if !value.trim().is_empty() => Path::new(value.trim()),
        Some(_) => return Err("ACP session cwd must be a string".into()),
        None => return current_session_cwd(),
    };
    if !path.is_absolute() {
        return Err("ACP session cwd must be absolute".into());
    }
    canonical_session_cwd(path)
}

fn current_session_cwd() -> Result<String, Box<dyn Error>> {
    canonical_session_cwd(&std::env::current_dir()?)
}

fn canonical_session_cwd(path: &Path) -> Result<String, Box<dyn Error>> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("ACP session cwd is unavailable: {error}"))?;
    if !canonical.is_dir() {
        return Err("ACP session cwd is not a directory".into());
    }
    Ok(canonical.to_string_lossy().to_string())
}

fn load_persisted_session(params: &Value) -> Result<LoadedAcpSession, Box<dyn Error>> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("ACP session id is required")?;
    let cwd = resolve_session_cwd(params.get("cwd"))?;
    let stored = acp_sessions::load(session_id)?
        .ok_or_else(|| format!("ACP session was not found: {session_id}"))?;
    if stored.cwd != cwd {
        return Err("ACP session cwd does not match the stored session".into());
    }
    Ok(LoadedAcpSession {
        session_id: session_id.to_string(),
        cwd,
        created_at: stored.created_at,
        history: stored
            .turns
            .into_iter()
            .map(|turn| AcpTurn {
                user: turn.user,
                assistant: turn.assistant,
            })
            .collect(),
    })
}

fn replay_session_history(
    writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>,
    session_id: &str,
    history: &[AcpTurn],
) -> Result<(), Box<dyn Error>> {
    for (index, turn) in history.iter().enumerate() {
        write_message(
            writer,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "messageId": format!("{session_id}:user:{index}"),
                        "content": {"type": "text", "text": turn.user}
                    }
                }
            }),
        )?;
        write_message(
            writer,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "messageId": format!("{session_id}:assistant:{index}"),
                        "content": {"type": "text", "text": turn.assistant}
                    }
                }
            }),
        )?;
    }
    Ok(())
}

fn persist_session_turn(
    session_id: &str,
    cwd: &str,
    created_at: u64,
    history: &Arc<Mutex<Vec<AcpTurn>>>,
    user: &str,
    assistant: &str,
) -> Result<(), Box<dyn Error>> {
    let mut updated = history.lock().expect("ACP session history lock").clone();
    updated.push(AcpTurn {
        user: user.to_string(),
        assistant: assistant.to_string(),
    });
    let stored = StoredAcpSession {
        schema_version: "acp_session.v1".to_string(),
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        created_at,
        updated_at: unix_timestamp(),
        turns: updated
            .iter()
            .map(|turn| StoredAcpTurn {
                user: turn.user.clone(),
                assistant: turn.assistant.clone(),
            })
            .collect(),
    };
    acp_sessions::save(&stored)?;
    *history.lock().expect("ACP session history lock") = updated;
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn write_message(
    writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>,
    message: Value,
) -> Result<(), Box<dyn Error>> {
    let mut writer = writer.lock().expect("ACP writer lock");
    serde_json::to_writer(&mut *writer, &message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_error(
    writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>,
    id: Option<Value>,
    code: i64,
    message: &str,
) -> Result<(), Box<dyn Error>> {
    write_message(
        writer,
        json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": {"code": code, "message": message}
        }),
    )
}

fn text_sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{acp_client_id_from_initialize, prompt_text, render_prompt_with_history, AcpTurn};
    use serde_json::json;

    #[test]
    fn extracts_text_and_resource_links() {
        let prompt = prompt_text(&json!({
            "prompt": [
                {"type": "text", "text": "Inspect this project."},
                {"type": "resource_link", "uri": "file:///project/config.json", "name": "config"}
            ]
        }))
        .unwrap();
        assert!(prompt.contains("Inspect this project."));
        assert!(prompt.contains("file:///project/config.json"));
    }

    #[test]
    fn rejects_unsupported_only_audio_prompts() {
        let error = prompt_text(&json!({
            "prompt": [{"type": "audio", "data": "AAAA", "mimeType": "audio/wav"}]
        }))
        .unwrap_err();
        assert!(error.to_string().contains("not supported"));
    }

    #[test]
    fn renders_previous_turns_into_the_next_prompt() {
        let prompt = render_prompt_with_history(
            &[AcpTurn {
                user: "first user".to_string(),
                assistant: "first answer".to_string(),
            }],
            "follow up",
        )
        .unwrap();
        assert!(prompt.contains("User:\nfirst user"));
        assert!(prompt.contains("Assistant:\nfirst answer"));
        assert!(prompt.ends_with("Current user:\nfollow up"));
    }

    #[test]
    fn rejects_history_that_exceeds_the_runtime_prompt_limit() {
        let error = render_prompt_with_history(
            &[AcpTurn {
                user: "x".repeat(100_000),
                assistant: "y".repeat(40_000),
            }],
            "follow up",
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn initialize_client_info_becomes_stable_attribution() {
        assert_eq!(
            acp_client_id_from_initialize(&json!({
                "clientInfo": {"name": "Claude Code", "version": "1.2.3"}
            })),
            "acp:claude-code@1.2.3"
        );
        assert_eq!(acp_client_id_from_initialize(&json!({})), "acp:client");
    }
}
