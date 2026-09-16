use crate::skill::clients::manifest_supports_client;
use crate::skill::manifest::validate_skill_id;
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::{SkillStore, SKILL_SYNC_MODE_SYMLINK};
use crate::skill::target::SkillTarget;
use crate::skill::types::{SkillReceipt, SkillRecord};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[cfg(test)]
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[cfg(test)]
const CLIENT_ID: &str = "github-copilot";
const RECEIPT_NAME: &str = ".himind-render.json";

pub(crate) type DirectSkillTarget = SkillTarget;

#[derive(Debug, Clone, Serialize)]
struct RenderOutcome {
    skill_id: String,
    version: String,
    state: String,
    reason: Option<String>,
    rendered_root: PathBuf,
    files: Vec<String>,
}

pub(crate) fn status_for_target(
    client_id: &str,
    target: DirectSkillTarget,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let configured_sync_mode = store.sync_mode()?;
    let sync_mode = crate::skill::target::effective_sync_mode(&configured_sync_mode, &target);
    let items = store
        .list_records()?
        .into_iter()
        .map(|record| {
            skill_status_entry(
                &target.root,
                &target,
                agent_version,
                capability_facts,
                record,
                client_id,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "client_id": client_id,
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "project_skills": if target.is_workspace() {
            crate::skill::target::discover_project_skills(&target.root)
        } else {
            Vec::new()
        },
        "project_skill_conflicts": target
            .workspace_root
            .as_deref()
            .map(crate::skill::target::discover_project_skill_conflicts)
            .unwrap_or_default(),
        "target_exists": target.root.exists(),
        "target_mode": target_mode(&target),
        "sync_mode": configured_sync_mode,
        "render_mode": sync_mode,
        "items": items,
    }))
}

pub(crate) fn sync_for_target(
    client_id: &str,
    client_name: &str,
    target: DirectSkillTarget,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let mut rendered = Vec::new();
    let mut skipped = Vec::new();
    let mut blocked = Vec::new();
    for record in store.list_records()? {
        if !manifest_supports_client(&record.manifest, client_id) {
            continue;
        }
        if !crate::skill::target::target_allows_record(
            &target,
            &record.manifest.id,
            Some(&record.manifest.version),
        )? {
            continue;
        }
        let readiness =
            SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, client_id);
        match readiness.state.as_str() {
            "blocked" => blocked.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "reasons": readiness.reasons,
            })),
            "degraded" | "ready" => match render_skill(&target, &record, client_id, client_name) {
                Ok(outcome) => rendered.push(outcome),
                Err(error) => skipped.push(json!({
                    "skill_id": record.manifest.id,
                    "version": record.manifest.version,
                    "error": error.to_string(),
                })),
            },
            other => skipped.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "state": other,
            })),
        }
    }
    Ok(json!({
        "client_id": client_id,
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "rendered": rendered,
        "skipped": skipped,
        "blocked": blocked,
    }))
}

pub(crate) fn repair_for_target(
    client_id: &str,
    client_name: &str,
    target: DirectSkillTarget,
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, client_id);
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let rendered_root = target.root.join(skill_slug(&record)?);
    let backup_root = if rendered_root.exists() {
        let receipt = read_receipt(&rendered_root).map_err(|_| {
            format!(
                "{client_name} Skill 目录不是 HiMind 托管目录，拒绝修复: {}",
                rendered_root.display()
            )
        })?;
        if receipt.client != client_id || receipt.skill_id != record.manifest.id {
            return Err(format!("{client_name} Skill 托管收据与修复目标不匹配").into());
        }
        if receipt.target_kind != target.target_kind || receipt.workspace_id != target.workspace_id
        {
            return Err(format!("{client_name} Skill 托管收据属于其他安装目标，拒绝修复").into());
        }
        if validate_rendered_skill(&rendered_root, &receipt).is_ok() {
            None
        } else if preserve_modified {
            let backup = target.root.join(format!(
                ".himind-{}-user-backup-{}",
                skill_slug(&record)?,
                unique_stamp()
            ));
            fs::rename(&rendered_root, &backup)?;
            Some(backup)
        } else {
            fs::remove_dir_all(&rendered_root)?;
            None
        }
    } else {
        None
    };
    let outcome = render_skill(&target, &record, client_id, client_name)?;
    Ok(json!({
        "client_id": client_id,
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "rendered": outcome,
        "backup_root": backup_root.map(|path| crate::skill::target::display_path(&path)),
    }))
}

