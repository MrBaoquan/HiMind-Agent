use crate::app::local_package::{archive_directory, stage_local_package, PackageLimits};
use crate::capability::types::CapabilityDescriptor;
use crate::extension_contracts::{
    ExtensionAssetIdentity, ExtensionAssetKind, ExtensionCandidate, ExtensionCandidateState,
    ExtensionDependencyRef, ExtensionLock, ExtensionLockCapability, ExtensionLockConnector,
    ExtensionLockConnectorCredential, ExtensionLockDependency, ExtensionLockEnvironment,
    ExtensionLockRuntime, ExtensionSourceKind, ExtensionSourceRef,
    EXTENSION_CANDIDATE_SCHEMA_VERSION, EXTENSION_LOCK_SCHEMA_VERSION,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WORKFLOW_AUTHORING_MAX_FILES: usize = 100_000;
const WORKFLOW_AUTHORING_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowDraftManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub release_notes: String,
    /// 工作流要求的最低 Agent 版本。
    ///
    /// 与 `release_notes` 一样是发布元数据：草稿不带它，发布到工作台时这个约束
    /// 就会变成空串，等于市场对这个工作流不再做版本门禁。
    #[serde(default)]
    pub min_agent_version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub plugin_dependencies: Vec<WorkflowPluginDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowPluginDependency {
    pub plugin_id: String,
    #[serde(default)]
    pub required: bool,
    /// 依赖声明的最低版本；空串表示不限版本。
    #[serde(default)]
    pub min_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowDraft {
    pub package_id: String,
    pub version: String,
    pub name: String,
    pub manifest: WorkflowDraftManifest,
    pub source_root: PathBuf,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    pub state: ExtensionCandidateState,
    #[serde(default)]
    pub test_report: Value,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub tested_at: Option<String>,
    #[serde(default)]
    pub confirmed_at: Option<String>,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub dashboard_submission_id: Option<String>,
    #[serde(default)]
    pub dashboard_draft_id: Option<String>,
    #[serde(default)]
    pub lock: Option<ExtensionLock>,
    #[serde(default)]
    pub lock_path: Option<PathBuf>,
}

impl WorkflowDraft {
    /// 该版本的候选是否已经离开本机（提交到工作台或发布到分发渠道）。
    ///
    /// 未离开本机的候选只是构建产物，允许随源码重建；一旦提交，内容必须冻结，
    /// 修改只能通过递增版本号表达。
    pub(crate) fn released_from_local(&self) -> bool {
        matches!(self.state, ExtensionCandidateState::Submitted)
            || self.submitted_at.is_some()
            || self.dashboard_submission_id.is_some()
    }

    pub(crate) fn candidate_record(&self) -> Result<ExtensionCandidate, Box<dyn Error>> {
        let package = crate::workflow::load_from_directory(&self.source_root)?;
        let dependencies = workflow_dependency_refs(&package.dependencies);
        let candidate = ExtensionCandidate {
            schema_version: EXTENSION_CANDIDATE_SCHEMA_VERSION.to_string(),
            kind: ExtensionAssetKind::Workflow,
            id: self.package_id.clone(),
            version: self.version.clone(),
            candidate_sha256: self.candidate_sha256.clone(),
            workspace_root: self.source_root.to_string_lossy().to_string(),
            source: ExtensionSourceRef {
                kind: ExtensionSourceKind::Local,
                id: "local:workflow-workspace".to_string(),
                repository: String::new(),
                reference: String::new(),
                commit: String::new(),
                subdirectory: String::new(),
            },
            dependencies,
            test_report: self.test_report.clone(),
            state: self.state,
            blockers: Vec::new(),
            warnings: Vec::new(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        };
        candidate.validate()?;
        Ok(candidate)
    }
}

pub(crate) fn list() -> Result<Vec<WorkflowDraft>, Box<dyn Error>> {
    list_from_root(&drafts_root())
}

pub(crate) fn read(package_id: &str, version: &str) -> Result<WorkflowDraft, Box<dyn Error>> {
    read_from_root(&drafts_root(), package_id, version)
}

pub(crate) fn save_from_source(source_root: &Path) -> Result<WorkflowDraft, Box<dyn Error>> {
    save_from_source_to_root(source_root, &drafts_root())
}

pub(crate) fn test(package_id: &str, version: &str) -> Result<WorkflowDraft, Box<dyn Error>> {
    test_in_root(package_id, version, &drafts_root(), None)
}

pub(crate) fn test_with_capabilities(
    package_id: &str,
    version: &str,
    capabilities: &[CapabilityDescriptor],
) -> Result<WorkflowDraft, Box<dyn Error>> {
    test_in_root(package_id, version, &drafts_root(), Some(capabilities))
}

pub(crate) fn confirm(package_id: &str, version: &str) -> Result<WorkflowDraft, Box<dyn Error>> {
    confirm_in_root(package_id, version, None)
}

pub(crate) fn confirm_with_capabilities(
    package_id: &str,
    version: &str,
    capabilities: &[CapabilityDescriptor],
) -> Result<WorkflowDraft, Box<dyn Error>> {
    confirm_in_root(package_id, version, Some(capabilities))
}

fn confirm_in_root(
    package_id: &str,
    version: &str,
    capabilities: Option<&[CapabilityDescriptor]>,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let root = drafts_root();
    let mut draft = test_in_root(package_id, version, &root, capabilities)?;
    draft.confirmed_at = Some(now_stamp());
    draft.state = ExtensionCandidateState::Confirmed;
    draft.updated_at = now_stamp();
    persist(&root, &draft)?;
    Ok(draft)
}

pub(crate) fn submit(
    options: &crate::Options,
    agent_id: &str,
    package_id: &str,
    version: &str,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let draft = read(package_id, version)?;
    if draft.state != ExtensionCandidateState::Confirmed {
        return Err("Workflow Candidate 尚未确认".into());
    }
    // 分发目标门禁：只有把工件交给组织工作台的项目才允许提审。
    crate::extension_projects::ensure_distribution_target(
        crate::extension_projects::ExtensionProjectKind::Workflow,
        package_id,
        crate::extension_contracts::DistributionTarget::Workbench,
    )?;
    let lock_path = draft
        .lock_path
        .as_deref()
        .ok_or("Workflow Candidate 缺少依赖锁")?;
    let lock = draft.lock.as_ref().ok_or("Workflow Candidate 缺少依赖锁")?;
    lock.validate()?;
    let access = crate::api::oauth::platform_access_token(
        options,
        crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
    )?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let source = crate::extension_projects::submission_source(
        crate::extension_projects::ExtensionProjectKind::Workflow,
        package_id,
    )?;
    let submitted = crate::api::distribution::submit_workflow(
        &client,
        &options.api_base(),
        agent_id,
        &access.token,
        &draft.candidate_path,
        lock_path,
        &draft.test_report,
        None,
        &source,
        "",
    )?;
    let dashboard_draft_id = submitted
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Dashboard 未返回 Workflow 审核记录")?;
    mark_submitted(package_id, version, dashboard_draft_id)
}

pub(crate) fn mark_submitted(
    package_id: &str,
    version: &str,
    dashboard_draft_id: &str,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let mut draft = read(package_id, version)?;
    if draft.state != ExtensionCandidateState::Confirmed {
        return Err("Workflow Candidate 尚未确认".into());
    }
    draft.state = ExtensionCandidateState::Submitted;
    draft.submitted_at = Some(now_stamp());
    draft.dashboard_draft_id = Some(dashboard_draft_id.to_string());
    draft.updated_at = now_stamp();
    persist(&drafts_root(), &draft)?;
    Ok(draft)
}

fn save_from_source_to_root(
    source_root: &Path,
    storage_root: &Path,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let source_root = source_root.canonicalize()?;
    let package = crate::workflow::load_from_directory(&source_root)?;
    let draft_root = draft_version_root(storage_root, &package.id, &package.version);
    fs::create_dir_all(&draft_root)?;
    let staging = draft_root.join("package");
    stage_local_package(
        &source_root,
        &staging,
        &PackageLimits {
            max_files: WORKFLOW_AUTHORING_MAX_FILES,
            max_bytes: WORKFLOW_AUTHORING_MAX_BYTES,
            label: "Workflow 候选",
        },
        |_| true,
    )?;
    let candidate_path = draft_root.join(format!("{}-{}.hmwf", package.id, package.version));
    let previous = read_from_root(storage_root, &package.id, &package.version).ok();
    let temporary = draft_root.join(format!(
        ".{}-{}.{}.staging",
        package.id,
        package.version,
        unique_suffix()
    ));
    archive_directory(&staging, &temporary)?;
    let candidate_sha256 = sha256_file(&temporary)?;
    if candidate_path.exists() {
        let existing = sha256_file(&candidate_path)?;
        if !existing.eq_ignore_ascii_case(&candidate_sha256) {
            if previous
                .as_ref()
                .is_some_and(WorkflowDraft::released_from_local)
            {
                let _ = fs::remove_file(&temporary);
                return Err(format!(
                    "Workflow 候选版本内容发生变化: {} v{}；该版本已提交，修改请递增版本号",
                    package.id, package.version
                )
                .into());
            }
            fs::remove_file(&candidate_path)?;
            fs::rename(&temporary, &candidate_path)?;
        } else {
            let _ = fs::remove_file(&temporary);
        }
    } else {
        fs::rename(&temporary, &candidate_path)?;
    }
    let now = now_stamp();
    let draft = WorkflowDraft {
        package_id: package.id.clone(),
        version: package.version.clone(),
        name: package.name.clone(),
        manifest: WorkflowDraftManifest {
            id: package.id,
            name: package.name,
            description: package.description,
            version: package.version,
            release_notes: package.release_notes,
            min_agent_version: package.min_agent_version,
            capabilities: package.capabilities,
            plugin_dependencies: package
                .dependencies
                .plugins
                .iter()
                .map(|plugin| WorkflowPluginDependency {
                    plugin_id: plugin.id().to_string(),
                    required: plugin.required,
                    min_version: plugin.min_version.trim().to_string(),
                })
                .collect(),
        },
        source_root,
        candidate_path,
        candidate_sha256,
        state: ExtensionCandidateState::Candidate,
        test_report: json!({}),
        created_at: previous
            .as_ref()
            .map(|value| value.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
        tested_at: None,
        confirmed_at: None,
        submitted_at: None,
        dashboard_submission_id: None,
        dashboard_draft_id: None,
        lock: None,
        lock_path: None,
    };
    persist(storage_root, &draft)?;
    Ok(draft)
}

fn test_in_root(
    package_id: &str,
    version: &str,
    storage_root: &Path,
    capabilities: Option<&[CapabilityDescriptor]>,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let mut draft = read_from_root(storage_root, package_id, version)?;
    let package = crate::workflow::load_from_directory(&draft.source_root)?;
    let preflight = capabilities.map(|capabilities| {
        crate::workflow::preflight(&package, crate::VERSION, capabilities, &Value::Null)
    });
    if let Some(report) = preflight.as_ref().filter(|report| !report.ready) {
        return Err(format!("Workflow Preflight 未通过: {}", report.blockers.join("；")).into());
    }
    let test_root = draft_version_root(storage_root, package_id, version).join("test-package");
    stage_local_package(
        &draft.source_root,
        &test_root,
        &PackageLimits {
            max_files: WORKFLOW_AUTHORING_MAX_FILES,
            max_bytes: WORKFLOW_AUTHORING_MAX_BYTES,
            label: "Workflow 候选测试",
        },
        |_| true,
    )?;
    let rebuilt = draft
        .candidate_path
        .with_file_name(format!(".{package_id}-{version}.test.hmwf"));
    archive_directory(&test_root, &rebuilt)?;
    let rebuilt_sha256 = sha256_file(&rebuilt)?;
    let _ = fs::remove_file(&rebuilt);
    if !rebuilt_sha256.eq_ignore_ascii_case(&draft.candidate_sha256) {
        return Err("Workflow 源目录在 Candidate 生成后发生变化".into());
    }
    let contract = crate::workflow::contract_dry_run_report(&package)?;
    let lock = resolve_dependency_lock(&package, &draft.candidate_sha256, preflight.as_ref())?;
    let lock_path =
        draft_version_root(storage_root, package_id, version).join("extension-lock.json");
    fs::write(&lock_path, serde_json::to_vec_pretty(&lock)?)?;
    draft.lock = Some(lock);
    draft.lock_path = Some(lock_path.clone());
    let lock_sha256 = sha256_file(&lock_path)?;
    draft.test_report = json!({
        "candidate_sha256": draft.candidate_sha256.clone(),
        "agent_version": crate::VERSION,
        "tested_at": now_stamp(),
        "lock_sha256": lock_sha256,
        "state": "passed",
        "checks": {
            "manifest": "passed",
            "assets": "passed",
            "step_graph": "passed",
            "candidate": "passed",
            "dependencies": "passed",
            "entrypoints": "passed",
            "loops": "passed",
            "artifacts": "passed",
            "contract_dry_run": "passed",
            "preflight": if preflight.is_some() { "passed" } else { "skipped" }
        },
        "preflight": preflight,
        "contract": contract,
        "workflow": {
            "id": package.id,
            "version": package.version,
            "name": package.name,
            "step_count": package.steps.len(),
            "artifact_count": package.artifacts.len()
        }
    });
    draft.state = ExtensionCandidateState::Tested;
    draft.tested_at = Some(now_stamp());
    draft.confirmed_at = None;
    draft.updated_at = now_stamp();
    persist(storage_root, &draft)?;
    Ok(draft)
}

fn resolve_dependency_lock(
    package: &crate::workflow::WorkflowPackage,
    candidate_sha256: &str,
    preflight: Option<&crate::workflow::WorkflowPreflight>,
) -> Result<ExtensionLock, Box<dyn Error>> {
    let mut dependencies = Vec::new();
    let mut missing = Vec::new();

    for declared in &package.dependencies.plugins {
        let plugin_id = declared.id();
        match crate::capability::plugin::find_plugin(plugin_id) {
            Ok(Some(plugin)) if plugin.enabled && plugin.error.is_none() => {
                record_dependency_gap(
                    declared.required,
                    version_gap(
                        "Plugin",
                        plugin_id,
                        &plugin.version,
                        declared.min_version.trim(),
                    ),
                    &mut missing,
                );
                let sha256 = super::store::package_payload_digest(
                    &crate::capability::plugin::plugin_content_dir(&plugin),
                )?;
                dependencies.push(ExtensionLockDependency {
                    kind: ExtensionAssetKind::Plugin,
                    id: plugin.id,
                    version: plugin.version,
                    sha256,
                    source_id: plugin.source,
                    required: declared.required,
                });
            }
            Ok(Some(plugin)) => record_dependency_gap(
                declared.required,
                format!(
                    "Plugin {plugin_id} 不可用{}",
                    plugin
                        .error
                        .as_deref()
                        .map(|error| format!(": {error}"))
                        .unwrap_or_default()
                ),
                &mut missing,
            ),
            Ok(None) => record_dependency_gap(
                declared.required,
                format!("缺少 Plugin {plugin_id}"),
                &mut missing,
            ),
            Err(error) => record_dependency_gap(
                declared.required,
                format!("读取 Plugin {plugin_id} 失败: {error}"),
                &mut missing,
            ),
        }
    }

    let skill_records = crate::skill::store::SkillStore::new().list_records()?;
    for declared in &package.dependencies.skills {
        let skill_id = declared.id();
        match skill_records
            .iter()
            .find(|record| record.manifest.id == *skill_id)
        {
            Some(record) => {
                record_dependency_gap(
                    declared.required,
                    version_gap(
                        "Skill",
                        skill_id,
                        &record.manifest.version,
                        declared.min_version.trim(),
                    ),
                    &mut missing,
                );
                let sha256 = super::store::package_payload_digest(&record.version_root)?;
                dependencies.push(ExtensionLockDependency {
                    kind: ExtensionAssetKind::Skill,
                    id: record.manifest.id.clone(),
                    version: record.manifest.version.clone(),
                    sha256,
                    source_id: "skill-store".to_string(),
                    required: declared.required,
                });
            }
            None => record_dependency_gap(
                declared.required,
                format!("缺少 Skill {skill_id}"),
                &mut missing,
            ),
        }
    }

    if !missing.is_empty() {
        return Err(format!("Workflow 依赖锁解析失败: {}", missing.join("；")).into());
    }

    dependencies.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.version.cmp(&right.version))
    });
    let lock = ExtensionLock {
        schema_version: EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
        root: ExtensionAssetIdentity {
            kind: ExtensionAssetKind::Workflow,
            id: package.id.clone(),
            version: package.version.clone(),
            sha256: candidate_sha256.to_string(),
        },
        dependencies,
        environment: preflight
            .map(|report| ExtensionLockEnvironment {
                capabilities: report
                    .capabilities
                    .iter()
                    .map(|capability| ExtensionLockCapability {
                        id: capability.id.clone(),
                        provider: capability.source.clone(),
                        availability: capability.availability.clone(),
                        required: true,
                    })
                    .collect(),
                connectors: report
                    .connectors
                    .iter()
                    .map(|connector| ExtensionLockConnector {
                        id: connector.id.clone(),
                        availability: connector.availability.clone(),
                        credential_ownership: connector.credential_ownership.clone(),
                        policy_revision: crate::store::connector_state::status(&connector.id)
                            .map(|state| state.remote_revision)
                            .unwrap_or_default(),
                        credentials: connector
                            .credentials
                            .iter()
                            .map(|credential| ExtensionLockConnectorCredential {
                                handle: credential.handle.clone(),
                                target: credential.target.clone(),
                                kind: credential.kind.clone(),
                                required: credential.required,
                            })
                            .collect(),
                        required: true,
                    })
                    .collect(),
                runtimes: report
                    .runtimes
                    .iter()
                    .map(|runtime| ExtensionLockRuntime {
                        id: runtime.id.clone(),
                        status: runtime.status.clone(),
                        version: runtime.version.clone(),
                        required: package.dependencies.runtimes.contains(&runtime.id),
                    })
                    .collect(),
            })
            .unwrap_or_default(),
        generated_at: now_stamp(),
    };
    lock.validate()?;
    Ok(lock)
}

fn list_from_root(root: &Path) -> Result<Vec<WorkflowDraft>, Box<dyn Error>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "draft.json" {
            if let Ok(draft) =
                serde_json::from_str::<WorkflowDraft>(&fs::read_to_string(entry.path())?)
            {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(drafts)
}

fn read_from_root(
    root: &Path,
    package_id: &str,
    version: &str,
) -> Result<WorkflowDraft, Box<dyn Error>> {
    let path = draft_version_root(root, package_id, version).join("draft.json");
    Ok(serde_json::from_str::<WorkflowDraft>(&fs::read_to_string(
        &path,
    )?)?)
}

fn persist(root: &Path, draft: &WorkflowDraft) -> Result<(), Box<dyn Error>> {
    let path = draft_version_root(root, &draft.package_id, &draft.version).join("draft.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}

fn workflow_dependency_refs(
    dependencies: &crate::workflow::WorkflowDependencies,
) -> Vec<ExtensionDependencyRef> {
    let mut items = dependencies
        .plugins
        .iter()
        .map(|item| dependency(ExtensionAssetKind::Plugin, item.id(), item.required))
        .chain(
            dependencies
                .skills
                .iter()
                .map(|item| dependency(ExtensionAssetKind::Skill, item.id(), item.required)),
        )
        .collect::<Vec<_>>();
    items.sort();
    items.dedup();
    items
}

fn dependency(kind: ExtensionAssetKind, id: &str, required: bool) -> ExtensionDependencyRef {
    ExtensionDependencyRef {
        kind,
        id: id.trim().to_string(),
        version: String::new(),
        sha256: String::new(),
        source_id: String::new(),
        required,
    }
}

/// 依赖缺失时按必需与否分流：必需依赖阻断打包，可选依赖只是让锁里没有这一条。
fn record_dependency_gap(required: bool, reason: String, missing: &mut Vec<String>) {
    if required && !reason.is_empty() {
        missing.push(reason);
    }
}

/// 已安装版本低于声明的最低版本时给出说明，满足则返回空串。
fn version_gap(kind: &str, id: &str, installed: &str, min_version: &str) -> String {
    if min_version.is_empty() {
        return String::new();
    }
    let (Ok(installed), Ok(minimum)) = (
        Version::parse(installed.trim()),
        Version::parse(min_version),
    ) else {
        return String::new();
    };
    if installed < minimum {
        return format!("{kind} {id} 版本 {installed} 低于声明的最低版本 {minimum}");
    }
    String::new()
}

fn draft_version_root(root: &Path, package_id: &str, version: &str) -> PathBuf {
    root.join(package_id).join(version)
}

fn drafts_root() -> PathBuf {
    crate::store::paths::agent_home().join("workflow-drafts")
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn now_stamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("unix-ms:{millis}")
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::types::CapabilityAvailability;

    fn workflow_root(name: &str) -> PathBuf {
        workflow_root_with_dependencies(name, Vec::<&str>::new())
    }

    fn workflow_root_with_dependencies(name: &str, plugins: Vec<&str>) -> PathBuf {
        workflow_root_with_declared_dependencies(
            name,
            json!({ "plugins": plugins, "skills": [], "connectors": [], "runtimes": [] }),
        )
    }

    fn workflow_root_with_declared_dependencies(name: &str, dependencies: Value) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-authoring-{name}-{}",
            unique_suffix()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("workflow.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "workflow_package.v1",
                "id": "com.example.authoring",
                "version": "1.0.0",
                "name": "Authoring Example",
                "min_agent_version": "0.3.47",
                "dependencies": dependencies,
                "steps": [{
                    "id": "START",
                    "title": "Start",
                    "kind": "manual",
                    "execution_mode": "sync"
                }],
                "artifacts": [],
                "ui": { "mode": "standard" }
            }))
            .unwrap(),
        )
        .unwrap();
        root
    }

    fn capability(id: &str) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: id.to_string(),
            version: "1.0.0".to_string(),
            name: id.to_string(),
            description: String::new(),
            risk_level: "read_only".to_string(),
            source: "test-provider".to_string(),
            contract_source: "test".to_string(),
            contract_generation: None,
            availability: CapabilityAvailability::Local,
            execution_mode: "sync".to_string(),
            supports_progress: false,
            supports_cancel: false,
            idempotency: "safe".to_string(),
            retry_policy: "none".to_string(),
            concurrency: "parallel".to_string(),
            approval_required: false,
            dashboard_provider: false,
            required_scope: None,
            dashboard_route: None,
            input_schema: json!({"type": "object"}),
        }
    }

    #[test]
    fn saves_and_tests_an_immutable_workflow_candidate() {
        let root = workflow_root("save-test");
        let source = root.join("source");
        let storage = root.join("storage");
        let draft = save_from_source_to_root(&source, &storage).unwrap();
        assert_eq!(draft.state, ExtensionCandidateState::Candidate);
        assert!(draft.candidate_path.is_file());
        let candidate = draft.candidate_record().unwrap();
        assert_eq!(candidate.kind, ExtensionAssetKind::Workflow);
        assert_eq!(candidate.id, "com.example.authoring");

        let tested = test_in_root("com.example.authoring", "1.0.0", &storage, None).unwrap();
        assert_eq!(tested.state, ExtensionCandidateState::Tested);
        assert_eq!(
            tested.test_report["checks"]["candidate"],
            Value::String("passed".to_string())
        );
        let lock = tested.lock.unwrap();
        assert_eq!(lock.root.id, "com.example.authoring");
        assert!(lock.dependencies.is_empty());
        assert!(tested.lock_path.unwrap().is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restaging_rebuilds_an_unsubmitted_candidate_when_the_source_changes() {
        let root = workflow_root("restage-unsubmitted");
        let source = root.join("source");
        let storage = root.join("storage");
        let first = save_from_source_to_root(&source, &storage).unwrap();
        assert_eq!(first.state, ExtensionCandidateState::Candidate);

        let manifest_path = source.join("workflow.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["name"] = json!("Authoring Renamed");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let restaged = save_from_source_to_root(&source, &storage).unwrap();
        assert_ne!(restaged.candidate_sha256, first.candidate_sha256);
        assert_eq!(restaged.state, ExtensionCandidateState::Candidate);
        assert_eq!(restaged.manifest.name, "Authoring Renamed");
        assert!(restaged.lock.is_none());
        assert!(restaged.confirmed_at.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restaging_a_submitted_candidate_requires_a_new_version() {
        let root = workflow_root("restage-submitted");
        let source = root.join("source");
        let storage = root.join("storage");
        save_from_source_to_root(&source, &storage).unwrap();
        let mut submitted = read_from_root(&storage, "com.example.authoring", "1.0.0").unwrap();
        submitted.state = ExtensionCandidateState::Submitted;
        submitted.submitted_at = Some(now_stamp());
        persist(&storage, &submitted).unwrap();

        let manifest_path = source.join("workflow.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["name"] = json!("Authoring Renamed");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let error = save_from_source_to_root(&source, &storage).unwrap_err();
        assert!(error.to_string().contains("递增版本号"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn optional_dependency_declared_as_object_does_not_block_the_lock() {
        let root = workflow_root_with_declared_dependencies(
            "optional-dependency",
            json!({
                "plugins": [],
                "skills": [{
                    "skill_id": "definitely-missing-himind-skill",
                    "required": false,
                    "min_version": "1.0.0"
                }],
                "connectors": [],
                "runtimes": []
            }),
        );
        let source = root.join("source");
        let storage = root.join("storage");
        save_from_source_to_root(&source, &storage).unwrap();
        let tested = test_in_root("com.example.authoring", "1.0.0", &storage, None).unwrap();
        assert_eq!(tested.state, ExtensionCandidateState::Tested);
        let lock = tested.lock.unwrap();
        assert!(lock.dependencies.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_blocks_when_a_required_plugin_dependency_cannot_be_resolved() {
        let root = workflow_root_with_dependencies(
            "missing-dependency",
            vec!["com.example.missing-plugin"],
        );
        let source = root.join("source");
        let storage = root.join("storage");
        save_from_source_to_root(&source, &storage).unwrap();
        let error = test_in_root("com.example.authoring", "1.0.0", &storage, None).unwrap_err();
        assert!(error
            .to_string()
            .contains("缺少 Plugin com.example.missing-plugin"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_test_blocks_when_workflow_preflight_is_not_ready() {
        let root = workflow_root("missing-capability");
        let source = root.join("source");
        let storage = root.join("storage");
        let manifest_path = source.join("workflow.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["capabilities"] = json!(["com.example.missing"]);
        manifest["steps"] = json!([{
            "id": "START",
            "title": "Start",
            "kind": "capability",
            "capability_id": "com.example.missing",
            "execution_mode": "sync"
        }]);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        save_from_source_to_root(&source, &storage).unwrap();
        let error = test_in_root("com.example.authoring", "1.0.0", &storage, Some(&[]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("com.example.missing"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_test_records_workflow_environment_binding() {
        let root = workflow_root("environment-binding");
        let source = root.join("source");
        let storage = root.join("storage");
        let manifest_path = source.join("workflow.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["capabilities"] = json!(["com.example.available"]);
        manifest["steps"] = json!([{
            "id": "START",
            "title": "Start",
            "kind": "capability",
            "capability_id": "com.example.available",
            "execution_mode": "sync"
        }]);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        save_from_source_to_root(&source, &storage).unwrap();
        let tested = test_in_root(
            "com.example.authoring",
            "1.0.0",
            &storage,
            Some(&[capability("com.example.available")]),
        )
        .unwrap();
        let environment = tested.lock.unwrap().environment;
        assert_eq!(environment.capabilities.len(), 1);
        assert_eq!(environment.capabilities[0].id, "com.example.available");
        assert_eq!(
            environment.capabilities[0].provider,
            "test-provider".to_string()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_rejects_source_changes_after_candidate_creation() {
        let root = workflow_root("changed-source");
        let source = root.join("source");
        let storage = root.join("storage");
        save_from_source_to_root(&source, &storage).unwrap();
        fs::write(source.join("steps.md"), "changed").unwrap();
        let error = test_in_root("com.example.authoring", "1.0.0", &storage, None).unwrap_err();
        assert!(error.to_string().contains("Candidate 生成后发生变化"));
        let _ = fs::remove_dir_all(root);
    }
}
