use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{load_from_directory, WorkflowPackage};
use crate::agent_core_contracts::InteractionEnvelope;
use crate::extension_contracts::{ExtensionAssetKind, ExtensionLock};
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
    #[serde(default)]
    artifact_sha256: String,
    #[serde(default)]
    extension_lock: Option<ExtensionLock>,
    #[serde(default)]
    lock_required: bool,
    #[serde(default)]
    versions: BTreeMap<String, WorkflowVersionMetadata>,
    installed_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WorkflowVersionMetadata {
    #[serde(default)]
    artifact_sha256: String,
    #[serde(default)]
    extension_lock: Option<ExtensionLock>,
    #[serde(default)]
    lock_required: bool,
}

impl WorkflowInstallation {
    fn metadata_for_version(&self, version: &str) -> WorkflowVersionMetadata {
        if let Some(metadata) = self.versions.get(version) {
            return metadata.clone();
        }
        if version == self.current_version {
            return WorkflowVersionMetadata {
                artifact_sha256: self.artifact_sha256.clone(),
                extension_lock: self.extension_lock.clone(),
                lock_required: self.lock_required,
            };
        }
        WorkflowVersionMetadata::default()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct InstalledWorkflow {
    pub package: WorkflowPackage,
    pub enabled: bool,
    pub previous_version: String,
    pub package_digest: String,
    pub source: String,
    pub artifact_sha256: String,
    pub extension_lock: Option<ExtensionLock>,
    pub lock_required: bool,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowStore {
    root: PathBuf,
    extension_state_root: PathBuf,
}

/// 单个已安装工作流在读取阶段暴露的校验问题。
///
/// 一个坏包不应该让整份已安装列表失败：市场与「我的能力」需要把它作为
/// 「校验失败」的条目呈现出来，并给出移除入口，否则用户被卡在没有出口的状态里。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkflowLoadIssue {
    pub package_id: String,
    pub version: String,
    pub message: String,
}

impl WorkflowStore {
    pub(crate) fn open_default() -> Result<Self, Box<dyn Error>> {
        let agent_home = crate::store::paths::agent_home();
        let store = Self {
            root: agent_home.join("workflows"),
            extension_state_root: agent_home.join("data"),
        };
        // 打开默认仓库时顺手收尾已经下线的工作流。退役只是清理，失败也没有
        // 理由让整份已安装列表打不开，所以这里不把错误抛回调用方。
        store.retire_removed_workflows();
        Ok(store)
    }

    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            extension_state_root: root.join(".extension-state"),
            root,
        }
    }

    /// 退役已下线的工作流：源码目录、安装台账与来源记录一起清干净。
    ///
    /// 工作流从扩展源下架或稳定 ID 改名之后，盘上那一份永远等不到更新：
    /// 目录扫描仍会把它列进「已安装」，台账与来源记录还会让它被自动更新和依赖
    /// 解析反复认领，用户看到的就是一条既装不上也更新不了的分叉记录。
    /// 退役 id 必须显式登记（见 [`retired_workflow_ids`]），历史 id 不会自己消失。
    fn retire_removed_workflows(&self) {
        let mut retired = Vec::new();
        for package_id in retired_workflow_ids() {
            let removed_dir = match self.product_root(package_id) {
                Ok(root) if root.exists() => fs::remove_dir_all(&root).is_ok(),
                _ => false,
            };
            // 台账是「已安装」列表与依赖校验的输入，来源记录是自动更新的输入；
            // 只删目录，这两处会长期留着界面里查不到的幽灵行。
            // 只有「台账里原本有这条」才算退役成功。早期这里取的是
            // `remove_at(..).is_ok()`，而该调用在没有条目时同样返回 Ok，
            // 于是每次都判为清掉了一行：退役日志会随调用频率反复打印
            // （工作流审批桥每秒开一次仓库，日志就是每秒一条）。
            let removed_ledger = matches!(
                crate::app::extension_lock::remove_at(
                    &crate::app::extension_lock::path_for_state_root(&self.extension_state_root),
                    "workflow",
                    package_id,
                ),
                Ok(true)
            );
            let provenance_path = self.extension_state_root.clone();
            crate::app::extension_source::remove_provenance_at(
                &provenance_path,
                "workflow",
                package_id,
            );
            if removed_dir || removed_ledger {
                retired.push(*package_id);
            }
        }
        if !retired.is_empty() {
            crate::approval::manager::ApprovalManager::global().add_log(
                "info",
                &format!(
                    "已退役 {} 个下线工作流：{}",
                    retired.len(),
                    retired.join("、")
                ),
            );
        }
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
        self.install_from_directory_with_metadata(source, require_signature, "", None, false)
    }

