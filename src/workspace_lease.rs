use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_LEASE_SECONDS: u64 = 3_600;
const MAX_LEASE_SECONDS: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceLease {
    pub schema_version: String,
    pub lease_id: String,
    pub workspace_root: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub target_id: String,
    pub mode: String,
    pub owner_client: String,
    #[serde(default)]
    pub owner_session: String,
    pub created_at_unix: u64,
    pub heartbeat_at_unix: u64,
    pub expires_at_unix: u64,
}

pub(crate) fn acquire(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("workspace lease requires workspace_root")?;
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let owner_client = input
        .get("owner_client")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("workspace lease requires owner_client")?;
    let owner_session = input
        .get("owner_session")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let mode = input
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("write")
        .trim()
        .to_ascii_lowercase();
    if !matches!(mode.as_str(), "read" | "write") {
        return Err("workspace lease mode must be read or write".into());
    }
    let ttl = input
        .get("ttl_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LEASE_SECONDS)
        .clamp(60, MAX_LEASE_SECONDS);
    let now = unix_now();
    let path = lease_path();
    let _lock = crate::store::atomic_file::lock(&path)?;
    let mut leases = load_leases(&path);
    leases.retain(|lease| lease.expires_at_unix > now);

    for existing in &leases {
        if existing.mode != "write" || existing.workspace_root != display_path(&workspace) {
            continue;
        }
        if existing.owner_client == owner_client && existing.owner_session == owner_session {
            continue;
        }
        if mode == "write" {
            return Err(format!(
                "workspace is already leased for write by {}: {}",
                existing.owner_client, existing.lease_id
            )
            .into());
        }
        return Err(format!(
            "workspace has an active write lease; read lease is allowed only through the owner session: {}",
            existing.lease_id
        )
        .into());
    }

    let lease_id = format!(
        "lease_{:x}",
        Sha256::digest(
            format!(
                "{}:{}:{}:{}",
                workspace.to_string_lossy(),
                owner_client,
                owner_session,
                now
            )
            .as_bytes()
        )
    );
    let lease = WorkspaceLease {
        schema_version: "workspace_lease.v1".to_string(),
        lease_id: lease_id.clone(),
        workspace_root: display_path(&workspace),
        project_id: input
            .get("project_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        target_id: input
            .get("target_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        mode,
        owner_client: owner_client.to_string(),
        owner_session: owner_session.to_string(),
        created_at_unix: now,
        heartbeat_at_unix: now,
        expires_at_unix: now.saturating_add(ttl),
    };
    leases.push(lease.clone());
    persist_leases(&path, &leases)?;
    Ok(json!({
        "ok": true,
        "lease": lease,
    }))
}

pub(crate) fn release(input: &Value) -> Result<Value, Box<dyn Error>> {
    let lease_id = input
        .get("lease_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("workspace lease release requires lease_id")?;
    let path = lease_path();
    let _lock = crate::store::atomic_file::lock(&path)?;
    let mut leases = load_leases(&path);
    let before = leases.len();
    leases.retain(|lease| lease.lease_id != lease_id);
    let released = leases.len() != before;
    persist_leases(&path, &leases)?;
    Ok(json!({
        "ok": true,
        "released": released,
        "lease_id": lease_id,
    }))
}

pub(crate) fn list(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace_filter = input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            PathBuf::from(value)
                .canonicalize()
                .map(|path| display_path(&path))
                .unwrap_or_else(|_| value.to_string())
        });
    let now = unix_now();
    let leases = load_leases(&lease_path())
        .into_iter()
        .filter(|lease| lease.expires_at_unix > now)
        .filter(|lease| {
            workspace_filter
                .as_ref()
                .is_none_or(|workspace| lease.workspace_root == *workspace)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "leases": leases,
    }))
}

pub(crate) fn validate_active(
    lease_id: &str,
    workspace_root: &Path,
    mode: &str,
) -> Result<WorkspaceLease, Box<dyn Error>> {
    let now = unix_now();
    let workspace = display_path(&workspace_root.canonicalize()?);
    let lease = load_leases(&lease_path())
        .into_iter()
        .find(|lease| lease.lease_id == lease_id.trim())
        .ok_or_else(|| format!("workspace lease was not found: {lease_id}"))?;
    if lease.expires_at_unix <= now {
        return Err(format!("workspace lease has expired: {lease_id}").into());
    }
    if lease.workspace_root != workspace {
        return Err("workspace lease does not belong to the requested workspace".into());
    }
    if mode == "write" && lease.mode != "write" {
        return Err("write operation requires a write workspace lease".into());
    }
    Ok(lease)
}

fn lease_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("data")
        .join("workspace-leases.json")
}

fn load_leases(path: &Path) -> Vec<WorkspaceLease> {
    fs::read(path)
        .ok()
        .and_then(|content| serde_json::from_slice(&content).ok())
        .unwrap_or_default()
}

fn persist_leases(path: &Path, leases: &[WorkspaceLease]) -> Result<(), Box<dyn Error>> {
    let content = serde_json::to_vec_pretty(leases)?;
    crate::store::atomic_file::atomic_write(path, &content)?;
    Ok(())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{acquire, list, release};
    use serde_json::json;

    #[test]
    fn write_lease_conflicts_and_release_unblocks() {
        let workspace = std::env::temp_dir().join(format!(
            "himind-workspace-lease-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let first = acquire(&json!({
            "workspace_root": workspace,
            "owner_client": "himind-ai",
            "owner_session": "session-1",
            "mode": "write"
        }))
        .unwrap();
        assert!(acquire(&json!({
            "workspace_root": workspace,
            "owner_client": "external-ai",
            "owner_session": "session-2",
            "mode": "write"
        }))
        .is_err());
        let lease_id = first["lease"]["lease_id"].as_str().unwrap();
        release(&json!({"lease_id": lease_id})).unwrap();
        assert!(
            list(&json!({"workspace_root": workspace})).unwrap()["leases"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(workspace);
    }
}