pub(crate) fn sync_record_for_target(
    client_id: &str,
    client_name: &str,
    target: DirectSkillTarget,
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, client_id);
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let outcome = render_skill(&target, record, client_id, client_name)?;
    Ok(json!({
        "client_id": client_id,
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "rendered": outcome,
    }))
}

pub(crate) fn uninstall_for_target(
    client_id: &str,
    client_name: &str,
    target: DirectSkillTarget,
    skill_id: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let removed = uninstall_skill(&target, skill_id, client_id, client_name)?;
    Ok(json!({
        "client_id": client_id,
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "removed": removed,
    }))
}

fn skill_status_entry(
    target_root: &Path,
    target: &SkillTarget,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
    record: SkillRecord,
    client_id: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let configured_sync_mode = SkillStore::new().sync_mode()?;
    let sync_mode = crate::skill::target::effective_sync_mode(&configured_sync_mode, target);
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, client_id);
    let rendered_root = target_root.join(skill_slug(&record)?);
    let receipt = read_receipt(&rendered_root).ok();
    let managing_profile = receipt
        .as_ref()
        .map(|receipt| receipt.agent_profile.clone());
    let modified_files = receipt
        .as_ref()
        .map(|receipt| rendered_drift(&rendered_root, receipt))
        .transpose()?
        .unwrap_or_default();
    let receipt_ok = receipt
        .as_ref()
        .map(|receipt| {
            receipt.client == client_id
                && receipt.skill_id == record.manifest.id
                && receipt.target_kind == target.target_kind
                && receipt.workspace_root
                    == target
                        .workspace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string())
                && receipt.workspace_id == target.workspace_id
                && modified_files.is_empty()
                && validate_rendered_skill(&rendered_root, receipt).is_ok()
        })
        .unwrap_or(false);
    // See the Codex adapter: a receipt that differs only in render mode is
    // valid content written in the previous style, not a user edit.
    let mode_stale = receipt
        .as_ref()
        .is_some_and(|receipt| receipt.render_mode != sync_mode);
    let supported = manifest_supports_client(&record.manifest, client_id);
    // Keep the same pin semantics as the Codex adapter: a project that
    // deliberately holds an older version is not "outdated", it simply has an
    // explicit update available.
    let pinned_version = target
        .workspace_root
        .as_deref()
        .map(|root| crate::skill::target::workspace_pinned_version(root, &record.manifest.id))
        .transpose()?
        .flatten();
    let expected_version = pinned_version
        .clone()
        .unwrap_or_else(|| record.manifest.version.clone());
    let update_available = pinned_version
        .as_deref()
        .is_some_and(|version| version != record.manifest.version);
    let client_state = if !supported {
        "unsupported"
    } else if readiness.state == "blocked" {
        "blocked"
    } else if !rendered_root.exists() {
        "not_installed"
    } else if !receipt_ok {
        "modified"
    } else if update_available && mode_stale {
        "outdated"
    } else if receipt
        .as_ref()
        .map(|receipt| receipt.version != expected_version)
        .unwrap_or(false)
    {
        "outdated"
    } else if mode_stale {
        "modified"
    } else {
        "installed"
    };
    let available_actions = match client_state {
        "not_installed" => vec!["install"],
        "outdated" => vec!["update", "uninstall"],
        "modified" => vec!["repair"],
        "installed" => vec!["uninstall"],
        _ => Vec::new(),
    };
    Ok(json!({
        "record": record,
        "readiness": readiness,
        "rendered_root": crate::skill::target::display_path(&rendered_root),
        "rendered": rendered_root.exists(),
        "rendered_valid": receipt_ok,
        "client_state": client_state,
        "installed_version": receipt.as_ref().map(|value| value.version.clone()),
        "managing_profile": managing_profile,
        "available_version": record.manifest.version,
        "pinned_version": pinned_version,
        "update_available": update_available,
        "last_synced_at": receipt.as_ref().map(|value| value.rendered_at.clone()),
        "managed_files": receipt.as_ref().map(|value| value.files.clone()).unwrap_or_default(),
        "modified_files": modified_files,
        "available_actions": available_actions,
    }))
}

