use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::credentials;

const ACP_SESSION_SCHEMA_VERSION: &str = "acp_session.v1";
const ACP_SESSION_INDEX_SCHEMA_VERSION: &str = "acp_session_index.v1";
const MAX_ACP_SESSION_BYTES: usize = 512 * 1024;
const MAX_ACP_SESSION_INDEX_BYTES: usize = 16 * 1024;
const MAX_ACP_SESSION_FILES: usize = 10_000;

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoredAcpSessionMetadata {
    pub schema_version: String,
    pub session_id: String,
    pub cwd: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: String,
    pub message_count: usize,
}

#[derive(Debug)]
pub(crate) struct AcpMetadataScan {
    pub sessions: Vec<StoredAcpSessionMetadata>,
    pub partial: bool,
}

impl StoredAcpSessionMetadata {
    fn from_session(session: &StoredAcpSession) -> Self {
        let title = session
            .turns
            .first()
            .map(|turn| crate::runtime::process::summarize_output(&turn.user, 120))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "ACP 会话 {}",
                    session.session_id.chars().take(8).collect::<String>()
                )
            });
        Self {
            schema_version: ACP_SESSION_INDEX_SCHEMA_VERSION.to_string(),
            session_id: session.session_id.clone(),
            cwd: session.cwd.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            title,
            message_count: session.turns.len().saturating_mul(2),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != ACP_SESSION_INDEX_SCHEMA_VERSION {
            return Err("ACP session index schema_version is invalid".to_string());
        }
        validate_session_id(&self.session_id)?;
        if self.cwd.trim().is_empty() || !Path::new(&self.cwd).is_absolute() {
            return Err("ACP session index cwd must be absolute".to_string());
        }
        if self.created_at == 0
            || self.updated_at < self.created_at
            || self.title.trim().is_empty()
            || self.title.chars().count() > 512
            || self.message_count > 1_000_000
        {
            return Err("ACP session index metadata is invalid".to_string());
        }
        Ok(())
    }
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
    let directory = match existing_sessions_dir() {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    load_at(&directory, session_id).map_err(Into::into)
}

pub(crate) fn list() -> Result<Vec<StoredAcpSession>, Box<dyn std::error::Error>> {
    list_at(&sessions_dir()?).map_err(Into::into)
}

pub(crate) fn list_metadata() -> io::Result<AcpMetadataScan> {
    list_metadata_at(&safe_sessions_dir()?)
}

pub(crate) fn load_for_history(
    session_id: &str,
) -> Result<Option<StoredAcpSession>, Box<dyn std::error::Error>> {
    let directory = match safe_sessions_dir() {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    load_history_at(&directory, session_id).map_err(Into::into)
}

pub(crate) fn delete(session_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    delete_at(&sessions_dir()?, session_id).map_err(Into::into)
}

fn sessions_dir() -> io::Result<PathBuf> {
    let directory = sessions_dir_path();
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn sessions_dir_path() -> PathBuf {
    super::paths::agent_home().join("acp").join("sessions")
}

fn existing_sessions_dir() -> io::Result<PathBuf> {
    let directory = sessions_dir_path();
    if directory.is_dir() {
        Ok(directory)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "ACP sessions directory not found",
        ))
    }
}

fn safe_sessions_dir() -> io::Result<PathBuf> {
    safe_sessions_dir_at(&super::paths::agent_home(), &sessions_dir_path())
}

fn safe_sessions_dir_at(agent_home: &Path, sessions_directory: &Path) -> io::Result<PathBuf> {
    let canonical_home = fs::canonicalize(agent_home)?;
    let expected = canonical_home.join("acp").join("sessions");
    let canonical_sessions = fs::canonicalize(sessions_directory)?;
    if canonical_sessions != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ACP sessions directory resolves outside the Agent data root",
        ));
    }
    Ok(canonical_sessions)
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
    result?;

    let metadata = StoredAcpSessionMetadata::from_session(session);
    let metadata_payload = serde_json::to_vec(&metadata)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if metadata_payload.len() > MAX_ACP_SESSION_INDEX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ACP session index exceeds the local size limit",
        ));
    }
    let protected_metadata = credentials::protect_secret_for_current_user(
        std::str::from_utf8(&metadata_payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let index_path = metadata_path(&path);
    let index_lock = super::atomic_file::lock(&index_path)?;
    let index_result = super::atomic_file::atomic_write(&index_path, protected_metadata.as_bytes());
    drop(index_lock);
    index_result.map(|_| path)
}

fn load_at(directory: &Path, session_id: &str) -> io::Result<Option<StoredAcpSession>> {
    let path = session_path(directory, session_id)?;
    load_path(&path)
}

fn load_history_at(directory: &Path, session_id: &str) -> io::Result<Option<StoredAcpSession>> {
    let canonical_root = fs::canonicalize(directory)?;
    let path = session_path(&canonical_root, session_id)?;
    let file_type = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file_type.is_symlink() || !file_type.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ACP session history must be a regular file",
        ));
    }
    let canonical_path = fs::canonicalize(&path)?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ACP session history resolves outside its storage root",
        ));
    }
    load_path(&canonical_path)
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

