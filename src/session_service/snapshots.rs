//! Short-lived metadata snapshots for activity pagination. No message bodies.
use crate::session_history::{SessionSourceStatus, SessionSummary};
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
const TTL: u64 = 15 * 60;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 32;
#[derive(Serialize, Deserialize)]
struct Snapshot {
    created: u64,
    binding: String,
    registry: String,
    sessions: Vec<SessionSummary>,
    sources: Vec<SessionSourceStatus>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn directory() -> std::path::PathBuf {
    crate::store::paths::agent_home().join("cache/session-activity-snapshots")
}
pub(super) fn save(
    binding: &str,
    registry: &str,
    sessions: &[SessionSummary],
    sources: &[SessionSourceStatus],
) -> Result<String, String> {
    let root = directory();
    fs::create_dir_all(&root).map_err(|_| "SESSION_SNAPSHOT_STORAGE_UNAVAILABLE")?;
    let _guard = crate::store::atomic_file::lock(&root.join("snapshot-index"))
        .map_err(|_| "SESSION_SNAPSHOT_STORAGE_UNAVAILABLE")?;
    let mut files = fs::read_dir(&root)
        .map_err(|_| "SESSION_SNAPSHOT_STORAGE_UNAVAILABLE")?
        .flatten()
        .filter(|e| {
            e.path().extension().is_some_and(|x| x == "json")
                && e.file_type().is_ok_and(|t| t.is_file())
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));
    let excess = files.len().saturating_sub(MAX_FILES - 1);
    for (i, file) in files.iter().enumerate() {
        let age = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(TTL + 1);
        if i < excess || age > TTL {
            let _ = fs::remove_file(file.path());
        }
    }
    let snapshot = Snapshot {
        created: now(),
        binding: binding.into(),
        registry: registry.into(),
        sessions: sessions.to_vec(),
        sources: sources.to_vec(),
    };
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| "SESSION_SNAPSHOT_INVALID")?;
    if bytes.len() > MAX_BYTES as usize {
        return Err("SESSION_SNAPSHOT_BUDGET_EXCEEDED".into());
    }
    let id = format!("{:032x}", rand::random::<u128>());
    crate::store::atomic_file::write_without_backup(&root.join(format!("{id}.json")), &bytes)
        .map_err(|_| "SESSION_SNAPSHOT_STORAGE_UNAVAILABLE")?;
    Ok(id)
}
pub(super) fn load(
    id: &str,
    binding: &str,
    registry: &str,
) -> Result<(Vec<SessionSummary>, Vec<SessionSourceStatus>), String> {
    if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("SESSION_CURSOR_INVALID".into());
    }
    let path = directory().join(format!("{id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|_| "SESSION_SNAPSHOT_EXPIRED")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_BYTES {
        return Err("SESSION_SNAPSHOT_INVALID".into());
    }
    let snapshot: Snapshot =
        serde_json::from_slice(&fs::read(path).map_err(|_| "SESSION_SNAPSHOT_INVALID")?)
            .map_err(|_| "SESSION_SNAPSHOT_INVALID")?;
    if now().saturating_sub(snapshot.created) > TTL {
        return Err("SESSION_SNAPSHOT_EXPIRED".into());
    }
    if snapshot.binding != binding || snapshot.registry != registry {
        return Err("SESSION_SNAPSHOT_CHANGED".into());
    }
    Ok((snapshot.sessions, snapshot.sources))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_activity_metadata_snapshot_survives_restart_and_checks_scope() {
        let _lock = crate::store::paths::test_env_lock();
        let root =
            std::env::temp_dir().join(format!("session-snapshot-test-{}", std::process::id()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let id = save("query-a", "registry-a", &[], &[]).unwrap();
        assert!(load(&id, "query-a", "registry-a").is_ok());
        assert_eq!(
            load(&id, "query-b", "registry-a").unwrap_err(),
            "SESSION_SNAPSHOT_CHANGED"
        );
        assert_eq!(
            load(&id, "query-a", "registry-b").unwrap_err(),
            "SESSION_SNAPSHOT_CHANGED"
        );
        assert!(load("../../foreign", "query-a", "registry-a").is_err());
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }
}
