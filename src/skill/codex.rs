use crate::skill::manifest::validate_skill_id;
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::{SkillStore, SKILL_SYNC_MODE_SYMLINK};
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

#[derive(Debug, Clone, Serialize)]
struct CodexTarget {
    root: PathBuf,
    source: String,
    configured: bool,
}

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
    let sync_mode = store.sync_mode()?;
    let target = resolve_target(&store);
    let records = store.list_records()?;
    let items = records
        .into_iter()
        .map(|record| skill_status_entry(&target.root, agent_version, capability_facts, record))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "client_id": "codex",
        "target_root": target.root.to_string_lossy().to_string(),
        "target_source": target.source,
        "target_configured": target.configured,
        "target_exists": target.root.exists(),
        "target_mode": target_mode(&target),
        "sync_mode": sync_mode,
        "items": items,
    }))
}

pub(crate) fn sync_record_json(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let target = resolve_target(&store);
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let outcome = render_skill(&target.root, record)?;
    Ok(json!({
        "client_id": "codex",
        "target_root": target.root.to_string_lossy().to_string(),
        "target_source": target.source,
        "target_configured": target.configured,
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
    let target = resolve_target(&store);
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let render_root = target.root.join(skill_id);
    let current_dir = render_root.join("current");
    let backup_root = if current_dir.exists() {
        match read_receipt(&current_dir) {
            Ok(receipt) if validate_rendered_skill(&current_dir, &receipt).is_ok() => None,
            _ if preserve_modified => {
                let backup = render_root.join(format!("user-backup-{}", unique_stamp()));
                fs::rename(&current_dir, &backup)?;
                Some(backup)
            }
            _ => {
                fs::remove_dir_all(&current_dir)?;
                None
            }
        }
    } else {
        None
    };
    let outcome = render_skill(&target.root, &record)?;
    Ok(json!({
        "client_id": "codex",
        "target_root": target.root.to_string_lossy().to_string(),
        "rendered": outcome,
        "backup_root": backup_root.map(|path| path.to_string_lossy().to_string()),
    }))
}

pub(crate) fn sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let target = resolve_target(&store);
    let records = store.list_records()?;
    let mut rendered = Vec::new();
    let mut skipped = Vec::new();
    let mut blocked = Vec::new();
    for record in records {
        let readiness =
            SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
        match readiness.state.as_str() {
            "blocked" => blocked.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "reasons": readiness.reasons,
            })),
            "degraded" | "ready" => match render_skill(&target.root, &record) {
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
        "target_root": target.root.to_string_lossy().to_string(),
        "target_source": target.source,
        "target_configured": target.configured,
        "rendered": rendered,
        "skipped": skipped,
        "blocked": blocked,
    }))
}

pub(crate) fn uninstall_json(skill_id: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    let store = SkillStore::new();
    let target = resolve_target(&store);
    let removed = uninstall_skill(&target.root, skill_id)?;
    Ok(json!({
        "client_id": "codex",
        "target_root": target.root.to_string_lossy().to_string(),
        "target_source": target.source,
        "target_configured": target.configured,
        "removed": removed,
    }))
}

fn skill_status_entry(
    target_root: &Path,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
    record: SkillRecord,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let sync_mode = SkillStore::new().sync_mode()?;
    let readiness =
        SkillReadiness::resolve(&record.manifest, capability_facts, agent_version, "codex");
    let render_root = target_root.join(&record.manifest.id);
    let current_dir = render_root.join("current");
    let receipt = read_receipt(&current_dir).ok();
    let modified_files = receipt
        .as_ref()
        .map(|receipt| rendered_drift(&current_dir, receipt))
        .transpose()?
        .unwrap_or_default();
    let receipt_ok = receipt
        .as_ref()
        .map(|receipt| {
            receipt.render_mode == sync_mode
                && modified_files.is_empty()
                && validate_rendered_skill(&current_dir, receipt).is_ok()
        })
        .unwrap_or(false);
    let client_state = if readiness.state == "blocked" {
        "blocked"
    } else if !current_dir.exists() {
        "not_installed"
    } else if !receipt_ok {
        "modified"
    } else if receipt
        .as_ref()
        .map(|receipt| receipt.version != record.manifest.version)
        .unwrap_or(false)
    {
        "outdated"
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
        "rendered_root": render_root.to_string_lossy().to_string(),
        "rendered": current_dir.exists(),
        "rendered_valid": receipt_ok,
        "client_state": client_state,
        "installed_version": receipt.as_ref().map(|value| value.version.clone()),
        "available_version": record.manifest.version,
        "last_synced_at": receipt.as_ref().map(|value| value.rendered_at.clone()),
        "managed_files": receipt.as_ref().map(|value| value.files.clone()).unwrap_or_default(),
        "modified_files": modified_files,
        "available_actions": available_actions,
    }))
}

