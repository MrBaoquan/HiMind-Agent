//! Persistent extension lock and install recovery metadata.

use crate::store::{atomic_file, paths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionLockDependency {
    pub asset_kind: String,
    pub asset_id: String,
    pub min_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionLockEntry {
    pub asset_kind: String,
    pub asset_id: String,
    pub version: String,
    pub source_id: String,
    pub source: String,
    pub repository: String,
    pub reference: String,
    pub catalog_path: String,
    pub source_commit: String,
    pub artifact_url: String,
    pub sha256: String,
    pub dependencies: Vec<ExtensionLockDependency>,
    pub agent_profile: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ExtensionLockFile {
    #[serde(default = "lock_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, ExtensionLockEntry>,
}

fn lock_schema_version() -> u32 {
    LOCK_SCHEMA_VERSION
}

pub(crate) fn path() -> PathBuf {
    paths::agent_home().join("data/extension.lock.json")
}

pub(crate) fn path_for_state_root(state_root: &Path) -> PathBuf {
    state_root.join("extension.lock.json")
}

pub(crate) fn load() -> Result<ExtensionLockFile, Box<dyn Error>> {
    load_at(&path())
}

fn load_at(path: &Path) -> Result<ExtensionLockFile, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(ExtensionLockFile {
            schema_version: LOCK_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        });
    }
    let mut value: ExtensionLockFile = serde_json::from_slice(&fs::read(path)?)?;
    if value.schema_version == 0 {
        value.schema_version = LOCK_SCHEMA_VERSION;
    }
    if value.schema_version != LOCK_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 extension.lock schema 版本: {}",
            value.schema_version
        )
        .into());
    }
    Ok(value)
}

pub(crate) fn save(value: &ExtensionLockFile) -> Result<(), Box<dyn Error>> {
    save_at(&path(), value)
}

