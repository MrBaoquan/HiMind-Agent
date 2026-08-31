use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Sender},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::app::builtin_ai_gateway::{RuntimeCapabilities, RuntimeCapabilitiesState};
use crate::app::builtin_ai_proxy::EventObserver;
use crate::runtime::builtin::{self, BuiltinAIRuntimeEvent};
use crate::Options;

const EVENT_UPLOAD_ATTEMPTS: usize = 4;
const RUNTIME_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const EVENT_OUTBOX_REPLAY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeHeartbeatTarget {
    binding_id: String,
    provider_session_id: String,
}

pub(crate) struct BuiltinAiEventSync {
    shutdown: Arc<AtomicBool>,
    capabilities: Arc<Mutex<RuntimeCapabilities>>,
    worker: Option<JoinHandle<()>>,
}

impl BuiltinAiEventSync {
    pub(crate) fn start(
        options: Options,
        initial_capabilities: RuntimeCapabilities,
    ) -> (Self, EventObserver) {
        if !options.mode().dashboard_enabled() {
            let capabilities = Arc::new(Mutex::new(initial_capabilities));
            return (
                Self {
                    shutdown: Arc::new(AtomicBool::new(true)),
                    capabilities,
                    worker: None,
                },
                Arc::new(|_| {}),
            );
        }
        let outbox_path = event_outbox_path();
        if let Err(error) = fs::create_dir_all(&outbox_path) {
            eprintln!("HiMind AI 会话事件 outbox 初始化失败：{error}");
        }
        let (sender, receiver) = mpsc::channel::<BuiltinAIRuntimeEvent>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let capabilities = Arc::new(Mutex::new(initial_capabilities));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_capabilities = Arc::clone(&capabilities);
        let worker_outbox_path = outbox_path.clone();
        let worker = thread::spawn(move || {
            let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
                Ok(client) => client,
                Err(error) => {
                    eprintln!("HiMind AI 会话同步不可用：{error}");
                    return;
                }
            };
            let mut last_heartbeat = Instant::now();
            let mut last_outbox_replay = Instant::now();
            let mut heartbeat_target: Option<RuntimeHeartbeatTarget> = None;
            while !worker_shutdown.load(Ordering::Acquire) {
                if !options.mode().dashboard_enabled() {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(event) => {
                        if let Ok(target) = upload_with_retry(
                            &client,
                            &options,
                            &worker_shutdown,
                            &worker_capabilities,
                            &event,
                        ) {
                            remove_persisted_event(&worker_outbox_path, &event);
                            if let Some(target) = target {
                                heartbeat_target = Some(target);
                                last_heartbeat = Instant::now();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if last_outbox_replay.elapsed() >= EVENT_OUTBOX_REPLAY_INTERVAL {
                            if let Some(event) = next_persisted_event(&worker_outbox_path) {
                                if let Ok(target) = upload_with_retry(
                                    &client,
                                    &options,
                                    &worker_shutdown,
                                    &worker_capabilities,
                                    &event,
                                ) {
                                    remove_persisted_event(&worker_outbox_path, &event);
                                    if let Some(target) = target {
                                        heartbeat_target = Some(target);
                                        last_heartbeat = Instant::now();
                                    }
                                }
                            }
                            last_outbox_replay = Instant::now();
                        }
                        if last_heartbeat.elapsed() >= RUNTIME_HEARTBEAT_INTERVAL {
                            if let Some(target) = heartbeat_target.as_ref() {
                                if let Ok(access) = crate::api::oauth::platform_access_token(
                                    &options,
                                    crate::api::oauth::AI_CONVERSATION_SCOPE,
                                ) {
                                    let _ = heartbeat_runtime_session(
                                        &client,
                                        &options,
                                        &access,
                                        target,
                                        &worker_capabilities,
                                    );
                                }
                            }
                            last_heartbeat = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        let projector = Arc::new(Mutex::new(builtin::interactive_event_projector()));
        let observer_projector = Arc::clone(&projector);
        let observer: EventObserver = Arc::new(move |raw| {
            let event = observer_projector
                .lock()
                .ok()
                .and_then(|mut projector| projector.project(&raw));
            if let Some(event) = event {
                enqueue_event(&sender, &outbox_path, event);
            }
        });
        (
            Self {
                shutdown,
                capabilities,
                worker: Some(worker),
            },
            observer,
        )
    }

    pub(crate) fn set_capabilities(&self, next: RuntimeCapabilities) {
        if let Ok(mut current) = self.capabilities.lock() {
            *current = next;
        }
    }

    pub(crate) fn capabilities_state(&self) -> RuntimeCapabilitiesState {
        Arc::clone(&self.capabilities)
    }

    pub(crate) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for BuiltinAiEventSync {
    fn drop(&mut self) {
        self.stop();
    }
}

fn enqueue_event(
    sender: &Sender<BuiltinAIRuntimeEvent>,
    outbox: &Path,
    event: BuiltinAIRuntimeEvent,
) {
    if let Err(error) = persist_event(outbox, &event) {
        eprintln!(
            "HiMind AI 会话事件无法写入 outbox（event={}）：{}",
            event.event_id, error
        );
    }
    if sender.send(event).is_err() {
        eprintln!("HiMind AI 会话同步 worker 已退出，事件将由 outbox 重放");
    }
}

fn event_outbox_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("outbox")
        .join("builtin-ai-events")
}

fn persisted_event_path(outbox: &Path, event: &BuiltinAIRuntimeEvent) -> PathBuf {
    let safe_id = if !event.event_id.is_empty()
        && event
            .event_id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.'))
    {
        event.event_id.clone()
    } else {
        let mut digest = Sha256::new();
        digest.update(event.event_id.as_bytes());
        format!("{:x}", digest.finalize())
    };
    outbox.join(format!(
        "{:020}-{:020}-{safe_id}.json",
        event.occurred_at_ms.max(0),
        event.sequence.max(0)
    ))
}

fn persist_event(outbox: &Path, event: &BuiltinAIRuntimeEvent) -> Result<(), String> {
    fs::create_dir_all(outbox).map_err(|error| error.to_string())?;
    let content = serde_json::to_vec(event).map_err(|error| error.to_string())?;
    crate::store::atomic_file::atomic_write(&persisted_event_path(outbox, event), &content)
        .map_err(|error| error.to_string())
}

fn remove_persisted_event(outbox: &Path, event: &BuiltinAIRuntimeEvent) {
    let _ = fs::remove_file(persisted_event_path(outbox, event));
}

fn next_persisted_event(outbox: &Path) -> Option<BuiltinAIRuntimeEvent> {
    let mut paths = fs::read_dir(outbox)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let Ok(content) = fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<BuiltinAIRuntimeEvent>(&content) {
            Ok(event) => return Some(event),
            Err(error) => eprintln!(
                "HiMind AI 会话事件 outbox 文件损坏（{}）：{error}",
                path.display()
            ),
        }
    }
    None
}

fn upload_with_retry(
    client: &Client,
    options: &Options,
    shutdown: &AtomicBool,
    capabilities: &Arc<Mutex<RuntimeCapabilities>>,
    event: &BuiltinAIRuntimeEvent,
) -> Result<Option<RuntimeHeartbeatTarget>, UploadError> {
    for attempt in 0..EVENT_UPLOAD_ATTEMPTS {
        if shutdown.load(Ordering::Acquire) {
            return Err(UploadError {
                message: "会话同步 worker 正在关闭".to_string(),
                transient: true,
            });
        }
        match upload_event(client, options, capabilities, event) {
            Ok(target) => return Ok(target),
            Err(error) if error.transient && attempt + 1 < EVENT_UPLOAD_ATTEMPTS => {
                thread::sleep(retry_delay(attempt));
            }
            Err(error) => {
                eprintln!(
                    "HiMind AI 会话事件同步失败（event={}）：{}",
                    event.event_id, error.message
                );
                return Err(error);
            }
        }
    }
    unreachable!("event upload retry loop must return from a match arm")
}

struct UploadError {
    message: String,
    transient: bool,
}

fn upload_event(
    client: &Client,
    options: &Options,
    capabilities: &Arc<Mutex<RuntimeCapabilities>>,
    event: &BuiltinAIRuntimeEvent,
) -> Result<Option<RuntimeHeartbeatTarget>, UploadError> {
    if !options.mode().dashboard_enabled() {
        return Err(UploadError {
            message: "当前运行模式不支持会话同步".to_string(),
            transient: false,
        });
    }
    let access =
        crate::api::oauth::platform_access_token(options, crate::api::oauth::AI_CONVERSATION_SCOPE)
            .map_err(|error| UploadError {
                message: error.to_string(),
                transient: true,
            })?;
    // Registering is idempotent and lets Dashboard expose a DSH session even
    // before the first message is mirrored. Older Dashboard versions may not
    // have this endpoint, so event delivery remains the source of truth.
    let current_capabilities = capabilities
        .lock()
        .map(|value| *value)
        .unwrap_or_else(|_| RuntimeCapabilities::conservative());
    let heartbeat_target = register_runtime_session(
        client,
        options,
        &access,
        &event.session_id,
        current_capabilities,
    )
    .ok();
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/events",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(event)
        .send()
        .map_err(|error| UploadError {
            message: error.to_string(),
            transient: true,
        })?;
    if response.status().is_success() {
        return Ok(heartbeat_target);
    }
    let status = response.status();
    Err(UploadError {
        message: format!("Dashboard returned HTTP {status}"),
        transient: transient_status(status),
    })
}

fn register_runtime_session(
    client: &Client,
    options: &Options,
    access: &crate::api::oauth::AgentAccessToken,
    session_id: &str,
    capabilities: RuntimeCapabilities,
) -> Result<RuntimeHeartbeatTarget, UploadError> {
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/sessions/register",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&json!({
            "provider": "himind.builtin",
            "provider_session_id": session_id,
            "status": "online",
            "generation": 1,
            "capabilities": capabilities.as_json(),
            "metadata": {
                "surface": "himind_agent",
                "runtime_contract": "himind.builtin",
                "remote_control_status": capabilities.remote_control_status()
            }
        }))
        .send()
        .map_err(|error| UploadError {
            message: error.to_string(),
            transient: true,
        })?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(UploadError {
            message: format!("Dashboard runtime session registration returned HTTP {status}"),
            transient: transient_status(status),
        });
    }
    #[derive(Deserialize)]
    struct RuntimeSessionRegistration {
        session: RuntimeSessionIdentity,
    }
    #[derive(Deserialize)]
    struct RuntimeSessionIdentity {
        id: String,
        provider_session_id: String,
    }
    let registered = response
        .json::<RuntimeSessionRegistration>()
        .map_err(|error| UploadError {
            message: error.to_string(),
            transient: true,
        })?;
    if registered.session.id.trim().is_empty()
        || registered.session.provider_session_id != session_id
    {
        return Err(UploadError {
            message: "Dashboard returned an invalid runtime session registration".to_string(),
            transient: false,
        });
    }
    Ok(RuntimeHeartbeatTarget {
        binding_id: registered.session.id,
        provider_session_id: registered.session.provider_session_id,
    })
}

fn heartbeat_runtime_session(
    client: &Client,
    options: &Options,
    access: &crate::api::oauth::AgentAccessToken,
    target: &RuntimeHeartbeatTarget,
    capabilities: &Arc<Mutex<RuntimeCapabilities>>,
) -> Result<(), UploadError> {
    let current_capabilities = capabilities
        .lock()
        .map(|value| *value)
        .unwrap_or_else(|_| RuntimeCapabilities::conservative());
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/sessions/{}/heartbeat",
            options.api_base.trim_end_matches('/'),
            target.binding_id
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&json!({
            "provider": "himind.builtin",
            "provider_session_id": target.provider_session_id,
            "status": "online",
            "generation": 1,
            "capabilities": current_capabilities.as_json(),
            "metadata": {
                "surface": "himind_agent",
                "runtime_contract": "himind.builtin",
                "remote_control_status": current_capabilities.remote_control_status()
            }
        }))
        .send()
        .map_err(|error| UploadError {
            message: error.to_string(),
            transient: true,
        })?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        Err(UploadError {
            message: format!("Dashboard runtime session heartbeat returned HTTP {status}"),
            transient: transient_status(status),
        })
    }
}

