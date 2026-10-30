use crate::app::extension_activation::{
    resolve_activation_target, ActivationKind, ActivationReceipt, ActivationStatus,
    ActivationStore, ActivationSurface,
};
use crate::skill::manifest::{
    validate_relative_package_path, validate_skill_id, validate_skill_manifest,
};
use crate::skill::resolver::{compare_versions, CapabilityFact, SkillReadiness};
use crate::skill::store::{SkillManagementPolicy, SkillStore};
use crate::skill::types::{
    SkillCapabilityDependency, SkillManifest, SkillPluginDependency, SkillScope,
};
use crate::Options;
use crate::VERSION;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::FileOptions;

const MAX_SKILL_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillDraftInput {
    pub id: String,
    pub name: String,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default = "default_agent_version")]
    pub min_agent_version: String,
    #[serde(default = "default_clients")]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<SkillCapabilityDependency>,
    #[serde(default)]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
    #[serde(default)]
    pub risk_summary: String,
    pub readme: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillPackageInput {
    pub package_path: PathBuf,
    #[serde(default)]
    pub revision_of_version: Option<String>,
    #[serde(default)]
    pub parent_submission_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuthoringDraft {
    pub manifest: SkillManifest,
    pub readme: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub revision_of: Option<String>,
    #[serde(default)]
    pub parent_submission_id: Option<String>,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub submitted_at: Option<String>,
    pub dashboard_draft_id: Option<String>,
    pub codex_target: Option<String>,
    #[serde(default)]
    pub client_targets: BTreeMap<String, String>,
    #[serde(default)]
    pub test_report: Option<Value>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuthoringTestResult {
    pub draft: AuthoringDraft,
    pub readiness: SkillReadiness,
    pub client_readiness: BTreeMap<String, SkillReadiness>,
    pub plugin_issues: Vec<String>,
    pub codex: serde_json::Value,
    pub clients: BTreeMap<String, serde_json::Value>,
    pub cleanup: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillActivationResult {
    pub draft: AuthoringDraft,
    pub activation: ActivationReceipt,
    pub development_path: PathBuf,
}

pub(crate) fn list() -> Result<Vec<AuthoringDraft>, Box<dyn Error>> {
    let root = drafts_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "draft.json" {
            if let Ok(draft) =
                serde_json::from_str::<AuthoringDraft>(&fs::read_to_string(entry.path())?)
            {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(drafts)
}

pub(crate) fn save(input: SkillDraftInput) -> Result<AuthoringDraft, Box<dyn Error>> {
    if input.readme.trim().is_empty() {
        return Err("SKILL.md 内容不能为空".into());
    }
    if input.release_notes.trim().is_empty() {
        return Err("请填写本版本更新说明".into());
    }
    let supplemental_files = input.files.clone();
    let mut contents = vec!["skill.json".to_string(), "SKILL.md".to_string()];
    for path in supplemental_files.keys() {
        validate_relative_package_path(path)?;
        if matches!(
            path.as_str(),
            "skill.json" | "SKILL.md" | ".himind-render.json"
        ) || crate::app::local_package::is_checksums_file(path)
        {
            return Err(format!("Skill 附加文件使用了保留路径: {path}").into());
        }
        contents.push(path.clone());
    }
    contents.sort();
    let manifest = SkillManifest {
        id: input.id.trim().to_string(),
        name: input.name.trim().to_string(),
        author: if input.author.trim().is_empty() {
            default_author()
        } else {
            input.author.trim().to_string()
        },
        categories: input.categories,
        version: input.version.trim().to_string(),
        scope: SkillScope::Organization,
        description: input.description.trim().to_string(),
        icon: String::new(),
        release_notes: input.release_notes.trim().to_string(),
        min_agent_version: input.min_agent_version.trim().to_string(),
        supported_clients: input.supported_clients,
        capabilities: input.capabilities,
        plugin_dependencies: input.plugin_dependencies,
        risk_summary: input.risk_summary.trim().to_string(),
        contents,
    };
    validate_skill_manifest(&manifest)?;
    let previous = read(&manifest.id, &manifest.version).ok();
    let root = draft_version_root(&manifest.id, &manifest.version);
    let package_root = root.join("package");
    if package_root.exists() {
        fs::remove_dir_all(&package_root)?;
    }
    fs::create_dir_all(&package_root)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(package_root.join("skill.json"), &manifest_bytes)?;
    fs::write(package_root.join("SKILL.md"), input.readme.as_bytes())?;
    for (path, content) in &supplemental_files {
        let target = package_root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, content.as_bytes())?;
    }
    let mut package_files = BTreeMap::from([
        ("SKILL.md".to_string(), input.readme.as_bytes().to_vec()),
        ("skill.json".to_string(), manifest_bytes.clone()),
    ]);
    package_files.extend(
        supplemental_files
            .iter()
            .map(|(path, content)| (path.clone(), content.as_bytes().to_vec())),
    );
    let checksums = package_checksums(&package_files);
    fs::write(package_root.join("checksums.sha256"), checksums.as_bytes())?;
    let candidate_path = root.join(format!("{}-{}.hmskill", manifest.id, manifest.version));
    build_archive(&candidate_path, &package_files, checksums.as_bytes())?;
    let candidate_sha256 = sha256_file(&candidate_path)?;
    let unchanged = previous
        .as_ref()
        .map(|draft| draft.candidate_sha256 == candidate_sha256)
        .unwrap_or(false);
    let draft = AuthoringDraft {
        manifest,
        readme: input.readme,
        files: supplemental_files,
        candidate_path,
        candidate_sha256,
        workspace_path: Some(package_root.clone()),
        source: "local_workspace".to_string(),
        revision_of: None,
        parent_submission_id: None,
        tested_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.tested_at.clone()),
        confirmed_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.confirmed_at.clone()),
        submitted_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.submitted_at.clone()),
        dashboard_draft_id: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.dashboard_draft_id.clone()),
        codex_target: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.codex_target.clone()),
        client_targets: previous
            .as_ref()
            .filter(|_| unchanged)
            .map(|value| value.client_targets.clone())
            .unwrap_or_default(),
        test_report: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.test_report.clone()),
        updated_at: now_stamp(),
    };
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn import_package(input: SkillPackageInput) -> Result<AuthoringDraft, Box<dyn Error>> {
    let source = input.package_path.canonicalize()?;
    if !source
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("hmskill") || value.eq_ignore_ascii_case("zip")
        })
    {
        return Err("Skill 候选包必须使用 .hmskill 或 .zip 扩展名".into());
    }
    if fs::metadata(&source)?.len() > MAX_SKILL_ARCHIVE_BYTES {
        return Err("Skill 候选包超过 16 MiB".into());
    }

    let staging = drafts_root().join(format!(".import-{}", now_stamp()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let import_result = (|| -> Result<AuthoringDraft, Box<dyn Error>> {
        crate::app::skill_manager::extract_archive(&source, &staging)?;
        crate::skill::manifest::normalize_standard_package(&staging)?;
        crate::app::skill_manager::verify_checksums(&staging)?;
        crate::app::skill_manager::verify_declared_contents(&staging)?;
        let manifest = crate::skill::manifest::validate_skill_package_root(&staging)?;
        let himind_package = source
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("hmskill"));
        if himind_package && manifest.release_notes.trim().is_empty() {
            return Err("请在 skill.json 中填写本版本更新说明 release_notes".into());
        }
        if himind_package && manifest.author.trim().is_empty() {
            return Err("请在 skill.json 中填写作者 author".into());
        }

        let candidate_sha256 = sha256_file(&source)?;
        let previous = read(&manifest.id, &manifest.version).ok();
        crate::app::extension_activation::ensure_workspace_identity(
            previous
                .as_ref()
                .and_then(|draft| draft.workspace_path.as_deref()),
            source.parent().unwrap_or(source.as_path()),
            crate::app::extension_activation::ActivationKind::Skill,
        )?;
        let unchanged = previous
            .as_ref()
            .is_some_and(|draft| draft.candidate_sha256 == candidate_sha256);
        let root = draft_version_root(&manifest.id, &manifest.version);
        fs::create_dir_all(&root)?;
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("hmskill");
        let candidate_path = root.join(format!(
            "{}-{}.{}",
            manifest.id, manifest.version, extension
        ));
        let same_candidate = candidate_path
            .canonicalize()
            .is_ok_and(|path| path == source);
        if !same_candidate {
            fs::copy(&source, &candidate_path)?;
        }
        let package_root = root.join("package");
        if package_root.exists() {
            fs::remove_dir_all(&package_root)?;
        }
        fs::rename(&staging, &package_root)?;
        let readme = fs::read_to_string(package_root.join("SKILL.md"))?;
        let mut files = BTreeMap::new();
        for relative in &manifest.contents {
            if matches!(relative.as_str(), "skill.json" | "SKILL.md") {
                continue;
            }
            files.insert(
                relative.clone(),
                String::from_utf8_lossy(&fs::read(package_root.join(relative))?).into_owned(),
            );
        }
        let draft = AuthoringDraft {
            manifest,
            readme,
            files,
            candidate_path,
            candidate_sha256,
            workspace_path: source.parent().map(Path::to_path_buf),
            source: "local_package".to_string(),
            revision_of: normalize_optional(input.revision_of_version),
            parent_submission_id: normalize_optional(input.parent_submission_id),
            tested_at: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.tested_at.clone()),
            confirmed_at: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.confirmed_at.clone()),
            submitted_at: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.submitted_at.clone()),
            dashboard_draft_id: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.dashboard_draft_id.clone()),
            codex_target: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.codex_target.clone()),
            client_targets: previous
                .as_ref()
                .filter(|_| unchanged)
                .map(|value| value.client_targets.clone())
                .unwrap_or_default(),
            test_report: previous
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|value| value.test_report.clone()),
            updated_at: now_stamp(),
        };
        persist(&draft)?;
        Ok(draft)
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(staging);
    }
    import_result
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
}

