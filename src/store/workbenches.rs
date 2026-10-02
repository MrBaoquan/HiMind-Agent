//! Workbench connections: the Agent's identity, one record per workbench.
//!
//! See `docs/adr/0008-workbench-connections.md`. The short version:
//!
//! * Local data (installed capabilities, AI services, approvals, backups)
//!   belongs to the Agent and exists exactly once.
//! * Identity belongs to a *connection*. Every connection carries its own
//!   snapshot of `agent-state.json`, its device id and its OAuth authorization
//!   file, so switching workbenches can never present environment A's agent
//!   credential to environment B.
//! * `workbenches.json` is the source of truth. The three legacy files are a
//!   materialised view of the active connection, so the rest of the code base
//!   keeps reading the paths it already knows.
//!
//! Credentials are moved as already-sealed DPAPI blobs. This module never
//! unprotects a secret and never writes a plaintext credential.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::oauth::authorization_path;
use crate::store::atomic_file;

const WORKBENCHES_FILE: &str = "workbenches.json";
const SCHEMA_VERSION: u32 = 1;

/// One workbench's identity, captured from the on-disk onboarding artifacts.
///
/// `state` and `authorization` are the raw JSON bodies exactly as they sit on
/// disk, which is what makes a switch lossless: moving a connection restores
/// the same bytes, including a half-finished credential rotation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ConnectionIdentity {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub authorized_at: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub refresh_expires_at: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_verified_at: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub captured_at: u64,
    /// Cheap equality key for "is the materialised view already this identity".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<Value>,
}

impl ConnectionIdentity {
    /// No onboarding artifact was ever captured for this connection.
    pub(crate) fn is_empty(&self) -> bool {
        self.state.is_none() && self.authorization.is_none() && self.agent_id.trim().is_empty()
    }