fn transient_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(match attempt {
        0 => 250,
        1 => 1_000,
        _ => 3_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn only_transient_http_statuses_are_retried() {
        assert!(transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(transient_status(StatusCode::BAD_GATEWAY));
        assert!(!transient_status(StatusCode::BAD_REQUEST));
        assert!(!transient_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn persisted_events_can_be_replayed_and_removed() {
        let outbox = std::env::temp_dir().join(format!(
            "himind-ai-event-outbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let event = BuiltinAIRuntimeEvent {
            schema_version: 1,
            session_id: "session-test".to_string(),
            event_id: "event-test".to_string(),
            sequence: 1,
            turn_id: String::new(),
            event_type: "approval.requested".to_string(),
            content: String::new(),
            label: "允许测试".to_string(),
            outcome: String::new(),
            request_rpc_id: "rpc-test".to_string(),
            approval_id: "approval-test".to_string(),
            question_payload: None,
            occurred_at_ms: 1,
        };
        let mut later = event.clone();
        later.event_id = "event-later".to_string();
        later.sequence = 2;
        later.occurred_at_ms = 2;
        persist_event(&outbox, &later).expect("persist later event");
        persist_event(&outbox, &event).expect("persist event");
        assert_eq!(next_persisted_event(&outbox), Some(event.clone()));
        remove_persisted_event(&outbox, &event);
        assert_eq!(next_persisted_event(&outbox), Some(later.clone()));
        remove_persisted_event(&outbox, &later);
        assert_eq!(next_persisted_event(&outbox), None);
        let _ = fs::remove_dir_all(outbox);
    }
}
