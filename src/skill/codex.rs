use crate::skill::clients::manifest_supports_client;
use crate::skill::manifest::validate_skill_id;
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::{SkillStore, SKILL_SYNC_MODE_SYMLINK};
use crate::skill::target::{self, SkillTarget};
use crate::skill::types::{SkillReceipt, SkillRecord};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

type CodexTarget = SkillTarget;

const RECEIPT_NAME: &str = ".himind-render.json";

#[derive(Debug, Clone, Serialize)]
struct RenderOutcome {
    skill_id: String,
    version: String,
    state: String,
    reason: Option<String>,
    rendered_root: PathBuf,
    files: Vec<String>,
}

pub(crate) fn status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let configured_sync_mode = store.sync_mode()?;
    let target = resolve_target(&store)?;
    let sync_mode = target::effective_sync_mode(&configured_sync_mode, &target);
    let records = store.list_records()?;
    let items = records
        .into_iter()
        .map(|record| {
            skill_status_entry(
                &target.root,
                &target,
                agent_version,
                capability_facts,
                record,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "client_id": "codex",
        "client_name": "Codex",
        "client_detected": target_detected(&target),
        "skill_standard": "agentskills.io",
        "support_level": "official",
        "support_note": "Codex 原生 Agent Skills",
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

pub(crate) fn is_detected() -> bool {
    resolve_target(&SkillStore::new())
        .map(|target| target_detected(&target))
        .unwrap_or(false)
}

pub(crate) fn sync_record_json(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let target = resolve_target(&store)?;
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let outcome = render_skill(&target, record)?;
    Ok(json!({
        "client_id": "codex",
        "target_root": crate::skill::target::display_path(&target.root),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "rendered": outcome,
    }))
}

pub(crate) fn repair_json(
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let target = resolve_target(&store)?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let slug = skill_slug(&record)?;
    let render_root = render_root_for_record(&target, &record)?;
    let backup_root = if render_root.exists() {
        let receipt = read_receipt(&render_root).map_err(|_| {
            format!(
                "Codex Skill 目录不是 HiMind 托管目录，拒绝修复: {}",
                render_root.display()
            )
        })?;
        if receipt.client != "codex" || receipt.skill_id != record.manifest.id {
            return Err("Codex Skill 托管收据与修复目标不匹配".into());
        }
        if receipt.target_kind != target.target_kind || receipt.workspace_id != target.workspace_id
        {
            return Err("Codex Skill 托管收据属于其他安装目标，拒绝修复".into());
        }
        if validate_rendered_skill(&render_root, &receipt).is_ok() {
            None
        } else if preserve_modified {
            let backup = target
                .root
                .join(format!(".himind-{slug}-user-backup-{}", unique_stamp()));
            fs::rename(&render_root, &backup)?;
            Some(backup)
        } else {
            fs::remove_dir_all(&render_root)?;
            None
        }
    } else {
        None
    };
    let outcome = render_skill(&target, &record)?;
    if render_root != target.root.join(&slug) && render_root.exists() {
        let _ = fs::remove_dir_all(&render_root);
        if let Some(parent) = render_root.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
    Ok(json!({
        "client_id": "codex",
        "target_root": crate::skill::target::display_path(&target.root),
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root.as_deref().map(crate::skill::target::display_path),
        "workspace_id": target.workspace_id,
        "rendered": outcome,
        "backup_root": backup_root.map(|path| crate::skill::target::display_path(&path)),
    }))
}

pub(crate) fn sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let target = resolve_target(&store)?;
    let records = store.list_records()?;
    let mut rendered = Vec::new();
    let mut skipped = Vec::new();
    let mut blocked = Vec::new();
    for record in records {
        if !manifest_supports_client(&record.manifest, "codex") {
            continue;
        }
        if !target::target_allows_record(
            &target,
            &record.manifest.id,
            Some(&record.manifest.version),
        )? {
            continue;
        }
        let readiness =
            SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
        match readiness.state.as_str() {
            "blocked" => blocked.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "reasons": readiness.reasons,
            })),
            "degraded" | "ready" => match render_skill(&target, &record) {
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
        "client_id": "codex",
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

pub(crate) fn uninstall_json(skill_id: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    let target = resolve_target(&store)?;
    let removed = uninstall_skill(&target, skill_id)?;
    Ok(json!({
        "client_id": "codex",
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
    _target_root: &Path,
    target: &SkillTarget,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
    record: SkillRecord,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let configured_sync_mode = SkillStore::new().sync_mode()?;
    let sync_mode = target::effective_sync_mode(&configured_sync_mode, target);
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    let render_root = render_root_for_record(target, &record)?;
    let receipt = read_receipt(&render_root).ok();
    let managing_profile = receipt
        .as_ref()
        .map(|receipt| receipt.agent_profile.clone());
    let modified_files = receipt
        .as_ref()
        .map(|receipt| rendered_drift(&render_root, receipt))
        .transpose()?
        .unwrap_or_default();
    let receipt_ok = receipt
        .as_ref()
        .map(|receipt| {
            receipt.target_kind == target.target_kind
                && receipt.workspace_root
                    == target
                        .workspace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string())
                && receipt.workspace_id == target.workspace_id
                && modified_files.is_empty()
                && receipt.client == "codex"
                && receipt.skill_id == record.manifest.id
                && validate_rendered_skill(&render_root, receipt).is_ok()
        })
        .unwrap_or(false);
    // Receipts written before project projections became copies (or a user who
    // switched copy/symlink globally) still describe valid content.  Treat the
    // render mode separately so the state points at the action that fixes it.
    let mode_stale = receipt
        .as_ref()
        .is_some_and(|receipt| receipt.render_mode != sync_mode);
    let supported = manifest_supports_client(&record.manifest, "codex");
    // A selected project pins the version it installed.  Comparing the
    // receipt with the Store head would otherwise report a deliberate pin as
    // "outdated"; compare with the pin and surface the newer Store version as
    // an explicit update instead.
    let pinned_version = target
        .workspace_root
        .as_deref()
        .map(|root| target::workspace_pinned_version(root, &record.manifest.id))
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
    } else if !render_root.exists() {
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
        "modified" => vec!["repair", "uninstall"],
        "installed" => vec!["repair", "uninstall"],
        _ => Vec::new(),
    };
    Ok(json!({
        "record": record,
        "readiness": readiness,
        "rendered_root": crate::skill::target::display_path(&render_root),
        "rendered": render_root.exists(),
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

fn target_mode(target: &CodexTarget) -> &'static str {
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

fn target_detected(target: &CodexTarget) -> bool {
    target.configured || target.root.exists() || target.root.parent().is_some_and(Path::exists)
}

fn render_root_for_record(
    target: &SkillTarget,
    record: &SkillRecord,
) -> Result<PathBuf, Box<dyn Error>> {
    let slug_root = target.root.join(skill_slug(record)?);
    if slug_root.exists() || target.is_workspace() {
        return Ok(slug_root);
    }
    // Pre-0.3.47 Codex projections used <skill-id>/current.  Keep reading
    // those receipts during migration; newly rendered content always uses the
    // portable <slug> directory layout.
    let legacy = target.root.join(&record.manifest.id).join("current");
    if legacy.join(RECEIPT_NAME).is_file() {
        return Ok(legacy);
    }
    Ok(slug_root)
}

fn render_skill(
    target: &SkillTarget,
    record: &SkillRecord,
) -> Result<RenderOutcome, Box<dyn Error>> {
    let configured_sync_mode = SkillStore::new().sync_mode()?;
    let sync_mode = target::effective_sync_mode(&configured_sync_mode, target);
    let slug = skill_slug(record)?;
    let target_root = &target.root;
    let render_root = target_root.join(&slug);
    let stamp = unique_stamp();
    let staging_dir = target_root.join(format!(".himind-{slug}-staging-{stamp}"));
    let backup_dir = target_root.join(format!(".himind-{slug}-backup-{stamp}"));
    fs::create_dir_all(target_root)?;
    let rendered_files = collect_rendered_files(&record.version_root, RECEIPT_NAME)?;
    let checksums = compute_checksums(&record.version_root, RECEIPT_NAME)?;

    if render_root.exists() {
        let existing = read_receipt(&render_root).map_err(|_| {
            format!(
                "Codex Skill 目录不是 HiMind 托管目录，拒绝覆盖: {}",
                render_root.display()
            )
        })?;
        if existing.client != "codex" || existing.skill_id != record.manifest.id {
            return Err("Codex Skill 托管收据与目标不匹配".into());
        }
        if existing.target_kind != target.target_kind
            || existing.workspace_id != target.workspace_id
        {
            return Err("Codex Skill 托管收据属于其他安装目标，拒绝覆盖".into());
        }
        validate_rendered_skill(&render_root, &existing)?;
        if existing.version == record.manifest.version
            && existing.skill_id == record.manifest.id
            && existing.source_root == record.version_root.to_string_lossy()
            && existing.render_mode == sync_mode
            && existing.checksums == checksums
        {
            let _ = fs::remove_dir_all(&staging_dir);
            target::record_deployment(
                target,
                "codex",
                &record.manifest.id,
                &record.manifest.version,
                &render_root,
            )?;
            target::record_workspace_skill(target, record, "himind-store")?;
            return Ok(RenderOutcome {
                skill_id: record.manifest.id.clone(),
                version: record.manifest.version.clone(),
                state: "skipped".to_string(),
                reason: None,
                rendered_root: render_root,
                files: existing.files,
            });
        }
    }

    copy_skill_tree(&record.version_root, &staging_dir, &sync_mode)?;
    let receipt = SkillReceipt {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        client: "codex".to_string(),
        agent_profile: crate::store::paths::profile_name(),
        source_root: record.version_root.to_string_lossy().to_string(),
        rendered_root: render_root.to_string_lossy().to_string(),
        rendered_at: stamp,
        render_mode: sync_mode,
        target_kind: target.target_kind.clone(),
        workspace_root: target
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        workspace_id: target.workspace_id.clone(),
        files: rendered_files.clone(),
        checksums,
    };
    fs::write(
        staging_dir.join(RECEIPT_NAME),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    if render_root.exists() {
        fs::rename(&render_root, &backup_dir)?;
    }
    if let Err(error) = fs::rename(&staging_dir, &render_root) {
        if backup_dir.exists() {
            let _ = fs::rename(&backup_dir, &render_root);
        }
        return Err(error.into());
    }
    if backup_dir.exists() {
        fs::remove_dir_all(&backup_dir)?;
    }
    target::record_deployment(
        target,
        "codex",
        &record.manifest.id,
        &record.manifest.version,
        &render_root,
    )?;
    target::record_workspace_skill(target, record, "himind-store")?;

    Ok(RenderOutcome {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        state: "rendered".to_string(),
        reason: None,
        rendered_root: render_root,
        files: rendered_files,
    })
}

fn uninstall_skill(
    target: &SkillTarget,
    skill_id: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let slug = skill_id
        .rsplit('.')
        .next()
        .ok_or("Skill ID 缺少可用目录名")?;
    validate_skill_slug(slug)?;
    let slug_root = target.root.join(slug);
    let legacy_root = target.root.join(skill_id);
    let legacy_current = legacy_root.join("current");
    let render_root = if slug_root.exists() {
        slug_root
    } else if legacy_current.join(RECEIPT_NAME).is_file() {
        legacy_current.clone()
    } else {
        slug_root
    };
    if !render_root.exists() {
        let _ = target::remove_deployment(target, "codex", skill_id);
        target::remove_workspace_skill_if_unused(target, skill_id)?;
        return Ok(json!({
            "skill_id": skill_id,
            "removed": false,
        }));
    }
    let receipt =
        read_receipt(&render_root).map_err(|_| "Codex Skill 目录不是 HiMind 托管目录，拒绝卸载")?;
    if receipt.client != "codex" || receipt.skill_id != skill_id {
        return Err("Codex Skill 托管收据与卸载目标不匹配".into());
    }
    if receipt.target_kind != target.target_kind || receipt.workspace_id != target.workspace_id {
        return Err("Codex Skill 托管收据属于其他安装目标，拒绝卸载".into());
    }
    remove_rendered_tree(&render_root, &receipt)?;
    if render_root == legacy_current {
        let _ = fs::remove_file(legacy_root.join("current.json"));
        let _ = fs::remove_file(legacy_root.join("previous.json"));
        let _ = fs::remove_dir(&legacy_root);
    }
    target::remove_deployment(target, "codex", skill_id)?;
    target::remove_workspace_skill_if_unused(target, skill_id)?;
    Ok(json!({
        "skill_id": skill_id,
        "removed": true,
    }))
}

fn resolve_target(store: &SkillStore) -> Result<CodexTarget, Box<dyn Error>> {
    // An explicitly selected project is a deployment target, so it takes
    // precedence over legacy client-directory overrides.  Otherwise a stale
    // HIMIND_CODEX_SKILL_DIR would silently turn a project sync into a global
    // sync.
    if let Some(workspace) = target::resolve_workspace_root(None)? {
        return SkillTarget::workspace(&workspace, ".agents/skills", "workspace");
    }
    if let Some(path) = env::var_os("HIMIND_CODEX_SKILL_DIR") {
        return Ok(SkillTarget::global(
            PathBuf::from(path),
            "env:HIMIND_CODEX_SKILL_DIR",
            true,
        ));
    }
    if let Some(path) = env::var_os("CODEX_SKILL_DIR") {
        return Ok(SkillTarget::global(
            PathBuf::from(path),
            "env:CODEX_SKILL_DIR",
            true,
        ));
    }
    let candidates = codex_default_candidates(store);
    if let Some((source, path)) = candidates.iter().find(|(_, path)| path.exists()).cloned() {
        return Ok(SkillTarget::global(path, source, false));
    }
    let (source, path) = candidates.into_iter().next().unwrap_or_else(|| {
        (
            "preview".to_string(),
            store.rendered_skill_root("codex", ".preview"),
        )
    });
    Ok(SkillTarget::global(path, source, false))
}

fn codex_default_candidates(store: &SkillStore) -> Vec<(String, PathBuf)> {
    let mut candidates = Vec::new();
    if let Some(userprofile) = env::var_os("USERPROFILE") {
        candidates.push((
            "userprofile:dot-agents".to_string(),
            PathBuf::from(&userprofile).join(".agents").join("skills"),
        ));
        candidates.push((
            "userprofile:dot-codex".to_string(),
            PathBuf::from(userprofile).join(".codex").join("skills"),
        ));
    }
    if let Some(local_appdata) = env::var_os("LOCALAPPDATA") {
        candidates.push((
            "localappdata:openai-codex".to_string(),
            PathBuf::from(&local_appdata)
                .join("OpenAI")
                .join("Codex")
                .join("skills"),
        ));
        candidates.push((
            "localappdata:codex".to_string(),
            PathBuf::from(local_appdata).join("Codex").join("skills"),
        ));
    }
    if let Some(appdata) = env::var_os("APPDATA") {
        candidates.push((
            "appdata:codex".to_string(),
            PathBuf::from(appdata).join("Codex").join("skills"),
        ));
    }
    candidates.push((
        "preview".to_string(),
        store.rendered_skill_root("codex", ".preview"),
    ));
    candidates
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
        return Err(format!("Skill ID 末段不能作为 Agent Skills 目录名: {slug}").into());
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
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if mode == SKILL_SYNC_MODE_SYMLINK {
            symlink_file(entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn collect_rendered_files(root: &Path, exclude_name: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !(entry.file_type().is_file() || entry.path().is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == exclude_name {
            continue;
        }
        if crate::skill::manifest::is_internal_package_file(&relative) {
            continue;
        }
        files.push(relative);
    }
    files.sort();
    Ok(files)
}

fn compute_checksums(
    root: &Path,
    exclude_name: &str,
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let mut items = BTreeMap::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !(entry.file_type().is_file() || entry.path().is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == exclude_name {
            continue;
        }
        if crate::skill::manifest::is_internal_package_file(&relative) {
            continue;
        }
        let checksum = checksum_file(entry.path())?;
        items.insert(relative, checksum);
    }
    Ok(items)
}

fn checksum_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let data = fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&data)))
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

fn read_receipt(root: &Path) -> Result<SkillReceipt, Box<dyn Error>> {
    let content = fs::read_to_string(root.join(RECEIPT_NAME))?;
    Ok(serde_json::from_str(
        content.trim_start_matches('\u{feff}'),
    )?)
}

fn validate_rendered_skill(root: &Path, receipt: &SkillReceipt) -> Result<(), Box<dyn Error>> {
    let checksums = compute_checksums(root, RECEIPT_NAME)?;
    if checksums != receipt.checksums {
        return Err(format!("rendered skill was modified: {}", receipt.skill_id).into());
    }
    Ok(())
}

fn rendered_drift(root: &Path, receipt: &SkillReceipt) -> Result<Vec<String>, Box<dyn Error>> {
    let actual = compute_checksums(root, RECEIPT_NAME)?;
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

fn remove_rendered_tree(root: &Path, receipt: &SkillReceipt) -> Result<(), Box<dyn Error>> {
    validate_rendered_skill(root, receipt)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

fn unique_stamp() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| format!("{}-{}", value.as_millis(), sequence))
        .unwrap_or_else(|_| format!("0-{}", sequence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::types::{SkillCapabilityDependency, SkillManifest, SkillRecord, SkillScope};

    #[test]
    fn computes_codex_target_preview_when_no_config_exists() {
        let store = SkillStore::new();
        let target = resolve_target(&store).unwrap();
        assert!(!target.source.is_empty());
    }

    #[test]
    fn renders_and_uninstalls_skill_tree() {
        let root = std::env::temp_dir().join(format!("himind-codex-test-{}", unique_stamp()));
        let store = SkillStore::with_root(root.clone());
        let skill_root = store.skill_root_for_scope(&SkillScope::Builtin, "demo.skill");
        let version_root = skill_root.join("versions").join("1.0.0");
        let manifest = SkillManifest {
            id: "demo.skill".to_string(),
            name: "Demo".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: String::new(),
            release_notes: "测试 Codex 渲染。".to_string(),
            min_agent_version: "0.2.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![SkillCapabilityDependency {
                id: "system.health".to_string(),
                required: true,
                min_version: Some("1.0.0".to_string()),
                max_version: None,
                provider: None,
            }],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        crate::skill::manifest::write_skill_package(&version_root, &manifest, "# Demo").unwrap();
        fs::create_dir_all(version_root.join(".himind")).unwrap();
        fs::write(
            version_root.join(".himind/manifest.json"),
            "{\"id\":\"demo.skill\"}",
        )
        .unwrap();
        let record = SkillRecord {
            manifest,
            root: skill_root.clone(),
            version_root: version_root.clone(),
            current: true,
            previous_version: None,
        };
        let target_root = root.join("rendered");
        let target = SkillTarget::global(target_root.clone(), "test", true);
        let outcome = render_skill(&target, &record).unwrap();
        assert_eq!(outcome.state, "rendered");
        assert!(!outcome.rendered_root.join(".himind").exists());
        let removed = uninstall_skill(&target, "demo.skill").unwrap();
        assert_eq!(removed["removed"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_receipt_cannot_be_uninstalled_from_another_workspace() {
        let root = std::env::temp_dir().join(format!("himind-codex-isolation-{}", unique_stamp()));
        let store = SkillStore::with_root(root.clone());
        let skill_root = store.skill_root_for_scope(&SkillScope::Builtin, "demo.skill");
        let version_root = skill_root.join("versions").join("1.0.0");
        let manifest = SkillManifest {
            id: "demo.skill".to_string(),
            name: "Demo".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: String::new(),
            release_notes: "测试隔离。".to_string(),
            min_agent_version: String::new(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            contents: vec!["SKILL.md".to_string()],
        };
        crate::skill::manifest::write_skill_package(&version_root, &manifest, "# Demo").unwrap();
        let record = SkillRecord {
            manifest,
            root: skill_root,
            version_root,
            current: true,
            previous_version: None,
        };
        let workspace_a = root.join("project-a");
        fs::create_dir_all(&workspace_a).unwrap();
        let target_a = SkillTarget::workspace(&workspace_a, ".agents/skills", "workspace").unwrap();
        let target_b = SkillTarget::global(target_a.root.clone(), "legacy-global", true);
        render_skill(&target_a, &record).unwrap();
        let error = uninstall_skill(&target_b, "demo.skill").unwrap_err();
        assert!(error.to_string().contains("属于其他安装目标"));
        assert!(target_a.root.join("skill").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unsafe_skill_id_on_uninstall() {
        let root = std::env::temp_dir().join(format!("himind-codex-test-{}", unique_stamp()));
        fs::create_dir_all(&root).unwrap();

        let target = SkillTarget::global(root.clone(), "test", true);
        let error = uninstall_skill(&target, "..\\outside").unwrap_err();

        assert!(error.to_string().contains("invalid skill id"));
        let _ = fs::remove_dir_all(root);
    }
}
