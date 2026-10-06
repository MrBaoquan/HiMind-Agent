//! Workspace instruction discovery and snapshots.
//!
//! DSH already owns the runtime loading of AGENTS.md/CLAUDE.md. This module
//! mirrors that deterministic discovery contract so HiMind can record what the
//! session was expected to load without duplicating the instructions in the
//! prompt or treating them as a permission source.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::atomic_file;

pub(crate) const RESOLVER_VERSION: &str = "workspace-instructions.v1";
pub(crate) const DEFAULT_MAX_BYTES: usize = 65_536;
pub(crate) const DEFAULT_MAX_SOURCE_BYTES: usize = 1_048_576;
pub(crate) const HIMIND_MANAGED_BEGIN: &str = "<!-- HIMIND:BEGIN WORKSPACE-INSTRUCTIONS -->";
pub(crate) const HIMIND_MANAGED_END: &str = "<!-- HIMIND:END WORKSPACE-INSTRUCTIONS -->";

const BASE_CANDIDATES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const LOCAL_CANDIDATES: &[&str] = &["AGENTS.local.md", "CLAUDE.local.md"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionResolverConfig {
    pub max_bytes: usize,
    pub max_source_bytes: usize,
    pub project_root_markers: Vec<String>,
    pub instruction_file_candidates: Vec<String>,
    pub local_instruction_file_candidates: Vec<String>,
}

impl Default for InstructionResolverConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            project_root_markers: vec![".git".to_string()],
            instruction_file_candidates: BASE_CANDIDATES.iter().map(|v| (*v).to_string()).collect(),
            local_instruction_file_candidates: LOCAL_CANDIDATES
                .iter()
                .map(|v| (*v).to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstructionScope {
    Global,
    Project,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstructionSourceStatus {
    Loaded,
    Omitted,
    Truncated,
    Conflicted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionSourceSnapshot {
    pub path: String,
    pub scope: InstructionScope,
    pub precedence: u32,
    pub digest: String,
    pub source_bytes: usize,
    pub rendered_bytes: usize,
    pub status: InstructionSourceStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionProjection {
    pub adapter_id: String,
    pub status: String,
    pub dsh_max_bytes: usize,
    pub dsh_max_source_bytes: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default)]
    pub project_root_markers: Vec<String>,
    #[serde(default)]
    pub instruction_file_candidates: Vec<String>,
    #[serde(default)]
    pub local_instruction_file_candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionSnapshot {
    pub schema_version: String,
    pub resolver_version: String,
    pub workspace_root: String,
    pub project_root: String,
    pub resolved_at_ms: u128,
    pub rendered_bytes: usize,
    pub rendered_digest: String,
    pub sources: Vec<InstructionSourceSnapshot>,
    pub projection: InstructionProjection,
    #[serde(default)]
    pub instruction_packs: Vec<crate::instruction_pack::InstructionPackRef>,
    #[serde(skip)]
    pub(crate) rendered_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionOverlay {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub content: String,
}

#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    scope: InstructionScope,
    precedence: u32,
    content: Vec<u8>,
    normalized: String,
}

/// Resolve the instruction chain using the same file names and precedence as
/// DSH's `dsh-agent-instructions` package.
pub(crate) fn resolve(
    workspace: &Path,
    dsh_home: &Path,
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    resolve_with_config_and_overlays(
        workspace,
        dsh_home,
        &InstructionResolverConfig::default(),
        "native_loaded",
        "",
        &[],
    )
}

pub(crate) fn resolve_with_overlays(
    workspace: &Path,
    dsh_home: &Path,
    overlays: &[InstructionOverlay],
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    resolve_with_config_and_overlays(
        workspace,
        dsh_home,
        &InstructionResolverConfig::default(),
        "projected_managed",
        "explicitly selected HiMind instruction packs",
        overlays,
    )
}

fn resolve_with_config(
    workspace: &Path,
    dsh_home: &Path,
    config: &InstructionResolverConfig,
    projection_status: &str,
    projection_reason: &str,
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    resolve_with_config_and_overlays(
        workspace,
        dsh_home,
        config,
        projection_status,
        projection_reason,
        &[],
    )
}

fn resolve_with_config_and_overlays(
    workspace: &Path,
    dsh_home: &Path,
    config: &InstructionResolverConfig,
    projection_status: &str,
    projection_reason: &str,
    overlays: &[InstructionOverlay],
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    let workspace_root = workspace.canonicalize()?;
    if !workspace_root.is_dir() {
        return Err(format!("工作区不是目录: {}", workspace.display()).into());
    }
    let dsh_home = dsh_home
        .canonicalize()
        .unwrap_or_else(|_| dsh_home.to_path_buf());
    let project_root = discover_project_root(&workspace_root, &config.project_root_markers);
    let mut candidates = Vec::new();
    let mut precedence = 0u32;

    let global = dsh_home.join("AGENTS.md");
    if global.is_file() {
        collect_candidate(
            &global,
            InstructionScope::Global,
            precedence,
            config.max_source_bytes,
            &mut candidates,
        )?;
        precedence = precedence.saturating_add(1);
    }

    let chain = directory_chain(&project_root, &workspace_root);
    for (index, directory) in chain.iter().enumerate() {
        let scope = if index == 0 {
            InstructionScope::Project
        } else {
            InstructionScope::Directory
        };
        for name in config
            .instruction_file_candidates
            .iter()
            .chain(config.local_instruction_file_candidates.iter())
        {
            let path = directory.join(name);
            if !path.is_file() {
                continue;
            }
            collect_candidate(
                &path,
                scope.clone(),
                precedence,
                config.max_source_bytes,
                &mut candidates,
            )?;
            precedence = precedence.saturating_add(1);
        }
    }

    for overlay in overlays {
        if overlay.content.trim().is_empty() {
            continue;
        }
        candidates.push(Candidate {
            path: PathBuf::from(format!(
                "himind://instruction-pack/{}/{}",
                overlay.id, overlay.version
            )),
            scope: InstructionScope::Project,
            precedence,
            content: overlay.content.trim().as_bytes().to_vec(),
            normalized: format!("{}:{}:{}", overlay.id, overlay.version, overlay.digest),
        });
        precedence = precedence.saturating_add(1);
    }

    // DSH folds identical same-directory candidates (for example a symlinked
    // CLAUDE.md that contains the same content as AGENTS.md). Keep the first
    // path so the snapshot remains stable and explainable.
    let mut deduped = Vec::with_capacity(candidates.len());
    let mut same_scope_content = HashSet::new();
    for candidate in candidates {
        let key = (
            candidate.path.parent().map(Path::to_path_buf),
            candidate.normalized.clone(),
        );
        if !same_scope_content.insert(key) {
            continue;
        }
        deduped.push(candidate);
    }

    let mut source_snapshots = deduped
        .iter()
        .map(|candidate| InstructionSourceSnapshot {
            path: display_path(&candidate.path),
            scope: candidate.scope.clone(),
            precedence: candidate.precedence,
            digest: digest(&candidate.content),
            source_bytes: candidate.content.len(),
            rendered_bytes: 0,
            status: InstructionSourceStatus::Omitted,
            reason: "instruction budget not yet allocated".to_string(),
        })
        .collect::<Vec<_>>();

    let mut remaining = config.max_bytes;
    let mut selected = vec![false; deduped.len()];
    let mut rendered_contents = vec![Vec::new(); deduped.len()];
    // Allocate from the most specific source backwards. This matches DSH's
    // behavior of dropping broad context before truncating the specific file.
    for index in (0..deduped.len()).rev() {
        if remaining == 0 {
            source_snapshots[index].reason = "instruction budget exhausted".to_string();
            continue;
        }
        let content = &deduped[index].content;
        if content.len() <= remaining {
            selected[index] = true;
            rendered_contents[index] = content.clone();
            remaining -= content.len();
            continue;
        }
        selected[index] = true;
        rendered_contents[index] = content[..remaining].to_vec();
        source_snapshots[index].status = InstructionSourceStatus::Truncated;
        source_snapshots[index].reason =
            "most-specific source truncated to fit DSH budget".to_string();
        source_snapshots[index].rendered_bytes = remaining;
        remaining = 0;
    }

    let mut rendered = Vec::new();
    for (index, content) in rendered_contents.iter().enumerate() {
        if !selected[index] {
            source_snapshots[index].status = InstructionSourceStatus::Omitted;
            source_snapshots[index].reason =
                "broader source omitted to preserve specific instructions".to_string();
            continue;
        }
        if source_snapshots[index].status != InstructionSourceStatus::Truncated {
            source_snapshots[index].status = InstructionSourceStatus::Loaded;
            source_snapshots[index].reason.clear();
        }
        source_snapshots[index].rendered_bytes = content.len();
        if !rendered.is_empty() {
            rendered.extend_from_slice(b"\n\n");
        }
        rendered.extend_from_slice(content);
    }

    Ok(InstructionSnapshot {
        schema_version: "instruction_snapshot.v1".to_string(),
        resolver_version: RESOLVER_VERSION.to_string(),
        workspace_root: display_path(&workspace_root),
        project_root: display_path(&project_root),
        resolved_at_ms: unix_time_millis(),
        rendered_bytes: rendered.len(),
        rendered_digest: digest(&rendered),
        sources: source_snapshots,
        projection: InstructionProjection {
            adapter_id: "himind-dsh".to_string(),
            status: projection_status.to_string(),
            dsh_max_bytes: config.max_bytes,
            dsh_max_source_bytes: config.max_source_bytes,
            reason: projection_reason.to_string(),
            project_root_markers: config.project_root_markers.clone(),
            instruction_file_candidates: config.instruction_file_candidates.clone(),
            local_instruction_file_candidates: config.local_instruction_file_candidates.clone(),
        },
        instruction_packs: overlays
            .iter()
            .map(|overlay| crate::instruction_pack::InstructionPackRef {
                id: overlay.id.clone(),
                version: overlay.version.clone(),
                digest: overlay.digest.clone(),
            })
            .collect(),
        rendered_content: String::from_utf8_lossy(&rendered).to_string(),
    })
}

/// Resolve and persist the session's instruction snapshot under its isolated
/// DSH home. The write is atomic and guarded so concurrent DSH sessions do not
/// corrupt each other's snapshots.
pub(crate) fn record_dsh_snapshot(
    dsh_home: &Path,
    workspace: &Path,
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    let (selected, overlays) = crate::instruction_pack::selected_overlays(workspace)?;
    let mut snapshot = if overlays.is_empty() {
        resolve(workspace, dsh_home)?
    } else {
        resolve_with_overlays(workspace, dsh_home, &overlays)?
    };
    snapshot.instruction_packs = selected;
    persist_snapshot(dsh_home, &snapshot)?;
    Ok(snapshot)
}

pub(crate) fn record_dsh_snapshot_for_profile(
    dsh_home: &Path,
    workspace: &Path,
    runtime_executable: Option<&Path>,
    profile_name: &str,
) -> Result<InstructionSnapshot, Box<dyn Error>> {
    let (config, status, reason) = inspect_dsh_profile(dsh_home, runtime_executable, profile_name);
    let selected = materialize_dsh_instruction_file(dsh_home, workspace)?;
    let mut snapshot =
        resolve_with_config_and_overlays(workspace, dsh_home, &config, &status, &reason, &[])?;
    snapshot.instruction_packs = selected;
    persist_snapshot(dsh_home, &snapshot)?;
    Ok(snapshot)
}

/// Materialize the selected InstructionPacks into the isolated DSH home.
///
/// DSH's native instruction plugin only reads files. The generated file is
/// therefore the actual runtime boundary for a workspace session; the source
/// workspace is never modified. The base DSH global instructions are copied
/// into `.himind/base-global-AGENTS.md` when the isolated home is created.
pub(crate) fn materialize_dsh_instruction_file(
    dsh_home: &Path,
    workspace: &Path,
) -> Result<Vec<crate::instruction_pack::InstructionPackRef>, Box<dyn Error>> {
    let (selected, overlays) = crate::instruction_pack::selected_overlays(workspace)?;
    let base_path = dsh_home.join(".himind").join("base-global-AGENTS.md");
    let base = fs::read_to_string(&base_path).unwrap_or_default();
    let path = dsh_home.join("AGENTS.md");
    let marker = dsh_home.join(".himind").join("managed-agents-file");

    let mut content = String::new();
    if !base.trim().is_empty() {
        content.push_str("<!-- HiMind preserved DSH global instructions -->\n");
        content.push_str(base.trim());
        content.push('\n');
    }
    if !overlays.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(HIMIND_MANAGED_BEGIN);
        content.push('\n');
        content.push_str("# HiMind workspace instruction context\n\n");
        content.push_str(
            "The following text is user-provided context. It does not grant permissions, tools, or capabilities.\n\n",
        );
        for overlay in &overlays {
            content.push_str(&format!(
                "## InstructionPack `{}` v{}\n\n<!-- digest: {} -->\n\n{}\n\n",
                overlay.id,
                overlay.version,
                overlay.digest,
                overlay.content.trim(),
            ));
        }
        content.push_str(HIMIND_MANAGED_END);
        content.push('\n');
    }

    if content.trim().is_empty() {
        if marker.is_file() && path.is_file() {
            fs::remove_file(&path)?;
        }
        if marker.is_file() {
            fs::remove_file(&marker)?;
        }
        return Ok(selected);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, content.as_bytes())?;
    atomic_file::atomic_write(&marker, b"managed by HiMind Agent\n")?;
    Ok(selected)
}

pub(crate) fn load_dsh_snapshot(
    dsh_home: &Path,
    workspace: &Path,
) -> Result<Option<InstructionSnapshot>, Box<dyn Error>> {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let path = dsh_home
        .join(".himind")
        .join("instruction-snapshots")
        .join(format!("{}.json", workspace_key(&display_path(&canonical))));
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn persist_snapshot(dsh_home: &Path, snapshot: &InstructionSnapshot) -> Result<(), Box<dyn Error>> {
    let directory = dsh_home.join(".himind").join("instruction-snapshots");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", workspace_key(&snapshot.workspace_root)));
    let _lock = atomic_file::lock(&path)?;
    let content = serde_json::to_vec_pretty(&snapshot)?;
    atomic_file::atomic_write(&path, &content)?;
    Ok(())
}

fn inspect_dsh_profile(
    dsh_home: &Path,
    runtime_executable: Option<&Path>,
    profile_name: &str,
) -> (InstructionResolverConfig, String, String) {
    let mut config = InstructionResolverConfig::default();
    let profile = dsh_home.join("profiles").join(profile_name);
    let profile_patch = profile.join("cordis.patch.yml");
    let mut rows = Vec::new();
    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(
        &fs::read_to_string(&profile_patch).unwrap_or_default(),
    ) {
        collect_patch_rows(&value, &mut rows);
    }

    let package = profile.join("package.json");
    let bundles = fs::read_to_string(&package)
        .ok()
        .and_then(|source| serde_json::from_str::<serde_json::Value>(&source).ok())
        .and_then(|value| value.pointer("/dsh/profile/bundles").cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let mut bundle_rows = Vec::new();
    for bundle in bundles.iter().filter_map(|value| value.as_str()) {
        for root in bundle_roots(dsh_home, runtime_executable) {
            let patch = root.join(bundle).join("cordis.patch.yml");
            if let Ok(source) = fs::read_to_string(patch) {
                if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&source) {
                    collect_patch_rows(&value, &mut bundle_rows);
                    break;
                }
            }
        }
    }
    bundle_rows.extend(rows);
    let Some(row) = bundle_rows
        .into_iter()
        .filter(|row| row.id == "agent-instructions")
        .last()
    else {
        return (
            config,
            "blocked".to_string(),
            "DSH profile does not mount dsh-agent-instructions".to_string(),
        );
    };
    if row.disabled {
        return (
            config,
            "blocked".to_string(),
            "DSH profile disables dsh-agent-instructions".to_string(),
        );
    }
    if let Some(value) = row.config {
        apply_config(&mut config, &value);
    }
    if config.max_bytes == 0 {
        return (
            config,
            "blocked".to_string(),
            "dsh-agent-instructions has no positive maxBytes".to_string(),
        );
    }
    (config, "native_loaded".to_string(), String::new())
}

#[derive(Debug)]
struct PatchRow {
    id: String,
    disabled: bool,
    config: Option<serde_yaml::Value>,
}

fn collect_patch_rows(value: &serde_yaml::Value, output: &mut Vec<PatchRow>) {
    let Some(sequence) = value.as_sequence() else {
        return;
    };
    for item in sequence {
        let Some(mapping) = item.as_mapping() else {
            continue;
        };
        if let Some(insert) = mapping.get(serde_yaml::Value::String("insert".to_string())) {
            collect_patch_rows(insert, output);
        }
        let id = mapping
            .get(serde_yaml::Value::String("id".to_string()))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let disabled = mapping
            .get(serde_yaml::Value::String("disabled".to_string()))
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        let config = mapping
            .get(serde_yaml::Value::String("config".to_string()))
            .cloned();
        output.push(PatchRow {
            id,
            disabled,
            config,
        });
    }
}

fn apply_config(config: &mut InstructionResolverConfig, value: &serde_yaml::Value) {
    let Some(mapping) = value.as_mapping() else {
        return;
    };
    if let Some(value) = mapping
        .get(serde_yaml::Value::String("maxBytes".to_string()))
        .and_then(serde_yaml::Value::as_u64)
    {
        config.max_bytes = value.min(usize::MAX as u64) as usize;
    }
    if let Some(value) = mapping
        .get(serde_yaml::Value::String("maxSourceBytes".to_string()))
        .and_then(serde_yaml::Value::as_u64)
    {
        config.max_source_bytes = value.min(usize::MAX as u64) as usize;
    }
    if let Some(value) = string_list(mapping, "projectRootMarkers") {
        config.project_root_markers = value;
    }
    if let Some(value) = string_list(mapping, "instructionFileCandidates") {
        config.instruction_file_candidates = value;
    }
    if let Some(value) = string_list(mapping, "localInstructionFileCandidates") {
        config.local_instruction_file_candidates = value;
    }
}

fn string_list(mapping: &serde_yaml::Mapping, key: &str) -> Option<Vec<String>> {
    mapping
        .get(serde_yaml::Value::String(key.to_string()))
        .and_then(serde_yaml::Value::as_sequence)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
}

fn bundle_roots(dsh_home: &Path, runtime_executable: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![dsh_home.join("profiles").join("node_modules")];
    if let Some(executable) = runtime_executable {
        let mut cursor = executable.parent();
        for _ in 0..4 {
            let Some(path) = cursor else { break };
            roots.push(path.join("node_modules"));
            cursor = path.parent();
        }
    }
    roots
}

fn collect_candidate(
    path: &Path,
    scope: InstructionScope,
    precedence: u32,
    max_source_bytes: usize,
    output: &mut Vec<Candidate>,
) -> Result<(), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(());
    }
    if metadata.len() > max_source_bytes as u64 {
        return Ok(());
    }
    let content = fs::read(path)?;
    if content.len() > max_source_bytes {
        return Ok(());
    }
    let normalized = strip_himind_managed_block(&String::from_utf8_lossy(&content))
        .replace("\r\n", "\n")
        .trim()
        .to_string();
    if normalized.is_empty() {
        return Ok(());
    }
    output.push(Candidate {
        path: path.to_path_buf(),
        scope,
        precedence,
        content: normalized.as_bytes().to_vec(),
        normalized,
    });
    Ok(())
}

fn strip_himind_managed_block(content: &str) -> String {
    let Some(begin) = content.find(HIMIND_MANAGED_BEGIN) else {
        return content.to_string();
    };
    let Some(end_offset) = content[begin..].find(HIMIND_MANAGED_END) else {
        return content.to_string();
    };
    let end = begin + end_offset + HIMIND_MANAGED_END.len();
    let mut remainder = String::with_capacity(content.len().saturating_sub(end - begin));
    remainder.push_str(&content[..begin]);
    remainder.push_str(&content[end..]);
    remainder
}

fn discover_project_root(workspace: &Path, markers: &[String]) -> PathBuf {
    let mut cursor = workspace.to_path_buf();
    loop {
        if markers.iter().any(|marker| cursor.join(marker).exists()) {
            return cursor;
        }
        let Some(parent) = cursor.parent() else {
            return workspace.to_path_buf();
        };
        if parent == cursor {
            return workspace.to_path_buf();
        }
        cursor = parent.to_path_buf();
    }
}

fn directory_chain(project_root: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut cursor = workspace.to_path_buf();
    loop {
        chain.push(cursor.clone());
        if cursor == project_root {
            break;
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor || !parent.starts_with(project_root) {
            break;
        }
        cursor = parent.to_path_buf();
    }
    chain.reverse();
    chain
}

fn digest(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("sha256:{:x}", hasher.finalize())
}

fn workspace_key(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-instructions-{label}-{}-{}",
            std::process::id(),
            unix_time_millis()
        ))
    }

    #[test]
    fn resolves_global_project_and_nested_instructions_in_precedence_order() {
        let root = temp_root("precedence");
        let workspace = root.join("project").join("packages").join("app");
        let dsh_home = root.join("dsh");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&dsh_home).unwrap();
        fs::create_dir_all(root.join("project").join(".git")).unwrap();
        fs::write(dsh_home.join("AGENTS.md"), "global").unwrap();
        fs::write(root.join("project").join("AGENTS.md"), "project").unwrap();
        fs::write(workspace.join("AGENTS.md"), "nested").unwrap();

        let snapshot = resolve(&workspace, &dsh_home).unwrap();
        assert_eq!(snapshot.sources.len(), 3);
        assert_eq!(snapshot.sources[0].scope, InstructionScope::Global);
        assert_eq!(snapshot.sources[1].scope, InstructionScope::Project);
        assert_eq!(snapshot.sources[2].scope, InstructionScope::Directory);
        assert!(snapshot
            .sources
            .iter()
            .all(|source| { source.status == InstructionSourceStatus::Loaded }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deduplicates_same_directory_content_and_preserves_specific_content_when_over_budget() {
        let root = temp_root("budget");
        let workspace = root.join("project");
        let dsh_home = root.join("dsh");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::create_dir_all(&dsh_home).unwrap();
        fs::write(workspace.join("AGENTS.md"), "same").unwrap();
        fs::write(workspace.join("CLAUDE.md"), "same").unwrap();
        let nested = workspace.join("deep");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("AGENTS.md"), "specific").unwrap();

        let snapshot = resolve(&nested, &dsh_home).unwrap();
        assert_eq!(snapshot.sources.len(), 2);
        assert_eq!(snapshot.sources[1].status, InstructionSourceStatus::Loaded);
        assert_eq!(snapshot.sources[1].rendered_bytes, "specific".len());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn records_snapshot_under_isolated_home() {
        let root = temp_root("record");
        let workspace = root.join("project");
        let dsh_home = root.join("dsh");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::create_dir_all(&dsh_home).unwrap();
        fs::write(workspace.join("AGENTS.md"), "instructions").unwrap();

        let snapshot = record_dsh_snapshot(&dsh_home, &workspace).unwrap();
        let files = fs::read_dir(dsh_home.join(".himind").join("instruction-snapshots"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .count();
        assert_eq!(files, 1);
        assert_eq!(snapshot.projection.status, "native_loaded");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn materializes_dsh_home_instruction_file_without_touching_workspace() {
        let root = temp_root("materialize");
        let workspace = root.join("project");
        let dsh_home = root.join("dsh");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::create_dir_all(dsh_home.join(".himind")).unwrap();
        fs::write(
            dsh_home.join(".himind").join("base-global-AGENTS.md"),
            "global rule",
        )
        .unwrap();

        let selected = materialize_dsh_instruction_file(&dsh_home, &workspace).unwrap();
        assert!(selected.is_empty());
        assert_eq!(
            fs::read_to_string(dsh_home.join("AGENTS.md")).unwrap(),
            "<!-- HiMind preserved DSH global instructions -->\nglobal rule\n"
        );
        assert!(!workspace.join("AGENTS.md").exists());

        fs::remove_file(dsh_home.join(".himind").join("base-global-AGENTS.md")).unwrap();
        materialize_dsh_instruction_file(&dsh_home, &workspace).unwrap();
        assert!(!dsh_home.join("AGENTS.md").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn strips_previous_himind_projection_before_resolving() {
        let root = temp_root("managed-block");
        let workspace = root.join("project");
        let dsh_home = root.join("dsh");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::create_dir_all(&dsh_home).unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            format!(
                "user rule\n\n{}\nold managed text\n{}\n",
                HIMIND_MANAGED_BEGIN, HIMIND_MANAGED_END
            ),
        )
        .unwrap();
        let snapshot = resolve_with_overlays(
            &workspace,
            &dsh_home,
            &[InstructionOverlay {
                id: "com.himind.instruction.test".to_string(),
                version: "1.0.0".to_string(),
                digest: "sha256:test".to_string(),
                content: "pack rule".to_string(),
            }],
        )
        .unwrap();
        assert!(snapshot.rendered_content.contains("user rule"));
        assert!(snapshot.rendered_content.contains("pack rule"));
        assert!(!snapshot.rendered_content.contains("old managed text"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profile_inspection_reads_agent_instruction_configuration() {
        let root = temp_root("profile-config");
        let profile = root.join("profiles").join("himind-headless");
        let bundle = root
            .join("profiles")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-base");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            profile.join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base"]}}}"#,
        )
        .unwrap();
        fs::write(
            bundle.join("cordis.patch.yml"),
            "- insert:\n    - id: agent-instructions\n      config:\n        maxBytes: 1234\n        maxSourceBytes: 4321\n        projectRootMarkers: [workspace-root]\n        instructionFileCandidates: [PROJECT.md]\n        localInstructionFileCandidates: [PROJECT.local.md]\n",
        )
        .unwrap();
        let (config, status, reason) = inspect_dsh_profile(&root, None, "himind-headless");
        assert_eq!(status, "native_loaded");
        assert!(reason.is_empty());
        assert_eq!(config.max_bytes, 1234);
        assert_eq!(config.max_source_bytes, 4321);
        assert_eq!(config.project_root_markers, vec!["workspace-root"]);
        assert_eq!(config.instruction_file_candidates, vec!["PROJECT.md"]);
        assert_eq!(
            config.local_instruction_file_candidates,
            vec!["PROJECT.local.md"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profile_inspection_reports_disabled_instruction_plugin() {
        let root = temp_root("profile-disabled");
        let profile = root.join("profiles").join("himind-headless");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("cordis.patch.yml"),
            "- id: agent-instructions\n  disabled: true\n",
        )
        .unwrap();
        let (_, status, reason) = inspect_dsh_profile(&root, None, "himind-headless");
        assert_eq!(status, "blocked");
        assert!(reason.contains("disables"));
        let _ = fs::remove_dir_all(root);
    }
}
