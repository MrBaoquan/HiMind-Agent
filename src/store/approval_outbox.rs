use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::credentials;

const MAX_APPROVAL_DECISION_BYTES: usize = 64 * 1024;

/// A locally decided approval that still needs to be acknowledged by Dashboard.
/// The payload is DPAPI protected on disk so approval identifiers and decisions
/// cannot be read by another Windows user account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ApprovalDecisionRecord {
    pub approval_id: String,
    pub approved: bool,
    pub idempotency_key: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub user_id: String,
    pub created_at: u64,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default)]
    pub next_attempt_at: u64,
    #[serde(default)]
    pub last_error: String,
}

fn outbox_dir(state_path: &Path) -> PathBuf {
    state_path.with_file_name("approval-decision-outbox")
}

pub(crate) fn store(
    state_path: &Path,
    record: &ApprovalDecisionRecord,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if record.approval_id.trim().is_empty() || record.idempotency_key.trim().is_empty() {
        return Err("approval decision outbox requires approval_id and idempotency_key".into());
    }
    let dir = outbox_dir(state_path);
    fs::create_dir_all(&dir)?;
    let payload = serde_json::to_string(record)?;
    let protected = credentials::protect_secret_for_current_user(&payload)?;
    let data = protected.as_bytes();
    if data.len() > MAX_APPROVAL_DECISION_BYTES {
        return Err("approval decision outbox record is too large".into());
    }
    let name = format!(
        "{}-{}.json",
        safe_name(&record.approval_id),
        safe_name(&record.idempotency_key)
    );
    let path = dir.join(name);
    let lock = super::atomic_file::lock(&path)
        .map_err(|error| io::Error::other(format!("lock approval outbox record: {error}")))?;
    let result = super::atomic_file::atomic_write(&path, data)
        .map_err(|error| io::Error::other(format!("write approval outbox record: {error}")));
    drop(lock);
    result.map(|_| path).map_err(Into::into)
}

pub(crate) fn list(
    state_path: &Path,
) -> Result<Vec<(PathBuf, ApprovalDecisionRecord)>, Box<dyn std::error::Error>> {
    let dir = outbox_dir(state_path);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let encoded = match fs::read(&path) {
            Ok(value) if value.len() <= MAX_APPROVAL_DECISION_BYTES => value,
            _ => continue,
        };
        let Ok(protected) = String::from_utf8(encoded) else {
            continue;
        };
        let Ok(payload) = credentials::unprotect_secret_for_current_user(&protected) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<ApprovalDecisionRecord>(&payload) else {
            continue;
        };
        records.push((path, record));
    }
    records.sort_by(|left, right| {
        left.1
            .created_at
            .cmp(&right.1.created_at)
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(records)
}

pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn is_due(record: &ApprovalDecisionRecord, now: u64) -> bool {
    record.status == "pending" && (record.next_attempt_at == 0 || record.next_attempt_at <= now)
}

pub(crate) fn schedule_retry(record: &mut ApprovalDecisionRecord, now: u64, error: &str) {
    record.status = "pending".to_string();
    record.attempt_count = record.attempt_count.saturating_add(1);
    // 2s, 4s, 8s ... capped at one hour. Keep the record forever until the
    // Dashboard accepts it; an approval decision is an audit fact, not a best
    // effort notification.
    let shift = record.attempt_count.min(10);
    let delay = 2_u64.saturating_pow(shift).min(3600);
    record.next_attempt_at = now.saturating_add(delay);
    record.last_error = truncate_error(error);
}

pub(crate) fn mark_dead_letter(record: &mut ApprovalDecisionRecord, error: &str) {
    record.status = "dead_letter".to_string();
    record.attempt_count = record.attempt_count.saturating_add(1);
    record.next_attempt_at = u64::MAX;
    record.last_error = truncate_error(error);
}

fn default_status() -> String {
    "pending".to_string()
}

fn truncate_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 512;
    error.chars().take(MAX_ERROR_CHARS).collect()
}

fn safe_name(value: &str) -> String {
    let normalized = value.trim();
    if normalized.is_empty() {
        return "unknown".to_string();
    }
    normalized
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{is_due, mark_dead_letter, safe_name, schedule_retry, ApprovalDecisionRecord};

    #[test]
    fn retry_backoff_is_bounded_and_due_state_is_monotonic() {
        let mut record = ApprovalDecisionRecord {
            approval_id: "approval-1".into(),
            approved: true,
            idempotency_key: "request-1".into(),
            agent_id: "agent-1".into(),
            user_id: "user-1".into(),
            created_at: 100,
            status: "pending".into(),
            attempt_count: 0,
            next_attempt_at: 0,
            last_error: String::new(),
        };
        assert!(is_due(&record, 100));
        schedule_retry(&mut record, 100, "network down");
        assert_eq!(record.attempt_count, 1);
        assert_eq!(record.next_attempt_at, 102);
        assert!(!is_due(&record, 101));
        assert!(is_due(&record, 102));
        for _ in 0..20 {
            schedule_retry(&mut record, 100, "network down");
        }
        assert!(record.next_attempt_at <= 3700);
        assert_eq!(safe_name("approval/1"), "approval_1");
        mark_dead_letter(&mut record, "expired approval");
        assert_eq!(record.status, "dead_letter");
        assert!(!is_due(&record, u64::MAX));
    }
}