fn save_at(path: &Path, value: &ExtensionLockFile) -> Result<(), Box<dyn Error>> {
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub(crate) fn upsert(entry: ExtensionLockEntry) -> Result<(), Box<dyn Error>> {
    upsert_at(&path(), entry)
}

fn upsert_at(lock_path: &Path, entry: ExtensionLockEntry) -> Result<(), Box<dyn Error>> {
    let key = format!("{}:{}", entry.asset_kind, entry.asset_id);
    let mut lock = load_at(lock_path)?;
    lock.entries.insert(key, entry);
    save_at(lock_path, &lock)
}

pub(crate) fn remove(asset_kind: &str, asset_id: &str) -> Result<(), Box<dyn Error>> {
    remove_at(&path(), asset_kind, asset_id)
}

pub(crate) fn remove_at(
    lock_path: &Path,
    asset_kind: &str,
    asset_id: &str,
) -> Result<(), Box<dyn Error>> {
    let mut lock = load_at(lock_path)?;
    lock.entries
        .remove(&format!("{}:{}", asset_kind.trim(), asset_id.trim()));
    save_at(lock_path, &lock)
}

pub(crate) fn read(
    asset_kind: &str,
    asset_id: &str,
) -> Result<Option<ExtensionLockEntry>, Box<dyn Error>> {
    Ok(load()?
        .entries
        .get(&format!("{}:{}", asset_kind.trim(), asset_id.trim()))
        .cloned())
}

pub(crate) fn restore(
    asset_kind: &str,
    asset_id: &str,
    previous: Option<ExtensionLockEntry>,
) -> Result<(), Box<dyn Error>> {
    let mut lock = load()?;
    let key = format!("{}:{}", asset_kind.trim(), asset_id.trim());
    match previous {
        Some(entry) => {
            lock.entries.insert(key, entry);
        }
        None => {
            lock.entries.remove(&key);
        }
    }
    save(&lock)
}

pub(crate) fn list() -> Result<Vec<ExtensionLockEntry>, Box<dyn Error>> {
    Ok(load()?.entries.into_values().collect())
}

pub(crate) fn record_plugin(
    item: &crate::api::distribution::PluginCatalogItem,
) -> Result<(), Box<dyn Error>> {
    upsert(ExtensionLockEntry {
        asset_kind: "plugin".to_string(),
        asset_id: item.plugin_id.clone(),
        version: item.version.clone(),
        source_id: item.source.clone(),
        source: item.source.clone(),
        repository: String::new(),
        reference: String::new(),
        catalog_path: String::new(),
        source_commit: String::new(),
        artifact_url: item.download_url.clone(),
        sha256: item.sha256.clone(),
        dependencies: item
            .plugin_dependencies
            .iter()
            .map(|dependency| ExtensionLockDependency {
                asset_kind: "plugin".to_string(),
                asset_id: dependency.plugin_id.clone(),
                min_version: dependency.min_version.clone(),
            })
            .collect(),
        agent_profile: paths::profile_name(),
        updated_at: now_stamp(),
    })
}

pub(crate) fn record_source_plugin(
    source: &crate::app::extension_source::ExtensionSourceConfig,
    item: &crate::api::distribution::PluginCatalogItem,
) -> Result<(), Box<dyn Error>> {
    upsert(ExtensionLockEntry {
        asset_kind: "plugin".to_string(),
        asset_id: item.plugin_id.clone(),
        version: item.version.clone(),
        source_id: source.id.clone(),
        source: item.source.clone(),
        repository: source.repository.clone(),
        reference: source.reference.clone(),
        catalog_path: source.catalog_path.clone(),
        source_commit: source_commit(&source.reference),
        artifact_url: item.download_url.clone(),
        sha256: item.sha256.clone(),
        dependencies: item
            .plugin_dependencies
            .iter()
            .map(|dependency| ExtensionLockDependency {
                asset_kind: "plugin".to_string(),
                asset_id: dependency.plugin_id.clone(),
                min_version: dependency.min_version.clone(),
            })
            .collect(),
        agent_profile: paths::profile_name(),
        updated_at: now_stamp(),
    })
}

pub(crate) fn record_skill(
    item: &crate::api::distribution::SkillCatalogItem,
) -> Result<(), Box<dyn Error>> {
    upsert(ExtensionLockEntry {
        asset_kind: "skill".to_string(),
        asset_id: item.skill_id.clone(),
        version: item.version.clone(),
        source_id: item.source.clone(),
        source: item.source.clone(),
        repository: String::new(),
        reference: String::new(),
        catalog_path: String::new(),
        source_commit: String::new(),
        artifact_url: item.download_url.clone(),
        sha256: item.sha256.clone(),
        dependencies: item
            .plugin_dependencies
            .iter()
            .map(|dependency| ExtensionLockDependency {
                asset_kind: "plugin".to_string(),
                asset_id: dependency.plugin_id.clone(),
                min_version: dependency.min_version.clone(),
            })
            .collect(),
        agent_profile: paths::profile_name(),
        updated_at: now_stamp(),
    })
}

pub(crate) fn record_source_skill(
    source: &crate::app::extension_source::ExtensionSourceConfig,
    item: &crate::api::distribution::SkillCatalogItem,
) -> Result<(), Box<dyn Error>> {
    upsert(ExtensionLockEntry {
        asset_kind: "skill".to_string(),
        asset_id: item.skill_id.clone(),
        version: item.version.clone(),
        source_id: source.id.clone(),
        source: item.source.clone(),
        repository: source.repository.clone(),
        reference: source.reference.clone(),
        catalog_path: source.catalog_path.clone(),
        source_commit: source_commit(&source.reference),
        artifact_url: item.download_url.clone(),
        sha256: item.sha256.clone(),
        dependencies: item
            .plugin_dependencies
            .iter()
            .map(|dependency| ExtensionLockDependency {
                asset_kind: "plugin".to_string(),
                asset_id: dependency.plugin_id.clone(),
                min_version: dependency.min_version.clone(),
            })
            .collect(),
        agent_profile: paths::profile_name(),
        updated_at: now_stamp(),
    })
}

pub(crate) fn record_local_skill(
    manifest: &crate::skill::types::SkillManifest,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    record_local_skill_at(&path(), manifest, source)
}

pub(crate) fn record_local_skill_at(
    lock_path: &Path,
    manifest: &crate::skill::types::SkillManifest,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    upsert_at(
        lock_path,
        ExtensionLockEntry {
            asset_kind: "skill".to_string(),
            asset_id: manifest.id.clone(),
            version: manifest.version.clone(),
            source_id: source.to_string(),
            source: source.to_string(),
            repository: String::new(),
            reference: String::new(),
            catalog_path: String::new(),
            source_commit: String::new(),
            artifact_url: String::new(),
            sha256: String::new(),
            dependencies: manifest
                .plugin_dependencies
                .iter()
                .map(|dependency| ExtensionLockDependency {
                    asset_kind: "plugin".to_string(),
                    asset_id: dependency.plugin_id.clone(),
                    min_version: dependency.min_version.clone().unwrap_or_default(),
                })
                .collect(),
            agent_profile: paths::profile_name(),
            updated_at: now_stamp(),
        },
    )
}

pub(crate) fn record_local_plugin_at(
    lock_path: &Path,
    manifest: &crate::capability::plugin::PluginManifest,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    upsert_at(
        lock_path,
        ExtensionLockEntry {
            asset_kind: "plugin".to_string(),
            asset_id: manifest.id.clone(),
            version: manifest.version.clone(),
            source_id: source.to_string(),
            source: source.to_string(),
            repository: String::new(),
            reference: String::new(),
            catalog_path: String::new(),
            source_commit: String::new(),
            artifact_url: String::new(),
            sha256: String::new(),
            dependencies: manifest
                .plugin_dependencies
                .iter()
                .map(|dependency| ExtensionLockDependency {
                    asset_kind: "plugin".to_string(),
                    asset_id: dependency.plugin_id.clone(),
                    min_version: dependency.min_version.clone(),
                })
                .collect(),
            agent_profile: paths::profile_name(),
            updated_at: now_stamp(),
        },
    )
}

pub(crate) fn source_commit(reference: &str) -> String {
    let value = reference.trim();
    if value.len() >= 7 && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        value.to_ascii_lowercase()
    } else {
        String::new()
    }
}

