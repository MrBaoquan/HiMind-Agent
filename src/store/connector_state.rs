use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::store::atomic_file;

const STORE_FILE: &str = "state.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectorState {
    pub connector_id: String,
    pub enabled: bool,
    pub revoked: bool,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub remote_revision: u64,
    #[serde(default)]
    pub reason: String,
    pub updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConnectorStateStore {
    #[serde(default)]
    connectors: BTreeMap<String, ConnectorState>,
}

pub(crate) fn list() -> Result<Vec<ConnectorState>, Box<dyn Error>> {
    list_at(&store_path())
}

pub(crate) fn status(connector_id: &str) -> Result<ConnectorState, Box<dyn Error>> {
    status_at(&store_path(), connector_id)
}

pub(crate) fn set_enabled(
    connector_id: &str,
    enabled: bool,
) -> Result<ConnectorState, Box<dyn Error>> {
    set_enabled_at(&store_path(), connector_id, enabled)
}

fn set_enabled_at(
    path: &Path,
    connector_id: &str,
    enabled: bool,
) -> Result<ConnectorState, Box<dyn Error>> {
    update_at(path, connector_id, |state| {
        state.enabled = enabled;
        state.source = "local".to_string();
        state.remote_revision = 0;
        if enabled && state.revoked {
            return Err("revoked connector must be restored before it can be enabled".into());
        }
        if !enabled {
            state.reason.clear();
        }
        Ok(())
    })
}

pub(crate) fn revoke(connector_id: &str, reason: &str) -> Result<ConnectorState, Box<dyn Error>> {
    let state = revoke_at(&store_path(), connector_id, reason)?;
    crate::store::connector_credentials::remove_by_connector(connector_id)?;
    Ok(state)
}

pub(crate) fn revoke_at(
    path: &Path,
    connector_id: &str,
    reason: &str,
) -> Result<ConnectorState, Box<dyn Error>> {
    let reason = reason.trim();
    if reason.len() > 500 {
        return Err("connector revoke reason is too long".into());
    }
    update_at(path, connector_id, |state| {
        state.enabled = false;
        state.revoked = true;
        state.source = "local".to_string();
        state.remote_revision = 0;
        state.reason = reason.to_string();
        Ok(())
    })
}

pub(crate) fn restore(connector_id: &str) -> Result<ConnectorState, Box<dyn Error>> {
    restore_at(&store_path(), connector_id)
}

fn restore_at(path: &Path, connector_id: &str) -> Result<ConnectorState, Box<dyn Error>> {
    update_at(path, connector_id, |state| {
        state.revoked = false;
        state.source = "local".to_string();
        state.remote_revision = 0;
        state.reason.clear();
        Ok(())
    })
}

pub(crate) fn apply_remote_policy_at(
    path: &Path,
    connector_id: &str,
    revoked: bool,
    reason: &str,
    revision: u64,
) -> Result<ConnectorState, Box<dyn Error>> {
    validate_identifier(connector_id)?;
    let reason = reason.trim();
    if reason.len() > 500 {
        return Err("connector remote revoke reason is too long".into());
    }
    update_at(path, connector_id, |state| {
        if state.source == "local" && state.revoked {
            return Ok(());
        }
        if revision < state.remote_revision {
            return Ok(());
        }
        if revoked {
            state.enabled = false;
            state.revoked = true;
            state.source = "dashboard".to_string();
            state.remote_revision = revision;
            state.reason = reason.to_string();
        } else if state.source == "dashboard" {
            state.revoked = false;
            state.source = "local".to_string();
            state.remote_revision = revision;
            state.reason.clear();
        }
        Ok(())
    })
}

pub(crate) fn ensure_available(connector_id: &str) -> Result<(), Box<dyn Error>> {
    let state = status(connector_id)?;
    if state.revoked {
        return Err(format!(
            "connector {} is revoked{}",
            connector_id,
            if state.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", state.reason)
            }
        )
        .into());
    }
    if !state.enabled {
        return Err(format!("connector {connector_id} is disabled").into());
    }
    Ok(())
}