    pub(crate) fn authorized(&self) -> bool {
        self.authorization.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WorkbenchConnection {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    /// Free-form label from the user ("本地开发", "生产", "A 团队").
    #[serde(default)]
    pub purpose: String,
    pub api_base: String,
    #[serde(default)]
    pub added_at: u64,
    #[serde(default)]
    pub last_used_at: u64,
    #[serde(default)]
    pub identity: ConnectionIdentity,
}

impl WorkbenchConnection {
    pub(crate) fn registered(&self) -> bool {
        !self.identity.agent_id.trim().is_empty()
    }
}

/// Serializable view for the Agent UI. Contains no secret material.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkbenchConnectionView {
    pub id: String,
    pub display_name: String,
    pub purpose: String,
    pub api_base: String,
    pub active: bool,
    pub registered: bool,
    pub authorized: bool,
    pub agent_id: String,
    pub user_id: String,
    pub user_name: String,
    pub scope: Vec<String>,
    pub authorized_at: u64,
    pub refresh_expires_at: u64,
    pub last_used_at: u64,
    /// `authorized` | `registered` | `unregistered`
    pub state: String,
}

impl WorkbenchConnectionView {
    fn of(connection: &WorkbenchConnection, active_id: &str) -> Self {
        let authorized = connection.identity.authorized();
        let registered = connection.registered();
        Self {
            id: connection.id.clone(),
            display_name: connection.display_name.clone(),
            purpose: connection.purpose.clone(),
            api_base: connection.api_base.clone(),
            active: connection.id == active_id,
            registered,
            authorized,
            agent_id: connection.identity.agent_id.clone(),
            user_id: connection.identity.user_id.clone(),
            user_name: connection.identity.user_name.clone(),
            scope: connection
                .identity
                .scope
                .split_whitespace()
                .map(|value| value.to_string())
                .collect(),
            authorized_at: connection.identity.authorized_at,
            refresh_expires_at: connection.identity.refresh_expires_at,
            last_used_at: connection.last_used_at,
            state: if authorized {
                "authorized".to_string()
            } else if registered {
                "registered".to_string()
            } else {
                "unregistered".to_string()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkbenchStore {
    version: u32,
    #[serde(default)]
    active_connection_id: String,
    /// Set between "active id changed" and "identity materialised on disk".
    /// A crash in that window leaves the marker behind, and the next start
    /// materialises the active connection instead of guessing.
    #[serde(default)]
    pending_materialize: bool,
    #[serde(default)]
    connections: Vec<WorkbenchConnection>,
}

impl WorkbenchStore {
    fn new() -> Self {
        Self {
            version: SCHEMA_VERSION,
            active_connection_id: String::new(),
            pending_materialize: false,
            connections: Vec::new(),
        }
    }

    pub(crate) fn connections(&self) -> &[WorkbenchConnection] {
        &self.connections
    }

    pub(crate) fn active_connection_id(&self) -> &str {
        &self.active_connection_id
    }

    pub(crate) fn active_connection(&self) -> Option<&WorkbenchConnection> {
        self.connections
            .iter()
            .find(|connection| connection.id == self.active_connection_id)
    }

    pub(crate) fn views(&self) -> Vec<WorkbenchConnectionView> {
        self.connections
            .iter()
            .map(|connection| WorkbenchConnectionView::of(connection, &self.active_connection_id))
            .collect()
    }
}

pub(crate) fn path_for(state_path: &Path) -> PathBuf {
    state_path.with_file_name(WORKBENCHES_FILE)
}

fn device_file(state_path: &Path) -> PathBuf {
    state_path.with_extension("device-id")
}

/// Serialize the read-modify-write cycle of the store across processes.
///
/// `atomic_file::atomic_write` makes every *write* whole, not the sequence
/// around it. The GUI, the stdio MCP companion and the updater each run
/// `load` → edit → `save`; two that overlap lose one of the two edits, and the
/// loser is usually the enrollment that just happened. The lock is a plain
/// exclusive file lock beside the store, so it covers every process that
/// shares this data root.
///
/// It is deliberately **not reentrant** (Windows `LockFileEx` and POSIX `flock`
/// both block a second acquisition in the same process). Every public entry
/// point therefore takes it exactly once and delegates to a `*_locked` helper,
/// while those helpers call each other freely. `load`/`save` stay unlocked:
/// reads only ever see a whole file, and writes are always already inside a
/// locked section.
fn lock(state_path: &Path) -> Result<atomic_file::AtomicFileLock, Box<dyn Error>> {
    Ok(atomic_file::lock(&path_for(state_path))?)
}

/// Read the store if it exists. Never creates or migrates.
pub(crate) fn load(state_path: &Path) -> Result<Option<WorkbenchStore>, Box<dyn Error>> {
    let path = path_for(state_path);
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let mut store = serde_json::from_str::<WorkbenchStore>(&content)?;
    if store.active_connection_id.trim().is_empty() || store.active_connection().is_none() {
        // An unreadable active pointer must not silently pick one: the next
        // `save` would then materialise a connection the user never chose.
        // Fall back to the first record, which is what migration produces.
        store.active_connection_id = store
            .connections
            .first()
            .map(|connection| connection.id.clone())
            .unwrap_or_default();
    }
    Ok(Some(store))
}

pub(crate) fn save(state_path: &Path, store: &WorkbenchStore) -> Result<(), Box<dyn Error>> {
    let path = path_for(state_path);
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(store)?)?;
    Ok(())
}

fn require_store(state_path: &Path) -> Result<WorkbenchStore, Box<dyn Error>> {
    load(state_path)?.ok_or_else(|| "工作台连接尚未初始化".into())
}

pub(crate) fn active_connection(state_path: &Path) -> Option<WorkbenchConnection> {
    let store = load(state_path).ok().flatten()?;
    store.active_connection().cloned()
}

/// Idempotent migration entry point.
///
/// The first call on an install that predates this ADR turns the existing
/// legacy files into connection #1 and makes it active. Later calls are a
/// plain read.
pub(crate) fn ensure(
    state_path: &Path,
    fallback_api_base: &str,
) -> Result<WorkbenchStore, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    ensure_locked(state_path, fallback_api_base)
}

fn ensure_locked(
    state_path: &Path,
    fallback_api_base: &str,
) -> Result<WorkbenchStore, Box<dyn Error>> {
    adopt_legacy_authorization(state_path)?;
    if let Some(store) = load(state_path)? {
        return Ok(store);
    }
    let mut store = WorkbenchStore::new();
    let api_base = normalize_api_base(fallback_api_base);
    let now = unix_now();
    let identity = snapshot_from_disk(state_path)?;
    let connection = WorkbenchConnection {
        id: new_connection_id(&store),
        display_name: default_display_name(&api_base),
        purpose: String::new(),
        api_base,
        added_at: now,
        last_used_at: now,
        identity,
    };
    store.active_connection_id = connection.id.clone();
    store.connections.push(connection);
    save(state_path, &store)?;
    Ok(store)
}

/// Move a pre-ADR authorization file into the per-profile `data` directory.
///
/// `authorization_path` falls back to `<home>/agent-user-authorization.json`
/// whenever the canonical file is missing, so on an old install every
/// connection would resolve to that one shared file — the exact crossover this
/// module exists to prevent. Materialising a connection would then write the
/// shared file instead of its own.
///
/// The bytes are identical and only the location changes, so the migration is
/// invisible to the rest of the code base. It is idempotent: once the canonical
/// file exists, the fallback can no longer trigger.
fn adopt_legacy_authorization(state_path: &Path) -> Result<(), Box<dyn Error>> {
    let canonical = state_path.with_file_name("agent-user-authorization.json");
    if canonical.is_file() {
        return Ok(());
    }
    let Some(home) = state_path
        .parent()
        .filter(|directory| {
            directory
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("data"))
        })
        .and_then(Path::parent)
    else {
        return Ok(());
    };
    let legacy = home.join("agent-user-authorization.json");
    if !legacy.is_file() {
        return Ok(());
    }
    // Adopt the same bytes the OAuth reader would resolve, including its
    // backup fallback, so migration can never downgrade a recoverable file.
    let Some(content) = readable_json(&legacy) else {
        return Ok(());
    };
    if let Some(parent) = canonical.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_file::atomic_write(&canonical, &content)?;
    remove_with_backup(&legacy)?;
    Ok(())
}

/// The contents of `path`, or of its `.bak`, whichever parses as JSON first.
fn readable_json(path: &Path) -> Option<Vec<u8>> {
    for candidate in [path.to_path_buf(), atomic_file::backup_path(path)] {
        let Ok(content) = fs::read(&candidate) else {
            continue;
        };
        if serde_json::from_slice::<Value>(&content).is_ok() {
            return Some(content);
        }
    }
    None
}

/// Reconcile the materialised view with the store on startup.
///
/// The direction of the repair is decided by `pending_materialize`, not by
/// which file looks newer: after a completed switch the store must win, and
/// after an enrollment the disk must win.
pub(crate) fn sync_active(
    state_path: &Path,
    fallback_api_base: &str,
) -> Result<WorkbenchStore, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    sync_active_locked(state_path, fallback_api_base)
}

fn sync_active_locked(
    state_path: &Path,
    fallback_api_base: &str,
) -> Result<WorkbenchStore, Box<dyn Error>> {
    let mut store = ensure_locked(state_path, fallback_api_base)?;
    let Some(target) = store.active_connection().cloned() else {
        return Ok(store);
    };
    if store.pending_materialize {
        materialize(state_path, &target)?;
        store.pending_materialize = false;
        save(state_path, &store)?;
        return Ok(store);
    }
    let disk = snapshot_from_disk(state_path)?;
    if disk.fingerprint == target.identity.fingerprint {
        return Ok(store);
    }
    if disk.is_empty() || foreign_owner(&store, &disk.agent_id, &target.id).is_some() {
        // Either there is nothing on disk to lose, or the disk is holding an
        // identity another connection already claims. Both mean the store is
        // authoritative here: restore the stored identity instead of adopting
        // a foreign one.
        materialize(state_path, &target)?;
    } else {
        // The disk holds a newer enrollment/authorization than the store. It
        // is what this Agent actually ran with, so it becomes the record.
        capture_into(state_path, &mut store, &target.id)?;
        save(state_path, &store)?;
    }
    Ok(store)
}

/// Fold the current on-disk identity back into the active connection.
///
/// Call this after anything that writes `agent-state.json` or the OAuth
/// authorization file, so the store stays authoritative without polling.
pub(crate) fn capture_active(state_path: &Path) -> Result<bool, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    capture_active_locked(state_path)
}

