use crate::skill::manifest::{
    load_skill_manifest, validate_skill_package_root, write_skill_package,
};
use crate::skill::resolver::compare_versions;
use crate::skill::types::{SkillManifest, SkillPointer, SkillRecord, SkillScope};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SkillManagementPolicy {
    #[serde(default = "default_skill_management")]
    pub management: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub assignment_id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default = "default_policy_true")]
    pub allow_uninstall: bool,
}

fn default_skill_management() -> String {
    "user_managed".to_string()
}

fn default_policy_true() -> bool {
    true
}

pub(crate) const SKILL_SYNC_MODE_COPY: &str = "copy";
pub(crate) const SKILL_SYNC_MODE_SYMLINK: &str = "symlink";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SkillSyncSettings {
    #[serde(default = "default_skill_sync_mode")]
    pub mode: String,
}

fn default_skill_sync_mode() -> String {
    SKILL_SYNC_MODE_COPY.to_string()
}

pub(crate) fn normalize_skill_sync_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        SKILL_SYNC_MODE_COPY => Some(SKILL_SYNC_MODE_COPY),
        SKILL_SYNC_MODE_SYMLINK => Some(SKILL_SYNC_MODE_SYMLINK),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct SkillSeed {
    manifest: SkillManifest,
    readme: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct SkillStore {
    root: PathBuf,
    extension_state_root: PathBuf,
}

impl SkillStore {
    pub(crate) fn new() -> Self {
        let agent_home = crate::store::paths::agent_home();
        Self {
            root: agent_home.join("skills"),
            extension_state_root: agent_home.join("data"),
        }
    }

    pub(crate) fn with_root(root: PathBuf) -> Self {
        let extension_state_root = root.join(".extension-state");
        Self {
            root,
            extension_state_root,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn sync_settings(&self) -> Result<SkillSyncSettings, Box<dyn Error>> {
        let path = self.root.join("sync-settings.json");
        if !path.exists() {
            return Ok(SkillSyncSettings {
                mode: default_skill_sync_mode(),
            });
        }
        let content = fs::read_to_string(path)?;
        let mut settings: SkillSyncSettings =
            serde_json::from_str(content.trim_start_matches('\u{feff}'))?;
        settings.mode = normalize_skill_sync_mode(&settings.mode)
            .unwrap_or(SKILL_SYNC_MODE_COPY)
            .to_string();
        Ok(settings)
    }

    pub(crate) fn sync_mode(&self) -> Result<String, Box<dyn Error>> {
        Ok(self.sync_settings()?.mode)
    }

    pub(crate) fn set_sync_mode(&self, mode: &str) -> Result<SkillSyncSettings, Box<dyn Error>> {
        let mode = normalize_skill_sync_mode(mode).ok_or_else(|| {
            format!("unsupported Skill sync mode: {mode}; expected copy or symlink")
        })?;
        fs::create_dir_all(&self.root)?;
        let settings = SkillSyncSettings {
            mode: mode.to_string(),
        };
        fs::write(
            self.root.join("sync-settings.json"),
            serde_json::to_vec_pretty(&settings)?,
        )?;
        Ok(settings)
    }

    pub(crate) fn bootstrap_builtin_skills(&self) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(self.root.join("builtin"))?;
        fs::create_dir_all(self.root.join("managed"))?;
        fs::create_dir_all(self.root.join("user"))?;
        fs::create_dir_all(self.root.join("rendered"))?;
        self.retire_removed_skills()?;
        for seed in builtin_skill_seeds() {
            self.ensure_seed(&seed)?;
        }
        Ok(())
    }

    fn retire_removed_skills(&self) -> Result<(), Box<dyn Error>> {
        for skill_id in retired_skill_ids() {
            for scope in ["builtin", "managed", "user"] {
                let skill_root = self.root.join(scope).join(skill_id);
                if skill_root.exists() {
                    fs::remove_dir_all(skill_root)?;
                }
            }
            let clients = std::iter::once("codex").chain(
                crate::skill::clients::DIRECTORY_CLIENTS
                    .iter()
                    .map(|definition| definition.id),
            );
            for client in clients {
                let rendered_root = self.root.join("rendered").join(client).join(skill_id);
                if rendered_root.exists() {
                    fs::remove_dir_all(rendered_root)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn list_records(&self) -> Result<Vec<SkillRecord>, Box<dyn Error>> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for scope_root in [
            self.root.join("builtin"),
            self.root.join("managed"),
            self.root.join("user"),
        ] {
            if !scope_root.exists() {
                continue;
            }
            for entry in fs::read_dir(scope_root)?.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if let Some(record) = self.read_skill_record(&path)? {
                    if seen.insert(record.manifest.id.clone()) {
                        items.push(record);
                    }
                }
            }
        }
        items.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        Ok(items)
    }

    pub(crate) fn get_record(&self, skill_id: &str) -> Result<Option<SkillRecord>, Box<dyn Error>> {
        for scope_root in [
            self.root.join("builtin"),
            self.root.join("managed"),
            self.root.join("user"),
        ] {
            let scope_skill_root = scope_root.join(skill_id);
            if !scope_skill_root.exists() {
                continue;
            }
            if let Some(record) = self.read_skill_record(&scope_skill_root)? {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub(crate) fn remove_organization_skill(&self, skill_id: &str) -> Result<bool, Box<dyn Error>> {
        if skill_id.is_empty()
            || !skill_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("Skill ID 无效".into());
        }
        let root = self.skill_root_for_scope(&SkillScope::Organization, skill_id);
        let existed = root.exists();
        if existed {
            fs::remove_dir_all(&root)?;
        }
        let _ = crate::app::extension_lock::remove_at(
            &crate::app::extension_lock::path_for_state_root(&self.extension_state_root),
            "skill",
            skill_id,
        );
        let clients = std::iter::once("codex").chain(
            crate::skill::clients::DIRECTORY_CLIENTS
                .iter()
                .map(|definition| definition.id),
        );
        for client in clients {
            let rendered = self.rendered_skill_root(client, skill_id);
            if rendered.exists() {
                fs::remove_dir_all(rendered)?;
            }
        }
        Ok(existed)
    }

    pub(crate) fn remove_installed_skill(&self, skill_id: &str) -> Result<bool, Box<dyn Error>> {
        let Some(record) = self.get_record(skill_id)? else {
            return Ok(false);
        };
        if record.manifest.scope == SkillScope::Builtin {
            return Err("系统内置技能不能卸载".into());
        }
        let root = self.skill_root_for_scope(&record.manifest.scope, skill_id);
        if !root.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(root)?;
        let _ = crate::app::extension_lock::remove_at(
            &crate::app::extension_lock::path_for_state_root(&self.extension_state_root),
            "skill",
            skill_id,
        );
        Ok(true)
    }

    pub(crate) fn apply_management_policy(
        &self,
        skill_id: &str,
        policy: &SkillManagementPolicy,
    ) -> Result<(), Box<dyn Error>> {
        let root = self.skill_root_for_scope(&SkillScope::Organization, skill_id);
        if !root.exists() {
            return Ok(());
        }
        fs::write(root.join("policy.json"), serde_json::to_vec_pretty(policy)?)?;
        Ok(())
    }

    pub(crate) fn management_policy(
        &self,
        skill_id: &str,
    ) -> Result<Option<SkillManagementPolicy>, Box<dyn Error>> {
        let path = self
            .skill_root_for_scope(&SkillScope::Organization, skill_id)
            .join("policy.json");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
    }

    pub(crate) fn rendered_skill_root(&self, client_id: &str, skill_id: &str) -> PathBuf {
        self.root.join("rendered").join(client_id).join(skill_id)
    }

    pub(crate) fn skill_version_dir(
        &self,
        scope: &SkillScope,
        skill_id: &str,
        version: &str,
    ) -> PathBuf {
        self.scope_root(scope)
            .join(skill_id)
            .join("versions")
            .join(version)
    }

    pub(crate) fn skill_root_for_scope(&self, scope: &SkillScope, skill_id: &str) -> PathBuf {
        self.scope_root(scope).join(skill_id)
    }

    pub(crate) fn install_organization_package(
        &self,
        package_root: &Path,
        expected_id: &str,
        expected_version: &str,
    ) -> Result<SkillRecord, Box<dyn Error>> {
        self.install_scoped_package(
            package_root,
            expected_id,
            expected_version,
            SkillScope::Organization,
            "商城 Skill 必须使用 organization scope",
        )
    }

    pub(crate) fn install_user_package(
        &self,
        package_root: &Path,
        expected_id: &str,
        expected_version: &str,
    ) -> Result<SkillRecord, Box<dyn Error>> {
        self.install_scoped_package(
            package_root,
            expected_id,
            expected_version,
            SkillScope::User,
            "本地 Skill 必须使用 user scope",
        )
    }

    fn install_scoped_package(
        &self,
        package_root: &Path,
        expected_id: &str,
        expected_version: &str,
        scope: SkillScope,
        scope_error: &str,
    ) -> Result<SkillRecord, Box<dyn Error>> {
        self.bootstrap_builtin_skills()?;
        let manifest = validate_skill_package_root(package_root)?;
        if manifest.scope != scope {
            return Err(scope_error.into());
        }
        if manifest.id != expected_id || manifest.version != expected_version {
            return Err("Skill Manifest ID 或版本与发布记录不一致".into());
        }
        let skill_root = self.skill_root_for_scope(&manifest.scope, expected_id);
        let versions_root = skill_root.join("versions");
        fs::create_dir_all(&versions_root)?;
        let mut transaction = crate::app::extension_lock::InstallGuard::begin_at(
            &self.extension_state_root.join("extension-transactions"),
            "skill",
            expected_id,
            expected_version,
            &skill_root,
        )?;
        let staging = skill_root.join(format!("staging-{}", now_stamp()));
        copy_package_tree(package_root, &staging)?;
        transaction.stage("staged")?;
        let version_root = versions_root.join(expected_version);
        if version_root.exists() {
            let existing = fs::read(version_root.join("checksums.sha256"))?;
            let incoming = fs::read(staging.join("checksums.sha256"))?;
            if existing != incoming {
                let _ = fs::remove_dir_all(&staging);
                return Err("同一 Skill 版本已存在且内容不同，请提升版本号".into());
            }
            fs::remove_dir_all(&staging)?;
        } else if let Err(error) = fs::rename(&staging, &version_root) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        let current_pointer = SkillPointer {
            version: manifest.version.clone(),
            path: format!("versions/{}", manifest.version),
            updated_at: now_stamp(),
        };
        let current_path = skill_root.join("current.json");
        if current_path.exists() {
            if let Ok(existing) = fs::read(&current_path) {
                fs::write(skill_root.join("previous.json"), existing)?;
            }
        }
        fs::write(&current_path, serde_json::to_vec_pretty(&current_pointer)?)?;
        transaction.stage("installed")?;
        let record: SkillRecord = self
            .read_skill_record(&skill_root)?
            .ok_or_else(|| -> Box<dyn Error> { "Skill 安装后无法从 Store 读取".into() })?;
        crate::app::extension_lock::record_local_skill_at(
            &crate::app::extension_lock::path_for_state_root(&self.extension_state_root),
            &record.manifest,
            "agent_store",
        )?;
        transaction.stage("lock_committed")?;
        transaction.commit()?;
        Ok(record)
    }

    fn scope_root(&self, scope: &SkillScope) -> PathBuf {
        match scope {
            SkillScope::Builtin => self.root.join("builtin"),
            SkillScope::Organization => self.root.join("managed"),
            SkillScope::User => self.root.join("user"),
        }
    }

    fn ensure_seed(&self, seed: &SkillSeed) -> Result<(), Box<dyn Error>> {
        let skill_root = self.skill_root_for_scope(&seed.manifest.scope, &seed.manifest.id);
        let current = skill_root.join("current.json");
        if current.exists() {
            let existing = self.read_skill_record(&skill_root)?;
            if let Some(record) = existing {
                if record.manifest.version == seed.manifest.version {
                    return Ok(());
                }
            }
        }
        self.write_versioned_skill(&seed.manifest, seed.readme)
    }

    fn write_versioned_skill(
        &self,
        manifest: &SkillManifest,
        readme: &str,
    ) -> Result<(), Box<dyn Error>> {
        let skill_root = self.skill_root_for_scope(&manifest.scope, &manifest.id);
        let version_dir = skill_root.join("versions").join(&manifest.version);
        fs::create_dir_all(&version_dir)?;
        write_skill_package(&version_dir, manifest, readme)?;

        let current_pointer = SkillPointer {
            version: manifest.version.clone(),
            path: format!("versions/{}", manifest.version),
            updated_at: now_stamp(),
        };
        let current_path = skill_root.join("current.json");
        if current_path.exists() {
            let previous_path = skill_root.join("previous.json");
            if let Ok(existing) = fs::read_to_string(&current_path) {
                let _ = fs::write(&previous_path, existing);
            }
        }
        fs::create_dir_all(&skill_root)?;
        fs::write(&current_path, serde_json::to_vec_pretty(&current_pointer)?)?;
        Ok(())
    }

    fn read_skill_record(&self, skill_root: &Path) -> Result<Option<SkillRecord>, Box<dyn Error>> {
        let current_path = skill_root.join("current.json");
        let previous_path = skill_root.join("previous.json");
        let current_pointer = read_pointer(&current_path)?;
        let previous_pointer = read_pointer(&previous_path)?;
        let version_root = if let Some(pointer) = current_pointer.clone() {
            skill_root.join(pointer.path)
        } else if let Some(latest) = latest_version_dir(skill_root)? {
            latest
        } else {
            return Ok(None);
        };
        if !version_root.exists() {
            return Ok(None);
        }
        let manifest = load_skill_manifest(&version_root)?;
        let previous_version = previous_pointer.map(|pointer| pointer.version);
        Ok(Some(SkillRecord {
            manifest,
            root: skill_root.to_path_buf(),
            version_root,
            current: current_pointer.is_some(),
            previous_version,
        }))
    }
}

fn copy_package_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn read_pointer(path: &Path) -> Result<Option<SkillPointer>, Box<dyn Error>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let pointer: SkillPointer = serde_json::from_str(content.trim_start_matches('\u{feff}'))?;
    Ok(Some(pointer))
}

fn latest_version_dir(skill_root: &Path) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let versions_root = skill_root.join("versions");
    if !versions_root.exists() {
        return Ok(None);
    }
    let mut latest: Option<(String, PathBuf)> = None;
    for entry in fs::read_dir(versions_root)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(version_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if latest
            .as_ref()
            .map(|(current, _)| {
                compare_versions(version_name, current) == std::cmp::Ordering::Greater
            })
            .unwrap_or(true)
        {
            latest = Some((version_name.to_string(), path));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn now_stamp() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| format!("{}-{}", value.as_millis(), sequence))
        .unwrap_or_else(|_| format!("0-{}", sequence))
}

fn builtin_skill_seeds() -> Vec<SkillSeed> {
    Vec::new()
}

pub(crate) fn retired_skill_ids() -> &'static [&'static str] {
    &[
        "com.himind.skill.environment-doctor",
        "com.himind.skill.image-delivery-preflight",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-skill-store-test-{}-{}",
            std::process::id(),
            now_stamp()
        ))
    }

    #[test]
    fn retires_removed_builtin_skill_seed() {
        let root = test_store_root();
        let store = SkillStore::with_root(root.clone());
        let retired_builtin = root
            .join("builtin")
            .join("com.himind.skill.environment-doctor");
        let retired_managed = root
            .join("managed")
            .join("com.himind.skill.image-delivery-preflight");
        fs::create_dir_all(&retired_builtin).unwrap();
        fs::create_dir_all(&retired_managed).unwrap();
        fs::write(retired_builtin.join("legacy.txt"), "retired").unwrap();
        fs::write(retired_managed.join("legacy.txt"), "retired").unwrap();
        store.bootstrap_builtin_skills().unwrap();
        let records = store.list_records().unwrap();
        assert!(records.is_empty());
        assert!(!retired_builtin.exists());
        assert!(!retired_managed.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sync_mode_defaults_to_copy_and_persists_supported_values() {
        let root = test_store_root();
        let store = SkillStore::with_root(root.clone());
        assert_eq!(store.sync_mode().unwrap(), SKILL_SYNC_MODE_COPY);
        assert_eq!(
            store.set_sync_mode(SKILL_SYNC_MODE_SYMLINK).unwrap().mode,
            SKILL_SYNC_MODE_SYMLINK
        );
        assert_eq!(store.sync_mode().unwrap(), SKILL_SYNC_MODE_SYMLINK);
        assert!(store.set_sync_mode("junction").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prefers_current_pointer_over_latest_version() {
        let root = test_store_root();
        let store = SkillStore::with_root(root.clone());
        let skill_root = store.skill_root_for_scope(&SkillScope::Builtin, "demo.skill");
        let current_version = "2.0.0";
        let previous_version = "1.0.0";
        let current_manifest = SkillManifest {
            id: "demo.skill".to_string(),
            name: "Demo".to_string(),
            author: String::new(),
            categories: vec![],
            version: current_version.to_string(),
            scope: SkillScope::Builtin,
            description: String::new(),
            release_notes: "测试版本指针。".to_string(),
            min_agent_version: "0.2.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        let previous_manifest = SkillManifest {
            version: previous_version.to_string(),
            ..current_manifest.clone()
        };
        write_skill_package(
            &skill_root.join("versions").join(current_version),
            &current_manifest,
            "# Demo",
        )
        .unwrap();
        write_skill_package(
            &skill_root.join("versions").join(previous_version),
            &previous_manifest,
            "# Demo",
        )
        .unwrap();
        fs::create_dir_all(&skill_root).unwrap();
        fs::write(
            skill_root.join("current.json"),
            serde_json::to_vec_pretty(&SkillPointer {
                version: current_version.to_string(),
                path: format!("versions/{current_version}"),
                updated_at: now_stamp(),
            })
            .unwrap(),
        )
        .unwrap();
        let record = store.read_skill_record(&skill_root).unwrap().unwrap();
        assert_eq!(record.manifest.version, current_version);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_different_content_for_existing_organization_version() {
        let root = test_store_root();
        let store = SkillStore::with_root(root.clone());
        let package = root.join("package");
        let manifest = SkillManifest {
            id: "com.himind.skill.immutable-test".to_string(),
            name: "Immutable Test".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::Organization,
            description: String::new(),
            release_notes: "测试不可变版本。".to_string(),
            min_agent_version: crate::VERSION.to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        write_skill_package(&package, &manifest, "# First").unwrap();
        fs::write(package.join("checksums.sha256"), "first package checksums").unwrap();
        store
            .install_organization_package(&package, &manifest.id, &manifest.version)
            .unwrap();
        write_skill_package(&package, &manifest, "# Changed").unwrap();
        fs::write(
            package.join("checksums.sha256"),
            "changed package checksums",
        )
        .unwrap();
        let error = store
            .install_organization_package(&package, &manifest.id, &manifest.version)
            .unwrap_err();
        assert!(error.to_string().contains("内容不同"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removes_installed_skill_from_the_agent_store() {
        let root = test_store_root();
        let store = SkillStore::with_root(root.clone());
        let package = root.join("package");
        let manifest = SkillManifest {
            id: "com.himind.skill.remove-test".to_string(),
            name: "卸载测试".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::User,
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: String::new(),
            supported_clients: vec!["himind-ai".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        write_skill_package(&package, &manifest, "# Remove test").unwrap();
        store
            .install_user_package(&package, &manifest.id, &manifest.version)
            .unwrap();

        assert!(store.get_record(&manifest.id).unwrap().is_some());
        assert!(store.remove_installed_skill(&manifest.id).unwrap());
        assert!(store.get_record(&manifest.id).unwrap().is_none());
        assert!(!store.remove_installed_skill(&manifest.id).unwrap());

        let _ = fs::remove_dir_all(root);
    }
}
