use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::store::atomic_file;
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};

const STORE_FILE: &str = "credentials.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConnectorCredentialRecord {
    handle: String,
    connector_id: String,
    kind: String,
    protected_value: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ConnectorCredentialSummary {
    pub handle: String,
    pub connector_id: String,
    pub kind: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedConnectorCredential {
    pub handle: String,
    pub connector_id: String,
    pub kind: String,
    pub value: String,
}

pub(crate) fn set_file_path(
    handle: &str,
    connector_id: &str,
    path: &Path,
) -> Result<ConnectorCredentialSummary, Box<dyn Error>> {
    let path = path.canonicalize()?;
    if !path.is_file() {
        return Err("connector credential file does not exist".into());
    }
    set_value(handle, connector_id, "file_path", &path.to_string_lossy())
}

pub(crate) fn set_secret(
    handle: &str,
    connector_id: &str,
    secret: &str,
) -> Result<ConnectorCredentialSummary, Box<dyn Error>> {
    if secret.is_empty() {
        return Err("connector credential secret is empty".into());
    }
    set_value(handle, connector_id, "secret", secret)
}

fn set_value(
    handle: &str,
    connector_id: &str,
    kind: &str,
    value: &str,
) -> Result<ConnectorCredentialSummary, Box<dyn Error>> {
    validate_identifier("credential handle", handle)?;
    validate_identifier("connector id", connector_id)?;
    if !matches!(kind, "file_path" | "secret") {
        return Err("connector credential kind is invalid".into());
    }
    let path = store_path();
    let _lock = atomic_file::lock(&path)?;
    let mut records = read_records(&path)?;
    records.retain(|record| record.handle != handle);
    let record = ConnectorCredentialRecord {
        handle: handle.to_string(),
        connector_id: connector_id.to_string(),
        kind: kind.to_string(),
        protected_value: protect_secret_for_current_user(value)?,
        updated_at: unix_timestamp_string(),
    };
    records.push(record.clone());
    records.sort_by(|left, right| left.handle.cmp(&right.handle));
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&records)?)?;
    Ok(summary(record))
}

pub(crate) fn resolve(handle: &str) -> Result<Option<ResolvedConnectorCredential>, Box<dyn Error>> {
    let records = read_records(&store_path())?;
    records
        .into_iter()
        .find(|record| record.handle == handle)
        .map(|record| {
            Ok(ResolvedConnectorCredential {
                handle: record.handle,
                connector_id: record.connector_id,
                kind: record.kind,
                value: unprotect_secret_for_current_user(&record.protected_value)?,
            })
        })
        .transpose()
}

pub(crate) fn list() -> Result<Vec<ConnectorCredentialSummary>, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut summaries = read_records(&path)?
        .into_iter()
        .map(summary)
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| left.handle.cmp(&right.handle));
    Ok(summaries)
}

pub(crate) fn remove(handle: &str) -> Result<bool, Box<dyn Error>> {
    let path = store_path();
    let _lock = atomic_file::lock(&path)?;
    let mut records = read_records(&path)?;
    let before = records.len();
    records.retain(|record| record.handle != handle);
    if records.len() == before {
        return Ok(false);
    }
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&records)?)?;
    Ok(true)
}

fn read_records(path: &Path) -> Result<Vec<ConnectorCredentialRecord>, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn store_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("connectors")
        .join(STORE_FILE)
}

fn summary(record: ConnectorCredentialRecord) -> ConnectorCredentialSummary {
    ConnectorCredentialSummary {
        handle: record.handle,
        connector_id: record.connector_id,
        kind: record.kind,
        updated_at: record.updated_at,
    }
}

fn validate_identifier(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid {name}: {value}").into());
    }
    Ok(())
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}