fn capture_active_locked(state_path: &Path) -> Result<bool, Box<dyn Error>> {
    let Some(mut store) = load(state_path)? else {
        return Ok(false);
    };
    let active_id = store.active_connection_id.clone();
    if active_id.is_empty() {
        return Ok(false);
    }
    if !capture_into(state_path, &mut store, &active_id)? {
        return Ok(false);
    }
    save(state_path, &store)?;
    Ok(true)
}

/// Fold the on-disk identity back into the active connection, never failing the
/// caller's write.
///
/// Every writer of `agent-state.json` and the OAuth authorization file calls
/// this. Losing the capture would only cost bookkeeping (the startup
/// [`sync_active`] repairs it), so a capture error must not turn a successful
/// enrollment into an error the user sees.
pub(crate) fn capture_active_quiet(state_path: &Path) {
    if let Err(error) = capture_active(state_path) {
        eprintln!("workbench connection capture deferred: {error}");
    }
}

pub(crate) fn add(
    state_path: &Path,
    api_base: &str,
    display_name: &str,
    purpose: &str,
) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    add_locked(state_path, api_base, display_name, purpose)
}

fn add_locked(
    state_path: &Path,
    api_base: &str,
    display_name: &str,
    purpose: &str,
) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let mut store = require_store(state_path)?;
    let api_base = normalize_api_base(api_base);
    if api_base.is_empty() {
        return Err("请填写工作台地址".into());
    }
    if let Some(existing) = store
        .connections
        .iter()
        .find(|connection| connection.api_base.eq_ignore_ascii_case(&api_base))
    {
        return Err(format!("这个地址已经在列表里了：{}", display_name_of(existing)).into());
    }
    let now = unix_now();
    let display_name = display_name.trim();
    let connection = WorkbenchConnection {
        id: new_connection_id(&store),
        display_name: if display_name.is_empty() {
            default_display_name(&api_base)
        } else {
            display_name.to_string()
        },
        purpose: purpose.trim().to_string(),
        api_base,
        added_at: now,
        last_used_at: 0,
        identity: ConnectionIdentity::default(),
    };
    store.connections.push(connection.clone());
    save(state_path, &store)?;
    Ok(connection)
}