pub(crate) fn create_revision(
    skill_id: &str,
    version: &str,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    let previous = read(skill_id, version)?;
    let next_version = bump_patch(version);
    if read(skill_id, &next_version).is_ok() {
        return Err(format!("Skill 版本已存在: {next_version}").into());
    }
    let input = SkillDraftInput {
        id: previous.manifest.id.clone(),
        name: previous.manifest.name.clone(),
        author: previous.manifest.author.clone(),
        categories: previous.manifest.categories.clone(),
        version: next_version,
        description: previous.manifest.description.clone(),
        release_notes: format!("基于 v{version} 的功能改进与问题修复。"),
        min_agent_version: previous.manifest.min_agent_version.clone(),
        supported_clients: previous.manifest.supported_clients.clone(),
        capabilities: previous.manifest.capabilities.clone(),
        plugin_dependencies: previous.manifest.plugin_dependencies.clone(),
        risk_summary: previous.manifest.risk_summary.clone(),
        readme: previous.readme.clone(),
        files: previous.files.clone(),
    };
    let mut draft = save(input)?;
    draft.source = "revision".to_string();
    draft.revision_of = Some(version.to_string());
    draft.parent_submission_id = previous.dashboard_draft_id;
    draft.tested_at = None;
    draft.confirmed_at = None;
    draft.submitted_at = None;
    draft.dashboard_draft_id = None;
    draft.test_report = None;
    persist(&draft)?;
    Ok(draft)
}

fn bump_patch(version: &str) -> String {
    let mut p = version.split('.').map(|v| v.parse::<u64>().unwrap_or(0));
    format!(
        "{}.{}.{}",
        p.next().unwrap_or(0),
        p.next().unwrap_or(0),
        p.next().unwrap_or(0) + 1
    )
}

fn default_author() -> String {
    "未授权用户".to_string()
}

