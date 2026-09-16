use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Serialize;
use std::error::Error;
use std::time::Duration;

use crate::api::client::load_agent_state;
use crate::api::oauth::{platform_access_token, AI_CONVERSATION_SCOPE};
use crate::store::local_runs::{LocalRunLedger, ProjectionOutboxRecord};
use crate::Options;

const PROJECTION_BATCH_LIMIT: usize = 32;
const PROJECTION_TIMEOUT_SECONDS: u64 = 20;

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectionFlushReport {
    pub projected: usize,
    pub retried: usize,
    pub dead_letter: usize,
    pub skipped: usize,
}

pub(crate) fn flush_pending_projections(
    options: &Options,
) -> Result<ProjectionFlushReport, Box<dyn Error>> {
    if !options.mode().dashboard_enabled() {
        return Ok(ProjectionFlushReport::default());
    }
    let ledger = LocalRunLedger::open_default()?;
    let records = ledger.pending_projections(PROJECTION_BATCH_LIMIT)?;
    if records.is_empty() {
        return Ok(ProjectionFlushReport::default());
    }
    let access = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    let state = load_agent_state(&options.state_path)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(PROJECTION_TIMEOUT_SECONDS))
        .build()?;
    let mut report = ProjectionFlushReport::default();
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
            options.api_base.trim_end_matches('/')
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
    status == StatusCode::REQUEST_TIMEOUT
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

    #[test]
    fn retry_delay_backs_off_and_caps() {
        assert_eq!(projection_retry_delay(0), 30);
        assert_eq!(projection_retry_delay(1), 60);
        assert_eq!(projection_retry_delay(5), 960);
        assert_eq!(projection_retry_delay(99), 960);
    }

    #[test]
    fn transient_status_classification_is_explicit() {
        assert!(is_transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(StatusCode::BAD_GATEWAY));
        assert!(!is_transient_status(StatusCode::CONFLICT));
        assert!(!is_transient_status(StatusCode::BAD_REQUEST));
    }
}
