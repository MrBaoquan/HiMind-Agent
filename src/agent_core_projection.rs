use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Serialize;
use std::error::Error;
use std::time::Duration;

use crate::api::client::load_agent_state;
use crate::api::oauth::{platform_access_token, AI_CONVERSATION_SCOPE};
use crate::store::local_runs::{LocalRunLedger, ProjectionDeadLetterGroup, ProjectionOutboxRecord};
use crate::Options;

const PROJECTION_BATCH_LIMIT: usize = 32;
const PROJECTION_TIMEOUT_SECONDS: u64 = 20;

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionFlushReport {
    pub recovered: usize,
    pub projected: usize,
    pub retried: usize,
    pub dead_letter: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionSyncStatus {
    pub dashboard_enabled: bool,
    pub state: String,
    pub total: u64,
    pub pending: u64,
    pub retrying: u64,
    pub projected: u64,
    pub dead_letter: u64,
    pub oldest_pending_at: String,
    pub last_error: String,
    /// 同步失败按原因归组（最多 3 类）：界面只需给出「卡在哪一类错误上」，不用展开全部记录。
    pub dead_letter_reasons: Vec<ProjectionDeadLetterGroup>,
}

/// 手工重投死信的结果：重投只把记录放回队列，真正上报由投影循环或 `--drain` 完成。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionRequeueReport {
    pub requeued: usize,
    pub dead_letter_before: u64,
    pub dead_letter_after: u64,
    pub pending_after: u64,
    pub remaining_reasons: Vec<ProjectionDeadLetterGroup>,
}

/// 一次性排空队列的结果，用于批量恢复后立刻确认「确实追上去了」。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionDrainReport {
    pub batches: usize,
    pub projected: usize,
    pub retried: usize,
    pub dead_letter: usize,
    pub skipped: usize,
    pub pending_after: u64,
    pub stopped_reason: String,
}

pub(crate) fn projection_sync_status(
    options: &Options,
) -> Result<ProjectionSyncStatus, Box<dyn Error>> {
    let ledger = LocalRunLedger::open_default()?;
    let summary = ledger.projection_outbox_summary()?;
    let dashboard_enabled = options.mode().dashboard_enabled();
    let state = if !dashboard_enabled {
        "local_only"
    } else if summary.dead_letter > 0 {
        "attention"
    } else if summary.pending > 0 {
        "pending"
    } else {
        "synced"
    };
    let dead_letter_reasons = if summary.dead_letter > 0 {
        ledger.dead_letter_projection_groups(3)?
    } else {
        Vec::new()
    };
    Ok(ProjectionSyncStatus {
        dashboard_enabled,
        state: state.to_string(),
        total: summary.total,
        pending: summary.pending,
        retrying: summary.retrying,
        projected: summary.projected,
        dead_letter: summary.dead_letter,
        oldest_pending_at: summary.oldest_pending_at,
        last_error: summary.last_error,
        dead_letter_reasons,
    })
}

/// 重投死信：`None` 覆盖全部死信，`Some(片段)` 只覆盖 `last_error` 命中该片段的记录。
pub(crate) fn requeue_dead_letter_projections(
    error_fragment: Option<&str>,
) -> Result<ProjectionRequeueReport, Box<dyn Error>> {
    let ledger = LocalRunLedger::open_default()?;
    let dead_letter_before = ledger.projection_outbox_summary()?.dead_letter;
    let requeued = ledger.requeue_dead_letter_projections(error_fragment)?;
    let summary = ledger.projection_outbox_summary()?;
    Ok(ProjectionRequeueReport {
        requeued,
        dead_letter_before,
        dead_letter_after: summary.dead_letter,
        pending_after: summary.pending,
        remaining_reasons: ledger.dead_letter_projection_groups(5)?,
    })
}

/// 重投之后把待发队列一次排空，省掉「重投完还要盯着 30 秒一轮的循环」。
///
/// 循环每轮最多处理 [`PROJECTION_BATCH_LIMIT`] 条，因此这里按批推进；某一轮完全没有进展
/// （例如工作台仍不可达、记录全部在重试退避中）就停下，把结果交回调用方判断。
pub(crate) fn drain_pending_projections(
    options: &Options,
    max_batches: usize,
) -> Result<ProjectionDrainReport, Box<dyn Error>> {
    if !options.mode().dashboard_enabled() {
        return Err("AI 工作台未启用，本地运行记录会保留到重新对接后再同步".into());
    }
    let mut report = ProjectionDrainReport::default();
    for _ in 0..max_batches.clamp(1, 2000) {
        let batch = flush_pending_projections(options)?;
        report.batches += 1;
        report.projected += batch.projected;
        report.retried += batch.retried;
        report.dead_letter += batch.dead_letter;
        report.skipped += batch.skipped;
        if batch.projected + batch.retried + batch.dead_letter + batch.skipped == 0 {
            break;
        }
    }
    let summary = LocalRunLedger::open_default()?.projection_outbox_summary()?;
    report.pending_after = summary.pending;
    report.stopped_reason = if summary.pending == 0 {
        "drained".to_string()
    } else if report.batches >= max_batches.clamp(1, 2000) {
        "batch_limit".to_string()
    } else {
        "no_progress".to_string()
    };
    Ok(report)
}