fn render_skill(
    target: &SkillTarget,
    record: &SkillRecord,
    client_id: &str,
    client_name: &str,
) -> Result<RenderOutcome, Box<dyn Error>> {
    let configured_sync_mode = SkillStore::new().sync_mode()?;
    let sync_mode = crate::skill::target::effective_sync_mode(&configured_sync_mode, target);
    let slug = skill_slug(record)?;
    let target_root = &target.root;
    let rendered_root = target_root.join(&slug);
    let stamp = unique_stamp();
    let staging_root = target_root.join(format!(".himind-{slug}-staging-{stamp}"));
    let backup_root = target_root.join(format!(".himind-{slug}-backup-{stamp}"));
    fs::create_dir_all(target_root)?;
    let files = collect_rendered_files(&record.version_root)?;
    let checksums = compute_checksums(&record.version_root)?;

    if rendered_root.exists() {
        let receipt = read_receipt(&rendered_root).map_err(|_| {
            format!(
                "{client_name} Skill 目录不是 HiMind 托管目录，拒绝覆盖: {}",
                rendered_root.display()
            )
        })?;
        if receipt.client != client_id || receipt.skill_id != record.manifest.id {
            return Err(format!(
                "{client_name} Skill 目录归属于其他 Skill，拒绝覆盖: {}",
                rendered_root.display()
            )
            .into());
        }
        if receipt.target_kind != target.target_kind || receipt.workspace_id != target.workspace_id
        {
            return Err(format!(
                "{client_name} Skill 托管收据属于其他安装目标，拒绝覆盖: {}",
                rendered_root.display()
            )
            .into());
        }
        validate_rendered_skill(&rendered_root, &receipt)?;
        if receipt.version == record.manifest.version
            && receipt.source_root == record.version_root.to_string_lossy()
            && receipt.render_mode == sync_mode
            && receipt.checksums == checksums
        {
            crate::skill::target::record_deployment(
                target,
                client_id,
                &record.manifest.id,
                &record.manifest.version,
                &rendered_root,
            )?;
            crate::skill::target::record_workspace_skill(target, record, "himind-store")?;
            return Ok(RenderOutcome {
                skill_id: record.manifest.id.clone(),
                version: record.manifest.version.clone(),
                state: "skipped".to_string(),
                reason: None,
                rendered_root,
                files: receipt.files,
            });
        }
    }

    copy_skill_tree(&record.version_root, &staging_root, &sync_mode)?;
    let receipt = SkillReceipt {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        client: client_id.to_string(),
        agent_profile: crate::store::paths::profile_name(),
        source_root: record.version_root.to_string_lossy().to_string(),
        rendered_root: rendered_root.to_string_lossy().to_string(),
        rendered_at: stamp,
        render_mode: sync_mode,
        target_kind: target.target_kind.clone(),
        workspace_root: target
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        workspace_id: target.workspace_id.clone(),
        files: files.clone(),
        checksums,
    };
    fs::write(
        staging_root.join(RECEIPT_NAME),
        serde_json::to_vec_pretty(&receipt)?,
    )?;

    if rendered_root.exists() {
        fs::rename(&rendered_root, &backup_root)?;
    }
    if let Err(error) = fs::rename(&staging_root, &rendered_root) {
        if backup_root.exists() {
            let _ = fs::rename(&backup_root, &rendered_root);
        }
        return Err(error.into());
    }
    if backup_root.exists() {
        fs::remove_dir_all(&backup_root)?;
    }
    crate::skill::target::record_deployment(
        target,
        client_id,
        &record.manifest.id,
        &record.manifest.version,
        &rendered_root,
    )?;
    crate::skill::target::record_workspace_skill(target, record, "himind-store")?;
    Ok(RenderOutcome {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        state: "rendered".to_string(),
        reason: None,
        rendered_root,
        files,
    })
}

fn uninstall_skill(
    target: &SkillTarget,
    skill_id: &str,
    client_id: &str,
    client_name: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let slug = skill_id
        .rsplit('.')
        .next()
        .ok_or("Skill ID 缺少可用目录名")?;
    validate_skill_slug(slug)?;
    let rendered_root = target.root.join(slug);
    if !rendered_root.exists() {
        let _ = crate::skill::target::remove_deployment(target, client_id, skill_id);
        crate::skill::target::remove_workspace_skill_if_unused(target, skill_id)?;
        return Ok(json!({"skill_id": skill_id, "removed": false}));
    }
    let receipt = read_receipt(&rendered_root).map_err(|_| {
        format!(
            "{client_name} Skill 目录不是 HiMind 托管目录，拒绝卸载: {}",
            rendered_root.display()
        )
    })?;
    if receipt.client != client_id || receipt.skill_id != skill_id {
        return Err(format!("{client_name} Skill 托管收据与卸载目标不匹配").into());
    }
    if receipt.target_kind != target.target_kind || receipt.workspace_id != target.workspace_id {
        return Err(format!("{client_name} Skill 托管收据属于其他安装目标，拒绝卸载").into());
    }
    validate_rendered_skill(&rendered_root, &receipt)?;
    fs::remove_dir_all(&rendered_root)?;
    crate::skill::target::remove_deployment(target, client_id, skill_id)?;
    crate::skill::target::remove_workspace_skill_if_unused(target, skill_id)?;
    Ok(json!({"skill_id": skill_id, "removed": true}))
}

