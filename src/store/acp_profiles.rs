use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const ACP_PROFILE_SCHEMA_VERSION: &str = "acp_runtime_profiles.v1";
const MAX_ACP_PROFILES: usize = 100;
const MAX_ACP_PROFILE_ARGUMENTS: usize = 64;
const MAX_ACP_PROFILE_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcpRuntimeProfileRecord {
    pub provider_id: String,
    pub display_name: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub version: String,
    pub permission_policy: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcpRuntimeProfiles {
    schema_version: String,
    #[serde(default)]
    profiles: Vec<AcpRuntimeProfileRecord>,
}

impl Default for AcpRuntimeProfiles {
    fn default() -> Self {
        Self {
            schema_version: ACP_PROFILE_SCHEMA_VERSION.to_string(),
            profiles: Vec::new(),
        }
    }
}

pub(crate) fn list() -> Result<Vec<AcpRuntimeProfileRecord>, Box<dyn std::error::Error>> {
    list_at(&profiles_path()?).map_err(Into::into)
}

pub(crate) fn upsert(
    profile: AcpRuntimeProfileRecord,
) -> Result<AcpRuntimeProfileRecord, Box<dyn std::error::Error>> {
    let path = profiles_path()?;
    upsert_at(&path, profile).map_err(Into::into)
}

pub(crate) fn remove(provider_id: &str) -> Result<bool, Box<dyn std::error::Error>> {
    remove_at(&profiles_path()?, provider_id).map_err(Into::into)
}

pub(crate) fn set_enabled(
    provider_id: &str,
    enabled: bool,
) -> Result<AcpRuntimeProfileRecord, Box<dyn std::error::Error>> {
    let path = profiles_path()?;
    let mut document = read_document(&path)?;
    let profile = document
        .profiles
        .iter_mut()
        .find(|profile| profile.provider_id == normalize_provider_id(provider_id))
        .ok_or_else(|| format!("ACP runtime profile was not found: {provider_id}"))?;
    profile.enabled = enabled;
    let record = profile.clone();
    write_document(&path, &document)?;
    Ok(record)
}

pub(crate) fn normalize_provider_id(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("acp.") {
        value.to_string()
    } else {
        format!("acp.{value}")
    }
}

fn profiles_path() -> io::Result<PathBuf> {
    let directory = super::paths::agent_home().join("acp");
    fs::create_dir_all(&directory)?;
    Ok(directory.join("runtime-profiles.json"))
}

fn list_at(path: &Path) -> io::Result<Vec<AcpRuntimeProfileRecord>> {
    Ok(read_document(path)?.profiles)
}

fn upsert_at(
    path: &Path,
    mut profile: AcpRuntimeProfileRecord,
) -> io::Result<AcpRuntimeProfileRecord> {
    profile.provider_id = normalize_provider_id(&profile.provider_id);
    validate_profile(&profile)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut document = read_document(path)?;
    if let Some(existing) = document
        .profiles
        .iter_mut()
        .find(|existing| existing.provider_id == profile.provider_id)
    {
        *existing = profile.clone();
    } else {
        document.profiles.push(profile.clone());
    }
    document
        .profiles
        .sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    write_document(path, &document)?;
    Ok(profile)
}

fn remove_at(path: &Path, provider_id: &str) -> io::Result<bool> {
    let provider_id = normalize_provider_id(provider_id);
    let mut document = read_document(path)?;
    let previous = document.profiles.len();
    document
        .profiles
        .retain(|profile| profile.provider_id != provider_id);
    if previous == document.profiles.len() {
        return Ok(false);
    }
    write_document(path, &document)?;
    Ok(true)
}

fn read_document(path: &Path) -> io::Result<AcpRuntimeProfiles> {
    let content = match fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AcpRuntimeProfiles::default())
        }
        Err(error) => return Err(error),
    };
    if content.len() > MAX_ACP_PROFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACP runtime profile store exceeds the local size limit",
        ));
    }
    let document: AcpRuntimeProfiles = serde_json::from_slice(&content)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.schema_version != ACP_PROFILE_SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACP runtime profile store schema_version is invalid",
        ));
    }
    for profile in &document.profiles {
        validate_profile(profile)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    Ok(document)
}

fn write_document(path: &Path, document: &AcpRuntimeProfiles) -> io::Result<()> {
    if document.profiles.len() > MAX_ACP_PROFILES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ACP runtime profile count exceeds the local limit",
        ));
    }
    let payload = serde_json::to_vec_pretty(document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_ACP_PROFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ACP runtime profile store exceeds the local size limit",
        ));
    }
    let lock = super::atomic_file::lock(path)?;
    let result = super::atomic_file::atomic_write(path, &payload);
    drop(lock);
    result
}

fn validate_profile(profile: &AcpRuntimeProfileRecord) -> Result<(), String> {
    if !valid_provider_id(&profile.provider_id) {
        return Err("ACP runtime provider id is invalid".to_string());
    }
    if profile.display_name.trim().is_empty() || profile.display_name.len() > 200 {
        return Err("ACP runtime display name is invalid".to_string());
    }
    if profile.executable.trim().is_empty() || profile.executable.len() > 4_000 {
        return Err("ACP runtime executable is invalid".to_string());
    }
    if profile.args.len() > MAX_ACP_PROFILE_ARGUMENTS
        || profile
            .args
            .iter()
            .any(|argument| argument.len() > 4_000 || argument.chars().any(char::is_control))
    {
        return Err("ACP runtime arguments are invalid".to_string());
    }
    if profile.version.len() > 200 || profile.version.chars().any(char::is_control) {
        return Err("ACP runtime version is invalid".to_string());
    }
    if !matches!(
        profile.permission_policy.as_str(),
        "deny" | "allow_once" | "prompt"
    ) {
        return Err("ACP runtime permission policy is invalid".to_string());
    }
    Ok(())
}

fn valid_provider_id(value: &str) -> bool {
    let Some(id) = value.strip_prefix("acp.") else {
        return false;
    };
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{list_at, remove_at, upsert_at, AcpRuntimeProfileRecord};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn profile() -> AcpRuntimeProfileRecord {
        AcpRuntimeProfileRecord {
            provider_id: "fixture".to_string(),
            display_name: "ACP Fixture".to_string(),
            executable: "pwsh".to_string(),
            args: vec!["-NoProfile".to_string()],
            version: "1.0.0".to_string(),
            permission_policy: "deny".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn upserts_lists_disables_and_removes_profiles() {
        let path = std::env::temp_dir().join(format!(
            "himind-acp-profiles-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profile = upsert_at(&path, profile()).unwrap();
        assert_eq!(profile.provider_id, "acp.fixture");
        assert_eq!(list_at(&path).unwrap().len(), 1);
        assert!(!set_enabled_at(&path, "fixture", false).unwrap().enabled);
        assert!(remove_at(&path, "acp.fixture").unwrap());
        assert!(list_at(&path).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    fn set_enabled_at(
        path: &std::path::Path,
        provider_id: &str,
        enabled: bool,
    ) -> Result<AcpRuntimeProfileRecord, String> {
        let mut document = super::read_document(path).map_err(|error| error.to_string())?;
        let profile = document
            .profiles
            .iter_mut()
            .find(|profile| profile.provider_id == super::normalize_provider_id(provider_id))
            .ok_or("profile missing")?;
        profile.enabled = enabled;
        let record = profile.clone();
        super::write_document(path, &document).map_err(|error| error.to_string())?;
        Ok(record)
    }
}