pub(crate) fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallTransaction {
    asset_kind: String,
    asset_id: String,
    version: String,
    target_root: String,
    stage: String,
    created_at: String,
}

/// Recover abandoned staging directories and restore a pointer if a process
/// terminated after moving current to previous.
pub(crate) fn recover() -> Result<usize, Box<dyn Error>> {
    recover_at(&paths::agent_home().join("data/extension-transactions"))
}

fn recover_at(root: &Path) -> Result<usize, Box<dyn Error>> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut recovered = 0;
    for entry in fs::read_dir(&root)?.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<InstallTransaction>(&fs::read(entry.path())?)
        else {
            continue;
        };
        let target = PathBuf::from(&record.target_root);
        if record.asset_kind.eq_ignore_ascii_case("skill") {
            let current = target.join("current.json");
            let previous = target.join("previous.json");
            if !current.exists() && previous.exists() {
                let _ = fs::rename(previous, current);
            }
        } else if !target.join("current").exists() && target.join("previous").exists() {
            let _ = fs::rename(target.join("previous"), target.join("current"));
        }
        if target.is_dir() {
            for child in fs::read_dir(&target)?.flatten() {
                let name = child.file_name().to_string_lossy().to_string();
                if name.starts_with("staging-")
                    || name.starts_with("current-")
                    || name.starts_with("swap-")
                    || name.starts_with("restore-")
                {
                    let _ = if child.path().is_dir() {
                        fs::remove_dir_all(child.path())
                    } else {
                        fs::remove_file(child.path())
                    };
                }
            }
        }
        let _ = fs::remove_file(entry.path());
        recovered += 1;
    }
    Ok(recovered)
}

pub(crate) struct InstallGuard {
    path: PathBuf,
    record: InstallTransaction,
}

impl InstallGuard {
    pub(crate) fn begin(
        asset_kind: &str,
        asset_id: &str,
        version: &str,
        target_root: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        Self::begin_at(
            &paths::agent_home().join("data/extension-transactions"),
            asset_kind,
            asset_id,
            version,
            target_root,
        )
    }

