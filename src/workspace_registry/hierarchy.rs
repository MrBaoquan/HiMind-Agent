//! Explicit directory-to-subproject transactions. Planning never writes or scans histories.
use super::*;

#[cfg(test)]
#[path = "hierarchy_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubprojectEntry {
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubprojectInput {
    pub parent_id: String,
    pub expected_revision: String,
    #[serde(default)]
    pub parent_name: Option<String>,
    pub entries: Vec<SubprojectEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SubprojectChange {
    pub workspace_id: String,
    pub path: String,
    pub previous_project_id: String,
    pub project: RegistryProject,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SubprojectPreview {
    pub revision: String,
    pub parent: RegistryProject,
    pub changes: Vec<SubprojectChange>,
    pub rollback_revision: Option<String>,
    pub applied_revision: Option<String>,
}

pub(crate) fn plan(registry: &Registry, input: &SubprojectInput) -> Result<(Registry, SubprojectPreview), Box<dyn Error>> {
    let revision = registry.revision();
    if input.expected_revision != revision { return Err("WORKSPACE_REGISTRY_CHANGED: 请刷新后重新预览".into()); }
    if input.entries.is_empty() || input.entries.len() > 100 { return Err("SCHEMA_INVALID: 请选择 1–100 个目录".into()); }
    let mut next = registry.clone();
    let parent = next.project(&input.parent_id).ok_or("PROJECT_NOT_FOUND")?.clone();
    if let Some(name) = &input.parent_name {
        upsert_project(&mut next, ProjectInput { id: parent.id.clone(), name: name.clone(), aliases: parent.aliases.clone(), kind: parent.kind.clone(), ..Default::default() })?;
    }
    let mut seen = std::collections::HashSet::new();
    let mut changes = Vec::new();
    for entry in &input.entries {
        if !seen.insert(&entry.workspace_id) { return Err("PATH_CONFLICT: 同一目录只能选择一次".into()); }
        let workspace = next.workspaces.iter().find(|w| w.id == entry.workspace_id).ok_or("PATH_INVALID: 目录已不存在")?.clone();
        if workspace.project_id != input.parent_id { return Err("PATH_CONFLICT: 目录归属已改变".into()); }
        if entry.name.trim().is_empty() || entry.name.trim().chars().count() > 40 { return Err("SCHEMA_INVALID: 主显示名需为 1–40 个字符".into()); }
        if next.find_project(entry.name.trim()).is_some() { return Err("ALIAS_CONFLICT: 项目名已存在，请更换名称".into()); }
        let mut child = upsert_project(&mut next, ProjectInput {
            name: entry.name.clone(), aliases: entry.aliases.clone(),
            kind: if entry.kind.is_empty() { KIND_PRODUCT.into() } else { entry.kind.clone() },
            parent_id: ParentField::Set(input.parent_id.clone()), ..Default::default()
        })?;
        // IDs and timestamps in a preview are deterministic; applying it yields the same identities.
        use sha2::{Digest, Sha256};
        let id = format!("prj-sub-{:x}", Sha256::digest(format!("{revision}:{}", workspace.id).as_bytes()));
        let created_id = child.id.clone();
        child.id = id;
        child.created_at = registry.updated_at.clone();
        child.updated_at = registry.updated_at.clone();
        *next.projects.iter_mut().find(|p| p.id == created_id).ok_or("PROJECT_NOT_FOUND")? = child.clone();
        if next.children(&input.parent_id).iter().any(|other| other.id != child.id && other.display_name() == child.display_name()) {
            return Err("ALIAS_CONFLICT: 同级中文显示名重复".into());
        }
        next.workspaces.iter_mut().find(|w| w.id == workspace.id).ok_or("PATH_INVALID")?.project_id = child.id.clone();
        changes.push(SubprojectChange { workspace_id: workspace.id, path: workspace.path, previous_project_id: workspace.project_id, project: child });
    }
    let problems = next.validate();
    if !problems.is_empty() { return Err(format!("SCHEMA_INVALID: {}", problems.join("; ")).into()); }
    let parent = next.project(&input.parent_id).ok_or("PROJECT_NOT_FOUND")?.clone();
    Ok((next, SubprojectPreview { revision, parent, changes, rollback_revision: None, applied_revision: None }))
}

fn snapshot_path(path: &Path, revision: &str) -> Result<PathBuf, Box<dyn Error>> {
    if revision.len() != 64 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("SCHEMA_INVALID: 快照标识无效".into()); }
    Ok(path.with_file_name(format!("workspace-hierarchy-{revision}.json")))
}

pub(crate) fn apply_at(path: &Path, input: &SubprojectInput) -> Result<SubprojectPreview, Box<dyn Error>> {
    let _lock = atomic_file::lock(path)?;
    let current = Registry::load_from(path)?;
    let (mut next, mut preview) = plan(&current, input)?;
    next.updated_at = now_iso();
    // A dedicated snapshot survives subsequent unrelated registry writes.
    atomic_file::atomic_write(&snapshot_path(path, &preview.revision)?, &serde_json::to_vec_pretty(&current)?)?;
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(&next)?)?;
    preview.rollback_revision = Some(preview.revision.clone());
    preview.applied_revision = Some(next.revision());
    Ok(preview)
}

pub(crate) fn restore_at(path: &Path, before: &str, expected_revision: &str) -> Result<(), Box<dyn Error>> {
    let _lock = atomic_file::lock(path)?;
    let current = Registry::load_from(path)?;
    if current.revision() != expected_revision { return Err("WORKSPACE_REGISTRY_CHANGED: 存在后续修改，不能覆盖恢复".into()); }
    let snapshot = snapshot_path(path, before)?;
    let bytes = fs::read(snapshot)?;
    let mut previous = decode(&bytes)?;
    if previous.revision() != before || !previous.validate().is_empty() { return Err("SCHEMA_INVALID: 快照校验失败".into()); }
    previous.updated_at = now_iso();
    atomic_file::atomic_write(path, &serde_json::to_vec_pretty(&previous)?)?;
    Ok(())
}