fn list_at(path: &Path) -> Result<Vec<ConnectorState>, Box<dyn Error>> {
    Ok(read_store(path)?.connectors.into_values().collect())
}

pub(crate) fn status_at(path: &Path, connector_id: &str) -> Result<ConnectorState, Box<dyn Error>> {
    validate_identifier(connector_id)?;
    let store = read_store(path)?;
    Ok(store
        .connectors
        .get(connector_id)
        .cloned()
        .unwrap_or_else(|| default_state(connector_id)))
}

fn update_at(
    path: &Path,
    connector_id: &str,
    mutate: impl FnOnce(&mut ConnectorState) -> Result<(), Box<dyn Error>>,
) -> Result<ConnectorState, Box<dyn Error>> {
    validate_identifier(connector_id)?;
    let _lock = atomic_file::lock(path)?;
    let mut store = read_store(path)?;
    let mut state = store
        .connectors
        .remove(connector_id)
        .unwrap_or_else(|| default_state(connector_id));
    mutate(&mut state)?;
    state.updated_at = unix_timestamp_string();
    store
        .connectors
        .insert(connector_id.to_string(), state.clone());
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(&store)?)?;
    Ok(state)
}

fn default_state(connector_id: &str) -> ConnectorState {
    ConnectorState {
        connector_id: connector_id.to_string(),
        enabled: true,
        revoked: false,
        source: "local".to_string(),
        remote_revision: 0,
        reason: String::new(),
        updated_at: String::new(),
    }
}

fn default_source() -> String {
    "local".to_string()
}

fn read_store(path: &Path) -> Result<ConnectorStateStore, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(ConnectorStateStore::default());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub(crate) fn store_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("connectors")
        .join(STORE_FILE)
}

fn validate_identifier(value: &str) -> Result<(), Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid connector id: {value}").into());
    }
    Ok(())
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        apply_remote_policy_at, list_at, restore_at, revoke_at, set_enabled_at, status_at,
    };

    #[test]
    fn connector_state_fails_closed_after_disable_or_revoke() {
        let root = std::env::temp_dir().join(format!(
            "himind-connector-state-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = root.join("state.json");
        assert!(status_at(&path, "wechat-miniprogram").unwrap().enabled);
        set_enabled_at(&path, "wechat-miniprogram", false).unwrap();
        assert!(!status_at(&path, "wechat-miniprogram").unwrap().enabled);
        set_enabled_at(&path, "wechat-miniprogram", true).unwrap();
        assert!(status_at(&path, "wechat-miniprogram").unwrap().enabled);

        revoke_at(&path, "wechat-miniprogram", "credential breach").unwrap();
        let revoked = status_at(&path, "wechat-miniprogram").unwrap();
        assert!(revoked.revoked);
        assert_eq!(revoked.reason, "credential breach");
        assert!(set_enabled_at(&path, "wechat-miniprogram", true).is_err());

        restore_at(&path, "wechat-miniprogram").unwrap();
        set_enabled_at(&path, "wechat-miniprogram", true).unwrap();
        assert!(!status_at(&path, "wechat-miniprogram").unwrap().revoked);
        let remote_revoked =
            apply_remote_policy_at(&path, "wechat-miniprogram", true, "dashboard revocation", 2)
                .unwrap();
        assert_eq!(remote_revoked.source, "dashboard");
        assert!(remote_revoked.revoked);
        let stale_restore =
            apply_remote_policy_at(&path, "wechat-miniprogram", false, "", 1).unwrap();
        assert!(stale_restore.revoked);
        let remote_restore =
            apply_remote_policy_at(&path, "wechat-miniprogram", false, "", 3).unwrap();
        assert!(!remote_restore.revoked);
        assert_eq!(remote_restore.source, "local");
        assert_eq!(list_at(&path).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