pub(crate) fn flush_pending_projections(
    options: &Options,
) -> Result<ProjectionFlushReport, Box<dyn Error>> {
    if !options.mode().dashboard_enabled() {
        return Ok(ProjectionFlushReport::default());
    }
    let ledger = LocalRunLedger::open_default()?;
    let recovered =
        ledger.requeue_dead_letter_projections_with_error_fragment("HTTP 401 Unauthorized")?;
    let records = ledger.pending_projections(PROJECTION_BATCH_LIMIT)?;
    if records.is_empty() {
        return Ok(ProjectionFlushReport {
            recovered,
            ..ProjectionFlushReport::default()
        });
    }
    let access = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    let state = load_agent_state(&options.state_path)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(PROJECTION_TIMEOUT_SECONDS))
        .build()?;
    let mut report = ProjectionFlushReport {
        recovered,
        ..ProjectionFlushReport::default()
    };
    for record in records {
        if record.projection_type != "run_projection" {
            ledger.mark_projection_dead_letter(
                record.id,
                &format!("unsupported projection type: {}", record.projection_type),
            )?;
            report.dead_letter += 1;
            continue;
        }
        match deliver_projection(&client, options, &state.agent_id, &access.token, &record) {
            Ok(()) => {
                ledger.mark_projection_projected(record.id)?;
                report.projected += 1;
            }
            Err(DeliveryError::Transient(error)) => {
                let next_attempt_at =
                    unix_timestamp().saturating_add(projection_retry_delay(record.attempts));
                ledger.mark_projection_failed(record.id, &error, &next_attempt_at.to_string())?;
                report.retried += 1;
            }
            Err(DeliveryError::Permanent(error)) => {
                ledger.mark_projection_dead_letter(record.id, &error)?;
                report.dead_letter += 1;
            }
            Err(DeliveryError::Skipped) => {
                report.skipped += 1;
            }
        }
    }
    Ok(report)
}

#[derive(Debug)]
enum DeliveryError {
    Transient(String),
    Permanent(String),
    Skipped,
}

fn deliver_projection(
    client: &Client,
    options: &Options,
    agent_id: &str,
    access_token: &str,
    record: &ProjectionOutboxRecord,
) -> Result<(), DeliveryError> {
    if !record.next_attempt_at.is_empty() {
        if let Ok(next_attempt_at) = record.next_attempt_at.parse::<u64>() {
            if next_attempt_at > unix_timestamp() {
                return Err(DeliveryError::Skipped);
            }
        }
    }
    let response = client
        .post(format!(
            "{}/api/integrations/agent-core/v1/projections",
            options.api_base().trim_end_matches('/')
        ))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&record.payload)
        .send()
        .map_err(|error| DeliveryError::Transient(error.to_string()))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let detail = response
        .text()
        .unwrap_or_default()
        .chars()
        .take(2_000)
        .collect::<String>();
    let message = format!("Dashboard projection returned HTTP {status}: {detail}");
    if is_transient_status(status) {
        Err(DeliveryError::Transient(message))
    } else {
        Err(DeliveryError::Permanent(message))
    }
}

fn is_transient_status(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn projection_retry_delay(attempts: u32) -> u64 {
    let exponent = attempts.min(5);
    30_u64.saturating_mul(1_u64 << exponent)
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn retry_delay_backs_off_and_caps() {
        assert_eq!(projection_retry_delay(0), 30);
        assert_eq!(projection_retry_delay(1), 60);
        assert_eq!(projection_retry_delay(5), 960);
        assert_eq!(projection_retry_delay(99), 960);
    }

    #[test]
    fn transient_status_classification_is_explicit() {
        assert!(is_transient_status(StatusCode::UNAUTHORIZED));
        assert!(is_transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(StatusCode::BAD_GATEWAY));
        assert!(!is_transient_status(StatusCode::CONFLICT));
        assert!(!is_transient_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn projection_delivery_posts_expected_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]).to_string();
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .unwrap();
            request
        });

        let options = crate::Options {
            api_base: crate::api_base_cell(format!("http://{address}")),
            ..crate::Options::from_env()
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();
        let record = ProjectionOutboxRecord {
            id: 1,
            projection_type: "run_projection".to_string(),
            aggregate_id: "run-1".to_string(),
            dedupe_key: "projection:run-1".to_string(),
            payload: json!({
                "schema_version": "run_projection.v1",
                "projection_id": "projection-1",
                "idempotency_key": "projection-idem-1",
                "sent_at": "1",
                "interaction": {},
                "run": {}
            }),
            status: "pending".to_string(),
            attempts: 0,
            next_attempt_at: String::new(),
            last_error: String::new(),
        };
        deliver_projection(&client, &options, "agent-1", "test-token", &record).unwrap();
        let request = server.join().unwrap();
        let normalized = request.to_ascii_lowercase();
        assert!(request.starts_with("POST /api/integrations/agent-core/v1/projections HTTP/1.1"));
        assert!(normalized.contains("authorization: bearer test-token"));
        assert!(normalized.contains("x-himind-agent-id: agent-1"));
        assert!(normalized.contains("content-type: application/json"));
    }
}
