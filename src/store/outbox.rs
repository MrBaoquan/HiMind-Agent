use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_OUTBOX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskReportRecord {
    pub task_id: String,
    pub agent_id: String,
    pub execution_id: String,
    pub lease_id: String,
    pub status: String,
    pub progress: i32,
    pub detail: String,
    pub result: Value,
    pub error: String,
}

pub(crate) fn outbox_dir(state_path: &Path) -> PathBuf {
    state_path.with_file_name("task-report-outbox")
}

pub(crate) fn store_report(state_path: &Path, report: &TaskReportRecord) -> io::Result<PathBuf> {
    let dir = outbox_dir(state_path);
    fs::create_dir_all(&dir)?;
    let name = format!(
        "{}-{}-{}.json",
        safe_name(&report.task_id),
        safe_name(&report.execution_id),
        safe_name(&report.status)
    );
    let path = dir.join(name);
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        safe_name(&report.task_id),
        timestamp()
    ));
    let data = serde_json::to_vec(report).map_err(io::Error::other)?;
    if data.len() > MAX_OUTBOX_REPORT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "task report is too large",
        ));
    }
    fs::write(&temp, data)?;
    fs::rename(&temp, &path)?;
    Ok(path)
}

pub(crate) fn list_reports(state_path: &Path) -> io::Result<Vec<(PathBuf, TaskReportRecord)>> {
    let dir = outbox_dir(state_path);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut reports = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let data = match fs::read(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if data.len() > MAX_OUTBOX_REPORT_BYTES {
            continue;
        }
        if let Ok(report) = serde_json::from_slice::<TaskReportRecord>(&data) {
            reports.push((path, report));
        }
    }
    reports.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(reports)
}

pub(crate) fn remove_report(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn remove_reports_for_execution(
    state_path: &Path,
    task_id: &str,
    execution_id: &str,
    except: Option<&Path>,
) -> io::Result<usize> {
    let mut removed = 0;
    for (path, report) in list_reports(state_path)? {
        if report.task_id != task_id
            || report.execution_id != execution_id
            || except.is_some_and(|value| value == path)
        {
            continue;
        }
        remove_report(&path)?;
        removed += 1;
    }
    Ok(removed)
}

fn safe_name(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "unknown".to_string();
    }
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_lists_and_removes_report_atomically() {
        let root = std::env::temp_dir().join(format!("himind-outbox-{}", timestamp()));
        let state = root.join("agent-state.json");
        let report = TaskReportRecord {
            task_id: "task/1".to_string(),
            agent_id: "agent-1".to_string(),
            execution_id: "exec-1".to_string(),
            lease_id: "lease-1".to_string(),
            status: "finished".to_string(),
            progress: 100,
            detail: "done".to_string(),
            result: serde_json::json!({"ok": true}),
            error: String::new(),
        };
        let path = store_report(&state, &report).unwrap();
        let listed = list_reports(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.task_id, "task/1");
        remove_report(&path).unwrap();
        assert!(list_reports(&state).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removes_superseded_reports_for_the_same_execution() {
        let root = std::env::temp_dir().join(format!("himind-outbox-prune-{}", timestamp()));
        let state = root.join("agent-state.json");
        let mut report = TaskReportRecord {
            task_id: "task-1".to_string(),
            agent_id: "agent-1".to_string(),
            execution_id: "exec-1".to_string(),
            lease_id: "lease-1".to_string(),
            status: "running".to_string(),
            progress: 50,
            detail: "copying".to_string(),
            result: serde_json::json!({}),
            error: String::new(),
        };
        store_report(&state, &report).unwrap();
        report.status = "finished".to_string();
        let finished = store_report(&state, &report).unwrap();
        assert_eq!(
            remove_reports_for_execution(&state, "task-1", "exec-1", Some(&finished)).unwrap(),
            1
        );
        let listed = list_reports(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.status, "finished");
        let _ = fs::remove_dir_all(root);
    }
}