fn target_mode(target: &CodexTarget) -> &'static str {
    if target.source == "preview" {
        "preview"
    } else if target.configured {
        "configured"
    } else {
        "detected"
    }
}

fn render_skill(target_root: &Path, record: &SkillRecord) -> Result<RenderOutcome, Box<dyn Error>> {
    let sync_mode = SkillStore::new().sync_mode()?;
    let render_root = target_root.join(&record.manifest.id);
    let current_dir = render_root.join("current");
    let previous_dir = render_root.join("previous");
    let staging_dir = render_root.join(format!(".staging-{}", unique_stamp()));
    fs::create_dir_all(&render_root)?;
    let rendered_files = collect_rendered_files(&record.version_root, ".himind-render.json")?;
    let checksums = compute_checksums(&record.version_root, ".himind-render.json")?;

    if current_dir.exists() {
        let existing = read_receipt(&current_dir)?;
        validate_rendered_skill(&current_dir, &existing)?;
        if existing.version == record.manifest.version
            && existing.skill_id == record.manifest.id
            && existing.source_root == record.version_root.to_string_lossy()
            && existing.render_mode == sync_mode
            && existing.checksums == checksums
        {
            let _ = fs::remove_dir_all(&staging_dir);
            return Ok(RenderOutcome {
                skill_id: record.manifest.id.clone(),
                version: record.manifest.version.clone(),
                state: "skipped".to_string(),
                reason: None,
                rendered_root: current_dir,
                files: existing.files,
            });
        }
        if previous_dir.exists() {
            fs::remove_dir_all(&previous_dir)?;
        }
        fs::rename(&current_dir, &previous_dir)?;
        write_pointer(
            &render_root.join("previous.json"),
            &existing.version,
            &previous_dir,
        )?;
    }

    copy_skill_tree(&record.version_root, &staging_dir, &sync_mode)?;
    let receipt = SkillReceipt {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        client: "codex".to_string(),
        source_root: record.version_root.to_string_lossy().to_string(),
        rendered_root: current_dir.to_string_lossy().to_string(),
        rendered_at: unique_stamp(),
        render_mode: sync_mode,
        files: rendered_files.clone(),
        checksums,
    };
    fs::write(
        staging_dir.join(".himind-render.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    if current_dir.exists() {
        fs::remove_dir_all(&current_dir)?;
    }
    fs::rename(&staging_dir, &current_dir)?;
    write_pointer(
        &render_root.join("current.json"),
        &record.manifest.version,
        &current_dir,
    )?;

    Ok(RenderOutcome {
        skill_id: record.manifest.id.clone(),
        version: record.manifest.version.clone(),
        state: "rendered".to_string(),
        reason: None,
        rendered_root: current_dir,
        files: rendered_files,
    })
}

fn uninstall_skill(
    target_root: &Path,
    skill_id: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    let render_root = target_root.join(skill_id);
    let current_dir = render_root.join("current");
    let previous_dir = render_root.join("previous");
    if !render_root.exists() {
        return Ok(json!({
            "skill_id": skill_id,
            "removed": false,
        }));
    }
    if current_dir.exists() {
        let receipt = read_receipt(&current_dir)?;
        validate_rendered_skill(&current_dir, &receipt)?;
        remove_rendered_tree(&current_dir, &receipt)?;
    }
    if previous_dir.exists() {
        let receipt = read_receipt(&previous_dir)?;
        let _ = validate_rendered_skill(&previous_dir, &receipt);
        let _ = fs::remove_dir_all(&previous_dir);
    }
    let _ = fs::remove_file(render_root.join("current.json"));
    let _ = fs::remove_file(render_root.join("previous.json"));
    if render_root.read_dir()?.next().is_none() {
        fs::remove_dir_all(&render_root)?;
    }
    Ok(json!({
        "skill_id": skill_id,
        "removed": true,
    }))
}

fn resolve_target(store: &SkillStore) -> CodexTarget {
    if let Some(path) = env::var_os("HIMIND_CODEX_SKILL_DIR") {
        return CodexTarget {
            root: PathBuf::from(path),
            source: "env:HIMIND_CODEX_SKILL_DIR".to_string(),
            configured: true,
        };
    }
    if let Some(path) = env::var_os("CODEX_SKILL_DIR") {
        return CodexTarget {
            root: PathBuf::from(path),
            source: "env:CODEX_SKILL_DIR".to_string(),
            configured: true,
        };
    }
    let candidates = codex_default_candidates(store);
    if let Some((source, path)) = candidates.iter().find(|(_, path)| path.exists()).cloned() {
        return CodexTarget {
            root: path,
            source,
            configured: false,
        };
    }
    let (source, path) = candidates.into_iter().next().unwrap_or_else(|| {
        (
            "preview".to_string(),
            store.rendered_skill_root("codex", ".preview"),
        )
    });
    CodexTarget {
        root: path,
        source,
        configured: false,
    }
}

fn codex_default_candidates(store: &SkillStore) -> Vec<(String, PathBuf)> {
    let mut candidates = Vec::new();
    if let Some(userprofile) = env::var_os("USERPROFILE") {
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
    let content = fs::read_to_string(root.join(".himind-render.json"))?;
    Ok(serde_json::from_str(
        content.trim_start_matches('\u{feff}'),
    )?)
}

fn validate_rendered_skill(root: &Path, receipt: &SkillReceipt) -> Result<(), Box<dyn Error>> {
    let checksums = compute_checksums(root, ".himind-render.json")?;
    if checksums != receipt.checksums {
        return Err(format!("rendered skill was modified: {}", receipt.skill_id).into());
    }
    Ok(())
}

fn rendered_drift(root: &Path, receipt: &SkillReceipt) -> Result<Vec<String>, Box<dyn Error>> {
    let actual = compute_checksums(root, ".himind-render.json")?;
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

fn write_pointer(path: &Path, version: &str, target: &Path) -> Result<(), Box<dyn Error>> {
    let pointer = json!({
        "version": version,
        "path": target.file_name().and_then(|value| value.to_str()).unwrap_or_default(),
        "updated_at": unique_stamp(),
    });
    fs::write(path, serde_json::to_vec_pretty(&pointer)?)?;
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
        let target = resolve_target(&store);
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
        let record = SkillRecord {
            manifest,
            root: skill_root.clone(),
            version_root: version_root.clone(),
            current: true,
            previous_version: None,
        };
        let target_root = root.join("rendered");
        let outcome = render_skill(&target_root, &record).unwrap();
        assert_eq!(outcome.state, "rendered");
        let removed = uninstall_skill(&target_root, "demo.skill").unwrap();
        assert_eq!(removed["removed"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unsafe_skill_id_on_uninstall() {
        let root = std::env::temp_dir().join(format!("himind-codex-test-{}", unique_stamp()));
        fs::create_dir_all(&root).unwrap();

        let error = uninstall_skill(&root, "..\\outside").unwrap_err();

        assert!(error.to_string().contains("invalid skill id"));
        let _ = fs::remove_dir_all(root);
    }
}