pub(crate) fn rename(
    state_path: &Path,
    id: &str,
    display_name: &str,
    purpose: &str,
) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    rename_locked(state_path, id, display_name, purpose)
}

fn rename_locked(
    state_path: &Path,
    id: &str,
    display_name: &str,
    purpose: &str,
) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let mut store = require_store(state_path)?;
    let Some(target) = store
        .connections
        .iter_mut()
        .find(|connection| connection.id == id)
    else {
        return Err(format!("未知的工作台连接: {id}").into());
    };
    let display_name = display_name.trim();
    if !display_name.is_empty() {
        target.display_name = display_name.to_string();
    }
    target.purpose = purpose.trim().to_string();
    let updated = target.clone();
    save(state_path, &store)?;
    Ok(updated)
}

pub(crate) fn remove(state_path: &Path, id: &str) -> Result<(), Box<dyn Error>> {
    let _lock = lock(state_path)?;
    remove_locked(state_path, id)
}

fn remove_locked(state_path: &Path, id: &str) -> Result<(), Box<dyn Error>> {
    let mut store = require_store(state_path)?;
    if store.active_connection_id == id {
        return Err("正在使用的工作台不能删除，请先切换到其它工作台".into());
    }
    let before = store.connections.len();
    store.connections.retain(|connection| connection.id != id);
    if store.connections.len() == before {
        return Err(format!("未知的工作台连接: {id}").into());
    }
    save(state_path, &store)?;
    Ok(())
}

/// Make `id` the active connection and materialise its identity on disk.
pub(crate) fn switch(state_path: &Path, id: &str) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    switch_locked(state_path, id)
}

fn switch_locked(state_path: &Path, id: &str) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let mut store = require_store(state_path)?;
    if !store
        .connections
        .iter()
        .any(|connection| connection.id == id)
    {
        return Err(format!("未知的工作台连接: {id}").into());
    }
    if store.active_connection_id != id {
        let current = store.active_connection_id.clone();
        if !current.is_empty() {
            capture_into(state_path, &mut store, &current)?;
        }
        store.active_connection_id = id.to_string();
        store.pending_materialize = true;
        if let Some(target) = store
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
        {
            target.last_used_at = unix_now();
        }
        save(state_path, &store)?;

        let target = store
            .active_connection()
            .cloned()
            .ok_or("工作台连接已失效")?;
        materialize(state_path, &target)?;
        store.pending_materialize = false;
        save(state_path, &store)?;
    }
    store
        .active_connection()
        .cloned()
        .ok_or_else(|| "工作台连接已失效".into())
}

/// Fold the disk snapshot into the connection `id`, refusing a foreign
/// identity.
///
/// Adopting whatever is on disk is right for an enrollment (same connection,
/// possibly a new `agent_id` after re-registration) but wrong when the disk is
/// holding an identity another connection already claims: that would present
/// workbench B's credential as workbench A's. Re-enrollment of the active
/// workbench is unaffected — its new `agent_id` is nobody else's.
fn capture_into(
    state_path: &Path,
    store: &mut WorkbenchStore,
    id: &str,
) -> Result<bool, Box<dyn Error>> {
    let snapshot = snapshot_from_disk(state_path)?;
    if let Some(owner) = foreign_owner(store, &snapshot.agent_id, id) {
        eprintln!(
            "workbench capture refused: connection {id} would take agent {} that belongs to {owner}",
            snapshot.agent_id
        );
        return Ok(false);
    }
    let Some(target) = store
        .connections
        .iter_mut()
        .find(|connection| connection.id == id)
    else {
        return Ok(false);
    };
    if target.identity.fingerprint == snapshot.fingerprint {
        return Ok(false);
    }
    target.identity = snapshot;
    Ok(true)
}