    pub(crate) fn begin_at(
        root: &Path,
        asset_kind: &str,
        asset_id: &str,
        version: &str,
        target_root: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(&root)?;
        let path = root.join(format!(
            "{}-{}-{}.json",
            asset_kind,
            sanitize(asset_id),
            now_stamp()
        ));
        let record = InstallTransaction {
            asset_kind: asset_kind.to_string(),
            asset_id: asset_id.to_string(),
            version: version.to_string(),
            target_root: target_root.to_string_lossy().to_string(),
            stage: "prepared".to_string(),
            created_at: now_stamp(),
        };
        atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
        Ok(Self { path, record })
    }

    pub(crate) fn stage(&mut self, stage: &str) -> Result<(), Box<dyn Error>> {
        self.record.stage = stage.to_string();
        atomic_file::atomic_write(&self.path, &serde_json::to_vec_pretty(&self.record)?)?;
        Ok(())
    }

    pub(crate) fn commit(mut self) -> Result<(), Box<dyn Error>> {
        self.record.stage = "committed".to_string();
        atomic_file::atomic_write(&self.path, &serde_json::to_vec_pretty(&self.record)?)?;
        fs::remove_file(&self.path)?;
        Ok(())
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {}
}

fn sanitize(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                byte as char
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lock_entries_round_trip_and_restore() {
        let root = std::env::temp_dir().join(format!("himind-extension-lock-{}", now_stamp()));
        let lock_path = root.join("extension.lock.json");
        let entry = ExtensionLockEntry {
            asset_kind: "plugin".to_string(),
            asset_id: "com.example.tool".to_string(),
            version: "1.0.0".to_string(),
            source_id: "local".to_string(),
            source: "local".to_string(),
            repository: String::new(),
            reference: String::new(),
            catalog_path: String::new(),
            source_commit: String::new(),
            artifact_url: String::new(),
            sha256: "a".repeat(64),
            dependencies: Vec::new(),
            agent_profile: "test".to_string(),
            updated_at: now_stamp(),
        };
        upsert_at(&lock_path, entry.clone()).unwrap();
        assert_eq!(
            load_at(&lock_path).unwrap().entries.values().next(),
            Some(&entry)
        );
        let mut lock = load_at(&lock_path).unwrap();
        lock.entries.clear();
        save_at(&lock_path, &lock).unwrap();
        assert!(load_at(&lock_path).unwrap().entries.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_removes_abandoned_staging_and_restores_current() {
        let root = std::env::temp_dir().join(format!("himind-extension-recovery-{}", now_stamp()));
        let target = root.join("plugins").join("demo");
        fs::create_dir_all(target.join("previous")).unwrap();
        fs::create_dir_all(target.join("staging-abandoned")).unwrap();
        let tx = root.join("data/extension-transactions");
        fs::create_dir_all(&tx).unwrap();
        fs::write(
            tx.join("pending.json"),
            serde_json::to_vec(&InstallTransaction {
                asset_kind: "plugin".to_string(),
                asset_id: "demo".to_string(),
                version: "1.0.0".to_string(),
                target_root: target.to_string_lossy().to_string(),
                stage: "installed".to_string(),
                created_at: now_stamp(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(super::recover_at(&tx).unwrap(), 1);
        assert!(target.join("current").exists());
        assert!(!target.join("staging-abandoned").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_restores_skill_pointer_and_removes_staging() {
        let root = std::env::temp_dir().join(format!("himind-skill-recovery-{}", now_stamp()));
        let target = root.join("skills").join("managed").join("demo.skill");
        fs::create_dir_all(target.join("staging-abandoned")).unwrap();
        fs::write(target.join("previous.json"), br#"{"version":"1.0.0"}"#).unwrap();
        let tx = root.join("data/extension-transactions");
        fs::create_dir_all(&tx).unwrap();
        fs::write(
            tx.join("pending.json"),
            serde_json::to_vec(&InstallTransaction {
                asset_kind: "skill".to_string(),
                asset_id: "demo.skill".to_string(),
                version: "1.0.0".to_string(),
                target_root: target.to_string_lossy().to_string(),
                stage: "installed".to_string(),
                created_at: now_stamp(),
            })
            .unwrap(),
        )
        .unwrap();

        assert_eq!(super::recover_at(&tx).unwrap(), 1);
        assert!(target.join("current.json").exists());
        assert!(!target.join("previous.json").exists());
        assert!(!target.join("staging-abandoned").exists());
        let _ = fs::remove_dir_all(root);
    }
}
