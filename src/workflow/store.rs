use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{load_from_directory, WorkflowPackage};
use crate::store::atomic_file;

const INSTALLATION_FILE: &str = "installation.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WorkflowInstallation {
    package_id: String,
    current_version: String,
    #[serde(default)]
    previous_version: String,
    enabled: bool,
    package_digest: String,
    #[serde(default)]
    source: String,
    installed_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct InstalledWorkflow {
    pub package: WorkflowPackage,
    pub enabled: bool,
    pub previous_version: String,
    pub package_digest: String,
    pub source: String,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowStore {
    root: PathBuf,
}

impl WorkflowStore {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        Ok(Self::new(
            crate::store::paths::agent_home().join("workflows"),
        ))
    }

    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) fn install_from_directory(
        &self,
        source: &Path,
    ) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let source = source.canonicalize()?;
        let package = load_from_directory(&source)?;
        let product_root = self.product_root(&package.id)?;
        fs::create_dir_all(product_root.join("versions"))?;
        let _lock = atomic_file::lock(&product_root.join(INSTALLATION_FILE))?;

        let version_root = product_root
            .join("versions")
            .join(safe_segment(&package.version)?);
        let digest = package_digest(&source)?;
        if version_root.exists() {
            let existing_digest = package_digest(&version_root)?;
            if existing_digest != digest {
                return Err("installed workflow version content is immutable".into());
            }
        } else {
            let staging = product_root.join(format!(
                ".staging-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            if staging.exists() {
                fs::remove_dir_all(&staging)?;
            }
            copy_package(&source, &staging)?;
            if let Some(parent) = version_root.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&staging, &version_root)?;
        }

        let now = timestamp();
        let previous = self.load_installation(&package.id)?;
        let installation = WorkflowInstallation {
            package_id: package.id.clone(),
            current_version: package.version.clone(),
            previous_version: previous
                .as_ref()
                .map(|item| item.current_version.clone())
                .unwrap_or_default(),
            enabled: previous.as_ref().map(|item| item.enabled).unwrap_or(true),
            package_digest: digest,
            source: source.to_string_lossy().to_string(),
            installed_at: previous
                .as_ref()
                .map(|item| item.installed_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        };
        self.save_installation(&installation)?;
        self.installed_from(&installation)
    }

    pub(crate) fn list(&self) -> Result<Vec<InstalledWorkflow>, Box<dyn Error>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let package_id = entry.file_name().to_string_lossy().to_string();
            if let Some(installation) = self.load_installation(&package_id)? {
                items.push(self.installed_from(&installation)?);
            }
        }
        items.sort_by(|left, right| left.package.id.cmp(&right.package.id));
        Ok(items)
    }

    pub(crate) fn set_enabled(
        &self,
        package_id: &str,
        enabled: bool,
    ) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let _lock = atomic_file::lock(&self.product_root(package_id)?.join(INSTALLATION_FILE))?;
        let mut installation = self
            .load_installation(package_id)?
            .ok_or("workflow package is not installed")?;
        if installation.enabled == enabled {
            return self.installed_from(&installation);
        }
        installation.enabled = enabled;
        installation.updated_at = timestamp();
        self.save_installation(&installation)?;
        self.installed_from(&installation)
    }

    pub(crate) fn rollback(&self, package_id: &str) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let _lock = atomic_file::lock(&self.product_root(package_id)?.join(INSTALLATION_FILE))?;
        let mut installation = self
            .load_installation(package_id)?
            .ok_or("workflow package is not installed")?;
        if installation.previous_version.trim().is_empty() {
            return Err("workflow package has no previous version to restore".into());
        }
        let current = installation.current_version.clone();
        installation.current_version = installation.previous_version.clone();
        installation.previous_version = current;
        installation.updated_at = timestamp();
        self.save_installation(&installation)?;
        self.installed_from(&installation)
    }

    pub(crate) fn remove(&self, package_id: &str) -> Result<bool, Box<dyn Error>> {
        let root = self.product_root(package_id)?;
        if !root.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(root)?;
        Ok(true)
    }

    fn installed_from(
        &self,
        installation: &WorkflowInstallation,
    ) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let version_root = self
            .product_root(&installation.package_id)?
            .join("versions")
            .join(safe_segment(&installation.current_version)?);
        let package = load_from_directory(&version_root)?;
        Ok(InstalledWorkflow {
            package,
            enabled: installation.enabled,
            previous_version: installation.previous_version.clone(),
            package_digest: installation.package_digest.clone(),
            source: installation.source.clone(),
            installed_at: installation.installed_at.clone(),
            updated_at: installation.updated_at.clone(),
        })
    }

    fn product_root(&self, package_id: &str) -> Result<PathBuf, Box<dyn Error>> {
        Ok(self.root.join(safe_segment(package_id)?))
    }

    fn load_installation(
        &self,
        package_id: &str,
    ) -> Result<Option<WorkflowInstallation>, Box<dyn Error>> {
        let path = self.product_root(package_id)?.join(INSTALLATION_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
    }

    fn save_installation(&self, installation: &WorkflowInstallation) -> Result<(), Box<dyn Error>> {
        atomic_file::atomic_write(
            &self
                .product_root(&installation.package_id)?
                .join(INSTALLATION_FILE),
            &serde_json::to_vec_pretty(installation)?,
        )?;
        Ok(())
    }
}

fn copy_package(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "workflow package symlinks are not allowed: {}",
                source_path.display()
            )
            .into());
        }
        if metadata.is_dir() {
            copy_package(&source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn package_digest(root: &Path) -> Result<String, Box<dyn Error>> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    for relative in files {
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        digest.update(fs::read(root.join(&relative))?);
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "workflow package symlinks are not allowed: {}",
                entry.path().display()
            )
            .into());
        }
        if metadata.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(entry.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

fn safe_segment(value: &str) -> Result<String, Box<dyn Error>> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid workflow path segment: {value}").into());
    }
    Ok(value.to_string())
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_package() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery")
    }

    fn store() -> WorkflowStore {
        WorkflowStore::new(std::env::temp_dir().join(format!(
            "himind-workflow-store-{}-{}",
            std::process::id(),
            unique_suffix()
        )))
    }

    #[test]
    fn install_list_disable_and_remove() {
        let store = store();
        let installed = store.install_from_directory(&source_package()).unwrap();
        assert_eq!(
            installed.package.id,
            "com.himind.workflow.wechat-miniprogram-delivery"
        );
        assert!(installed.enabled);
        assert_eq!(store.list().unwrap().len(), 1);

        let disabled = store.set_enabled(&installed.package.id, false).unwrap();
        assert!(!disabled.enabled);
        assert!(store.remove(&installed.package.id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn repeated_install_is_idempotent() {
        let store = store();
        let first = store.install_from_directory(&source_package()).unwrap();
        let second = store.install_from_directory(&source_package()).unwrap();
        assert_eq!(first.package_digest, second.package_digest);
        assert_eq!(first.package.version, second.package.version);
    }

    #[test]
    fn changed_content_for_same_version_is_rejected() {
        let store = store();
        let source = source_package();
        let installed = store.install_from_directory(&source).unwrap();
        let version_root = store
            .product_root(&installed.package.id)
            .unwrap()
            .join("versions")
            .join(&installed.package.version);
        let mut readme = fs::read_to_string(version_root.join("README.md")).unwrap();
        readme.push_str("\nchanged\n");
        fs::write(version_root.join("README.md"), readme).unwrap();
        assert!(store.install_from_directory(&source).is_err());
    }
}