fn target_mode(target: &DirectSkillTarget) -> &'static str {
    if target.source == "preview" {
        "preview"
    } else if target.is_workspace() {
        "workspace"
    } else if target.configured {
        "configured"
    } else {
        "detected"
    }
}

fn skill_slug(record: &SkillRecord) -> Result<String, Box<dyn Error>> {
    let slug = record
        .manifest
        .id
        .rsplit('.')
        .next()
        .ok_or("Skill ID 缺少可用目录名")?;
    validate_skill_slug(slug)?;
    Ok(slug.to_string())
}

fn validate_skill_slug(slug: &str) -> Result<(), Box<dyn Error>> {
    if slug.is_empty()
        || slug.starts_with('-')
        || slug.ends_with('-')
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("Skill ID 末段不能作为 Copilot Skill 目录名: {slug}").into());
    }
    Ok(())
}

fn copy_skill_tree(
    source_root: &Path,
    target_root: &Path,
    mode: &str,
) -> Result<(), Box<dyn Error>> {
    if target_root.exists() {
        fs::remove_dir_all(target_root)?;
    }
    fs::create_dir_all(target_root)?;
    for entry in WalkDir::new(source_root) {
        let entry = entry?;
        if entry.path() == source_root {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(
                format!("skill package contains symlink: {}", entry.path().display()).into(),
            );
        }
        let relative = entry.path().strip_prefix(source_root)?;
        let relative_name = relative.to_string_lossy().replace('\\', "/");
        if crate::skill::manifest::is_internal_package_file(&relative_name) {
            continue;
        }
        let destination = target_root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            if mode == SKILL_SYNC_MODE_SYMLINK {
                symlink_file(entry.path(), &destination)?;
            } else {
                fs::copy(entry.path(), destination)?;
            }
        }
    }
    Ok(())
}

fn collect_rendered_files(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() || entry.path().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if relative != RECEIPT_NAME {
                if crate::skill::manifest::is_internal_package_file(&relative) {
                    continue;
                }
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn compute_checksums(root: &Path) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let mut checksums = BTreeMap::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() || entry.path().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if relative != RECEIPT_NAME {
                if crate::skill::manifest::is_internal_package_file(&relative) {
                    continue;
                }
                checksums.insert(
                    relative,
                    format!("{:x}", Sha256::digest(fs::read(entry.path())?)),
                );
            }
        }
    }
    Ok(checksums)
}

fn read_receipt(root: &Path) -> Result<SkillReceipt, Box<dyn Error>> {
    let content = fs::read_to_string(root.join(RECEIPT_NAME))?;
    Ok(serde_json::from_str(
        content.trim_start_matches('\u{feff}'),
    )?)
}

fn symlink_file(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(source, destination).map_err(|error| {
            format!(
                "cannot create Skill file symlink {} -> {}: {error}; enable Windows Developer Mode or use copy mode",
                destination.display(),
                source.display()
            )
            .into()
        })
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination).map_err(|error| {
            format!(
                "cannot create Skill file symlink {} -> {}: {error}",
                destination.display(),
                source.display()
            )
            .into()
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = source;
        let _ = destination;
        Err("Skill symlink mode is not supported on this platform".into())
    }
}

fn validate_rendered_skill(root: &Path, receipt: &SkillReceipt) -> Result<(), Box<dyn Error>> {
    if compute_checksums(root)? != receipt.checksums {
        return Err(format!("rendered skill was modified: {}", receipt.skill_id).into());
    }
    Ok(())
}