/// The id of a *different* connection that already claims `agent_id`.
///
/// An empty or unknown `agent_id` is never foreign: a fresh enrollment has not
/// been recorded anywhere yet, and the active connection is the only one that
/// could have produced it.
fn foreign_owner(store: &WorkbenchStore, agent_id: &str, id: &str) -> Option<String> {
    let agent_id = agent_id.trim();
    if agent_id.is_empty() {
        return None;
    }
    store
        .connections
        .iter()
        .find(|connection| connection.id != id && connection.identity.agent_id.trim() == agent_id)
        .map(|connection| connection.id.clone())
}

/// Make the connection for `api_base` active, creating it when this Agent has
/// never talked to that workbench.
///
/// This is what an explicit `--api` / `DASHBOARD_API_BASE` means after ADR
/// 0008: not "the address for this build", but "start on this workbench". The
/// previous connection keeps its identity in the store, so the explicit
/// override stays lossless instead of silently reusing another workbench's
/// credential.
pub(crate) fn activate_for(
    state_path: &Path,
    api_base: &str,
) -> Result<WorkbenchConnection, Box<dyn Error>> {
    let _lock = lock(state_path)?;
    let store = ensure_locked(state_path, api_base)?;
    let api_base = normalize_api_base(api_base);
    let existing = store
        .connections()
        .iter()
        .find(|connection| connection.api_base.eq_ignore_ascii_case(&api_base))
        .map(|connection| connection.id.clone());
    match existing {
        Some(id) => switch_locked(state_path, &id),
        None => {
            let created = add_locked(state_path, &api_base, "", "")?;
            switch_locked(state_path, &created.id)
        }
    }
}

/// Write a connection's identity into the legacy paths.
///
/// Both the file and its `.bak` are written or removed. Leaving a stale backup
/// behind is not cosmetic: `load_agent_state` and `read_stored_authorization`
/// both fall back to it, so a leftover backup would resurrect the previous
/// workbench's identity.
fn materialize(state_path: &Path, connection: &WorkbenchConnection) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if connection.identity.is_empty() {
        clear_identity_files(state_path)?;
        return Ok(());
    }
    match connection.identity.state.as_ref() {
        Some(value) => {
            atomic_file::atomic_write(state_path, &serde_json::to_vec_pretty(value)?)?;
        }
        None => remove_with_backup(state_path)?,
    }
    let device = device_file(state_path);
    let device_id = connection.identity.device_id.trim();
    if device_id.is_empty() {
        remove_with_backup(&device)?;
    } else {
        atomic_file::atomic_write(&device, device_id.as_bytes())?;
    }
    let authorization = authorization_path(state_path);
    match connection.identity.authorization.as_ref() {
        Some(value) => {
            atomic_file::atomic_write(&authorization, &serde_json::to_vec_pretty(value)?)?;
        }
        None => remove_with_backup(&authorization)?,
    }
    Ok(())
}

fn clear_identity_files(state_path: &Path) -> Result<(), Box<dyn Error>> {
    // Resolve the authorization location before deleting anything: once the
    // canonical file is gone, the resolver falls back to a legacy location one
    // level above `data`, and that copy would then be read instead.
    let authorization = authorization_path(state_path);
    remove_with_backup(state_path)?;
    remove_with_backup(&device_file(state_path))?;
    remove_with_backup(&authorization)?;
    Ok(())
}

