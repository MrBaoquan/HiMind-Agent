use reqwest::blocking::Client;
use reqwest::StatusCode;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::app::builtin_ai_proxy::EventObserver;
use crate::runtime::builtin::{self, BuiltinAIRuntimeEvent};
use crate::Options;

const EVENT_QUEUE_CAPACITY: usize = 256;
const EVENT_UPLOAD_ATTEMPTS: usize = 4;

pub(crate) struct BuiltinAiEventSync {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl BuiltinAiEventSync {
    pub(crate) fn start(options: Options) -> (Self, EventObserver) {
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
                Ok(client) => client,
                Err(error) => {
                    eprintln!("HiMind AI 会话同步不可用：{error}");
                    return;
                }
            };
            while !worker_shutdown.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(event) => upload_with_retry(&client, &options, &worker_shutdown, &event),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
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
                enqueue_event(&sender, event);
            }
        });
        (
            Self {
                shutdown,
                worker: Some(worker),
            },
            observer,
        )
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

fn enqueue_event(sender: &SyncSender<BuiltinAIRuntimeEvent>, event: BuiltinAIRuntimeEvent) {
    match sender.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(event)) => eprintln!(
            "HiMind AI 会话同步队列已满，事件稍后可由会话恢复补齐：{}",
            event.event_id
        ),
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn upload_with_retry(
    client: &Client,
    options: &Options,
    shutdown: &AtomicBool,
    event: &BuiltinAIRuntimeEvent,
) {
    for attempt in 0..EVENT_UPLOAD_ATTEMPTS {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        match upload_event(client, options, event) {
            Ok(()) => return,
            Err(error) if error.transient && attempt + 1 < EVENT_UPLOAD_ATTEMPTS => {
                thread::sleep(retry_delay(attempt));
            }
            Err(error) => {
                eprintln!(
                    "HiMind AI 会话事件同步失败（event={}）：{}",
                    event.event_id, error.message
                );
                return;
            }
        }
    }
}

struct UploadError {
    message: String,
    transient: bool,
}

fn upload_event(
    client: &Client,
    options: &Options,
    event: &BuiltinAIRuntimeEvent,
) -> Result<(), UploadError> {
    let access =
        crate::api::oauth::platform_access_token(options, crate::api::oauth::AI_CONVERSATION_SCOPE)
            .map_err(|error| UploadError {
                message: error.to_string(),
                transient: true,
            })?;
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
        return Ok(());
    }
    let status = response.status();
    Err(UploadError {
        message: format!("Dashboard returned HTTP {status}"),
        transient: transient_status(status),
    })
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

    #[test]
    fn only_transient_http_statuses_are_retried() {
        assert!(transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(transient_status(StatusCode::BAD_GATEWAY));
        assert!(!transient_status(StatusCode::BAD_REQUEST));
        assert!(!transient_status(StatusCode::UNAUTHORIZED));
    }
}
