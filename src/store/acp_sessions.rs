use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::credentials;

const ACP_SESSION_SCHEMA_VERSION: &str = "acp_session.v1";
const MAX_ACP_SESSION_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoredAcpTurn {
    pub user: String,
    pub assistant: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoredAcpSession {
    pub schema_version: String,
    pub session_id: String,
    pub cwd: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub turns: Vec<StoredAcpTurn>,
}

impl StoredAcpSession {
    pub(crate) fn new(session_id: String, cwd: String) -> Self {
        let now = unix_timestamp();
        Self {
            schema_version: ACP_SESSION_SCHEMA_VERSION.to_string(),
            session_id,
            cwd,
            created_at: now,
            updated_at: now,
            turns: Vec::new(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != ACP_SESSION_SCHEMA_VERSION {
            return Err("ACP session schema_version is invalid".to_string());
        }
        validate_session_id(&self.session_id)?;
        if self.cwd.trim().is_empty() || !Path::new(&self.cwd).is_absolute() {
            return Err("ACP session cwd must be absolute".to_string());
        }
        if self.created_at == 0 || self.updated_at < self.created_at {
            return Err("ACP session timestamps are invalid".to_string());
        }
        Ok(())
    }
}

pub(crate) fn save(session: &StoredAcpSession) -> Result<PathBuf, Box<dyn std::error::Error>> {
    save_at(&sessions_dir()?, session).map_err(Into::into)
}

pub(crate) fn load(
    session_id: &str,
) -> Result<Option<StoredAcpSession>, Box<dyn std::error::Error>> {
    load_at(&sessions_dir()?, session_id).map_err(Into::into)
}

pub(crate) fn list() -> Result<Vec<StoredAcpSession>, Box<dyn std::error::Error>> {
    list_at(&sessions_dir()?).map_err(Into::into)
}

pub(crate) fn delete(session_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    delete_at(&sessions_dir()?, session_id).map_err(Into::into)
}

fn sessions_dir() -> io::Result<PathBuf> {
    let directory = super::paths::agent_home().join("acp").join("sessions");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn save_at(directory: &Path, session: &StoredAcpSession) -> io::Result<PathBuf> {
    session
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    fs::create_dir_all(directory)?;
    let payload = serde_json::to_vec(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_ACP_SESSION_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ACP session exceeds the local size limit",
        ));
    }
    let protected = credentials::protect_secret_for_current_user(
        std::str::from_utf8(&payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let path = session_path(directory, &session.session_id)?;
    let lock = super::atomic_file::lock(&path)?;
    let result = super::atomic_file::atomic_write(&path, protected.as_bytes());
    drop(lock);
    result.map(|_| path)
}

fn load_at(directory: &Path, session_id: &str) -> io::Result<Option<StoredAcpSession>> {
    let path = session_path(directory, session_id)?;
    load_path(&path)
}

fn list_at(directory: &Path) -> io::Result<Vec<StoredAcpSession>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(session) = load_path(&path)? {
            sessions.push(session);
        }
    }
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

fn load_path(path: &Path) -> io::Result<Option<StoredAcpSession>> {
    let encoded = match fs::read(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if encoded.len() > MAX_ACP_SESSION_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACP session exceeds the local size limit",
        ));
    }
    let protected = String::from_utf8(encoded)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let payload = credentials::unprotect_secret_for_current_user(&protected)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let session: StoredAcpSession = serde_json::from_str(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    session
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(session))
}

fn delete_at(directory: &Path, session_id: &str) -> io::Result<()> {
    match fs::remove_file(session_path(directory, session_id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn session_path(directory: &Path, session_id: &str) -> io::Result<PathBuf> {
    validate_session_id(session_id)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let digest = format!("{:x}", Sha256::digest(session_id.as_bytes()));
    Ok(directory.join(format!("{}.json", &digest[..32])))
}

fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.len() > 512
        || session_id.chars().any(|value| value.is_control())
    {
        return Err("ACP session id is invalid".to_string());
    }
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{delete_at, list_at, load_at, save_at, StoredAcpSession, StoredAcpTurn};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-acp-session-store-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn protects_round_trips_and_deletes_session_history() {
        let root = temp_root();
        let mut session =
            StoredAcpSession::new("session-1".to_string(), root.to_string_lossy().to_string());
        session.turns.push(StoredAcpTurn {
            user: "first user".to_string(),
            assistant: "first answer".to_string(),
        });
        let path = save_at(&root, &session).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("first user"));
        assert_eq!(list_at(&root).unwrap(), vec![session.clone()]);
        assert_eq!(load_at(&root, "session-1").unwrap(), Some(session));
        delete_at(&root, "session-1").unwrap();
        assert_eq!(load_at(&root, "session-1").unwrap(), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
