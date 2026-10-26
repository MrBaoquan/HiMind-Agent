//! Per-invocation credential broker for external plugins.
//!
//! A plugin process never receives a long-lived store handle. For each
//! capability invocation the Agent opens a loopback-only listener, injects a
//! session token into the child environment, and accepts at most a small number
//! of credential acquisitions until the invocation ends.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::capability::plugin::{credential_declaration_fingerprint, PluginCredentialRequest};

pub(crate) const BROKER_PERMISSION: &str = "secret.broker.acquire";
pub(crate) const BROKER_URL_ENV: &str = "HIMIND_CREDENTIAL_BROKER_URL";
pub(crate) const BROKER_TOKEN_ENV: &str = "HIMIND_CREDENTIAL_BROKER_TOKEN";
const MAX_REQUESTS: usize = 8;
const MAX_REQUEST_BYTES: usize = 64 * 1024;

struct SessionState {
    plugin_id: String,
    capability: String,
    token: String,
    deadline: Instant,
    credentials: BTreeMap<String, (String, BTreeSet<String>)>,
}

pub(crate) struct BrokerSession {
    pub url: String,
    pub token: String,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for BrokerSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the nonblocking accept loop immediately.
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl BrokerSession {
    pub(crate) fn start(
        plugin_id: &str,
        capability: &str,
        credentials: &[PluginCredentialRequest],
        timeout: Duration,
    ) -> Result<Option<Self>, Box<dyn Error>> {
        if credentials.is_empty() {
            return Ok(None);
        }
        let fingerprint = credential_declaration_fingerprint(credentials);
        let declared = credentials
            .iter()
            .map(|credential| {
                (
                    credential.id.clone(),
                    (
                        fingerprint.clone(),
                        credential
                            .fields
                            .iter()
                            .map(|field| field.key.clone())
                            .collect::<BTreeSet<_>>(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = random_token();
        let session = Arc::new(SessionState {
            plugin_id: plugin_id.to_string(),
            capability: capability.to_string(),
            token: token.clone(),
            deadline: Instant::now() + timeout.min(Duration::from_secs(120)),
            credentials: declared,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let session_for_thread = Arc::clone(&session);
        let handle = thread::spawn(move || {
            let mut accepted = 0_usize;
            while !stop_for_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        if accepted >= MAX_REQUESTS {
                            let _ = write_json(
                                stream,
                                429,
                                json!({"error":"credential_request_limit"}),
                            );
                            continue;
                        }
                        accepted += 1;
                        if let Err(error) = handle_connection(stream, &session_for_thread) {
                            eprintln!("credential broker request failed: {error:?}");
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
            url: format!("http://{address}/acquire"),
            token,
            stop,
            address,
            handle: Some(handle),
        }))
    }
}

fn handle_connection(mut stream: TcpStream, session: &SessionState) -> Result<(), Box<dyn Error>> {
    if Instant::now() > session.deadline {
        let _ = crate::store::plugin_credentials::record_audit(
            "plugin",
            &session.plugin_id,
            "",
            &session.capability,
            "denied",
            "broker_session_expired",
        );
        let _ = write_json(stream, 401, json!({"error":"broker_session_expired"}));
        return Ok(());
    }
    let (method, path, authorization, body) = read_http_request(&mut stream)?;
    if method != "POST" || path != "/acquire" {
        let _ = crate::store::plugin_credentials::record_audit(
            "plugin",
            &session.plugin_id,
            "",
            &session.capability,
            "denied",
            "invalid_broker_request",
        );
        let _ = write_json(stream, 404, json!({"error":"not_found"}));
        return Ok(());
    }
    if authorization.as_deref() != Some(format!("Bearer {}", session.token).as_str()) {
        let _ = crate::store::plugin_credentials::record_audit(
            "plugin",
            &session.plugin_id,
            "",
            &session.capability,
            "denied",
            "invalid_broker_token",
        );
        let _ = write_json(stream, 401, json!({"error":"invalid_broker_token"}));
        return Ok(());
    }
    let parsed: Value = serde_json::from_slice(&body)?;
    let credential_id = parsed
        .get("credential_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let purpose = parsed
        .get("purpose")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if purpose != session.capability {
        let _ = crate::store::plugin_credentials::record_audit(
            "plugin",
            &session.plugin_id,
            credential_id,
            &session.capability,
            "denied",
            "credential_scope_mismatch",
        );
        let _ = write_json(stream, 403, json!({"error":"credential_scope_mismatch"}));
        return Ok(());
    }
    let Some((fingerprint, allowed_fields)) = session.credentials.get(credential_id) else {
        let _ = crate::store::plugin_credentials::record_audit(
            "plugin",
            &session.plugin_id,
            credential_id,
            &session.capability,
            "denied",
            "undeclared_credential",
        );
        let _ = write_json(stream, 403, json!({"error":"undeclared_credential"}));
        return Ok(());
    };
    match crate::store::plugin_credentials::broker_fields(
        &session.plugin_id,
        credential_id,
        fingerprint,
    ) {
        Ok(mut fields) => {
            fields.retain(|key, _| allowed_fields.contains(key));
            let _ = crate::store::plugin_credentials::record_audit(
                "plugin",
                &session.plugin_id,
                credential_id,
                &session.capability,
                "allowed",
                "",
            );
            let expires_at = unix_now() + 30;
            let response = json!({
                "credential_id": credential_id,
                "fields": fields,
                "expires_at": expires_at,
            });
            let _ = write_json(stream, 200, response);
            Ok(())
        }
        Err(error) => {
            let reason = error.to_string();
            let _ = crate::store::plugin_credentials::record_audit(
                "plugin",
                &session.plugin_id,
                credential_id,
                &session.capability,
                "denied",
                &reason,
            );
            let _ = write_json(
                stream,
                409,
                json!({"error":"credential_unavailable","message":reason}),
            );
            Ok(())
        }
    }
}

fn read_http_request(
    stream: &mut TcpStream,
) -> Result<(String, String, Option<String>, Vec<u8>), Box<dyn Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end;
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err("credential broker request ended before headers".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err("credential broker request too large".into());
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
        return Err("credential broker body too large".into());
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err("credential broker request too large".into());
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
    use crate::capability::plugin::{PluginCredentialField, PluginCredentialRequest};

    fn request() -> PluginCredentialRequest {
        PluginCredentialRequest {
            id: "demo.api".to_string(),
            label: "Demo API".to_string(),
            kind: "api_key".to_string(),
            fields: vec![
                PluginCredentialField {
                    key: "account".to_string(),
                    label: "账号".to_string(),
                    field_type: "text".to_string(),
                    secret: false,
                    required: true,
                    placeholder: String::new(),
                    help: String::new(),
                },
                PluginCredentialField {
                    key: "api_key".to_string(),
                    label: "API Key".to_string(),
                    field_type: "password".to_string(),
                    secret: true,
                    required: true,
                    placeholder: String::new(),
                    help: String::new(),
                },
            ],
            required: true,
            description: String::new(),
            docs_url: String::new(),
            verify_capability: String::new(),
        }
    }

    fn raw_broker_request(session: &BrokerSession, body: &str) -> String {
        use std::io::Read;
        let mut stream = TcpStream::connect(session.address).unwrap();
        write!(
            stream,
            "POST /acquire HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            session.address,
            session.token,
            body.len(),
            body
        )
        .unwrap();
        stream.flush().unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn broker_allows_declared_and_rejects_undeclared_credentials() {
        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-broker-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &root);

        let declared = vec![request()];
        let fingerprint = credential_declaration_fingerprint(&declared);
        let mut public_fields = BTreeMap::new();
        public_fields.insert("account".to_string(), "demo-user".to_string());
        let mut secret_fields = BTreeMap::new();
        secret_fields.insert("api_key".to_string(), "sk-broker-secret".to_string());
        secret_fields.insert("legacy_field".to_string(), "must-not-leak".to_string());
        crate::store::plugin_credentials::save_profile(
            "com.test.plugin",
            "demo.api",
            &public_fields,
            &secret_fields,
            &fingerprint,
        )
        .unwrap();

        let session = BrokerSession::start(
            "com.test.plugin",
            "demo.run",
            &declared,
            Duration::from_secs(5),
        )
        .unwrap()
        .unwrap();
        let allowed = raw_broker_request(
            &session,
            &json!({"credential_id":"demo.api","purpose":"demo.run"}).to_string(),
        );
        assert!(allowed.starts_with("HTTP/1.1 200 OK"), "{allowed}");
        assert!(allowed.contains("\"account\":\"demo-user\""), "{allowed}");
        assert!(
            allowed.contains("\"api_key\":\"sk-broker-secret\""),
            "{allowed}"
        );

        let wrong_purpose = raw_broker_request(
            &session,
            &json!({"credential_id":"demo.api","purpose":"other.run"}).to_string(),
        );
        assert!(
            wrong_purpose.starts_with("HTTP/1.1 403 Forbidden"),
            "{wrong_purpose}"
        );

        let denied = raw_broker_request(
            &session,
            &json!({"credential_id":"other.secret","purpose":"demo.run"}).to_string(),
        );
        assert!(denied.starts_with("HTTP/1.1 403 Forbidden"), "{denied}");

        drop(session);
        let audit = std::fs::read_to_string(root.join("credential-audit.jsonl")).unwrap();
        assert!(audit.contains("allowed"));
        assert!(audit.contains("undeclared_credential"));
        assert!(audit.contains("credential_scope_mismatch"));
        assert!(!audit.contains("sk-broker-secret"));

        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
