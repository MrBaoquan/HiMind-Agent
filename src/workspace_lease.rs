use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use fs4::fs_std::FileExt;

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
    #[serde(default, skip_serializing_if = "is_false")]
    pub runtime_managed: bool,
}

fn is_false(value: &bool) -> bool { !*value }

pub(crate) fn acquire(input: &Value) -> Result<Value, Box<dyn Error>> {
    acquire_inner(input, false).map(|(value, _)| value)
}

fn acquire_inner(input: &Value, runtime_managed: bool) -> Result<(Value, Option<fs::File>), Box<dyn Error>> {
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
    leases.retain(|lease| lease_is_active(lease, now));

    for existing in &leases {
        if existing.mode != "write" || existing.workspace_root != display_path(&workspace) {
            continue;
        }
        if existing.owner_client == owner_client && existing.owner_session == owner_session {
            if !runtime_managed && existing.mode == mode {
                return Ok((json!({"ok": true, "lease": existing}), None));
            }
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
        runtime_managed,
    };
    let lifetime_lock = if runtime_managed {
        let lock_path = runtime_lock_path(&lease_id);
        fs::create_dir_all(lock_path.parent().ok_or("missing lease lock parent")?)?;
        let file = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
        if !file.try_lock_exclusive()? { return Err("runtime lease is already held by another invocation".into()); }
        Some(file)
    } else { None };
    leases.push(lease.clone());
    persist_leases(&path, &leases)?;
    Ok((json!({
        "ok": true,
        "lease": lease,
    }), lifetime_lock))
}

/// The host holds an OS lock for the entire invocation. Process termination
/// releases it even when destructors cannot run, so a dead run cannot reserve
/// a workspace until its TTL expires.
pub(crate) struct RuntimeLeaseGuard {
    lease: WorkspaceLease,
    _lifetime_lock: fs::File,
}

impl RuntimeLeaseGuard {
    pub(crate) fn acquire(workspace: &Path, session: &str, ttl: u64) -> Result<Self, Box<dyn Error>> {
        let (value, lock) = acquire_inner(&json!({"workspace_root": workspace,
            "owner_client": "himind-runtime", "owner_session": session,
            "mode": "write", "ttl_seconds": ttl.saturating_add(60)}), true)?;
        Ok(Self { lease: serde_json::from_value(value["lease"].clone())?,
            _lifetime_lock: lock.ok_or("runtime lease requires a lifetime lock")? })
    }

    pub(crate) fn configure_child(&self, command: &mut std::process::Command) {
        command.env("HIMIND_RUNTIME_WORKSPACE_LEASE", &self.lease.lease_id);
        command.env("HIMIND_RUNTIME_LEASE_WORKSPACE", &self.lease.workspace_root);
    }
}

impl Drop for RuntimeLeaseGuard {
    fn drop(&mut self) {
        if let Err(error) = release(&json!({"lease_id": self.lease.lease_id})) {
            eprintln!("runtime lease cleanup deferred: {error}");
        }
    }
}

fn runtime_lock_path(id: &str) -> PathBuf {
    lease_path().with_file_name("runtime-lease-locks").join(format!("{id}.lock"))
}

fn lease_is_active(lease: &WorkspaceLease, now: u64) -> bool {
    if lease.expires_at_unix <= now { return false; }
    if !lease.runtime_managed { return true; }
    // Missing or unlocked means the owner has exited. Other IO errors remain
    // conservative: never clear a potentially live owner due to an ACL error.
    match fs::OpenOptions::new().read(true).write(true).open(runtime_lock_path(&lease.lease_id)) {
        Ok(file) => !file.try_lock_exclusive().unwrap_or(false),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Runtime children borrow the host lease. Their release detaches only; the
/// host remains the owner until the invocation terminates.
pub(crate) fn inherited_call(id: &str, input: &Value, inherited: Option<&(String, String)>) -> Option<Result<Value, Box<dyn Error>>> {
    if !matches!(id, "engineering.workspace.lease.acquire" | "engineering.workspace.lease.release") { return None; }
    let (lease_id, workspace) = inherited?;
    Some((|| {
        let lease = validate_active(&lease_id, Path::new(&workspace), "write")?;
        if id.ends_with(".acquire") {
            let requested = input.get("workspace_root").and_then(Value::as_str).ok_or("workspace_root is required")?;
            if display_path(&Path::new(requested).canonicalize()?) != lease.workspace_root {
                return Err("runtime lease may only be borrowed for its assigned workspace".into());
            }
            Ok(json!({"ok":true,"lease":lease,"inherited":true}))
        } else {
            if input.get("lease_id").and_then(Value::as_str) != Some(lease_id.as_str()) {
                return Err("runtime child cannot release another workspace lease".into());
            }
            Ok(json!({"ok":true,"released":false,"lease_id":lease_id,"inherited":true,"owner":"runtime_host"}))
        }
    })())
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
        .filter(|lease| lease_is_active(lease, now))
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
    if !lease_is_active(&lease, now) {
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

#[cfg(not(test))]
fn lease_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("data")
        .join("workspace-leases.json")
}

/// Tests run in parallel inside one process, and `HIMIND_AGENT_HOME` is
/// process-global: resolving the production path would let another test's
/// temporary home move this file between `acquire` and the `validate_active`
/// that follows it, which surfaces as "workspace lease was not found". Keep a
/// stable per-process file under the temp directory instead — parallel tests
/// share it deterministically, and access is already serialized by the file
/// lock, so leases for different workspaces never collide.
#[cfg(test)]
fn lease_path() -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "himind-agent-test-lease-home-{}",
            std::process::id()
        ))
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
    fn runtime_lease_releases_on_return_and_recovers_after_owner_exit() {
        let workspace = std::env::temp_dir().join(format!("runtime-lease-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&workspace).unwrap();
        {
            let _guard = super::RuntimeLeaseGuard::acquire(&workspace, "run/step", 60).unwrap();
            let inherited = (_guard.lease.lease_id.clone(), _guard.lease.workspace_root.clone());
            let borrowed = super::inherited_call("engineering.workspace.lease.acquire", &json!({"workspace_root":workspace}), Some(&inherited)).unwrap().unwrap();
            assert_eq!(borrowed["inherited"], true);
            let detached = super::inherited_call("engineering.workspace.lease.release", &json!({"lease_id":inherited.0}), Some(&inherited)).unwrap().unwrap();
            assert_eq!(detached["released"], false);
            assert!(super::validate_active(&inherited.0, &workspace, "write").is_ok());
            assert!(acquire(&json!({"workspace_root":workspace,"owner_client":"other","mode":"write"})).is_err());
            assert_eq!(list(&json!({"workspace_root":workspace})).unwrap()["leases"].as_array().unwrap().len(), 1);
        }
        assert!(list(&json!({"workspace_root":workspace})).unwrap()["leases"].as_array().unwrap().is_empty());
        let (value, lifetime_lock) = super::acquire_inner(&json!({"workspace_root":workspace,
            "owner_client":"himind-runtime","owner_session":"crashed","mode":"write"}), true).unwrap();
        let id = value["lease"]["lease_id"].as_str().unwrap();
        assert!(super::validate_active(id, &workspace, "write").is_ok());
        drop(lifetime_lock); // Simulate OS releasing handles after a crash, with the record still on disk.
        assert!(super::validate_active(id, &workspace, "write").is_err());
        let replacement = acquire(&json!({"workspace_root":workspace,"owner_client":"other","mode":"write"})).unwrap();
        release(&json!({"lease_id":replacement["lease"]["lease_id"]})).unwrap();
        let _ = std::fs::remove_dir_all(workspace);
    }

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

    #[test]
    fn lease_path_ignores_process_home_flips() {
        let before = super::lease_path();
        let _guard = crate::store::paths::test_env_lock();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var(
            "HIMIND_AGENT_HOME",
            std::env::temp_dir().join("himind-lease-home-probe"),
        );
        let after = super::lease_path();
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        assert_eq!(before, after);
    }
}