pub(crate) fn read(skill_id: &str, version: &str) -> Result<AuthoringDraft, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    if version.trim().is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err("Skill 版本无效".into());
    }
    let path = draft_version_root(skill_id, version).join("draft.json");
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub(crate) fn associate_workspace(
    mut draft: AuthoringDraft,
    workspace: &Path,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    draft.workspace_path = Some(workspace.canonicalize()?);
    draft.source = "workspace".to_string();
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn test(
    skill_id: &str,
    version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<AuthoringTestResult, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    let mut client_readiness = BTreeMap::new();
    // 按原因归并：一条依赖缺失会在每个客户端上各报一次，逐条铺开就是八行同义句。
    let mut readiness_blockers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for client_id in crate::skill::active_client_ids_for_manifest(&draft.manifest) {
        if client_readiness.contains_key(&client_id) {
            continue;
        }
        let readiness =
            SkillReadiness::resolve(&draft.manifest, capability_facts, VERSION, &client_id);
        if readiness.state == "blocked" {
            for reason in &readiness.reasons {
                readiness_blockers
                    .entry(reason.clone())
                    .or_insert_with(Vec::new)
                    .push(client_id.clone());
            }
        }
        client_readiness.insert(client_id, readiness);
    }
    if client_readiness.is_empty() {
        return Err("Skill 未声明支持的 AI 客户端".into());
    }
    let plugin_issues = plugin_dependency_issues(&draft.manifest.plugin_dependencies);
    if !readiness_blockers.is_empty() || !plugin_issues.is_empty() {
        let client_total = client_readiness.len().max(1);
        let mut parts: Vec<String> = readiness_blockers
            .iter()
            .map(|(reason, clients)| {
                let scope = if clients.len() >= client_total {
                    "全部客户端".to_string()
                } else {
                    let mut names = clients.clone();
                    names.sort();
                    let head: Vec<String> = names.iter().take(3).cloned().collect();
                    format!(
                        "{}{}",
                        head.join("、"),
                        if names.len() > head.len() { " 等" } else { "" }
                    )
                };
                format!("{scope}：{reason}")
            })
            .collect();
        parts.extend(plugin_issues);
        return Err(format!("Skill 本地预检未通过: {}", parts.join("；")).into());
    }
    let snapshot = SkillTestSnapshot::capture(&draft.manifest, capability_facts)?;
    let test_result =
        (|| -> Result<(AuthoringDraft, BTreeMap<String, Value>, Value), Box<dyn Error>> {
            let package_root = draft_version_root(skill_id, version).join("package");
            let store = SkillStore::new();
            // Candidate tests must exercise the same installation scope that
            // the package declares. Organization-scoped skills come from the
            // marketplace, while user-scoped packages are the independent
            // mode/local-source distribution path.
            let record = match draft.manifest.scope {
                SkillScope::Organization => store.install_organization_package(
                    &package_root,
                    &draft.manifest.id,
                    &draft.manifest.version,
                )?,
                SkillScope::User => store.install_user_package(
                    &package_root,
                    &draft.manifest.id,
                    &draft.manifest.version,
                )?,
                SkillScope::Builtin => {
                    return Err("候选 Skill 不能使用 builtin scope".into());
                }
            };
            store.apply_management_policy(
                &draft.manifest.id,
                &SkillManagementPolicy {
                    management: "user_managed".to_string(),
                    source: "authoring_candidate".to_string(),
                    assignment_id: String::new(),
                    reason: "本机候选测试".to_string(),
                    allow_uninstall: true,
                },
            )?;
            let clients =
                crate::skill::sync_record_to_supported_clients(&record, VERSION, capability_facts)?;
            let codex = clients.get("codex").cloned().unwrap_or(Value::Null);
            let client_targets = clients
                .iter()
                .filter_map(|(client, result)| {
                    result
                        .get("target_root")
                        .and_then(Value::as_str)
                        .map(|target| (client.clone(), target.to_string()))
                })
                .collect::<BTreeMap<_, _>>();
            draft.tested_at = Some(now_stamp());
            draft.confirmed_at = None;
            draft.submitted_at = None;
            draft.dashboard_draft_id = None;
            draft.codex_target = codex
                .get("target_root")
                .and_then(Value::as_str)
                .map(str::to_string);
            draft.client_targets = client_targets;
            draft.updated_at = now_stamp();
            persist(&draft)?;

            let mut cleanup_failures = Vec::new();
            for client_id in crate::skill::uninstall_client_ids_for_record(&record) {
                if client_id == "himind-ai" {
                    continue;
                }
                if let Err(error) =
                    crate::skill::unregister_skill_client_json(&record.manifest.id, &client_id)
                {
                    cleanup_failures.push(format!("{client_id}: {error}"));
                }
            }
            let store_removed = SkillStore::new()
                .remove_installed_skill(&record.manifest.id)
                .unwrap_or(false);
            let cleanup = serde_json::json!({
                "store_removed": store_removed,
                "failures": cleanup_failures,
                "state": if cleanup_failures.is_empty() && store_removed { "passed" } else { "failed" }
            });
            Ok((draft, clients, cleanup))
        })();
    let restored = snapshot.restore();
    let (mut draft, clients, cleanup) = match (test_result, restored) {
        (Ok(value), Ok(())) => value,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(restore_error)) => {
            return Err(
                format!("Skill 候选测试失败且恢复原状态失败: {error}; {restore_error}").into(),
            )
        }
    };
    if cleanup.get("state").and_then(Value::as_str) != Some("passed") {
        return Err(format!(
            "Skill 候选测试清理未通过: {}",
            cleanup
                .get("failures")
                .cloned()
                .unwrap_or_else(|| json!(["installed Skill 未从本地状态移除"]))
        )
        .into());
    }
    let codex = clients.get("codex").cloned().unwrap_or(Value::Null);
    let readiness = client_readiness
        .get("himind-ai")
        .or_else(|| client_readiness.get("codex"))
        .or_else(|| client_readiness.values().next())
        .cloned()
        .ok_or("Skill 未声明支持的 AI 客户端")?;
    let tested_at = draft.tested_at.clone().unwrap_or_else(now_stamp);
    let test_report = json!({
        "manifest": "passed",
        "dependencies": "passed",
        "package": "passed",
        "install": "passed",
        "registry": "passed",
        "mcp_contract": "passed",
        "client_registration": "passed",
        "lifecycle": {
            "registered": true,
            "unregistered": true,
            "state": "passed"
        },
        "cleanup": cleanup.clone(),
        "candidate_sha256": draft.candidate_sha256.clone(),
        "agent_version": VERSION,
        "tested_at": tested_at,
        "built_at": draft.updated_at,
        "codex_target": draft.codex_target.clone(),
        "client_targets": draft.client_targets.clone(),
    });
    draft.test_report = Some(test_report);
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(AuthoringTestResult {
        draft,
        readiness,
        client_readiness,
        plugin_issues,
        codex,
        clients,
        cleanup: serde_json::json!({
            "state": cleanup.get("state").cloned().unwrap_or(Value::String("failed".to_string())),
            "store_removed": cleanup.get("store_removed").cloned().unwrap_or(Value::Bool(false)),
            "failures": cleanup.get("failures").cloned().unwrap_or_else(|| json!([])),
            "existing_state_restored": true,
            "restored": true,
        }),
    })
}

struct SkillTestSnapshot {
    skill_id: String,
    skill_root: PathBuf,
    skill_backup: Option<PathBuf>,
    rendered_backups: Vec<(PathBuf, PathBuf)>,
    lock_entry: Option<crate::app::extension_lock::ExtensionLockEntry>,
    development_registry_path: PathBuf,
    development_registry_content: Option<Vec<u8>>,
}

