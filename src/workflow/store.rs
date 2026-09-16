use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
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
        self.install_from_directory_with_policy(source, false)
    }

    pub(crate) fn install_from_directory_with_policy(
        &self,
        source: &Path,
        require_signature: bool,
    ) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let source = source.canonicalize()?;
        validate_package_integrity(&source, require_signature)?;
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
            if let Err(error) = validate_package_integrity(&staging, require_signature) {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            let staged_digest = match package_digest(&staging) {
                Ok(value) => value,
                Err(error) => {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(error);
                }
            };
            if staged_digest != digest {
                let _ = fs::remove_dir_all(&staging);
                return Err("workflow package changed while it was being installed".into());
            }
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

    pub(crate) fn view_json(
        &self,
        package: &WorkflowPackage,
    ) -> Result<Option<Value>, Box<dyn Error>> {
        if package.ui.entry.trim().is_empty() {
            return Ok(None);
        }
        let path = self
            .product_root(&package.id)?
            .join("versions")
            .join(safe_segment(&package.version)?)
            .join(&package.ui.entry);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
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
        validate_package_integrity(&version_root, false)?;
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowSignatureMetadata {
    algorithm: String,
    key_id: String,
    signature: String,
}

fn validate_package_integrity(root: &Path, require_signature: bool) -> Result<(), Box<dyn Error>> {
    let checksums_path = root.join("checksums.sha256");
    let signature_path = root.join("manifest.sig");
    if !checksums_path.is_file() {
        if require_signature || signature_path.is_file() {
            return Err("workflow package checksums.sha256 is required".into());
        }
        return Ok(());
    }
    let checksums: HashMap<String, String> =
        crate::skill::manifest::parse_checksums(&fs::read_to_string(&checksums_path)?)?;
    let actual_files = package_files_without_signature_metadata(root)?;
    for relative in &actual_files {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let expected = checksums.get(&normalized).ok_or_else(|| {
            format!("workflow package file is not covered by checksums.sha256: {normalized}")
        })?;
        let actual = sha256_file(&root.join(relative))?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("workflow package checksum mismatch: {normalized}").into());
        }
    }
    for relative in checksums.keys() {
        if !actual_files
            .iter()
            .any(|path| path.to_string_lossy().replace('\\', "/") == *relative)
        {
            return Err(
                format!("workflow checksums.sha256 references missing file: {relative}").into(),
            );
        }
    }
    if signature_path.is_file() {
        let metadata: WorkflowSignatureMetadata =
            serde_json::from_slice(&fs::read(&signature_path)?)?;
        crate::app::system::verify_extension_artifact_signature(
            &checksums_path,
            &metadata.signature,
            &metadata.key_id,
            &metadata.algorithm,
            true,
        )?;
    } else if require_signature {
        return Err("workflow package manifest.sig is required".into());
    }
    Ok(())
}

fn package_files_without_signature_metadata(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?.to_path_buf();
        if relative == Path::new("checksums.sha256") || relative == Path::new("manifest.sig") {
            continue;
        }
        files.push(relative);
    }
    files.sort();
    Ok(files)
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
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
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use rand::rngs::OsRng;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{Pss, RsaPrivateKey, RsaPublicKey};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static SIGNING_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

    fn package_copy() -> PathBuf {
        let target = std::env::temp_dir().join(format!(
            "himind-workflow-package-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        copy_package(&source_package(), &target).unwrap();
        target
    }

    fn write_checksums(root: &Path) {
        let mut rows = Vec::new();
        for relative in package_files_without_signature_metadata(root).unwrap() {
            let digest = sha256_file(&root.join(&relative)).unwrap();
            rows.push(format!(
                "{digest}  {}\n",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
        rows.sort();
        fs::write(root.join("checksums.sha256"), rows.concat()).unwrap();
    }

    fn signing_env_lock() -> MutexGuard<'static, ()> {
        SIGNING_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
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

    #[test]
    fn require_signature_rejects_package_without_checksums() {
        let package = package_copy();
        let error = validate_package_integrity(&package, true).unwrap_err();
        assert!(error.to_string().contains("checksums.sha256"));
        let _ = fs::remove_dir_all(package);
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let package = package_copy();
        write_checksums(&package);
        let mut readme = fs::read_to_string(package.join("README.md")).unwrap();
        readme.push_str("\ntampered\n");
        fs::write(package.join("README.md"), readme).unwrap();
        let error = validate_package_integrity(&package, false).unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        let _ = fs::remove_dir_all(package);
    }

    #[test]
    fn require_signature_rejects_package_without_signature() {
        let package = package_copy();
        write_checksums(&package);
        let error = validate_package_integrity(&package, true).unwrap_err();
        assert!(error.to_string().contains("manifest.sig"));
        let _ = fs::remove_dir_all(package);
    }

    #[test]
    fn malformed_signature_metadata_is_rejected() {
        let package = package_copy();
        write_checksums(&package);
        fs::write(package.join("manifest.sig"), b"{not-json").unwrap();
        assert!(validate_package_integrity(&package, true).is_err());
        let _ = fs::remove_dir_all(package);
    }

    #[test]
    fn valid_signature_allows_required_install() {
        let _guard = signing_env_lock();
        let package = package_copy();
        write_checksums(&package);

        let key_id = "workflow-store-test-key";
        let trusted_root = std::env::temp_dir().join(format!(
            "himind-workflow-trusted-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&trusted_root).unwrap();
        let mut rng = OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        fs::write(
            trusted_root.join(format!("{key_id}.pem")),
            public_key.to_public_key_pem(LineEnding::LF).unwrap(),
        )
        .unwrap();
        let digest = Sha256::digest(fs::read(package.join("checksums.sha256")).unwrap());
        let signature = private_key
            .sign_with_rng(&mut rng, Pss::new::<Sha256>(), &digest)
            .unwrap();
        fs::write(
            package.join("manifest.sig"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "algorithm": "rsa-pss-sha256",
                "key_id": key_id,
                "signature": BASE64_STANDARD.encode(signature),
            }))
            .unwrap(),
        )
        .unwrap();

        let previous = std::env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR");
        std::env::set_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR", &trusted_root);
        let result = store().install_from_directory_with_policy(&package, true);
        match previous {
            Some(value) => std::env::set_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR", value),
            None => std::env::remove_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR"),
        }

        assert!(result.is_ok());
        let _ = fs::remove_dir_all(package);
        let _ = fs::remove_dir_all(trusted_root);
    }
}