    pub(crate) fn install_from_directory_with_metadata(
        &self,
        source: &Path,
        require_signature: bool,
        artifact_sha256: &str,
        extension_lock: Option<ExtensionLock>,
        lock_required: bool,
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
        let suffix = format!("{}-{}", std::process::id(), unique_suffix());
        let staging = product_root.join(format!(".staging-{suffix}"));
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
        if version_root.exists() {
            if package_digest(&version_root)? != digest {
                let _ = fs::remove_dir_all(&staging);
                return Err("installed workflow version content is immutable".into());
            }
            // 内容一致时仍然用刚校验过的制品替换现有目录：打包元数据（checksums.sha256 /
            // manifest.sig）不参与内容摘要，只有真的替换，重装同一个版本才能修掉
            // 「目录安装带进来的旧签名」——否则重装等于什么都没做。
            let retired = product_root.join(format!(".retired-{suffix}"));
            fs::rename(&version_root, &retired)?;
            if let Err(error) = fs::rename(&staging, &version_root) {
                let _ = fs::rename(&retired, &version_root);
                let _ = fs::remove_dir_all(&staging);
                return Err(error.into());
            }
            let _ = fs::remove_dir_all(&retired);
        } else {
            if let Some(parent) = version_root.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&staging, &version_root)?;
        }

