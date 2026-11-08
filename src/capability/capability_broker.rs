//! Per-invocation capability broker for plugin backend processes.
//!
//! Plugins can consume capabilities declared in their manifest, but never
//! receive the Agent endpoint, provider keys, or arbitrary Gateway access.

use base64::Engine;
use serde_json::{json, Value};
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::capability::plugin::external_capability_matches;

pub(crate) const BROKER_URL_ENV: &str = "HIMIND_CAPABILITY_BROKER_URL";
pub(crate) const BROKER_TOKEN_ENV: &str = "HIMIND_CAPABILITY_BROKER_TOKEN";
const MAX_REQUESTS: usize = 64;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CHAIN_DEPTH: usize = 8;

pub(crate) type Dispatcher =
    Arc<dyn Fn(&str, Value) -> Result<Value, String> + Send + Sync + 'static>;

struct SessionState {
    plugin_id: String,
    parent_capability: String,
    token: String,
    deadline: Instant,
    dependencies: Vec<String>,
    capability_chain: Vec<String>,
    dispatch: Dispatcher,
}

pub(crate) struct CapabilityBrokerSession {
    pub url: String,
    pub token: String,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for CapabilityBrokerSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn record_audit(plugin_id: &str, parent: &str, child: &str, result: &str, reason: &str) {
    crate::app::crash::record_event(
        "info",
        &format!(
            "plugin capability broker: plugin={plugin_id} parent={parent} child={child} result={result} reason={reason}"
        ),
    );
}

impl CapabilityBrokerSession {
    pub(crate) fn start(
        plugin_id: &str,
        parent_capability: &str,
        dependencies: &[String],
        capability_chain: Vec<String>,
        timeout: Duration,
        dispatch: Dispatcher,
    ) -> Result<Option<Self>, Box<dyn Error>> {
        if dependencies.is_empty() {
            return Ok(None);
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = random_token();
        let session = Arc::new(SessionState {
            plugin_id: plugin_id.to_string(),
            parent_capability: parent_capability.to_string(),
            token: token.clone(),
            deadline: Instant::now() + timeout.min(Duration::from_secs(600)),
            dependencies: dependencies.to_vec(),
            capability_chain,
            dispatch,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let session_for_thread = Arc::clone(&session);
        let handle = thread::spawn(move || {
            let mut accepted = 0_usize;
            while !stop_for_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        if accepted >= MAX_REQUESTS {
                            let _ = write_json(
                                stream,
                                429,
                                json!({"error":"capability_request_limit"}),
                            );
                            continue;
                        }
                        accepted += 1;
                        if let Err(error) = handle_connection(stream, &session_for_thread) {
                            eprintln!("capability broker request failed: {error:?}");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Some(Self {
            url: format!("http://{address}/invoke"),
            token,
            stop,
            address,
            handle: Some(handle),
        }))
    }
}

fn handle_connection(mut stream: TcpStream, session: &SessionState) -> Result<(), Box<dyn Error>> {
    let (method, path, authorization, body) = read_http_request(&mut stream)?;
    if Instant::now() > session.deadline {
        record_audit(
            &session.plugin_id,
            &session.parent_capability,
            "",
            "denied",
            "session_expired",
        );
        let _ = write_json(
            stream,
            401,
            json!({"error":"capability_broker_session_expired"}),
        );
        return Ok(());
    }
    if method != "POST" || path != "/invoke" {
        let _ = write_json(stream, 404, json!({"error":"not_found"}));
        return Ok(());
    }
    if authorization.as_deref() != Some(format!("Bearer {}", session.token).as_str()) {
        record_audit(
            &session.plugin_id,
            &session.parent_capability,
            "",
            "denied",
            "invalid_token",
        );
        let _ = write_json(
            stream,
            401,
            json!({"error":"invalid_capability_broker_token"}),
        );
        return Ok(());
    }
    let parsed: Value = serde_json::from_slice(&body)?;
    let capability = parsed
        .get("capability")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if capability.is_empty() {
        let _ = write_json(stream, 400, json!({"error":"capability_required"}));
        return Ok(());
    }
    if !session
        .dependencies
        .iter()
        .any(|pattern| external_capability_matches(pattern, capability))
    {
        record_audit(
            &session.plugin_id,
            &session.parent_capability,
            capability,
            "denied",
            "undeclared_capability",
        );
        let _ = write_json(stream, 403, json!({"error":"undeclared_capability"}));
        return Ok(());
    }
    if session
        .capability_chain
        .iter()
        .any(|item| item == capability)
    {
        record_audit(
            &session.plugin_id,
            &session.parent_capability,
            capability,
            "denied",
            "capability_cycle",
        );
        let _ = write_json(stream, 403, json!({"error":"capability_cycle"}));
        return Ok(());
    }
    if session.capability_chain.len() >= MAX_CHAIN_DEPTH {
        record_audit(
            &session.plugin_id,
            &session.parent_capability,
            capability,
            "denied",
            "capability_depth_exceeded",
        );
        let _ = write_json(stream, 403, json!({"error":"capability_depth_exceeded"}));
        return Ok(());
    }
    let input = parsed.get("input").cloned().unwrap_or_else(|| json!({}));
    match (session.dispatch)(capability, input) {
        Ok(result) => {
            record_audit(
                &session.plugin_id,
                &session.parent_capability,
                capability,
                "allowed",
                "",
            );
            let response = json!({"capability": capability, "result": result});
            let bytes = serde_json::to_vec(&response)?;
            if bytes.len() > MAX_RESPONSE_BYTES {
                let _ = write_json(
                    stream,
                    500,
                    json!({"error":"capability_response_too_large"}),
                );
            } else {
                let _ = write_json(stream, 200, response);
            }
        }
        Err(message) => {
            record_audit(
                &session.plugin_id,
                &session.parent_capability,
                capability,
                "failed",
                "dispatch_error",
            );
            let _ = write_json(
                stream,
                409,
                json!({"error":"capability_failed","message":message.chars().take(4096).collect::<String>()}),
            );
        }
    }
    Ok(())
}

fn read_http_request(
    stream: &mut TcpStream,
) -> Result<(String, String, Option<String>, Vec<u8>), Box<dyn Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end;
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err("capability broker request ended before headers".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err("capability broker request too large".into());
        }
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = position + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = headers.lines();
    let first = lines.next().unwrap_or_default();
    let mut first_parts = first.split_whitespace();
    let method = first_parts.next().unwrap_or_default().to_string();
    let path = first_parts.next().unwrap_or_default().to_string();
    let mut authorization = None;
    let mut content_length = 0_usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length > MAX_REQUEST_BYTES {
        return Err("capability broker body too large".into());
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err("capability broker request too large".into());
        }
    }
    let body_end = (header_end + content_length).min(buffer.len());
    Ok((
        method,
        path,
        authorization,
        buffer[header_end..body_end].to_vec(),
    ))
}

fn write_json(mut stream: TcpStream, status: u16, body: Value) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec(&body)?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn post(url: &str, token: &str, body: Value) -> (u16, Value) {
        let address = url
            .trim_start_matches("http://")
            .trim_end_matches("/invoke");
        let mut stream = TcpStream::connect(address).unwrap();
        let body = serde_json::to_vec(&body).unwrap();
        write!(
            stream,
            "POST /invoke HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response);
        let status = text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let body = text
            .split("\r\n\r\n")
            .nth(1)
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or(Value::Null);
        (status, body)
    }

    #[test]
    fn broker_allows_declared_and_rejects_undeclared_or_cycles() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_for_dispatch = Arc::clone(&seen);
        let dispatch: Dispatcher = Arc::new(move |capability, input| {
            seen_for_dispatch
                .lock()
                .unwrap()
                .push(capability.to_string());
            Ok(json!({"echo": input, "capability": capability}))
        });
        let session = CapabilityBrokerSession::start(
            "com.test.plugin",
            "audio.speech",
            &["media.audio.speech".to_string()],
            vec!["audio.speech".to_string()],
            Duration::from_secs(30),
            dispatch,
        )
        .unwrap()
        .unwrap();

        let (status, body) = post(
            &session.url,
            &session.token,
            json!({"capability":"media.audio.speech","input":{"text":"hello"}}),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["result"]["echo"]["text"], "hello");
        assert_eq!(seen.lock().unwrap().len(), 1);

        let (status, body) = post(
            &session.url,
            &session.token,
            json!({"capability":"media.video.generate","input":{}}),
        );
        assert_eq!(status, 403, "{body}");
        assert_eq!(body["error"], "undeclared_capability");
    }

    #[test]
    fn broker_rejects_capability_cycles() {
        let dispatch: Dispatcher = Arc::new(|_, _| Ok(json!({"ok":true})));
        let session = CapabilityBrokerSession::start(
            "com.test.plugin",
            "audio.speech",
            &["audio.*".to_string()],
            vec!["audio.speech".to_string()],
            Duration::from_secs(30),
            dispatch,
        )
        .unwrap()
        .unwrap();
        let (status, body) = post(
            &session.url,
            &session.token,
            json!({"capability":"audio.speech","input":{}}),
        );
        assert_eq!(status, 403, "{body}");
        assert_eq!(body["error"], "capability_cycle");
    }
}