fn list_metadata_at(directory: &Path) -> io::Result<AcpMetadataScan> {
    let canonical_root = fs::canonicalize(directory)?;
    let entries = fs::read_dir(&canonical_root)?;
    let mut metadata_paths = Vec::new();
    let mut body_stems = std::collections::HashSet::new();
    let mut metadata_stems = std::collections::HashSet::new();
    let mut partial = false;
    let mut visited_entries = 0usize;

    for entry in entries {
        visited_entries += 1;
        if visited_entries > MAX_ACP_SESSION_FILES {
            partial = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        if file_type.is_symlink() {
            partial = true;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            partial = true;
            continue;
        };
        match extension {
            "json" => {
                body_stems.insert(stem.to_string());
            }
            "meta" => {
                metadata_stems.insert(stem.to_string());
                metadata_paths.push(path);
            }
            _ => continue,
        }
        if body_stems.len() + metadata_stems.len() > MAX_ACP_SESSION_FILES {
            partial = true;
            break;
        }
    }

    if body_stems.iter().any(|stem| !metadata_stems.contains(stem)) {
        partial = true;
    }

    let mut sessions = Vec::new();
    for path in metadata_paths {
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            partial = true;
            continue;
        };
        if !body_stems.contains(stem) {
            partial = true;
            continue;
        }
        let canonical_path = match fs::canonicalize(&path) {
            Ok(path) if path.starts_with(&canonical_root) => path,
            _ => {
                partial = true;
                continue;
            }
        };
        match load_metadata_path(&canonical_path) {
            Ok(Some(metadata)) => {
                let expected = session_path(directory, &metadata.session_id)?;
                if metadata_path(&expected).file_name() != path.file_name() {
                    partial = true;
                    continue;
                }
                sessions.push(metadata);
            }
            Ok(None) => {}
            Err(_) => partial = true,
        }
    }
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(AcpMetadataScan { sessions, partial })
}

fn load_metadata_path(path: &Path) -> io::Result<Option<StoredAcpSessionMetadata>> {
    let encoded = match fs::read(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if encoded.len() > MAX_ACP_SESSION_INDEX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACP session index exceeds the local size limit",
        ));
    }
    let protected = String::from_utf8(encoded)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let payload = credentials::unprotect_secret_for_current_user(&protected)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let metadata: StoredAcpSessionMetadata = serde_json::from_str(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    metadata
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(metadata))
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
    let path = session_path(directory, session_id)?;
    for path in [path.clone(), metadata_path(&path)] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn metadata_path(session_path: &Path) -> PathBuf {
    session_path.with_extension("meta")
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
    use super::{
        delete_at, list_at, list_metadata_at, load_at, metadata_path, save_at, StoredAcpSession,
        StoredAcpTurn,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
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
        assert!(metadata_path(&path).is_file());
        delete_at(&root, "session-1").unwrap();
        assert_eq!(load_at(&root, "session-1").unwrap(), None);
        assert!(!metadata_path(&path).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_index_is_encrypted_and_listing_does_not_read_transcript_body() {
        let root = temp_root();
        let mut session =
            StoredAcpSession::new("session-2".to_string(), root.to_string_lossy().to_string());
        session.turns.push(StoredAcpTurn {
            user: "private prompt text".to_string(),
            assistant: "private response text".to_string(),
        });
        let body_path = save_at(&root, &session).unwrap();
        let index_path = metadata_path(&body_path);
        let raw_index = std::fs::read_to_string(&index_path).unwrap();
        assert!(!raw_index.contains("private prompt text"));
        assert!(!raw_index.contains("private response text"));

        std::fs::write(&body_path, b"body intentionally unreadable").unwrap();
        let scan = list_metadata_at(&root).unwrap();
        assert!(!scan.partial);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].session_id, "session-2");
        assert_eq!(scan.sessions[0].message_count, 2);
        assert!(scan.sessions[0].title.contains("private prompt text"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_index_reports_legacy_body_without_reading_it() {
        let root = temp_root();
        let session = StoredAcpSession::new(
            "legacy-session".to_string(),
            root.to_string_lossy().to_string(),
        );
        let body_path = save_at(&root, &session).unwrap();
        std::fs::remove_file(metadata_path(&body_path)).unwrap();

        let scan = list_metadata_at(&root).unwrap();
        assert!(scan.partial);
        assert!(scan.sessions.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_preview_rejects_a_symlinked_session_file() {
        let root = temp_root();
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.with_extension("outside");
        std::fs::write(&outside, b"outside session payload").unwrap();
        let session = super::session_path(&root, "linked-session").unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, &session);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &session);
        if linked.is_err() {
            let _ = std::fs::remove_file(outside);
            let _ = std::fs::remove_dir_all(root);
            return;
        }

        assert!(super::load_history_at(&root, "linked-session").is_err());
        let _ = std::fs::remove_file(session);
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_scan_skips_a_symlinked_index_file() {
        let root = temp_root();
        let mut session = StoredAcpSession::new(
            "linked-index-session".to_string(),
            root.to_string_lossy().to_string(),
        );
        session.turns.push(StoredAcpTurn {
            user: "synthetic prompt".to_string(),
            assistant: "synthetic response".to_string(),
        });
        let body = save_at(&root, &session).unwrap();
        let index = metadata_path(&body);
        std::fs::remove_file(&index).unwrap();
        let outside = root.with_extension("outside-index");
        std::fs::write(&outside, b"external metadata payload").unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, &index);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &index);
        if linked.is_err() {
            let _ = std::fs::remove_file(outside);
            let _ = std::fs::remove_dir_all(root);
            return;
        }

        let scan = list_metadata_at(&root).unwrap();
        assert!(scan.partial);
        assert!(scan.sessions.is_empty());
        let _ = std::fs::remove_file(index);
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_scan_rejects_a_redirected_storage_root() {
        let home = temp_root();
        let outside = home.with_extension("outside-root");
        std::fs::create_dir_all(home.join("acp")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let sessions = home.join("acp").join("sessions");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &sessions);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &sessions);
        if linked.is_err() {
            let _ = std::fs::remove_dir_all(outside);
            let _ = std::fs::remove_dir_all(home);
            return;
        }

        assert!(super::safe_sessions_dir_at(&home, &sessions).is_err());
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&sessions);
        #[cfg(unix)]
        let _ = std::fs::remove_file(sessions);
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(home);
    }
}