fn remove_with_backup(path: &Path) -> Result<(), Box<dyn Error>> {
    for candidate in [path.to_path_buf(), atomic_file::backup_path(path)] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn snapshot_from_disk(state_path: &Path) -> Result<ConnectionIdentity, Box<dyn Error>> {
    let state = read_json_if_present(state_path)?;
    let authorization = read_json_if_present(&authorization_path(state_path))?;
    let device_id = fs::read_to_string(device_file(state_path))
        .map(|value| value.trim().to_string())
        .unwrap_or_default();

    let mut identity = ConnectionIdentity::default();
    if let Some(value) = state.as_ref() {
        identity.agent_id = string_field(value, "agent_id");
    }
    if let Some(value) = authorization.as_ref() {
        identity.user_id = string_field(value, "user_id");
        identity.user_name = string_field(value, "display_name");
        identity.scope = string_field(value, "scope");
        identity.refresh_expires_at = u64_field(value, "refresh_expires_at");
        identity.authorized_at = u64_field(value, "updated_at");
        identity.last_verified_at = u64_field(value, "last_verified_at");
    }
    identity.device_id = device_id;
    identity.fingerprint = fingerprint_of(&state, &identity.device_id, &authorization);
    identity.captured_at = unix_now();
    identity.state = state;
    identity.authorization = authorization;
    Ok(identity)
}

fn read_json_if_present(path: &Path) -> Result<Option<Value>, Box<dyn Error>> {
    match fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(value) => Ok(Some(value)),
            Err(_) => Ok(None),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn fingerprint_of(state: &Option<Value>, device_id: &str, authorization: &Option<Value>) -> String {
    let mut hasher = DefaultHasher::new();
    state
        .as_ref()
        .map(|value| value.to_string())
        .unwrap_or_default()
        .hash(&mut hasher);
    device_id.trim().hash(&mut hasher);
    authorization
        .as_ref()
        .map(|value| value.to_string())
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn normalize_api_base(api_base: &str) -> String {
    api_base.trim().trim_end_matches('/').to_string()
}

fn display_name_of(connection: &WorkbenchConnection) -> String {
    if connection.display_name.trim().is_empty() {
        default_display_name(&connection.api_base)
    } else {
        connection.display_name.clone()
    }
}

/// A readable default name derived from the address, so a freshly added
/// connection is never shown as an empty row.
fn default_display_name(api_base: &str) -> String {
    let trimmed = api_base.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return "工作台".to_string();
    }
    match url::Url::parse(trimmed) {
        Ok(parsed) => match parsed.port() {
            Some(port) => format!("{}:{}", parsed.host_str().unwrap_or(trimmed), port),
            None => parsed.host_str().unwrap_or(trimmed).to_string(),
        },
        Err(_) => trimmed.to_string(),
    }
}

fn new_connection_id(store: &WorkbenchStore) -> String {
    let mut nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    loop {
        let candidate = format!("conn-{nanos:016x}");
        if !store
            .connections
            .iter()
            .any(|connection| connection.id == candidate)
        {
            return candidate;
        }
        nanos = nanos.wrapping_add(1);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        add, capture_active, ensure, load, path_for, remove, rename, switch, sync_active,
        ConnectionIdentity, WorkbenchStore,
    };
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempHome {
        root: PathBuf,
    }

    impl TempHome {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "himind-workbenches-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("data")).unwrap();
            Self { root }
        }

        fn state_path(&self) -> PathBuf {
            self.root.join("data/agent-state.json")
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_identity(state_path: &Path, agent_id: &str, credential: &str, user: &str) {
        fs::write(
            state_path,
            serde_json::to_vec_pretty(&json!({
                "agent_id": agent_id,
                "credential_protected": credential,
                "credential_updated_at": 1,
                "device_id": format!("device-{agent_id}"),
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            state_path.with_extension("device-id"),
            format!("device-{agent_id}"),
        )
        .unwrap();
        fs::write(
            state_path.with_file_name("agent-user-authorization.json"),
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "agent_id": agent_id,
                "user_id": user,
                "scope": "agent.profile",
                "refresh_token_protected": format!("sealed-{agent_id}"),
                "refresh_expires_at": 4_000_000_000u64,
                "updated_at": 10u64,
                "display_name": "张三",
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn migration_creates_exactly_one_connection_and_is_idempotent() {
        let home = TempHome::new("migrate");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");

        let store = ensure(&state_path, "http://127.0.0.1:18083/").unwrap();
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.active_connection_id(), store.connections()[0].id);
        assert_eq!(store.connections()[0].api_base, "http://127.0.0.1:18083");
        assert_eq!(store.connections()[0].identity.agent_id, "agent-a");
        assert_eq!(store.connections()[0].identity.user_name, "张三");
        assert!(store.connections()[0].identity.authorized());

        // Running migration again must not add a second record, and must not
        // overwrite the stored identity with an empty one.
        let again = ensure(&state_path, "http://elsewhere").unwrap();
        assert_eq!(again.connections().len(), 1);
        assert_eq!(again.connections()[0].api_base, "http://127.0.0.1:18083");
        assert_eq!(again.active_connection_id(), store.active_connection_id());
    }

    #[test]
    fn legacy_authorization_file_is_adopted_so_connections_never_share_it() {
        let home = TempHome::new("legacy-auth");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");

        // Pre-ADR layout: the OAuth file sits one level above `data`, and
        // `authorization_path` still falls back to it when the canonical file
        // is missing. Left in place, every connection would resolve to it.
        let canonical = state_path.with_file_name("agent-user-authorization.json");
        let legacy = home.root.join("agent-user-authorization.json");
        fs::rename(&canonical, &legacy).unwrap();

        ensure(&state_path, "http://a.example").unwrap();
        assert!(!legacy.exists(), "legacy file must be moved, not copied");
        assert!(canonical.is_file());

        let b = add(&state_path, "http://b.example", "B 工作台", "").unwrap();
        switch(&state_path, &b.id).unwrap();
        assert!(
            !canonical.exists(),
            "an unauthorized connection must not inherit another connection's authorization"
        );
    }

    #[test]
    fn switching_keeps_both_identities_and_never_leaves_a_stale_backup() {
        let home = TempHome::new("switch");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        ensure(&state_path, "http://a.example").unwrap();

        let b = add(&state_path, "http://b.example", "B 工作台", "生产").unwrap();
        switch(&state_path, &b.id).unwrap();

        // B never enrolled: the legacy view must be empty, including backups,
        // otherwise A's identity would be read back for B.
        assert!(!state_path.exists());
        assert!(!super::atomic_file::backup_path(&state_path).exists());
        assert!(!state_path.with_extension("device-id").exists());
        assert!(!state_path
            .with_file_name("agent-user-authorization.json")
            .exists());

        // A's identity is still in the store, byte for byte.
        let store = load(&state_path).unwrap().unwrap();
        let a = store
            .connections()
            .iter()
            .find(|connection| connection.api_base == "http://a.example")
            .unwrap();
        assert_eq!(a.identity.agent_id, "agent-a");
        assert!(a.identity.state.is_some());
        let a_id = a.id.clone();

        // Switching back restores A on disk.
        switch(&state_path, &a_id).unwrap();
        let restored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(restored["agent_id"], "agent-a");
        assert_eq!(
            fs::read_to_string(state_path.with_extension("device-id")).unwrap(),
            "device-agent-a"
        );
        assert!(state_path
            .with_file_name("agent-user-authorization.json")
            .is_file());

        let final_store = load(&state_path).unwrap().unwrap();
        assert_eq!(final_store.connections().len(), 2);
    }

    #[test]
    fn an_enrolled_connection_is_captured_without_losing_the_other_one() {
        let home = TempHome::new("capture");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        ensure(&state_path, "http://a.example").unwrap();

        // B is added and switched to before it is enrolled; then the enrollment
        // (performed by the existing /enroll path) lands on disk.
        let b = add(&state_path, "http://b.example", "B 工作台", "").unwrap();
        switch(&state_path, &b.id).unwrap();
        write_identity(&state_path, "agent-b", "sealed-b", "user-b");

        assert!(capture_active(&state_path).unwrap());
        let store = load(&state_path).unwrap().unwrap();
        let a = store
            .connections()
            .iter()
            .find(|connection| connection.api_base == "http://a.example")
            .unwrap();
        let b = store
            .connections()
            .iter()
            .find(|connection| connection.api_base == "http://b.example")
            .unwrap();
        assert_eq!(a.identity.agent_id, "agent-a");
        assert_eq!(b.identity.agent_id, "agent-b");
        assert!(b.identity.authorized());

        // Nothing changed on disk, so a second capture is a no-op.
        assert!(!capture_active(&state_path).unwrap());
    }

    #[test]
    fn a_foreign_identity_on_disk_is_never_handed_to_another_connection() {
        let home = TempHome::new("foreign");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        ensure(&state_path, "http://a.example").unwrap();

        // B enrolls, so from here on `agent-b` belongs to B.
        let b = add(&state_path, "http://b.example", "B 工作台", "").unwrap();
        switch(&state_path, &b.id).unwrap();
        write_identity(&state_path, "agent-b", "sealed-b", "user-b");
        assert!(capture_active(&state_path).unwrap());

        // A is active again while the disk still holds B's identity — the
        // crossover a shared data root used to produce. Startup must restore
        // A's own identity instead of adopting B's.
        let a_id = load(&state_path)
            .unwrap()
            .unwrap()
            .connections()
            .iter()
            .find(|connection| connection.api_base == "http://a.example")
            .unwrap()
            .id
            .clone();
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path_for(&state_path)).unwrap()).unwrap();
        raw["active_connection_id"] = json!(a_id);
        raw["pending_materialize"] = json!(false);
        fs::write(
            path_for(&state_path),
            serde_json::to_vec_pretty(&raw).unwrap(),
        )
        .unwrap();
        write_identity(&state_path, "agent-b", "sealed-b", "user-b");

        sync_active(&state_path, "http://a.example").unwrap();
        let store = load(&state_path).unwrap().unwrap();
        let of = |api_base: &str| {
            store
                .connections()
                .iter()
                .find(|connection| connection.api_base == api_base)
                .unwrap()
                .identity
                .agent_id
                .clone()
        };
        assert_eq!(of("http://a.example"), "agent-a");
        assert_eq!(of("http://b.example"), "agent-b");
        let disk: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(disk["agent_id"], "agent-a");
    }

    #[test]
    fn startup_sync_prefers_disk_after_enrollment_and_store_after_a_switch() {
        let home = TempHome::new("sync");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        ensure(&state_path, "http://a.example").unwrap();

        // Enrollment happened but the process died before capture.
        write_identity(&state_path, "agent-a2", "sealed-a2", "user-a");
        let store = sync_active(&state_path, "http://a.example").unwrap();
        assert_eq!(store.connections()[0].identity.agent_id, "agent-a2");

        // A switch that crashed between "save" and "materialise" leaves the
        // pending flag behind; the store then wins.
        let b = add(&state_path, "http://b.example", "B", "").unwrap();
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path_for(&state_path)).unwrap()).unwrap();
        raw["active_connection_id"] = json!(b.id);
        raw["pending_materialize"] = json!(true);
        fs::write(
            path_for(&state_path),
            serde_json::to_vec_pretty(&raw).unwrap(),
        )
        .unwrap();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");

        sync_active(&state_path, "http://a.example").unwrap();
        assert!(
            !state_path.exists(),
            "B has no identity, so the disk view is empty"
        );
    }

    #[test]
    fn removing_and_renaming_update_the_store_without_touching_the_agent() {
        let home = TempHome::new("crud");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        ensure(&state_path, "http://a.example").unwrap();
        let active_id = load(&state_path)
            .unwrap()
            .unwrap()
            .active_connection_id()
            .to_string();

        let b = add(&state_path, "http://b.example", "", "").unwrap();
        assert_eq!(b.display_name, "b.example");

        let renamed = rename(&state_path, &b.id, "测试工作台", "本地开发").unwrap();
        assert_eq!(renamed.display_name, "测试工作台");
        assert_eq!(renamed.purpose, "本地开发");

        // The active connection is never removable, and a duplicate address is
        // rejected instead of silently creating a second record.
        assert!(remove(&state_path, &active_id).is_err());
        assert!(add(&state_path, "http://b.example", "重复", "").is_err());

        remove(&state_path, &b.id).unwrap();
        assert_eq!(load(&state_path).unwrap().unwrap().connections().len(), 1);
    }

    #[test]
    fn views_expose_state_without_secret_material() {
        let home = TempHome::new("views");
        let state_path = home.state_path();
        write_identity(&state_path, "agent-a", "sealed-a", "user-a");
        let store = ensure(&state_path, "http://a.example").unwrap();
        let views = store.views();
        assert_eq!(views.len(), 1);
        assert!(views[0].active);
        assert!(views[0].authorized);
        assert_eq!(views[0].state, "authorized");
        assert_eq!(views[0].user_name, "张三");
        assert_eq!(views[0].scope, vec!["agent.profile".to_string()]);
        let encoded = serde_json::to_string(&views).unwrap();
        assert!(
            !encoded.contains("sealed-"),
            "views must not leak sealed blobs"
        );
    }

    #[test]
    fn store_round_trips_and_rejects_an_unknown_active_pointer() {
        let home = TempHome::new("roundtrip");
        let state_path = home.state_path();
        ensure(&state_path, "http://a.example").unwrap();
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path_for(&state_path)).unwrap()).unwrap();
        raw["active_connection_id"] = json!("conn-missing");
        fs::write(
            path_for(&state_path),
            serde_json::to_vec_pretty(&raw).unwrap(),
        )
        .unwrap();

        let store: WorkbenchStore = load(&state_path).unwrap().unwrap();
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.active_connection_id(), store.connections()[0].id);
        let _ = ConnectionIdentity::default();
        let _ = Path::new("");
    }
}