        let now = timestamp();
        let previous = self.load_installation(&package.id)?;
        let previous_metadata = previous
            .as_ref()
            .map(|item| item.metadata_for_version(&package.version))
            .unwrap_or_default();
        let extension_lock = extension_lock.or(previous_metadata.extension_lock);
        let artifact_sha256 = if artifact_sha256.trim().is_empty() {
            previous_metadata.artifact_sha256
        } else {
            artifact_sha256.trim().to_string()
        };
        let lock_required = lock_required || previous_metadata.lock_required;
        let mut versions = previous
            .as_ref()
            .map(|item| item.versions.clone())
            .unwrap_or_default();
        let version_metadata = WorkflowVersionMetadata {
            artifact_sha256: artifact_sha256.clone(),
            extension_lock: extension_lock.clone(),
            lock_required,
        };
        validate_workflow_extension_lock(&package, &version_metadata)?;
        versions.insert(package.version.clone(), version_metadata);
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
            artifact_sha256,
            extension_lock,
            lock_required,
            versions,
            installed_at: previous
                .as_ref()
                .map(|item| item.installed_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        };
        self.save_installation(&installation)?;
        self.installed_from(&installation)
    }

    /// 已安装列表，附带读取失败的条目。
    ///
    /// 只有目录级故障（读不到安装目录）才算致命；单个产品读取失败降级为一条
    /// issue 交给调用方展示，避免一个坏制品拖垮整份列表。
    pub(crate) fn list_with_issues(
        &self,
    ) -> Result<(Vec<InstalledWorkflow>, Vec<WorkflowLoadIssue>), Box<dyn Error>> {
        if !self.root.is_dir() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut items = Vec::new();
        let mut issues = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let package_id = entry.file_name().to_string_lossy().to_string();
            let installation = match self.load_installation(&package_id) {
                Ok(Some(installation)) => installation,
                Ok(None) => continue,
                Err(error) => {
                    issues.push(WorkflowLoadIssue {
                        package_id,
                        version: String::new(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            match self.installed_from(&installation) {
                Ok(item) => items.push(item),
                Err(error) => issues.push(WorkflowLoadIssue {
                    package_id: installation.package_id.clone(),
                    version: installation.current_version.clone(),
                    message: error.to_string(),
                }),
            }
        }
        items.sort_by(|left, right| left.package.id.cmp(&right.package.id));
        issues.sort_by(|left, right| left.package_id.cmp(&right.package_id));
        Ok((items, issues))
    }

    pub(crate) fn list(&self) -> Result<Vec<InstalledWorkflow>, Box<dyn Error>> {
        Ok(self.list_with_issues()?.0)
    }

    /// 目标包读取失败时的精确提示：区分「没装 / 停用」与「装了但校验不过」。
    fn missing_package_error(&self, package_id: &str) -> String {
        match self.list_with_issues() {
            Ok((_, issues)) => match issues
                .into_iter()
                .find(|issue| issue.package_id == package_id)
            {
                Some(issue) => format!(
                    "workflow package failed validation: {package_id}@{}: {}",
                    issue.version, issue.message
                ),
                None => format!("workflow package not found or disabled: {package_id}"),
            },
            Err(_) => format!("workflow package not found or disabled: {package_id}"),
        }
    }

    pub(crate) fn load_version(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<WorkflowPackage, Box<dyn Error>> {
        if version.trim().is_empty() {
            return Err("workflow package version is required".into());
        }
        let version_root = self
            .product_root(package_id)?
            .join("versions")
            .join(safe_segment(version)?);
        if !version_root.is_dir() {
            return Err(format!(
                "workflow package version is not installed: {package_id}@{version}"
            )
            .into());
        }
        validate_package_integrity(&version_root, false)?;
        load_from_directory(&version_root)
    }

    pub(crate) fn load_enabled_for_run(
        &self,
        package_id: &str,
    ) -> Result<InstalledWorkflow, Box<dyn Error>> {
        let package = self
            .list()?
            .into_iter()
            .find(|item| item.package.id == package_id && item.enabled)
            .ok_or_else(|| self.missing_package_error(package_id))?;
        let installation = self
            .load_installation(package_id)?
            .ok_or("workflow package installation metadata is missing")?;
        let metadata = installation.metadata_for_version(&package.package.version);
        validate_workflow_extension_lock(&package.package, &metadata)?;
        Ok(self.installed_from(&installation)?)
    }

    pub(crate) fn load_for_run_interaction(
        &self,
        interaction: &InteractionEnvelope,
    ) -> Result<WorkflowPackage, Box<dyn Error>> {
        let workflow = interaction
            .business_context
            .get("workflow")
            .ok_or("workflow run does not contain a package reference")?;
        let package_id = workflow
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("workflow run does not contain a package id")?;
        let version = workflow
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let expected_digest = workflow
            .get("package_digest")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let package = if !version.trim().is_empty() {
            self.load_version(package_id, version)?
        } else {
            self.list()?
                .into_iter()
                .find(|item| item.package.id == package_id && item.enabled)
                .map(|item| item.package)
                .ok_or_else(|| self.missing_package_error(package_id))?
        };
        if !expected_digest.trim().is_empty() {
            let actual_digest = package_digest(&package.source_root)?;
            if !actual_digest.eq_ignore_ascii_case(expected_digest) {
                return Err(format!(
                    "workflow package content changed after the run started: {package_id}@{version}"
                )
                .into());
            }
        }
        let installation = self
            .load_installation(package_id)?
            .ok_or("workflow package installation metadata is missing")?;
        let metadata = installation.metadata_for_version(&package.version);
        validate_workflow_extension_lock(&package, &metadata)?;
        Ok(package)
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
        // 先校验目标版本能不能读出来再落盘：回滚到一个坏版本时，安装元数据必须保持
        // 原样，否则本来能用的工作流会被一次失败的回滚一起带走。
        let restored = self.installed_from(&installation)?;
        self.save_installation(&installation)?;
        Ok(restored)
    }

    pub(crate) fn remove(&self, package_id: &str) -> Result<bool, Box<dyn Error>> {
        let root = self.product_root(package_id)?;
        if !root.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(root)?;
        // 来源记录跟着资产走：留着会让自动更新把已经卸掉的工作流当成待更新项。
        // 走本仓库自己的状态根，测试或非默认 profile 不会删到别人的记录。
        crate::app::extension_source::remove_provenance_at(
            &self.extension_state_root,
            "workflow",
            package_id,
        );
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
        let metadata = installation.metadata_for_version(&installation.current_version);
        Ok(InstalledWorkflow {
            package,
            enabled: installation.enabled,
            previous_version: installation.previous_version.clone(),
            package_digest: installation.package_digest.clone(),
            source: installation.source.clone(),
            artifact_sha256: metadata.artifact_sha256,
            extension_lock: metadata.extension_lock,
            lock_required: metadata.lock_required,
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
        let mut installation: WorkflowInstallation = serde_json::from_slice(&fs::read(path)?)?;
        if !installation.current_version.trim().is_empty()
            && (!installation.artifact_sha256.trim().is_empty()
                || installation.extension_lock.is_some()
                || installation.lock_required)
            && !installation
                .versions
                .contains_key(&installation.current_version)
        {
            installation.versions.insert(
                installation.current_version.clone(),
                WorkflowVersionMetadata {
                    artifact_sha256: installation.artifact_sha256.clone(),
                    extension_lock: installation.extension_lock.clone(),
                    lock_required: installation.lock_required,
                },
            );
        }
        Ok(Some(installation))
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

fn validate_workflow_extension_lock(
    package: &WorkflowPackage,
    metadata: &WorkflowVersionMetadata,
) -> Result<(), Box<dyn Error>> {
    let Some(lock) = metadata.extension_lock.as_ref() else {
        if metadata.lock_required {
            return Err(format!(
                "workflow release lock is required: {}@{}",
                package.id, package.version
            )
            .into());
        }
        return Ok(());
    };
    lock.validate()?;
    if lock.root.kind != ExtensionAssetKind::Workflow
        || lock.root.id != package.id
        || lock.root.version != package.version
    {
        return Err("workflow release lock identity does not match the installed package".into());
    }
    if metadata.artifact_sha256.trim().is_empty()
        || !lock
            .root
            .sha256
            .eq_ignore_ascii_case(&metadata.artifact_sha256)
    {
        return Err(
            "workflow release lock artifact SHA-256 does not match the installed package".into(),
        );
    }

    for dependency in lock
        .dependencies
        .iter()
        .filter(|dependency| dependency.required)
    {
        match dependency.kind {
            ExtensionAssetKind::Plugin => {
                let plugin =
                    crate::capability::plugin::find_plugin(&dependency.id)?.ok_or_else(|| {
                        format!("workflow lock requires missing Plugin {}", dependency.id)
                    })?;
                if !plugin.enabled || plugin.error.is_some() {
                    return Err(format!(
                        "workflow lock requires unavailable Plugin {}: {}",
                        dependency.id,
                        plugin.error.unwrap_or_else(|| "disabled".to_string())
                    )
                    .into());
                }
                if plugin.version != dependency.version {
                    return Err(format!(
                        "workflow lock requires Plugin {}@{}, installed {}",
                        dependency.id, dependency.version, plugin.version
                    )
                    .into());
                }
                let actual = package_payload_digest(
                    &crate::capability::plugin::plugin_content_dir(&plugin),
                )?;
                if !actual.eq_ignore_ascii_case(&dependency.sha256) {
                    return Err(
                        format!("workflow lock Plugin {} content changed", dependency.id).into(),
                    );
                }
            }
            ExtensionAssetKind::Skill => {
                let store = crate::skill::store::SkillStore::new();
                let version_root = [
                    crate::skill::types::SkillScope::Builtin,
                    crate::skill::types::SkillScope::Organization,
                    crate::skill::types::SkillScope::User,
                ]
                .into_iter()
                .map(|scope| store.skill_version_dir(&scope, &dependency.id, &dependency.version))
                .find(|path| path.is_dir())
                .ok_or_else(|| {
                    format!(
                        "workflow lock requires missing Skill {}@{}",
                        dependency.id, dependency.version
                    )
                })?;
                let manifest = crate::skill::manifest::load_skill_manifest(&version_root)?;
                if manifest.id != dependency.id || manifest.version != dependency.version {
                    return Err(format!(
                        "workflow lock Skill {} content identity is inconsistent",
                        dependency.id
                    )
                    .into());
                }
                let actual = package_payload_digest(&version_root)?;
                if !actual.eq_ignore_ascii_case(&dependency.sha256) {
                    return Err(
                        format!("workflow lock Skill {} content changed", dependency.id).into(),
                    );
                }
            }
            ExtensionAssetKind::Workflow => {
                return Err(format!(
                    "workflow lock contains unsupported nested Workflow dependency {}",
                    dependency.id
                )
                .into());
            }
        }
    }
    for capability in &lock.environment.capabilities {
        if !package.capabilities.contains(&capability.id) {
            return Err(format!(
                "workflow environment lock contains undeclared Capability {}",
                capability.id
            )
            .into());
        }
    }
    for connector in &lock.environment.connectors {
        if !package.dependencies.connectors.contains(&connector.id) {
            return Err(format!(
                "workflow environment lock contains undeclared Connector {}",
                connector.id
            )
            .into());
        }
        crate::store::connector_state::ensure_available(&connector.id).map_err(|error| {
            format!(
                "workflow environment lock requires unavailable Connector {}: {error}",
                connector.id
            )
        })?;
    }
    for runtime in &lock.environment.runtimes {
        if !package.supported_runtimes.contains(&runtime.id) {
            return Err(format!(
                "workflow environment lock contains undeclared Runtime {}",
                runtime.id
            )
            .into());
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowSignatureMetadata {
    algorithm: String,
    key_id: String,
    signature: String,
}

pub(crate) fn package_signature_identity(
    root: &Path,
) -> Result<Option<(String, String)>, Box<dyn Error>> {
    let path = root.join("manifest.sig");
    if !path.is_file() {
        return Ok(None);
    }
    let metadata: WorkflowSignatureMetadata = serde_json::from_slice(&fs::read(path)?)?;
    Ok(Some((metadata.key_id, metadata.algorithm)))
}

fn validate_package_integrity(root: &Path, require_signature: bool) -> Result<(), Box<dyn Error>> {
    let checksums_path = root.join("checksums.sha256");
    let signature_path = root.join("manifest.sig");
    if !checksums_path.is_file() {
        if signature_path.is_file() {
            // 带签名却缺清单时无法验签：把可执行的修复方向放在最前面，列表行截断后仍能读懂。
            return Err(
                "无法验证制品签名：制品带 manifest.sig 但缺少 checksums.sha256。\
                 请补上配套的 checksums.sha256，或改用不含 manifest.sig 的制品。"
                    .into(),
            );
        }
        if require_signature {
            return Err("制品缺少 checksums.sha256：该安装要求带校验清单的制品。".into());
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
        )
        .map_err(|error| {
            format!(
                "无法验证制品签名（key_id={}, algorithm={}）：{error}",
                metadata.key_id, metadata.algorithm
            )
        })?;
    } else if require_signature {
        return Err("制品缺少 manifest.sig：该安装要求已签名制品。".into());
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
        if is_packaging_metadata(&relative) {
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

/// 制品内容摘要：只覆盖扩展自身的内容。
///
/// `checksums.sha256` / `manifest.sig` 是打包与签名产物，同一个版本从本地目录安装
/// 和从归档安装时它们的字节不同。摘要只看内容，来源切换与重新打包才不会被误判成
/// 「同版本内容被改写」。
pub(crate) fn package_digest(root: &Path) -> Result<String, Box<dyn Error>> {
    digest_package_files(root, |_| true)
}

/// 安装期写入的内容目录的本地状态文件：记录来源、治理、授权等本机事实，
/// 同一个版本换台机器装一次内容就变一次，不属于扩展内容。
const INSTALL_METADATA_FILE: &str = "policy.json";

/// 依赖内容摘要：按「进包内容」口径计算，用于依赖锁钉版本。
///
/// 依赖锁描述的是「依赖的哪一份内容」，而同一个版本会随安装来源落地成不同的
/// 本机文件集合：从本地扩展源安装会直接物化开发工作区（含源码、旧制品、安装期
/// 写入的 policy.json），从发布制品安装只落一份载荷。用整目录摘要，发布机上算出的
/// 锁在任何一台从制品安装的机器上都会校验失败；过滤到载荷之后，两条路径才会算出
/// 同一个值。注意这与 `package_digest` 的用途不同：后者比较的是「同一份本地物化
/// 是否被改写」，必须看到目录里的全部内容。
pub(crate) fn package_payload_digest(root: &Path) -> Result<String, Box<dyn Error>> {
    digest_package_files(root, |relative| {
        let normalized = relative.replace('\\', "/");
        crate::app::local_package::is_portable_payload_path(&normalized)
            && !is_install_metadata(&normalized)
    })
}

fn is_install_metadata(relative: &str) -> bool {
    !relative.contains('/') && relative.eq_ignore_ascii_case(INSTALL_METADATA_FILE)
}

fn digest_package_files(
    root: &Path,
    include: impl Fn(&str) -> bool,
) -> Result<String, Box<dyn Error>> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    // 摘要里的路径统一成“/”分隔，并按这个规范形式排序：同一份内容在 Windows 与
    // 其它平台上必须算出同一个值，否则依赖锁会变成「只在打锁的那台机器上有效」。
    // 排序也必须用规范路径而不是平台路径，`a/b` 与 `a-b` 两种写法在两种排序下
    // 的先后并不一致。
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for relative in files {
        if is_packaging_metadata(&relative) {
            continue;
        }
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if !include(&normalized) {
            continue;
        }
        entries.push((normalized, relative));
    }
    entries.sort();
    let mut digest = Sha256::new();
    for (normalized, relative) in entries {
        digest.update(normalized.as_bytes());
        digest.update([0]);
        digest.update(fs::read(root.join(&relative))?);
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn is_packaging_metadata(relative: &Path) -> bool {
    // 只认根目录下的打包元数据：同名文件出现在子目录里就是扩展自己的内容。
    let at_root = relative
        .parent()
        .map(|parent| parent.as_os_str().is_empty())
        .unwrap_or(true);
    at_root
        && relative
            .file_name()
            .and_then(|name| name.to_str())
            .map(crate::app::local_package::is_packaging_metadata)
            .unwrap_or(false)
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

/// 已下线、需要从用户机器上清掉的工作流 id。
///
/// 这里登记的是「产品里已经没有这一份了」的稳定 ID：扩展源下架的定制工作流，
/// 以及改名之后留下的历史 ID。历史 ID 不会自己消失——存量机器上永远留着一条
/// 既装不上也更新不了的分叉记录，所以必须显式列出。
pub(crate) fn retired_workflow_ids() -> &'static [&'static str] {
    &[
        // 「微信小程序开发交付」是项目定制流程，2026-09-28 按方案 A 删除：
        // 源码、市场条目与发行版全部下架，不再作为通用交付流程提供。
        "com.himind.workflow.wechat-miniprogram-delivery",
        // 稳定 ID 从 `wechat-miniprogram-experience-upload` 收敛到
        // `wechat-experience-upload`，随后又迁到 himind-ext-projects 仓库。
        // 旧 ID 在任何扩展源里都不再出现，只会在存量机器上白占一条记录。
        "com.himind.workflow.wechat-miniprogram-experience-upload",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use rand::rngs::OsRng;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{Pss, RsaPrivateKey, RsaPublicKey};
    fn source_package() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload")
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

    #[test]
    fn install_list_disable_and_remove() {
        let store = store();
        let installed = store.install_from_directory(&source_package()).unwrap();
        assert_eq!(
            installed.package.id,
            "com.himind.workflow.wechat-experience-upload"
        );
        assert!(installed.enabled);
        assert_eq!(store.list().unwrap().len(), 1);

        let disabled = store.set_enabled(&installed.package.id, false).unwrap();
        assert!(!disabled.enabled);
        assert!(store.remove(&installed.package.id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    fn lock_entry(id: &str) -> crate::app::extension_lock::ExtensionLockEntry {
        crate::app::extension_lock::ExtensionLockEntry {
            asset_kind: "workflow".to_string(),
            asset_id: id.to_string(),
            version: "1.0.0".to_string(),
            source_id: "github:mrbaoquan/himind-extensions".to_string(),
            source: "github".to_string(),
            repository: "mrbaoquan/himind-extensions".to_string(),
            reference: format!("workflow/{id}@1.0.0"),
            catalog_path: ".himind/catalog.json".to_string(),
            source_commit: String::new(),
            artifact_url: String::new(),
            artifact_id: String::new(),
            sha256: String::new(),
            dependencies: Vec::new(),
            agent_profile: "development".to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn retiring_a_workflow_clears_its_directory_ledger_and_provenance() {
        let store = store();
        let retired = "com.himind.workflow.wechat-miniprogram-delivery";
        let kept = "com.himind.workflow.keep-me";
        let retired_root = store.product_root(retired).unwrap();
        fs::create_dir_all(&retired_root).unwrap();
        fs::write(retired_root.join("legacy.txt"), "retired").unwrap();

        // 目录、台账、来源记录是「已安装」的三份事实。只删目录，用户机器上就会
        // 长期留着一条界面里查不到、自动更新与依赖解析却仍然认得的工作流。
        let state_root = store.extension_state_root.clone();
        let mut lock = crate::app::extension_lock::ExtensionLockFile::default();
        lock.entries
            .insert(format!("workflow:{retired}"), lock_entry(retired));
        lock.entries
            .insert(format!("workflow:{kept}"), lock_entry(kept));
        fs::create_dir_all(&state_root).unwrap();
        fs::write(
            crate::app::extension_lock::path_for_state_root(&state_root),
            serde_json::to_vec_pretty(&lock).unwrap(),
        )
        .unwrap();
        let provenance = state_root.join("extension-provenance");
        fs::create_dir_all(&provenance).unwrap();
        for id in [retired, kept] {
            fs::write(provenance.join(format!("workflow-{id}.json")), "{}").unwrap();
        }

        store.retire_removed_workflows();

        let remaining: crate::app::extension_lock::ExtensionLockFile = serde_json::from_slice(
            &fs::read(crate::app::extension_lock::path_for_state_root(&state_root)).unwrap(),
        )
        .unwrap();
        assert!(!remaining
            .entries
            .contains_key(&format!("workflow:{retired}")));
        assert!(remaining.entries.contains_key(&format!("workflow:{kept}")));
        assert!(!provenance.join(format!("workflow-{retired}.json")).exists());
        assert!(provenance.join(format!("workflow-{kept}.json")).exists());
        assert!(!retired_root.exists());
        let _ = fs::remove_dir_all(state_root);
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
    fn loads_an_exact_installed_version_after_upgrade() {
        let store = store();
        let first_source = package_copy();
        let mut first_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(first_source.join("workflow.json")).unwrap()).unwrap();
        first_manifest["version"] = serde_json::json!("1.0.0");
        fs::write(
            first_source.join("workflow.json"),
            serde_json::to_vec_pretty(&first_manifest).unwrap(),
        )
        .unwrap();
        store.install_from_directory(&first_source).unwrap();

        let second_source = package_copy();
        let second_version = crate::workflow::load_from_directory(&second_source)
            .unwrap()
            .version;
        let installed = store.install_from_directory(&second_source).unwrap();
        assert_eq!(installed.package.version, second_version);
        assert_eq!(store.list().unwrap()[0].package.version, second_version);

        let historical = store
            .load_version("com.himind.workflow.wechat-experience-upload", "1.0.0")
            .unwrap();
        assert_eq!(historical.version, "1.0.0");
        let _ = fs::remove_dir_all(first_source);
        let _ = fs::remove_dir_all(second_source);
    }

    #[test]
    fn rollback_restores_the_matching_release_lock_metadata() {
        let store = store();
        let first_source = package_copy();
        let mut first_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(first_source.join("workflow.json")).unwrap()).unwrap();
        first_manifest["version"] = serde_json::json!("1.0.0");
        fs::write(
            first_source.join("workflow.json"),
            serde_json::to_vec_pretty(&first_manifest).unwrap(),
        )
        .unwrap();
        let first_package = crate::workflow::load_from_directory(&first_source).unwrap();
        let first_sha = "a".repeat(64);
        let first_lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: first_package.id.clone(),
                version: first_package.version.clone(),
                sha256: first_sha.clone(),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment::default(),
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        store
            .install_from_directory_with_metadata(
                &first_source,
                false,
                &first_sha,
                Some(first_lock),
                true,
            )
            .unwrap();

        let second_source = package_copy();
        let second_package = crate::workflow::load_from_directory(&second_source).unwrap();
        let second_sha = "b".repeat(64);
        let second_lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: second_package.id.clone(),
                version: second_package.version.clone(),
                sha256: second_sha.clone(),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment::default(),
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        store
            .install_from_directory_with_metadata(
                &second_source,
                false,
                &second_sha,
                Some(second_lock),
                true,
            )
            .unwrap();

        let rolled_back = store.rollback(&first_package.id).unwrap();
        assert_eq!(rolled_back.package.version, "1.0.0");
        assert_eq!(rolled_back.artifact_sha256, first_sha);
        assert!(store.load_enabled_for_run(&first_package.id).is_ok());
        let _ = fs::remove_dir_all(first_source);
        let _ = fs::remove_dir_all(second_source);
    }

    #[test]
    fn run_interaction_rejects_package_content_replacement() {
        let store = store();
        let package = store.install_from_directory(&source_package()).unwrap();
        let expected_digest = package_digest(
            &store
                .load_version(&package.package.id, &package.package.version)
                .unwrap()
                .source_root,
        )
        .unwrap();
        let interaction: InteractionEnvelope = serde_json::from_value(serde_json::json!({
            "schema_version": "interaction_envelope.v1",
            "interaction_id": "int-workflow",
            "correlation_id": "corr-workflow",
            "idempotency_key": "idem-workflow",
            "source": "workflow",
            "transport": "local",
            "principal": {"local_principal_id": "local-user"},
            "agent_id": "local-agent",
            "business_context": {
                "workflow": {
                    "id": package.package.id,
                    "version": package.package.version,
                    "package_digest": expected_digest
                }
            },
            "created_at": "2026-09-16T00:00:00Z"
        }))
        .unwrap();
        store.load_for_run_interaction(&interaction).unwrap();

        let version_root = store
            .product_root(&package.package.id)
            .unwrap()
            .join("versions")
            .join(&package.package.version);
        let mut readme = fs::read_to_string(version_root.join("README.md")).unwrap();
        readme.push_str("\ntampered\n");
        fs::write(version_root.join("README.md"), readme).unwrap();
        assert!(store
            .load_for_run_interaction(&interaction)
            .unwrap_err()
            .to_string()
            .contains("content changed after the run started"));
    }

    #[test]
    fn install_requires_a_release_lock_from_a_managed_catalog() {
        let store = store();
        let error = store
            .install_from_directory_with_metadata(
                &source_package(),
                false,
                &"a".repeat(64),
                None,
                true,
            )
            .unwrap_err();
        assert!(error.to_string().contains("release lock is required"));
    }

    #[test]
    fn install_rejects_a_missing_locked_dependency() {
        let store = store();
        let artifact_sha256 = "a".repeat(64);
        let package = crate::workflow::load_from_directory(&source_package()).unwrap();
        let lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: package.id.clone(),
                version: package.version.clone(),
                sha256: artifact_sha256.clone(),
            },
            dependencies: vec![crate::extension_contracts::ExtensionLockDependency {
                kind: ExtensionAssetKind::Plugin,
                id: "com.himind.plugin.missing-for-lock-test".to_string(),
                version: "1.0.0".to_string(),
                sha256: "b".repeat(64),
                source_id: String::new(),
                required: true,
            }],
            environment: crate::extension_contracts::ExtensionLockEnvironment::default(),
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        let error = store
            .install_from_directory_with_metadata(
                &source_package(),
                false,
                &artifact_sha256,
                Some(lock),
                true,
            )
            .unwrap_err();
        assert!(error.to_string().contains("missing Plugin"));
    }

    #[test]
    fn install_rejects_environment_lock_with_undeclared_capability() {
        let store = store();
        let source = source_package();
        let package = load_from_directory(&source).unwrap();
        let artifact_sha256 = "a".repeat(64);
        let lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: package.id.clone(),
                version: package.version.clone(),
                sha256: artifact_sha256.clone(),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment {
                capabilities: vec![crate::extension_contracts::ExtensionLockCapability {
                    id: "com.example.undeclared".to_string(),
                    provider: "test".to_string(),
                    availability: "local".to_string(),
                    required: true,
                }],
                ..Default::default()
            },
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        let error = store
            .install_from_directory_with_metadata(
                &source,
                false,
                &artifact_sha256,
                Some(lock),
                true,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("undeclared Capability"));
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

    /// 打包元数据（checksums.sha256 / manifest.sig）描述的是「怎么被打包的」，不是扩展内容。
    /// 同一个版本从目录装一次、再从归档装一次，内容没变就不算改写；重装还必须真的把
    /// 新制品的打包元数据换上去，否则带旧签名的坏包重装之后还是坏的。
    #[test]
    fn reinstall_replaces_packaging_metadata_for_identical_content() {
        let store = store();
        let source = package_copy();
        let installed = store.install_from_directory(&source).unwrap();
        let version_root = store
            .product_root(&installed.package.id)
            .unwrap()
            .join("versions")
            .join(&installed.package.version);
        assert!(!version_root.join("checksums.sha256").exists());

        write_checksums(&source);
        store.install_from_directory(&source).unwrap();
        assert!(version_root.join("checksums.sha256").is_file());
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = fs::remove_dir_all(source);
    }

    /// 发布时写进 Release Lock 的依赖摘要必须与安装后重算的口径一致：两端都只算
    /// 扩展内容，跳过 `checksums.sha256` / `manifest.sig`。否则发布者本机装过的
    /// 依赖与使用者新装的同一版本依赖算出的摘要不同，远端安装会必然失败。
    #[test]
    fn dependency_digest_ignores_packaging_metadata() {
        let root = package_copy();
        let bare = package_digest(&root).unwrap();

        write_checksums(&root);
        fs::write(root.join("manifest.sig"), "signature-bytes").unwrap();
        assert_eq!(package_digest(&root).unwrap(), bare);

        let mut readme = fs::read_to_string(root.join("README.md")).unwrap();
        readme.push_str("\nchanged\n");
        fs::write(root.join("README.md"), readme).unwrap();
        assert_ne!(package_digest(&root).unwrap(), bare);

        let _ = fs::remove_dir_all(root);
    }

    /// 依赖锁钉的是「进包内容」。同一个版本的插件从开发工作区安装（含源码、构建输入、
    /// 旧制品、构建缓存和安装期写入的 policy.json）与从发布制品安装（只有载荷），必须
    /// 算出同一个摘要，否则发布机上生成的锁在任何使用者机器上都校验不过。
    #[test]
    fn dependency_payload_digest_is_independent_of_install_route() {
        let root = std::env::temp_dir().join(format!(
            "himind-payload-digest-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let development = root.join("development");
        let published = root.join("published");
        for directory in [&development, &published] {
            fs::create_dir_all(directory.join("bin")).unwrap();
            fs::write(
                directory.join("plugin.json"),
                "{\"id\":\"com.himind.example\"}",
            )
            .unwrap();
            fs::write(directory.join("bin/tool.exe"), "binary").unwrap();
        }
        fs::write(development.join("main.go"), "package main").unwrap();
        fs::write(development.join("go.mod"), "module example").unwrap();
        fs::write(development.join("tool-1.0.0.hmpkg"), "old artifact").unwrap();
        fs::create_dir_all(development.join("dist")).unwrap();
        fs::write(development.join("dist/tool.hmpkg"), "old artifact").unwrap();
        fs::write(
            development.join("policy.json"),
            "{\"source\":\"development\"}",
        )
        .unwrap();
        fs::write(development.join("checksums.sha256"), "stale\n").unwrap();
        fs::write(published.join("checksums.sha256"), "fresh\n").unwrap();

        assert_eq!(
            package_payload_digest(&development).unwrap(),
            package_payload_digest(&published).unwrap()
        );
        // 整目录摘要仍然区分这两份目录：它回答的是「本机这份物化有没有被改写」。
        assert_ne!(
            package_digest(&development).unwrap(),
            package_digest(&published).unwrap()
        );
        // 载荷内容真的变了，摘要必须跟着变，否则锁就失去意义。
        fs::write(published.join("bin/tool.exe"), "patched binary").unwrap();
        assert_ne!(
            package_payload_digest(&development).unwrap(),
            package_payload_digest(&published).unwrap()
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 一个读不出来的制品不能让整份已安装列表消失：它必须作为 issue 报出来，
    /// UI 才能把它显示成「读取失败」并给出移除出口。
    #[test]
    fn unreadable_package_is_reported_as_an_issue() {
        let store = store();
        let installed = store.install_from_directory(&source_package()).unwrap();
        let version_root = store
            .product_root(&installed.package.id)
            .unwrap()
            .join("versions")
            .join(&installed.package.version);
        write_checksums(&version_root);
        let mut readme = fs::read_to_string(version_root.join("README.md")).unwrap();
        readme.push_str("\ntampered\n");
        fs::write(version_root.join("README.md"), readme).unwrap();

        let (items, issues) = store.list_with_issues().unwrap();
        assert!(items.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].package_id, installed.package.id);
        assert_eq!(issues[0].version, installed.package.version);
        assert!(issues[0].message.contains("checksum mismatch"));
        assert!(store.list().unwrap().is_empty());
        // 读取失败要说清是「装了但校验不过」，而不是含糊的「没装或已停用」。
        let error = store
            .load_enabled_for_run(&installed.package.id)
            .unwrap_err();
        assert!(error.to_string().contains("failed validation"));
    }

    /// 回滚目标读不出来时必须原样保留安装元数据：一次失败的回滚不该把本来能用的
    /// 工作流一起带走。
    #[test]
    fn rollback_keeps_installation_when_target_version_is_unreadable() {
        let store = store();
        let first_source = package_copy();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(first_source.join("workflow.json")).unwrap()).unwrap();
        manifest["version"] = serde_json::json!("1.0.0");
        fs::write(
            first_source.join("workflow.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        store.install_from_directory(&first_source).unwrap();

        let second_source = package_copy();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(second_source.join("workflow.json")).unwrap())
                .unwrap();
        manifest["version"] = serde_json::json!("2.0.0");
        fs::write(
            second_source.join("workflow.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let upgraded = store.install_from_directory(&second_source).unwrap();
        assert_eq!(upgraded.package.version, "2.0.0");
        assert_eq!(upgraded.previous_version, "1.0.0");

        let old_root = store
            .product_root(&upgraded.package.id)
            .unwrap()
            .join("versions")
            .join("1.0.0");
        write_checksums(&old_root);
        let mut readme = fs::read_to_string(old_root.join("README.md")).unwrap();
        readme.push_str("\ntampered\n");
        fs::write(old_root.join("README.md"), readme).unwrap();

        assert!(store.rollback(&upgraded.package.id).is_err());
        let installation = store
            .load_installation(&upgraded.package.id)
            .unwrap()
            .unwrap();
        assert_eq!(installation.current_version, "2.0.0");
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = fs::remove_dir_all(first_source);
        let _ = fs::remove_dir_all(second_source);
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
        let _guard = crate::app::system::signing_env_lock();
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