fn rendered_drift(root: &Path, receipt: &SkillReceipt) -> Result<Vec<String>, Box<dyn Error>> {
    let actual = compute_checksums(root)?;
    let mut changed = Vec::new();
    for (path, checksum) in &receipt.checksums {
        if actual.get(path) != Some(checksum) {
            changed.push(path.clone());
        }
    }
    for path in actual.keys() {
        if !receipt.checksums.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    Ok(changed)
}

fn unique_stamp() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| format!("{}-{sequence}", value.as_millis()))
        .unwrap_or_else(|_| format!("0-{sequence}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::types::{SkillManifest, SkillScope};

    fn record(root: &Path) -> SkillRecord {
        let version_root = root.join("source");
        fs::create_dir_all(&version_root).unwrap();
        fs::write(
            version_root.join("SKILL.md"),
            "---\nname: copilot-test\ndescription: test\n---\n# Test",
        )
        .unwrap();
        fs::write(version_root.join("skill.json"), "{}").unwrap();
        SkillRecord {
            manifest: SkillManifest {
                id: "com.himind.skill.copilot-test".to_string(),
                name: "Copilot 测试".to_string(),
                author: String::new(),
                categories: vec![],
                version: "1.0.0".to_string(),
                scope: SkillScope::Organization,
                description: String::new(),
                release_notes: "测试 Copilot 渲染。".to_string(),
                min_agent_version: String::new(),
                supported_clients: vec![CLIENT_ID.to_string()],
                capabilities: vec![],
                plugin_dependencies: vec![],
                risk_summary: String::new(),
                contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
            },
            root: root.to_path_buf(),
            version_root,
            current: true,
            previous_version: None,
        }
    }

    #[test]
    fn renders_directly_under_copilot_skill_name_and_uninstalls() {
        let root = env::temp_dir().join(format!("himind-copilot-test-{}", unique_stamp()));
        let target = root.join("target");
        let record = record(&root);
        let target = SkillTarget::global(target.clone(), "test", true);
        let outcome = render_skill(&target, &record, CLIENT_ID, "Copilot").unwrap();
        assert_eq!(outcome.rendered_root, target.root.join("copilot-test"));
        assert!(outcome.rendered_root.join("SKILL.md").exists());
        assert!(!outcome.rendered_root.join("current").exists());
        assert_eq!(
            uninstall_skill(&target, &record.manifest.id, CLIENT_ID, "Copilot").unwrap()["removed"],
            true
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_skill_renderer_records_the_selected_client() {
        let root = env::temp_dir().join(format!("himind-workbuddy-test-{}", unique_stamp()));
        let target = root.join("target");
        let record = record(&root);
        let target = SkillTarget::global(target.clone(), "test", true);
        let outcome = render_skill(&target, &record, "workbuddy", "WorkBuddy").unwrap();
        let receipt = read_receipt(&outcome.rendered_root).unwrap();
        assert_eq!(receipt.client, "workbuddy");
        assert_eq!(
            uninstall_skill(&target, &record.manifest.id, "workbuddy", "WorkBuddy").unwrap()
                ["removed"],
            true
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_to_overwrite_unmanaged_copilot_skill() {
        let root = env::temp_dir().join(format!("himind-copilot-test-{}", unique_stamp()));
        let target = root.join("target");
        let record = record(&root);
        let unmanaged = target.join("copilot-test");
        fs::create_dir_all(&unmanaged).unwrap();
        fs::write(unmanaged.join("SKILL.md"), "manual").unwrap();
        let target = SkillTarget::global(target.clone(), "test", true);
        let error = render_skill(&target, &record, CLIENT_ID, "Copilot").unwrap_err();
        assert!(error.to_string().contains("拒绝覆盖"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_receipt_cannot_be_uninstalled_from_another_workspace() {
        // A project projection is materialized content, never a link into the
        // machine-local Store.
        let root = env::temp_dir().join(format!("himind-copilot-copy-{}", unique_stamp()));
        let record = record(&root);
        let workspace = root.join("project");
        fs::create_dir_all(&workspace).unwrap();
        let target = SkillTarget::workspace(&workspace, ".agents/skills", "workspace").unwrap();
        let outcome = render_skill(&target, &record, CLIENT_ID, "Copilot").unwrap();
        let rendered_skill = outcome.rendered_root.join("SKILL.md");
        let metadata = fs::symlink_metadata(&rendered_skill).unwrap();
        assert!(metadata.file_type().is_file());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(
            read_receipt(&outcome.rendered_root).unwrap().render_mode,
            crate::skill::store::SKILL_SYNC_MODE_COPY
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_receipt_is_isolated_from_other_targets() {
        let root = env::temp_dir().join(format!("himind-copilot-isolation-{}", unique_stamp()));
        let record = record(&root);
        let workspace_a = root.join("project-a");
        fs::create_dir_all(&workspace_a).unwrap();
        let target_a = SkillTarget::workspace(&workspace_a, ".agents/skills", "workspace").unwrap();
        let target_b = SkillTarget::global(target_a.root.clone(), "legacy-global", true);
        render_skill(&target_a, &record, CLIENT_ID, "Copilot").unwrap();
        let error =
            uninstall_skill(&target_b, &record.manifest.id, CLIENT_ID, "Copilot").unwrap_err();
        assert!(error.to_string().contains("属于其他安装目标"));
        assert!(target_a.root.join("copilot-test").exists());
        let _ = fs::remove_dir_all(root);
    }
}
