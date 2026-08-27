use crate::capability::plugin::{parse_plugin_manifest, PluginManifest};
use crate::skill::manifest::load_skill_manifest;
use crate::skill::types::SkillManifest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DEVELOPMENT_TOOLS_PLUGIN_ID: &str = "com.himind.extension-development-tools";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionProjectKind {
    Plugin,
    Skill,
}

impl ExtensionProjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProjectRecord {
    id: String,
    kind: ExtensionProjectKind,
    extension_id: String,
    name: String,
    description: String,
    version: String,
    workspace_path: PathBuf,
    source: String,
    #[serde(default)]
    source_repository: String,
    #[serde(default)]
    source_default_branch: String,
    #[serde(default)]
    source_subdirectory: String,
    #[serde(default)]
    source_commit: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionProject {
    pub id: String,
    pub kind: ExtensionProjectKind,
    pub extension_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// UTF-8 display path for the Tauri/JSON boundary. Keep the internal
    /// registry as PathBuf so filesystem operations remain lossless.
    pub workspace_path: String,
    pub workspace_available: bool,
    pub source: String,
    pub source_repository: String,
    pub source_default_branch: String,
    pub source_subdirectory: String,
    pub source_commit: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExtensionProjectSourceInput {
    pub source_repository: String,
    #[serde(default)]
    pub source_default_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    #[serde(default)]
    pub source_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AssociateExtensionProjectInput {
    pub kind: ExtensionProjectKind,
    pub extension_id: String,
    #[serde(flatten)]
    pub source: ExtensionProjectSourceInput,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExtensionSubmissionSource {
    pub source_repository: String,
    pub source_default_branch: String,
    pub source_subdirectory: String,
    pub source_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateExtensionProjectInput {
    pub kind: ExtensionProjectKind,
    pub slug: String,
    #[serde(default)]
    pub extension_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "draft", rename_all = "snake_case")]
pub(crate) enum ExtensionCandidate {
    Plugin(crate::plugin_authoring::PluginDraft),
    Skill(crate::skill::authoring::AuthoringDraft),
}

pub(crate) fn list() -> Result<Vec<ExtensionProject>, Box<dyn Error>> {
    let path = registry_path();
    let mut records = read_records(&path)?;
    let mut changed = migrate_legacy_projects(&mut records);
    changed |= merge_shared_workspace_projects(&mut records);

    for record in &mut records {
        if !record.workspace_path.is_dir() {
            continue;
        }
        if let Ok(current) = project_record_from_path(&record.workspace_path, &record.source) {
            if record.extension_id == current.extension_id && record.kind == current.kind {
                if record.name != current.name
                    || record.description != current.description
                    || record.version != current.version
                {
                    record.name = current.name;
                    record.description = current.description;
                    record.version = current.version;
                    record.updated_at = current.updated_at;
                    changed = true;
                }
            }
        }
    }
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    if changed {
        write_records(&path, &records)?;
    }
    Ok(records.into_iter().map(ExtensionProject::from).collect())
}

/// Reconciles the selected aggregate Git workspace into the current Agent profile.
/// The source directory is shared, while this registry (and all drafts/candidates)
/// remains profile-local. Existing manually registered projects are preserved.
fn merge_shared_workspace_projects(records: &mut Vec<ProjectRecord>) -> bool {
    let mut changed = false;
    if !crate::extension_workspace::settings().valid {
        return false;
    }
    let discovered_items = crate::extension_workspace::discover();
    let discovered_ids: std::collections::HashSet<String> = discovered_items
        .iter()
        .map(|item| format!("{}:{}", item.kind, item.id))
        .collect();
    let before = records.len();
    records
        .retain(|record| record.source != "git_workspace" || discovered_ids.contains(&record.id));
    changed |= records.len() != before;
    for discovered in discovered_items {
        let Ok(mut candidate) = project_record_from_path(&discovered.path, "git_workspace") else {
            continue;
        };
        if candidate.extension_id != discovered.id {
            continue;
        }
        candidate.source_repository = discovered.source_repository.clone();
        candidate.source_default_branch = discovered.source_default_branch.clone();
        candidate.source_subdirectory = discovered.source_subdirectory.clone();
        let Some(existing) = records.iter_mut().find(|record| record.id == candidate.id) else {
            records.push(candidate);
            changed = true;
            continue;
        };
        // Do not replace a developer's explicitly opened workspace. Once a record
        // came from the shared catalog, keep its path synchronized with the catalog.
        if existing.source == "git_workspace" || existing.workspace_path == candidate.workspace_path
        {
            if existing.kind != candidate.kind
                || existing.extension_id != candidate.extension_id
                || existing.name != candidate.name
                || existing.description != candidate.description
                || existing.version != candidate.version
                || existing.workspace_path != candidate.workspace_path
                || existing.source != "git_workspace"
                || existing.source_repository != candidate.source_repository
                || existing.source_default_branch != candidate.source_default_branch
                || existing.source_subdirectory != candidate.source_subdirectory
            {
                let source_commit = existing.source_commit.clone();
                *existing = candidate;
                existing.source_commit = source_commit;
                changed = true;
            }
        }
    }
    changed
}

pub(crate) fn get(project_id: &str) -> Result<ExtensionProject, Box<dyn Error>> {
    Ok(find_record(project_id)?.into())
}

pub(crate) fn current_workspace() -> Result<Value, Box<dyn Error>> {
    let workspace = current_workspace_path()?;
    let project = project_record_from_path(&workspace, "ai_workspace")
        .ok()
        .map(ExtensionProject::from);
    let kind = project
        .as_ref()
        .map(|item| item.kind.as_str())
        .unwrap_or("directory");
    Ok(json!({
        "workspace_root": crate::extension_workspace::display_path(&workspace),
        "kind": kind,
        "project": project,
    }))
}

pub(crate) fn current_workspace_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(env::var_os("HIMIND_AI_WORKSPACE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?)
        .canonicalize()?)
}

pub(crate) fn register(path: &Path) -> Result<ExtensionProject, Box<dyn Error>> {
    let canonical = path.canonicalize()?;
    let registry = registry_path();
    let mut records = read_records(&registry)?;
    let mut record = project_record_from_path(&canonical, "local_workspace")?;
    if let Some(existing) = records.iter().find(|item| item.id == record.id) {
        record.source_repository = existing.source_repository.clone();
        record.source_default_branch = existing.source_default_branch.clone();
        record.source_subdirectory = existing.source_subdirectory.clone();
        record.source_commit = existing.source_commit.clone();
    }
    records.retain(|item| item.id != record.id);
    records.push(record.clone());
    write_records(&registry, &records)?;
    Ok(record.into())
}

pub(crate) fn associate(
    path: &Path,
    input: AssociateExtensionProjectInput,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let canonical = path.canonicalize()?;
    let detected = project_record_from_path(&canonical, "local_workspace")?;
    if detected.kind != input.kind || detected.extension_id != input.extension_id.trim() {
        return Err("所选目录与协作项目不匹配".into());
    }
    let project = register(&canonical)?;
    update_source(&project.id, input.source)
}

pub(crate) fn update_source(
    project_id: &str,
    input: ExtensionProjectSourceInput,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let path = registry_path();
    let mut records = read_records(&path)?;
    let record = records
        .iter_mut()
        .find(|record| record.id == project_id)
        .ok_or("扩展项目不存在")?;
    record.source_repository = input.source_repository.trim().to_string();
    record.source_default_branch = input.source_default_branch.trim().to_string();
    record.source_subdirectory = input.source_subdirectory.trim().replace('\\', "/");
    record.source_commit = input.source_commit.trim().to_string();
    record.updated_at = now_stamp();
    let output = ExtensionProject::from(record.clone());
    write_records(&path, &records)?;
    Ok(output)
}

pub(crate) fn submission_source(
    kind: ExtensionProjectKind,
    extension_id: &str,
) -> Result<ExtensionSubmissionSource, Box<dyn Error>> {
    let id = format!("{}:{}", kind.as_str(), extension_id.trim());
    let Some(record) = read_records(&registry_path())?
        .into_iter()
        .find(|record| record.id == id)
    else {
        return Ok(ExtensionSubmissionSource::default());
    };
    Ok(ExtensionSubmissionSource {
        source_repository: record.source_repository,
        source_default_branch: record.source_default_branch,
        source_subdirectory: record.source_subdirectory,
        source_commit: record.source_commit,
    })
}

pub(crate) fn create(
    parent: &Path,
    input: CreateExtensionProjectInput,
    author: &str,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let parent = parent.canonicalize()?;
    let slug = normalize_slug(&input.slug)?;
    let category = if input.category.trim().is_empty() {
        "software-engineering"
    } else {
        input.category.trim()
    };
    let release_notes = "创建初始版本。";
    let result = match input.kind {
        ExtensionProjectKind::Plugin => invoke_development_tool(
            "extension.plugin.scaffold",
            json!({
                "workspace_root": parent,
                "output_dir": parent,
                "name": slug,
                "display_name": input.name.trim(),
                "description": input.description.trim(),
                "author": author.trim(),
                "categories": [category],
                "release_notes": release_notes,
                "template": if input.template.trim().is_empty() { "readonly-tool" } else { input.template.trim() },
            }),
        )?,
        ExtensionProjectKind::Skill => invoke_development_tool(
            "extension.skill.scaffold",
            json!({
                "workspace_root": parent,
                "output_dir": parent,
                "slug": slug,
                "id": input.extension_id.trim(),
                "name": input.name.trim(),
                "version": "0.1.0",
                "min_agent_version": crate::VERSION,
                "description": input.description.trim(),
                "author": author.trim(),
                "categories": [category],
                "release_notes": release_notes,
                "supported_clients": ["agent-skills"],
            }),
        )?,
    };
    let root = result
        .get("root")
        .and_then(Value::as_str)
        .ok_or("扩展开发工具未返回项目目录")?;
    register(Path::new(root))
}

pub(crate) fn build(project_id: &str) -> Result<ExtensionCandidate, Box<dyn Error>> {
    let project = find_record(project_id)?;
    let workspace = project.workspace_path.canonicalize()?;
    // The commit is provenance metadata for Dashboard submissions; developers do not need to manage it.
    if !project.source_repository.trim().is_empty() {
        if let Some(commit) = git_head(&workspace) {
            let _ = update_source_commit(project_id, &commit);
        }
    }
    cleanup_temporary_candidates(&workspace);
    let extension = match project.kind {
        ExtensionProjectKind::Plugin => "hmpkg",
        ExtensionProjectKind::Skill => "hmskill",
    };
    let temporary = workspace.join(format!(".himind-candidate-{}.{}", now_stamp(), extension));
    let result = (|| -> Result<ExtensionCandidate, Box<dyn Error>> {
        if project.kind == ExtensionProjectKind::Plugin {
            invoke_development_tool(
                "extension.plugin.build",
                json!({"workspace_root": workspace, "path": workspace}),
            )?;
        }
        invoke_development_tool(
            match project.kind {
                ExtensionProjectKind::Plugin => "extension.plugin.package",
                ExtensionProjectKind::Skill => "extension.skill.package",
            },
            json!({"workspace_root": workspace, "path": workspace, "output": temporary}),
        )?;
        match project.kind {
            ExtensionProjectKind::Plugin => {
                let draft =
                    crate::plugin_authoring::save(crate::plugin_authoring::PluginDraftInput {
                        package_path: temporary.clone(),
                        revision_of_version: None,
                        parent_submission_id: None,
                    })?;
                Ok(ExtensionCandidate::Plugin(
                    crate::plugin_authoring::associate_workspace(draft, &workspace)?,
                ))
            }
            ExtensionProjectKind::Skill => {
                let draft = crate::skill::authoring::import_package(
                    crate::skill::authoring::SkillPackageInput {
                        package_path: temporary.clone(),
                        revision_of_version: None,
                        parent_submission_id: None,
                    },
                )?;
                Ok(ExtensionCandidate::Skill(
                    crate::skill::authoring::associate_workspace(draft, &workspace)?,
                ))
            }
        }
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn update_source_commit(project_id: &str, commit: &str) -> Result<(), Box<dyn Error>> {
    let path = registry_path();
    let mut records = read_records(&path)?;
    let Some(record) = records.iter_mut().find(|record| record.id == project_id) else {
        return Ok(());
    };
    if record.source_commit == commit {
        return Ok(());
    }
    record.source_commit = commit.to_string();
    record.updated_at = now_stamp();
    write_records(&path, &records)
}

fn git_head(workspace: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

pub(crate) fn remove(project_id: &str) -> Result<(), Box<dyn Error>> {
    let path = registry_path();
    let mut records = read_records(&path)?;
    let before = records.len();
    records.retain(|record| record.id != project_id);
    if records.len() == before {
        return Err("扩展项目不存在".into());
    }
    write_records(&path, &records)
}

fn find_record(project_id: &str) -> Result<ProjectRecord, Box<dyn Error>> {
    read_records(&registry_path())?
        .into_iter()
        .find(|record| record.id == project_id)
        .ok_or_else(|| "扩展项目不存在".into())
}

fn invoke_development_tool(capability_id: &str, input: Value) -> Result<Value, Box<dyn Error>> {
    let plugin = crate::capability::plugin::find_plugin(DEVELOPMENT_TOOLS_PLUGIN_ID)?
        .ok_or("请先安装 AI 扩展开发工具")?;
    if !plugin.enabled || plugin.circuit_open {
        return Err("AI 扩展开发工具当前不可用".into());
    }
    if !plugin
        .capabilities
        .iter()
        .any(|capability| capability.id == capability_id)
    {
        return Err(format!("AI 扩展开发工具缺少能力: {capability_id}").into());
    }
    crate::capability::plugin::invoke_plugin_capability_for_plugin(
        DEVELOPMENT_TOOLS_PLUGIN_ID,
        capability_id,
        input,
        None,
    )
    .map_err(|error| friendly_tool_error(&error.to_string()).into())
}

fn friendly_tool_error(error: &str) -> String {
    let detail = error
        .find('{')
        .and_then(|index| serde_json::from_str::<Value>(&error[index..]).ok())
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| error.to_string());
    match detail.as_str() {
        "skill categories is required" => "请先在 skill.json 中选择功能分类".to_string(),
        "release_notes is required" => "请先填写本版本更新说明".to_string(),
        "author is required; use the current Agent authorized user" => {
            "请先在扩展清单中填写作者".to_string()
        }
        _ => detail,
    }
}

fn project_record_from_path(path: &Path, source: &str) -> Result<ProjectRecord, Box<dyn Error>> {
    let plugin_path = path.join("plugin.json");
    let skill_path = path.join("skill.json");
    if plugin_path.is_file() && skill_path.is_file() {
        return Err("项目目录不能同时包含 plugin.json 和 skill.json".into());
    }
    if plugin_path.is_file() {
        let manifest = parse_plugin_manifest(&fs::read_to_string(plugin_path)?)?;
        validate_plugin_identity(&manifest)?;
        return Ok(record(
            ExtensionProjectKind::Plugin,
            manifest.id,
            manifest.name,
            manifest.description,
            manifest.version,
            path,
            source,
        ));
    }
    if skill_path.is_file() {
        let manifest = load_skill_manifest(path)?;
        return Ok(skill_record(manifest, path, source));
    }
    Err("所选目录不是 HiMind 插件或技能项目".into())
}

fn skill_record(manifest: SkillManifest, path: &Path, source: &str) -> ProjectRecord {
    record(
        ExtensionProjectKind::Skill,
        manifest.id,
        manifest.name,
        manifest.description,
        manifest.version,
        path,
        source,
    )
}

fn record(
    kind: ExtensionProjectKind,
    extension_id: String,
    name: String,
    description: String,
    version: String,
    path: &Path,
    source: &str,
) -> ProjectRecord {
    ProjectRecord {
        id: format!("{}:{extension_id}", kind.as_str()),
        kind,
        extension_id,
        name,
        description,
        version,
        workspace_path: path.to_path_buf(),
        source: source.to_string(),
        source_repository: String::new(),
        source_default_branch: String::new(),
        source_subdirectory: String::new(),
        source_commit: String::new(),
        updated_at: now_stamp(),
    }
}

fn validate_plugin_identity(manifest: &PluginManifest) -> Result<(), Box<dyn Error>> {
    if manifest.id.trim().is_empty()
        || manifest.id.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err("plugin.json 中的插件 ID 无效".into());
    }
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err("plugin.json 缺少名称或版本".into());
    }
    Ok(())
}

fn normalize_slug(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.starts_with('-')
        || value.ends_with('-')
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-')
    {
        return Err("项目标识只能使用小写字母、数字和连字符".into());
    }
    Ok(value)
}

fn migrate_legacy_projects(records: &mut Vec<ProjectRecord>) -> bool {
    let mut changed = false;
    for draft in crate::plugin_authoring::list().unwrap_or_default() {
        let id = format!("plugin:{}", draft.manifest.id);
        if records.iter().any(|record| record.id == id) {
            continue;
        }
        let candidates = [
            draft.workspace_path.clone(),
            draft.development_path.clone(),
            draft
                .candidate_path
                .parent()
                .map(|path| path.join("package")),
        ];
        if let Some(record) = candidates
            .into_iter()
            .flatten()
            .find_map(|path| project_record_from_path(&path, "legacy_candidate").ok())
        {
            records.push(record);
            changed = true;
        }
    }
    for draft in crate::skill::authoring::list().unwrap_or_default() {
        let id = format!("skill:{}", draft.manifest.id);
        if records.iter().any(|record| record.id == id) {
            continue;
        }
        let candidates = [
            draft.workspace_path.clone(),
            draft
                .candidate_path
                .parent()
                .map(|path| path.join("package")),
        ];
        if let Some(record) = candidates
            .into_iter()
            .flatten()
            .find_map(|path| project_record_from_path(&path, "legacy_candidate").ok())
        {
            records.push(record);
            changed = true;
        }
    }
    changed
}

fn cleanup_temporary_candidates(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_file()
            && name.starts_with(".himind-candidate-")
            && (name.ends_with(".hmpkg") || name.ends_with(".hmskill"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn read_records(path: &Path) -> Result<Vec<ProjectRecord>, Box<dyn Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn write_records(path: &Path, records: &[ProjectRecord]) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(records)?)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn registry_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSION_PROJECTS_FILE") {
        return PathBuf::from(path);
    }
    crate::store::paths::agent_home().join("extension-projects.json")
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

impl From<ProjectRecord> for ExtensionProject {
    fn from(value: ProjectRecord) -> Self {
        Self {
            workspace_available: value.workspace_path.is_dir(),
            id: value.id,
            kind: value.kind,
            extension_id: value.extension_id,
            name: value.name,
            description: value.description,
            version: value.version,
            workspace_path: crate::extension_workspace::display_path(&value.workspace_path),
            source: value.source,
            source_repository: value.source_repository,
            source_default_branch: value.source_default_branch,
            source_subdirectory: value.source_subdirectory,
            source_commit: value.source_commit,
            updated_at: value.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plugin_and_skill_projects_with_stable_ids() {
        let root = env::temp_dir().join(format!("himind-project-detect-{}", now_stamp()));
        let plugin = root.join("plugin");
        let skill = root.join("skill");
        fs::create_dir_all(&plugin).unwrap();
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"id":"com.himind.project-test","name":"项目测试插件","description":"测试插件项目识别","version":"0.1.0"}"#,
        )
        .unwrap();
        fs::write(
            skill.join("skill.json"),
            r#"{"id":"com.himind.skill.project-test","name":"项目测试技能","author":"测试用户","categories":["software-engineering"],"version":"0.1.0","scope":"organization","description":"测试技能项目识别","release_notes":"创建初始版本。","min_agent_version":"0.3.0","supported_clients":["codex"],"capabilities":[],"plugin_dependencies":[],"risk_summary":"read_only","contents":["skill.json","SKILL.md"]}"#,
        )
        .unwrap();
        fs::write(skill.join("SKILL.md"), "# 项目测试技能\n").unwrap();

        let plugin_record = project_record_from_path(&plugin, "test").unwrap();
        let skill_record = project_record_from_path(&skill, "test").unwrap();
        assert_eq!(plugin_record.id, "plugin:com.himind.project-test");
        assert_eq!(skill_record.id, "skill:com.himind.skill.project-test");
        assert_eq!(plugin_record.kind, ExtensionProjectKind::Plugin);
        assert_eq!(skill_record.kind, ExtensionProjectKind::Skill);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_ambiguous_project_directory() {
        let root = env::temp_dir().join(format!("himind-project-ambiguous-{}", now_stamp()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.json"), "{}").unwrap();
        fs::write(root.join("skill.json"), "{}").unwrap();
        assert!(project_record_from_path(&root, "test").is_err());
        let _ = fs::remove_dir_all(root);
    }
}