impl SkillTestSnapshot {
    fn capture(
        manifest: &SkillManifest,
        capability_facts: &[CapabilityFact],
    ) -> Result<Self, Box<dyn Error>> {
        let store = SkillStore::new();
        store.bootstrap_builtin_skills()?;
        let skill_root = store.skill_root_for_scope(&manifest.scope, &manifest.id);
        let mut rendered_paths = Vec::new();
        if let Ok(status) = crate::skill::client_status_json(VERSION, capability_facts) {
            if let Some(clients) = status.as_object() {
                for (client_id, client_status) in clients {
                    if client_id == "himind-ai" {
                        continue;
                    }
                    let Some(items) = client_status.get("items").and_then(Value::as_array) else {
                        continue;
                    };
                    if let Some(rendered) = items.iter().find_map(|item| {
                        let matches = item.pointer("/record/manifest/id").and_then(Value::as_str)
                            == Some(manifest.id.as_str());
                        matches
                            .then(|| item.get("rendered_root").and_then(Value::as_str))
                            .flatten()
                    }) {
                        let rendered = PathBuf::from(rendered);
                        if rendered.is_dir() && rendered.join(".himind-render.json").is_file() {
                            rendered_paths.push(rendered);
                        }
                    }
                }
            }
        }
        let skill_backup = if skill_root.exists() {
            let backup =
                skill_root.with_file_name(format!(".{}.test-backup-{}", manifest.id, now_stamp()));
            fs::rename(&skill_root, &backup)?;
            Some(backup)
        } else {
            None
        };

        let mut rendered_backups: Vec<(PathBuf, PathBuf)> = Vec::new();
        for rendered in rendered_paths {
            let backup =
                rendered.with_file_name(format!(".{}.test-backup-{}", manifest.id, now_stamp()));
            if let Err(error) = fs::rename(&rendered, &backup) {
                for (original, previous) in rendered_backups.into_iter().rev() {
                    if previous.exists() {
                        let _ = fs::rename(previous, original);
                    }
                }
                if let Some(previous) = &skill_backup {
                    if previous.exists() {
                        let _ = fs::rename(previous, &skill_root);
                    }
                }
                return Err(error.into());
            }
            rendered_backups.push((rendered, backup));
        }

        Ok(Self {
            skill_id: manifest.id.clone(),
            skill_root,
            skill_backup,
            rendered_backups,
            lock_entry: crate::app::extension_lock::read("skill", &manifest.id)?,
            development_registry_path: crate::skill::development::registry_path(),
            development_registry_content: fs::read(crate::skill::development::registry_path()).ok(),
        })
    }

    fn restore(self) -> Result<(), Box<dyn Error>> {
        remove_candidate_rendered_copies(&self.skill_id)?;
        if self.skill_root.exists() {
            fs::remove_dir_all(&self.skill_root)?;
        }
        if let Some(backup) = self.skill_backup {
            if backup.exists() {
                fs::rename(backup, &self.skill_root)?;
            }
        }
        for (rendered, backup) in self.rendered_backups {
            if rendered.exists() {
                fs::remove_dir_all(&rendered)?;
            }
            if backup.exists() {
                fs::rename(backup, rendered)?;
            }
        }
        crate::app::extension_lock::restore("skill", &self.skill_id, self.lock_entry)?;
        if let Some(content) = self.development_registry_content {
            fs::write(&self.development_registry_path, content)?;
        } else if self.development_registry_path.exists() {
            fs::remove_file(&self.development_registry_path)?;
        }
        Ok(())
    }
}

fn remove_candidate_rendered_copies(skill_id: &str) -> Result<(), Box<dyn Error>> {
    let rendered_root = SkillStore::new().root().join("rendered");
    if !rendered_root.is_dir() {
        return Ok(());
    }
    for client_entry in fs::read_dir(rendered_root)?.flatten() {
        let client_root = client_entry.path();
        if !client_root.is_dir() {
            continue;
        }
        for skill_entry in fs::read_dir(client_root)?.flatten() {
            let candidate = skill_entry.path();
            if !candidate.is_dir()
                || candidate
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            let receipt_path = candidate.join(".himind-render.json");
            let matches = fs::read_to_string(receipt_path)
                .ok()
                .and_then(|content| serde_json::from_str::<Value>(&content).ok())
                .and_then(|value| {
                    value
                        .get("skill_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .is_some_and(|value| value == skill_id);
            if matches {
                fs::remove_dir_all(candidate)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn confirm(skill_id: &str, version: &str) -> Result<AuthoringDraft, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先安装到声明的 AI 客户端并完成本地测试".into());
    }
    draft.confirmed_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

/// Persist a tested Skill candidate as the current development build.  The
/// package is copied to a SHA-addressed slot so the draft workspace may be
/// edited or removed without changing the active version.
pub(crate) fn activate(
    skill_id: &str,
    version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<SkillActivationResult, Box<dyn Error>> {
    let draft = read(skill_id, version)?;
    let result = activate_draft_with_paths(
        draft,
        &development_versions_root(),
        &crate::skill::development::registry_path(),
        &ActivationStore::new(activation_store_path()),
        capability_facts,
    )?;
    persist(&result.draft)?;
    Ok(result)
}

pub(crate) fn rollback(
    skill_id: &str,
    workspace_root: &Path,
    capability_facts: &[CapabilityFact],
) -> Result<crate::app::extension_activation::ActivationRollbackResult, Box<dyn Error>> {
    let store = ActivationStore::new(activation_store_path());
    let target = resolve_activation_target(
        &crate::store::paths::profile_name(),
        workspace_root,
        &[],
        None,
    )?;
    let active = store
        .active_for(&target, ActivationKind::Skill, skill_id)?
        .filter(|receipt| receipt.status == ActivationStatus::Active)
        .ok_or("Skill 没有可回滚的开发激活记录")?;
    crate::app::extension_activation::ensure_receipt_workspace(&active, workspace_root)?;
    let previous_sha = active
        .previous_sha256
        .clone()
        .ok_or("Skill 没有可回滚的上一开发版")?;
    let slot = development_versions_root()
        .join(skill_id)
        .join(&previous_sha);
    let manifest = crate::skill::manifest::validate_skill_package_root(&slot)?;
    let record = crate::skill::development::record_at(&slot).ok_or("Skill 上一开发版无法读取")?;
    let registry_path = crate::skill::development::registry_path();
    let registry_before = fs::read(&registry_path).ok();
    let timestamp = now_stamp();
    let mut receipt = ActivationReceipt {
        schema_version: crate::app::extension_activation::EXTENSION_ACTIVATION_SCHEMA_VERSION
            .to_string(),
        kind: ActivationKind::Skill,
        extension_id: skill_id.to_string(),
        version: manifest.version,
        candidate_sha256: previous_sha,
        project_id: active.project_id.clone(),
        workspace_key: active.workspace_key.clone(),
        target: active.target.clone(),
        status: ActivationStatus::Active,
        previous_sha256: Some(active.candidate_sha256.clone()),
        surfaces: vec![
            ActivationSurface::visible("runtime"),
            ActivationSurface::visible("mcp"),
        ],
        activated_at: timestamp.clone(),
        updated_at: timestamp,
    };
    if let Err(error) = crate::skill::development::activate_skill_at(&slot, &registry_path) {
        restore_registry_file(&registry_path, registry_before.as_deref());
        return Err(error);
    }
    let clients = receipt.target.clients.clone();
    if !clients.is_empty() {
        match crate::skill::sync_development_record_to_clients(
            &record,
            VERSION,
            capability_facts,
            Some(&clients),
        ) {
            Ok(rendered) => {
                for client in rendered.keys() {
                    receipt
                        .surfaces
                        .push(ActivationSurface::visible(format!("client:{client}")));
                }
            }
            Err(error) => {
                receipt.status = ActivationStatus::Partial;
                receipt.surfaces.push(ActivationSurface {
                    id: "clients".to_string(),
                    state: crate::app::extension_activation::ActivationSurfaceState::Failed,
                    detail: error.to_string(),
                });
            }
        }
    }
    let mut transaction = match store.stage(&receipt) {
        Ok(transaction) => transaction,
        Err(error) => {
            restore_registry_file(&registry_path, registry_before.as_deref());
            return Err(error);
        }
    };
    if let Err(error) = store.commit(&mut transaction) {
        let _ = store.rollback(&mut transaction);
        restore_registry_file(&registry_path, registry_before.as_deref());
        return Err(error);
    }
    Ok(crate::app::extension_activation::ActivationRollbackResult {
        activation: receipt,
    })
}

fn activate_draft_with_paths(
    mut draft: AuthoringDraft,
    versions_root: &Path,
    registry_path: &Path,
    activation_store: &ActivationStore,
    capability_facts: &[CapabilityFact],
) -> Result<SkillActivationResult, Box<dyn Error>> {
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先安装到声明的 AI 客户端并完成本地测试".into());
    }
    let package_root =
        draft_version_root(&draft.manifest.id, &draft.manifest.version).join("package");
    let manifest = crate::skill::manifest::validate_skill_package_root(&package_root)?;
    if manifest.id != draft.manifest.id || manifest.version != draft.manifest.version {
        return Err("Skill 候选包身份与草稿不一致".into());
    }
    crate::app::skill_manager::verify_checksums(&package_root)?;
    crate::app::skill_manager::verify_declared_contents(&package_root)?;

    let slot = versions_root
        .join(&draft.manifest.id)
        .join(&draft.candidate_sha256);
    let staging = versions_root
        .join(&draft.manifest.id)
        .join(format!(".staging-{}", now_stamp()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let staged = (|| -> Result<(), Box<dyn Error>> {
        copy_package_tree(&package_root, &staging)?;
        crate::app::skill_manager::verify_checksums(&staging)?;
        crate::skill::manifest::validate_skill_package_root(&staging)?;
        if slot.exists() {
            fs::remove_dir_all(&slot)?;
        }
        if let Some(parent) = slot.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&staging, &slot)?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let workspace = draft
        .workspace_path
        .as_deref()
        .or_else(|| package_root.parent())
        .ok_or("Skill 候选缺少工作区")?;
    let workspace_key = crate::extension_projects::workspace_key_of(workspace);
    let target = resolve_activation_target(
        &crate::store::paths::profile_name(),
        workspace,
        &draft.client_targets.keys().cloned().collect::<Vec<_>>(),
        None,
    )?;
    let previous = activation_store
        .active_for(&target, ActivationKind::Skill, &draft.manifest.id)?
        .filter(|receipt| receipt.status == ActivationStatus::Active);
    let previous_record = previous.as_ref().and_then(|receipt| {
        crate::skill::development::record_at(
            &versions_root
                .join(&draft.manifest.id)
                .join(&receipt.candidate_sha256),
        )
    });
    let previous_sha256 = previous.map(|item| item.candidate_sha256);
    let now = now_stamp();
    let mut receipt = ActivationReceipt {
        schema_version: crate::app::extension_activation::EXTENSION_ACTIVATION_SCHEMA_VERSION
            .to_string(),
        kind: ActivationKind::Skill,
        extension_id: draft.manifest.id.clone(),
        version: draft.manifest.version.clone(),
        candidate_sha256: draft.candidate_sha256.clone(),
        project_id: format!("skill:{}@{}", draft.manifest.id, workspace_key),
        workspace_key,
        target,
        status: ActivationStatus::Active,
        previous_sha256,
        surfaces: vec![
            ActivationSurface::visible("runtime"),
            ActivationSurface::visible("mcp"),
        ],
        activated_at: now.clone(),
        updated_at: now,
    };
    let registry_before = fs::read(registry_path).ok();
    let active = crate::skill::development::activate_skill_at(&slot, registry_path);
    if let Err(error) = active {
        restore_registry_file(registry_path, registry_before.as_deref());
        let _ = fs::remove_dir_all(&slot);
        return Err(error);
    }
    let record = crate::skill::development::record_at(&slot).ok_or("Skill 开发槽写入后无法读取")?;
    let clients = receipt.target.clients.clone();
    if !clients.is_empty() {
        match crate::skill::sync_development_record_to_clients(
            &record,
            VERSION,
            capability_facts,
            Some(&clients),
        ) {
            Ok(rendered) => {
                for client in rendered.keys() {
                    receipt
                        .surfaces
                        .push(ActivationSurface::visible(format!("client:{client}")));
                }
            }
            Err(error) => {
                let surface_rollback = if let Some(previous_record) = previous_record.as_ref() {
                    crate::skill::sync_development_record_to_clients(
                        previous_record,
                        VERSION,
                        capability_facts,
                        Some(&clients),
                    )
                    .map(|_| ())
                } else {
                    crate::skill::remove_development_record_from_clients(&record, &clients)
                        .map(|_| ())
                };
                let registry_rollback =
                    try_restore_registry_file(registry_path, registry_before.as_deref());
                let _ = fs::remove_dir_all(&slot);
                return match (surface_rollback, registry_rollback) {
                    (Ok(()), Ok(())) => {
                        Err(format!(
                            "Skill 客户端投影失败，已回滚开发版投影与激活指针: {error}"
                        )
                        .into())
                    }
                    (surface, registry) => Err(format!(
                        "Skill 客户端投影失败且自动回滚不完整: {error}; clients={surface:?}; registry={registry:?}"
                    )
                    .into()),
                };
            }
        }
    }
    let mut transaction = match activation_store.stage(&receipt) {
        Ok(transaction) => transaction,
        Err(error) => {
            restore_registry_file(registry_path, registry_before.as_deref());
            let _ = fs::remove_dir_all(&slot);
            return Err(error);
        }
    };
    if let Err(error) = activation_store.commit(&mut transaction) {
        let rollback = activation_store.rollback(&mut transaction);
        restore_registry_file(registry_path, registry_before.as_deref());
        let _ = fs::remove_dir_all(&slot);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => {
                Err(format!("Skill 激活台账提交失败且回滚失败: {error}; {rollback_error}").into())
            }
        };
    }
    draft.updated_at = now_stamp();
    Ok(SkillActivationResult {
        draft,
        activation: receipt,
        development_path: slot,
    })
}

fn restore_registry_file(path: &Path, content: Option<&[u8]>) {
    match content {
        Some(content) => {
            let _ = crate::store::atomic_file::atomic_write(path, content);
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

fn try_restore_registry_file(path: &Path, content: Option<&[u8]>) -> Result<(), Box<dyn Error>> {
    match content {
        Some(content) => crate::store::atomic_file::atomic_write(path, content)?,
        None if path.exists() => fs::remove_file(path)?,
        None => {}
    }
    Ok(())
}

fn copy_package_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

pub(crate) fn mark_submitted(
    skill_id: &str,
    version: &str,
    dashboard_draft_id: &str,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_ready_to_submit(&draft)?;
    draft.submitted_at = Some(now_stamp());
    draft.dashboard_draft_id = Some(dashboard_draft_id.to_string());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn submit(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
    version: &str,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    let draft = read(skill_id, version)?;
    ensure_ready_to_submit(&draft)?;
    // 分发目标门禁：只有把工件交给组织工作台的项目才允许提审。
    crate::extension_projects::ensure_distribution_target(
        crate::extension_projects::ExtensionProjectKind::Skill,
        skill_id,
        crate::extension_contracts::DistributionTarget::Workbench,
    )?;
    if agent_id.trim().is_empty() {
        return Err("HiMind 账号尚未授权".into());
    }
    let access = crate::api::oauth::platform_access_token(
        options,
        crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
    )?;
    let report = draft.test_report.clone().unwrap_or_else(|| {
        serde_json::json!({
            "candidate_sha256": draft.candidate_sha256,
            "agent_version": VERSION,
            "built_at": draft.updated_at,
            "codex_target": draft.codex_target,
            "client_targets": draft.client_targets,
        })
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let source = crate::extension_projects::submission_source(
        crate::extension_projects::ExtensionProjectKind::Skill,
        skill_id,
    )?;
    let submitted = crate::api::distribution::submit_skill(
        &client,
        &options.api_base(),
        agent_id,
        &access.token,
        &draft.candidate_path,
        &report,
        draft.revision_of.as_deref(),
        &source,
    )?;
    let dashboard_draft_id = submitted
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or("Dashboard 未返回 Skill 审核记录")?;
    mark_submitted(skill_id, version, dashboard_draft_id)
}

pub(crate) fn ensure_ready_to_submit(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    ensure_candidate_unchanged(draft)?;
    if draft.tested_at.is_none() {
        return Err("Skill 候选包尚未完成测试".into());
    }
    if draft.confirmed_at.is_none() {
        return Err("Skill Candidate 尚未确认".into());
    }
    Ok(())
}

fn plugin_dependency_issues(dependencies: &[SkillPluginDependency]) -> Vec<String> {
    dependencies
        .iter()
        .filter_map(|dependency| {
            if !dependency.required {
                return None;
            }
            let plugin = match crate::capability::plugin::find_plugin(&dependency.plugin_id) {
                Ok(Some(plugin)) if plugin.enabled && plugin.error.is_none() => plugin,
                Ok(Some(_)) => {
                    return Some(format!("必需插件 {} 当前不可用", dependency.plugin_id));
                }
                Ok(None) => {
                    return Some(format!("缺少必需插件 {}", dependency.plugin_id));
                }
                Err(error) => {
                    return Some(format!(
                        "读取必需插件 {} 失败: {error}",
                        dependency.plugin_id
                    ));
                }
            };
            if plugin.version.is_empty() {
                return Some(format!("缺少必需插件 {}", dependency.plugin_id));
            }
            if let Some(minimum) = dependency.min_version.as_deref() {
                if compare_versions(&plugin.version, minimum) == Ordering::Less {
                    return Some(format!(
                        "插件 {} 版本低于 {}",
                        dependency.plugin_id, minimum
                    ));
                }
            }
            None
        })
        .collect()
}

fn persist(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    let root = draft_version_root(&draft.manifest.id, &draft.manifest.version);
    fs::create_dir_all(&root)?;
    fs::write(root.join("draft.json"), serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}

fn ensure_candidate_unchanged(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    if sha256_file(&draft.candidate_path)? != draft.candidate_sha256 {
        return Err("Skill 候选包已变化，请重新保存并测试".into());
    }
    Ok(())
}

fn build_archive(
    target: &Path,
    files: &BTreeMap<String, Vec<u8>>,
    checksums: &[u8],
) -> Result<(), Box<dyn Error>> {
    let file = File::create(target)?;
    let mut writer = zip::ZipWriter::new(file);
    // Candidate hashes are used for confirmation and submission identity.
    // ZIP metadata must therefore be reproducible across repeated saves.
    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    for (name, content) in files
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_slice()))
        .chain(std::iter::once(("checksums.sha256", checksums)))
    {
        writer.start_file(name, options)?;
        writer.write_all(content)?;
    }
    writer.finish()?;
    Ok(())
}

fn package_checksums(files: &BTreeMap<String, Vec<u8>>) -> String {
    files
        .iter()
        .map(|(name, content)| format!("{:x}  {name}\n", Sha256::digest(content)))
        .collect()
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn drafts_root() -> PathBuf {
    if let Some(root) = env::var_os("HIMIND_SKILL_DRAFTS_DIR") {
        return PathBuf::from(root);
    }
    crate::store::paths::agent_home().join("skill-drafts")
}

fn development_versions_root() -> PathBuf {
    if let Some(root) = env::var_os("HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR") {
        return PathBuf::from(root);
    }
    crate::store::paths::agent_home().join("skill-development-versions")
}

fn activation_store_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSION_ACTIVATION_STORE") {
        return PathBuf::from(path);
    }
    crate::store::paths::agent_home()
        .join("data")
        .join("extension-activations.json")
}

fn draft_version_root(skill_id: &str, version: &str) -> PathBuf {
    drafts_root().join(skill_id).join(version)
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn default_agent_version() -> String {
    VERSION.to_string()
}
fn default_clients() -> Vec<String> {
    vec![crate::skill::clients::PORTABLE_PROFILE_ID.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;

    fn input(readme: &str) -> SkillDraftInput {
        SkillDraftInput {
            id: "com.himind.skill.authoring-test".to_string(),
            name: "Authoring Test".to_string(),
            author: "马宝全".to_string(),
            categories: vec!["开发工具".to_string()],
            version: "1.0.0".to_string(),
            description: "Local authoring test".to_string(),
            release_notes: "新增本地 Skill 候选测试。".to_string(),
            min_agent_version: VERSION.to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            readme: readme.to_string(),
            files: BTreeMap::new(),
        }
    }

    #[test]
    fn creates_deterministic_candidate_and_invalidates_confirmation_after_edit() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!("himind-authoring-test-{}", now_stamp()));
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", &root);
        let mut first = save(input("# Demo")).unwrap();
        assert!(first.candidate_path.exists());
        first.tested_at = Some("tested".to_string());
        first.confirmed_at = Some("confirmed".to_string());
        persist(&first).unwrap();
        let unchanged = save(input("# Demo")).unwrap();
        assert_eq!(unchanged.candidate_sha256, first.candidate_sha256);
        assert!(unchanged.confirmed_at.is_some());
        let changed = save(input("# Demo\n\nChanged")).unwrap();
        assert_ne!(changed.candidate_sha256, first.candidate_sha256);
        assert!(changed.tested_at.is_none());
        assert!(changed.confirmed_at.is_none());
        let mut with_metadata = input("# Demo with metadata");
        with_metadata.files.insert(
            "agents/openai.yaml".to_string(),
            "interface:\n  display_name: \"候选测试\"\n".to_string(),
        );
        let metadata = save(with_metadata).unwrap();
        assert!(metadata
            .manifest
            .contents
            .contains(&"agents/openai.yaml".to_string()));
        assert!(
            draft_version_root(&metadata.manifest.id, &metadata.manifest.version)
                .join("package/agents/openai.yaml")
                .exists()
        );
        let imported = import_package(SkillPackageInput {
            package_path: metadata.candidate_path.clone(),
            revision_of_version: Some("0.9.0".to_string()),
            parent_submission_id: Some("submission-parent".to_string()),
        })
        .unwrap();
        assert_eq!(imported.candidate_sha256, metadata.candidate_sha256);
        assert_eq!(imported.revision_of.as_deref(), Some("0.9.0"));
        assert_eq!(
            imported.parent_submission_id.as_deref(),
            Some("submission-parent")
        );
        assert_eq!(imported.manifest.author, "马宝全");
        let revision = create_revision(&metadata.manifest.id, &metadata.manifest.version).unwrap();
        assert_eq!(revision.manifest.version, "1.0.1");
        assert_eq!(revision.revision_of.as_deref(), Some("1.0.0"));
        assert!(revision.tested_at.is_none());
        let mut locked = metadata.clone();
        locked.submitted_at = Some("submitted".to_string());
        persist(&locked).unwrap();
        let mut overwrite = input("# changed after submission");
        overwrite.files = locked.files.clone();
        let rebuilt = save(overwrite).unwrap();
        assert_ne!(rebuilt.candidate_sha256, locked.candidate_sha256);
        assert!(rebuilt.submitted_at.is_none());
        assert!(rebuilt.dashboard_draft_id.is_none());
        env::remove_var("HIMIND_SKILL_DRAFTS_DIR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_restores_previous_skill_sha_and_registry() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!("himind-skill-rollback-test-{}", now_stamp()));
        let versions = root.join("development-versions");
        let registry = root.join("skill-development.json");
        let activation_store = root.join("activations.json");
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", root.join("drafts"));
        env::set_var("HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR", &versions);
        env::set_var("HIMIND_SKILL_DEVELOPMENT_REGISTRY", &registry);
        env::set_var("HIMIND_EXTENSION_ACTIVATION_STORE", &activation_store);

        let store = ActivationStore::new(&activation_store);
        let mut shas = Vec::new();
        for readme in ["# First build", "# Second build"] {
            let mut draft = save(input(readme)).unwrap();
            draft.tested_at = Some(now_stamp());
            persist(&draft).unwrap();
            shas.push(draft.candidate_sha256.clone());
            activate_draft_with_paths(draft, &versions, &registry, &store, &[]).unwrap();
        }

        let draft = read(&input("# current").id, "1.0.0").unwrap();
        let workspace = draft.workspace_path.as_deref().unwrap();
        let target =
            resolve_activation_target(&crate::store::paths::profile_name(), workspace, &[], None)
                .unwrap();
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Skill, &draft.manifest.id)
                .unwrap()
                .unwrap()
                .candidate_sha256,
            shas[1]
        );

        let rolled_back = rollback(&draft.manifest.id, workspace, &[]).unwrap();
        assert_eq!(rolled_back.activation.candidate_sha256, shas[0]);
        let registry_value: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        let active_path = PathBuf::from(registry_value[0]["path"].as_str().unwrap());
        assert_eq!(
            active_path.canonicalize().unwrap(),
            versions
                .join(&draft.manifest.id)
                .join(&shas[0])
                .canonicalize()
                .unwrap()
        );
        assert!(crate::skill::development::record_at(&active_path).is_some());
        assert!(rolled_back
            .activation
            .surfaces
            .iter()
            .any(|surface| surface.id == "runtime"));

        for key in [
            "HIMIND_SKILL_DRAFTS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            "HIMIND_EXTENSION_ACTIVATION_STORE",
        ] {
            env::remove_var(key);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_client_projection_restores_previous_skill_and_registry() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!("s-{}-{}", std::process::id(), now_stamp()));
        let drafts = root.join("drafts");
        let versions = root.join("development-versions");
        let registry = root.join("skill-development.json");
        let activation_store = root.join("activations.json");
        let codex_skills = root.join("codex-skills");
        env::set_var("HIMIND_AGENT_HOME", root.join("home"));
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", &drafts);
        env::set_var("HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR", &versions);
        env::set_var("HIMIND_SKILL_DEVELOPMENT_REGISTRY", &registry);
        env::set_var("HIMIND_EXTENSION_ACTIVATION_STORE", &activation_store);
        env::set_var("HIMIND_CODEX_SKILL_DIR", &codex_skills);

        let store = ActivationStore::new(&activation_store);
        let mut first = save(input("# First projection")).unwrap();
        first.tested_at = Some(now_stamp());
        first.client_targets.insert(
            "codex".to_string(),
            codex_skills.to_string_lossy().to_string(),
        );
        persist(&first).unwrap();
        let first_activation =
            activate_draft_with_paths(first.clone(), &versions, &registry, &store, &[]).unwrap();

        let mut second = save(input("# Second projection")).unwrap();
        second.tested_at = Some(now_stamp());
        second.client_targets.insert(
            "codex".to_string(),
            codex_skills.to_string_lossy().to_string(),
        );
        persist(&second).unwrap();
        assert_eq!(
            store
                .active_for(
                    &first_activation.activation.target,
                    ActivationKind::Skill,
                    &first.manifest.id
                )
                .unwrap()
                .unwrap()
                .candidate_sha256,
            first_activation.activation.candidate_sha256
        );
        let registry_before_second_activation: Value =
            serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        assert!(registry_before_second_activation[0]["path"]
            .as_str()
            .unwrap()
            .contains(&first_activation.activation.candidate_sha256));
        env::set_var("HIMIND_TEST_FAIL_SKILL_SYNC_CLIENT", "himind-ai");
        let error = activate_draft_with_paths(second.clone(), &versions, &registry, &store, &[])
            .unwrap_err();
        assert!(error.to_string().contains("已回滚"));
        assert_eq!(
            store
                .active_for(
                    &first_activation.activation.target,
                    ActivationKind::Skill,
                    &first.manifest.id
                )
                .unwrap()
                .unwrap()
                .candidate_sha256,
            first_activation.activation.candidate_sha256
        );
        let registry_value: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        assert!(registry_value[0]["path"]
            .as_str()
            .unwrap()
            .contains(&first_activation.activation.candidate_sha256));
        let projected = walkdir::WalkDir::new(&codex_skills)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(projected.contains("First projection"));
        assert!(!projected.contains("Second projection"));
        assert!(!versions
            .join(&second.manifest.id)
            .join(&second.candidate_sha256)
            .exists());

        for key in [
            "HIMIND_AGENT_HOME",
            "HIMIND_SKILL_DRAFTS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            "HIMIND_EXTENSION_ACTIVATION_STORE",
            "HIMIND_CODEX_SKILL_DIR",
            "HIMIND_TEST_FAIL_SKILL_SYNC_CLIENT",
        ] {
            env::remove_var(key);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_first_projection_removes_partial_client_copy() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!("f-{}-{}", std::process::id(), now_stamp()));
        let versions = root.join("development-versions");
        let registry = root.join("skill-development.json");
        let activation_store = root.join("activations.json");
        let codex_skills = root.join("codex-skills");
        env::set_var("HIMIND_AGENT_HOME", root.join("home"));
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", root.join("drafts"));
        env::set_var("HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR", &versions);
        env::set_var("HIMIND_SKILL_DEVELOPMENT_REGISTRY", &registry);
        env::set_var("HIMIND_EXTENSION_ACTIVATION_STORE", &activation_store);
        env::set_var("HIMIND_CODEX_SKILL_DIR", &codex_skills);

        let mut draft = save(input("# First failed projection")).unwrap();
        draft.tested_at = Some(now_stamp());
        draft.client_targets.insert(
            "codex".to_string(),
            codex_skills.to_string_lossy().to_string(),
        );
        persist(&draft).unwrap();
        let store = ActivationStore::new(&activation_store);
        env::set_var("HIMIND_TEST_FAIL_SKILL_SYNC_CLIENT", "himind-ai");
        let error = activate_draft_with_paths(draft.clone(), &versions, &registry, &store, &[])
            .unwrap_err();
        assert!(error.to_string().contains("已回滚"));
        assert!(store
            .active_for(
                &resolve_activation_target(
                    &crate::store::paths::profile_name(),
                    &draft.workspace_path.clone().unwrap(),
                    &[],
                    None,
                )
                .unwrap(),
                ActivationKind::Skill,
                &draft.manifest.id,
            )
            .unwrap()
            .is_none());
        assert!(!codex_skills.join("authoring-test").exists());
        if registry.exists() {
            assert!(!fs::read_to_string(&registry)
                .unwrap()
                .contains(&versions.to_string_lossy().to_string()));
        }

        for key in [
            "HIMIND_AGENT_HOME",
            "HIMIND_SKILL_DRAFTS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            "HIMIND_EXTENSION_ACTIVATION_STORE",
            "HIMIND_CODEX_SKILL_DIR",
            "HIMIND_TEST_FAIL_SKILL_SYNC_CLIENT",
        ] {
            env::remove_var(key);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_refuses_to_overwrite_an_unmanaged_client_skill() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!("u-{}-{}", std::process::id(), now_stamp()));
        let versions = root.join("development-versions");
        let registry = root.join("skill-development.json");
        let activation_store = root.join("activations.json");
        let codex_skills = root.join("codex-skills");
        let user_skill = codex_skills.join("authoring-test");
        fs::create_dir_all(&user_skill).unwrap();
        let user_file = user_skill.join("SKILL.md");
        fs::write(&user_file, "user-owned skill").unwrap();
        env::set_var("HIMIND_AGENT_HOME", root.join("home"));
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", root.join("drafts"));
        env::set_var("HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR", &versions);
        env::set_var("HIMIND_SKILL_DEVELOPMENT_REGISTRY", &registry);
        env::set_var("HIMIND_EXTENSION_ACTIVATION_STORE", &activation_store);
        env::set_var("HIMIND_CODEX_SKILL_DIR", &codex_skills);

        let mut draft = save(input("# Managed candidate")).unwrap();
        draft.tested_at = Some(now_stamp());
        draft.client_targets.insert(
            "codex".to_string(),
            codex_skills.to_string_lossy().to_string(),
        );
        persist(&draft).unwrap();
        let store = ActivationStore::new(&activation_store);
        assert!(
            activate_draft_with_paths(draft.clone(), &versions, &registry, &store, &[]).is_err()
        );
        assert_eq!(fs::read_to_string(&user_file).unwrap(), "user-owned skill");
        assert!(store
            .active_for(
                &resolve_activation_target(
                    &crate::store::paths::profile_name(),
                    &draft.workspace_path.clone().unwrap(),
                    &[],
                    None,
                )
                .unwrap(),
                ActivationKind::Skill,
                &draft.manifest.id,
            )
            .unwrap()
            .is_none());
        if registry.exists() {
            assert!(!fs::read_to_string(&registry)
                .unwrap()
                .contains(&versions.to_string_lossy().to_string()));
        }

        for key in [
            "HIMIND_AGENT_HOME",
            "HIMIND_SKILL_DRAFTS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            "HIMIND_EXTENSION_ACTIVATION_STORE",
            "HIMIND_CODEX_SKILL_DIR",
        ] {
            env::remove_var(key);
        }
        let _ = fs::remove_dir_all(root);
    }
}
