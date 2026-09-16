//! Skill deployment targets.
//!
//! `SkillScope` describes who governs a Skill (builtin, organization, or user).
//! It deliberately does not describe where a Skill is installed.  A single
//! immutable record in the Agent store can be projected to the global client
//! directory or to one repository/workspace.  Keeping those concepts separate
//! allows the same Skill ID to be used at different versions by different
//! projects without changing the portable `SKILL.md` package format.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

const WORKSPACE_CONFIG_FILE: &str = "skill-workspace.json";
const DEPLOYMENTS_FILE: &str = "skill-deployments.json";
const WORKSPACE_LOCK_FILE: &str = "skills.lock.json";

pub(crate) const TARGET_KIND_GLOBAL: &str = "global";
pub(crate) const TARGET_KIND_WORKSPACE: &str = "workspace";
pub(crate) const MANAGEMENT_MODE_MANAGED: &str = "managed";
pub(crate) const MANAGEMENT_MODE_NATIVE: &str = "native";

/// A client-native directory that receives a rendered Skill.
///
/// The workspace fields are optional for backwards compatibility with old
/// global receipts.  They are populated for every new project deployment and
/// form the stable identity used by status/repair/uninstall operations.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillTarget {
    pub(crate) root: PathBuf,
    pub(crate) source: String,
    pub(crate) configured: bool,
    pub(crate) target_kind: String,
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) workspace_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillWorkspaceStatus {
    pub(crate) configured: bool,
    pub(crate) valid: bool,
    pub(crate) root: String,
    pub(crate) workspace_id: String,
    pub(crate) agents_skills_root: String,
    pub(crate) lock_path: String,
    pub(crate) managed_skill_count: usize,
    pub(crate) managed_skills: Vec<SkillWorkspaceSkillStatus>,
    pub(crate) error: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillWorkspaceSkillStatus {
    pub(crate) skill_id: String,
    pub(crate) version: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DiscoveredProjectSkill {
    pub(crate) skill_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) path: String,
    pub(crate) managed_by_himind: bool,
    pub(crate) management_mode: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillConflict {
    pub(crate) skill_id: String,
    pub(crate) managed_paths: Vec<String>,
    pub(crate) native_paths: Vec<String>,
    pub(crate) reason: String,
}

/// A project-local declaration for Skills installed by HiMind.  This file is
/// intentionally separate from the standard client directories: clients read
/// `SKILL.md`, while HiMind reads this lock to decide which Store records are
/// allowed to be projected into the workspace.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct SkillWorkspaceLockEntry {
    pub(crate) version: String,
    pub(crate) sha256: String,
    #[serde(default)]
    pub(crate) source: String,
    #[serde(default = "default_management_mode")]
    pub(crate) management: String,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct SkillWorkspaceLock {
    #[serde(default = "default_lock_schema_version")]
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) skills: BTreeMap<String, SkillWorkspaceLockEntry>,
}

fn default_lock_schema_version() -> u32 {
    1
}

fn default_management_mode() -> String {
    MANAGEMENT_MODE_MANAGED.to_string()
}

fn default_enabled() -> bool {
    true
}

/// A durable index of client projections.
///
/// The Store owns the immutable package, while this index owns the
/// relationship between that package and every client/workspace projection.
/// Keeping this relationship outside a single "current workspace" setting is
/// what lets a user install one Skill globally and in several repositories at
/// the same time, and lets global uninstall avoid deleting a package still
/// needed by another project.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct SkillDeployment {
    pub(crate) skill_id: String,
    pub(crate) version: String,
    pub(crate) client_id: String,
    pub(crate) target_kind: String,
    #[serde(default)]
    pub(crate) workspace_root: Option<String>,
    #[serde(default)]
    pub(crate) workspace_id: Option<String>,
    pub(crate) rendered_root: String,
    pub(crate) updated_at: String,
    #[serde(default = "default_management_mode")]
    pub(crate) management_mode: String,
    #[serde(default)]
    pub(crate) source: String,
    #[serde(default)]
    pub(crate) content_sha256: String,
}

impl SkillTarget {
    pub(crate) fn global(root: PathBuf, source: impl Into<String>, configured: bool) -> Self {
        Self {
            root,
            source: source.into(),
            configured,
            target_kind: TARGET_KIND_GLOBAL.to_string(),
            workspace_root: None,
            workspace_id: None,
        }
    }

    pub(crate) fn workspace(
        workspace_root: &Path,
        client_relative_dir: &str,
        source: impl Into<String>,
    ) -> Result<Self, Box<dyn Error>> {
        let workspace_root = canonical_workspace_root(workspace_root)?;
        let relative = validate_relative_directory(client_relative_dir)?;
        let root = workspace_root.join(relative);
        Ok(Self {
            root,
            source: source.into(),
            configured: true,
            target_kind: TARGET_KIND_WORKSPACE.to_string(),
            workspace_id: Some(workspace_id(&workspace_root)),
            workspace_root: Some(workspace_root),
        })
    }

    pub(crate) fn is_workspace(&self) -> bool {
        self.target_kind == TARGET_KIND_WORKSPACE
    }
}

/// Resolve an explicit workspace or the process-level opt-in used by CLI/MCP.
///
/// Project deployment is never inferred from the current directory.  Callers
/// must pass a workspace explicitly or set `HIMIND_SKILL_WORKSPACE`; this keeps
/// a normal global sync from unexpectedly modifying the repository in which
/// the Agent happened to be started.
pub(crate) fn resolve_workspace_root(
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if env::var("HIMIND_SKILL_TARGET")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case(TARGET_KIND_GLOBAL))
    {
        return Ok(None);
    }
    let candidate = explicit
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("HIMIND_SKILL_WORKSPACE").map(PathBuf::from))
        .or_else(load_persisted_workspace);
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    Ok(Some(canonical_workspace_root(&candidate)?))
}

pub(crate) fn workspace_lock_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".himind").join(WORKSPACE_LOCK_FILE)
}

pub(crate) fn read_workspace_lock(
    workspace_root: &Path,
) -> Result<SkillWorkspaceLock, Box<dyn Error>> {
    let path = workspace_lock_path(workspace_root);
    if !path.is_file() {
        return Ok(SkillWorkspaceLock {
            schema_version: default_lock_schema_version(),
            skills: BTreeMap::new(),
        });
    }
    let content = std::fs::read_to_string(path)?;
    let mut lock: SkillWorkspaceLock =
        serde_json::from_str(content.trim_start_matches('\u{feff}'))?;
    if lock.schema_version == 0 {
        lock.schema_version = default_lock_schema_version();
    }
    for entry in lock.skills.values_mut() {
        if entry.management.trim().is_empty() {
            entry.management = default_management_mode();
        }
    }
    Ok(lock)
}

fn write_workspace_lock(
    workspace_root: &Path,
    lock: &SkillWorkspaceLock,
) -> Result<(), Box<dyn Error>> {
    let path = workspace_lock_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(lock)?)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

/// Record a successful HiMind-managed deployment in the project lock file.
/// The lock is deliberately project-local and contains no client-specific
/// paths, so it can be committed or copied independently of the machine-wide
/// Store.  Client receipts remain the source of truth for file cleanup.
pub(crate) fn record_workspace_skill(
    target: &SkillTarget,
    record: &crate::skill::types::SkillRecord,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    let Some(workspace_root) = target.workspace_root.as_deref() else {
        return Ok(());
    };
    let mut lock = read_workspace_lock(workspace_root)?;
    lock.schema_version = default_lock_schema_version();
    lock.skills.insert(
        record.manifest.id.clone(),
        SkillWorkspaceLockEntry {
            version: record.manifest.version.clone(),
            sha256: package_content_sha256(&record.version_root)?,
            source: source.trim().to_string(),
            management: MANAGEMENT_MODE_MANAGED.to_string(),
            enabled: true,
        },
    );
    write_workspace_lock(workspace_root, &lock)
}

pub(crate) fn set_workspace_skill_enabled(
    workspace_root: &Path,
    skill_id: &str,
    enabled: bool,
) -> Result<bool, Box<dyn Error>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let mut lock = read_workspace_lock(&workspace_root)?;
    let Some(entry) = lock.skills.get_mut(skill_id) else {
        return Ok(false);
    };
    entry.enabled = enabled;
    write_workspace_lock(&workspace_root, &lock)?;
    Ok(true)
}

/// Return the version pinned for a workspace Skill, including deployments
/// written by older Agent versions before `.himind/skills.lock.json` existed.
/// `None` means this is a new explicit assignment and may be installed by a
/// user action.  A legacy deployment is intentionally treated as pinned so a
/// normal sync cannot silently upgrade it.
pub(crate) fn workspace_pinned_version(
    workspace_root: &Path,
    skill_id: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let lock = read_workspace_lock(&workspace_root)?;
    let deployments = deployments_for_skill(skill_id)?;
    Ok(pinned_version_in(
        &lock,
        &deployments,
        &workspace_id(&workspace_root),
        skill_id,
    ))
}

fn pinned_version_in(
    lock: &SkillWorkspaceLock,
    deployments: &[SkillDeployment],
    workspace_id: &str,
    skill_id: &str,
) -> Option<String> {
    if let Some(entry) = lock.skills.get(skill_id) {
        return Some(entry.version.clone());
    }
    deployments
        .iter()
        .filter(|item| {
            item.skill_id == skill_id
                && item.target_kind == TARGET_KIND_WORKSPACE
                && item.workspace_id.as_deref() == Some(workspace_id)
        })
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
        .map(|item| item.version.clone())
}

pub(crate) fn remove_workspace_skill(
    workspace_root: &Path,
    skill_id: &str,
) -> Result<bool, Box<dyn Error>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let mut lock = read_workspace_lock(&workspace_root)?;
    let removed = lock.skills.remove(skill_id).is_some();
    if removed {
        write_workspace_lock(&workspace_root, &lock)?;
    }
    Ok(removed)
}

pub(crate) fn remove_workspace_skill_if_unused(
    target: &SkillTarget,
    skill_id: &str,
) -> Result<(), Box<dyn Error>> {
    let Some(workspace_root) = target.workspace_root.as_deref() else {
        return Ok(());
    };
    // A disabled entry is explicit user state, not an orphan: keep the pinned
    // version so the project can be re-enabled later without losing the pin.
    if read_workspace_lock(workspace_root)?
        .skills
        .get(skill_id)
        .is_some_and(|entry| !entry.enabled)
    {
        return Ok(());
    }
    let still_deployed = deployments_for_skill(skill_id)?.into_iter().any(|item| {
        item.target_kind == TARGET_KIND_WORKSPACE && item.workspace_id == target.workspace_id
    });
    if !still_deployed {
        let _ = remove_workspace_skill(workspace_root, skill_id)?;
    }
    Ok(())
}

/// Drop the project assignment for a Skill.  This is used by an explicit
/// uninstall, where the user wants the project to forget the Skill entirely
/// instead of remembering a disabled entry.
pub(crate) fn remove_workspace_skill_entry(
    target: &SkillTarget,
    skill_id: &str,
) -> Result<(), Box<dyn Error>> {
    let Some(workspace_root) = target.workspace_root.as_deref() else {
        return Ok(());
    };
    let _ = remove_workspace_skill(workspace_root, skill_id)?;
    Ok(())
}

/// Create the project assignment for a Skill without moving an existing pin.
/// The caller uses this before disabling a Skill that was projected by an
/// older Agent version, so the disabled state has somewhere to live.
pub(crate) fn ensure_workspace_skill_entry(
    target: &SkillTarget,
    record: &crate::skill::types::SkillRecord,
    source: &str,
) -> Result<bool, Box<dyn Error>> {
    let Some(workspace_root) = target.workspace_root.as_deref() else {
        return Ok(false);
    };
    let mut lock = read_workspace_lock(workspace_root)?;
    if lock.skills.contains_key(&record.manifest.id) {
        return Ok(false);
    }
    lock.schema_version = default_lock_schema_version();
    lock.skills.insert(
        record.manifest.id.clone(),
        SkillWorkspaceLockEntry {
            version: record.manifest.version.clone(),
            sha256: package_content_sha256(&record.version_root)?,
            source: source.trim().to_string(),
            management: MANAGEMENT_MODE_MANAGED.to_string(),
            enabled: true,
        },
    );
    write_workspace_lock(workspace_root, &lock)?;
    Ok(true)
}

/// Return whether a Store record is explicitly assigned to this target.  A
/// global target accepts every Store record for backwards compatibility.  A
/// workspace target only accepts enabled entries in its lock file; legacy
/// deployments are accepted while the lock is being migrated.
pub(crate) fn target_allows_record(
    target: &SkillTarget,
    skill_id: &str,
    version: Option<&str>,
) -> Result<bool, Box<dyn Error>> {
    if !target.is_workspace() {
        return Ok(true);
    }
    let Some(root) = target.workspace_root.as_deref() else {
        return Ok(false);
    };
    let lock = read_workspace_lock(root)?;
    let deployments = deployments_for_skill(skill_id)?;
    Ok(record_allowed_in(
        &lock,
        &deployments,
        target.workspace_id.as_deref(),
        skill_id,
        version,
    ))
}

fn record_allowed_in(
    lock: &SkillWorkspaceLock,
    deployments: &[SkillDeployment],
    workspace_id: Option<&str>,
    skill_id: &str,
    version: Option<&str>,
) -> bool {
    if let Some(entry) = lock.skills.get(skill_id) {
        if !entry.enabled || entry.management != MANAGEMENT_MODE_MANAGED {
            return false;
        }
        return version.is_none_or(|expected| entry.version == expected);
    }
    // Migration window: packages deployed before the lock file existed are
    // accepted from the deployment index so a project cannot end up with an
    // orphaned projection that can no longer be repaired or removed.
    deployments.iter().any(|item| {
        item.skill_id == skill_id
            && item.target_kind == TARGET_KIND_WORKSPACE
            && item.workspace_id.as_deref() == workspace_id
            && version.is_none_or(|expected| item.version == expected)
    })
}

/// HiMind AI uses the Store directly rather than a client directory.  A
/// package is visible in a session when it is a builtin, has a global
/// deployment, or is assigned to the selected workspace.  Packages installed
/// before deployment records existed remain globally visible for backwards
/// compatibility.
pub(crate) fn skill_visible_to_himind(
    record: &crate::skill::types::SkillRecord,
    workspace_root: Option<&Path>,
) -> Result<bool, Box<dyn Error>> {
    if matches!(
        record.manifest.scope,
        crate::skill::types::SkillScope::Builtin
    ) {
        return Ok(true);
    }
    let deployments = deployments_for_skill(&record.manifest.id)?;
    if deployments.is_empty() {
        return Ok(true);
    }
    if deployments
        .iter()
        .any(|item| item.target_kind == TARGET_KIND_GLOBAL)
    {
        return Ok(true);
    }
    let Some(workspace_root) = workspace_root else {
        return Ok(false);
    };
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let id = workspace_id(&workspace_root);
    if let Some(entry) = read_workspace_lock(&workspace_root)?
        .skills
        .get(&record.manifest.id)
    {
        return Ok(entry.enabled && entry.management == MANAGEMENT_MODE_MANAGED);
    }
    Ok(deployments.iter().any(|item| {
        item.target_kind == TARGET_KIND_WORKSPACE && item.workspace_id.as_deref() == Some(&id)
    }))
}

fn package_content_sha256(root: &Path) -> Result<String, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ".himind-render.json" || relative == ".himind/manifest.json" {
            continue;
        }
        files.push((relative.to_ascii_lowercase(), entry.path().to_path_buf()));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(path)?);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn workspace_status() -> SkillWorkspaceStatus {
    let configured =
        env::var_os("HIMIND_SKILL_WORKSPACE").is_some() || persisted_workspace().is_some();
    let Some(value) = env::var_os("HIMIND_SKILL_WORKSPACE")
        .map(PathBuf::from)
        .or_else(persisted_workspace)
    else {
        return SkillWorkspaceStatus {
            configured: false,
            valid: false,
            root: String::new(),
            workspace_id: String::new(),
            agents_skills_root: String::new(),
            lock_path: String::new(),
            managed_skill_count: 0,
            managed_skills: Vec::new(),
            error: String::new(),
        };
    };
    match canonical_workspace_root(&PathBuf::from(value)) {
        Ok(root) => SkillWorkspaceStatus {
            configured,
            valid: true,
            workspace_id: workspace_id(&root),
            agents_skills_root: display_path(&root.join(".agents").join("skills")),
            lock_path: display_path(&workspace_lock_path(&root)),
            managed_skill_count: read_workspace_lock(&root)
                .map(|lock| lock.skills.values().filter(|entry| entry.enabled).count())
                .unwrap_or(0),
            managed_skills: read_workspace_lock(&root)
                .map(|lock| {
                    lock.skills
                        .into_iter()
                        .map(|(skill_id, entry)| SkillWorkspaceSkillStatus {
                            skill_id,
                            version: entry.version,
                            enabled: entry.enabled,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            root: display_path(&root),
            error: String::new(),
        },
        Err(error) => SkillWorkspaceStatus {
            configured,
            valid: false,
            root: String::new(),
            workspace_id: String::new(),
            agents_skills_root: String::new(),
            lock_path: String::new(),
            managed_skill_count: 0,
            managed_skills: Vec::new(),
            error: error.to_string(),
        },
    }
}

pub(crate) fn set_workspace(value: Option<&str>) -> Result<SkillWorkspaceStatus, Box<dyn Error>> {
    let value = value.map(str::trim).filter(|item| !item.is_empty());
    match value {
        Some(path) => {
            let root = canonical_workspace_root(Path::new(path))?;
            env::remove_var("HIMIND_SKILL_TARGET");
            env::set_var("HIMIND_SKILL_WORKSPACE", &root);
            persist_workspace(Some(&root.to_string_lossy()))?;
        }
        None => {
            env::remove_var("HIMIND_SKILL_WORKSPACE");
            env::set_var("HIMIND_SKILL_TARGET", TARGET_KIND_GLOBAL);
            persist_workspace(None)?;
        }
    }
    Ok(workspace_status())
}

/// Record a successful projection.  A receipt remains the authoritative
/// guard for filesystem mutation; this index is the cross-workspace lifecycle
/// view used by Store removal and diagnostics.
pub(crate) fn record_deployment(
    target: &SkillTarget,
    client_id: &str,
    skill_id: &str,
    version: &str,
    rendered_root: &Path,
) -> Result<(), Box<dyn Error>> {
    #[cfg(test)]
    {
        let _ = (target, client_id, skill_id, version, rendered_root);
        return Ok(());
    }
    #[cfg(not(test))]
    {
        let mut deployments = read_deployments()?;
        deployments.retain(|item| {
            !(item.skill_id == skill_id
                && item.client_id == client_id
                && item.target_kind == target.target_kind
                && item.workspace_id == target.workspace_id)
        });
        deployments.push(SkillDeployment {
            skill_id: skill_id.to_string(),
            version: version.to_string(),
            client_id: client_id.to_string(),
            target_kind: target.target_kind.clone(),
            workspace_root: target
                .workspace_root
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            workspace_id: target.workspace_id.clone(),
            rendered_root: rendered_root.to_string_lossy().to_string(),
            updated_at: deployment_stamp(),
            management_mode: MANAGEMENT_MODE_MANAGED.to_string(),
            source: "himind-store".to_string(),
            content_sha256: String::new(),
        });
        write_deployments(&deployments)
    }
}

pub(crate) fn remove_deployment(
    target: &SkillTarget,
    client_id: &str,
    skill_id: &str,
) -> Result<(), Box<dyn Error>> {
    #[cfg(test)]
    {
        let _ = (target, client_id, skill_id);
        return Ok(());
    }
    #[cfg(not(test))]
    {
        let mut deployments = read_deployments()?;
        let before = deployments.len();
        deployments.retain(|item| {
            !(item.skill_id == skill_id
                && item.client_id == client_id
                && item.target_kind == target.target_kind
                && item.workspace_id == target.workspace_id)
        });
        if deployments.len() != before {
            write_deployments(&deployments)?;
        }
        Ok(())
    }
}

pub(crate) fn deployments_for_skill(
    skill_id: &str,
) -> Result<Vec<SkillDeployment>, Box<dyn Error>> {
    #[cfg(test)]
    {
        let _ = skill_id;
        return Ok(Vec::new());
    }
    #[cfg(not(test))]
    {
        Ok(read_deployments()?
            .into_iter()
            .filter(|item| item.skill_id == skill_id)
            .collect())
    }
}

pub(crate) fn other_deployments_for_skill(
    skill_id: &str,
    target: &SkillTarget,
) -> Result<Vec<SkillDeployment>, Box<dyn Error>> {
    Ok(deployments_for_skill(skill_id)?
        .into_iter()
        .filter(|item| {
            !(item.target_kind == target.target_kind && item.workspace_id == target.workspace_id)
        })
        .collect())
}

fn deployments_path() -> PathBuf {
    #[cfg(test)]
    {
        return env::temp_dir().join(format!(
            "himind-skill-deployments-test-{}.json",
            std::process::id()
        ));
    }
    #[cfg(not(test))]
    {
        crate::store::paths::agent_home().join(DEPLOYMENTS_FILE)
    }
}

fn read_deployments() -> Result<Vec<SkillDeployment>, Box<dyn Error>> {
    let path = deployments_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(content.trim_start_matches('\u{feff}')).unwrap_or_default())
}

fn write_deployments(deployments: &[SkillDeployment]) -> Result<(), Box<dyn Error>> {
    let path = deployments_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(deployments)?)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn deployment_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Discover standard project Skills without importing them into the Agent
/// Store.  Native files remain owned by the repository/client; HiMind only
/// reports them and never overwrites or removes them without a receipt.
pub(crate) fn discover_project_skills(root: &Path) -> Vec<DiscoveredProjectSkill> {
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir()
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let readme = path.join("SKILL.md");
        let Ok(content) = std::fs::read_to_string(&readme) else {
            continue;
        };
        let Ok((name, description, _)) = crate::skill::manifest::parse_skill_frontmatter(&content)
        else {
            continue;
        };
        if path.file_name().and_then(|value| value.to_str()) != Some(name.as_str()) {
            continue;
        }
        let managed = path.join(".himind-render.json").is_file();
        items.push(DiscoveredProjectSkill {
            skill_id: name.clone(),
            name,
            description,
            path: display_path(&path),
            managed_by_himind: managed,
            management_mode: if managed {
                MANAGEMENT_MODE_MANAGED.to_string()
            } else {
                MANAGEMENT_MODE_NATIVE.to_string()
            },
        });
    }
    items.sort_by(|left, right| left.name.cmp(&right.name));
    items
}

/// Inspect all standard project-level Skill directories without taking
/// ownership of any native files.  A conflict is reported when a Skill ID is
/// present in both a HiMind receipt-backed projection and a repository-owned
/// directory; callers can warn the user before choosing which one to use.
pub(crate) fn discover_project_skill_conflicts(root: &Path) -> Vec<SkillConflict> {
    let mut directories = vec![root.join(".agents").join("skills")];
    directories.extend(
        crate::skill::clients::DIRECTORY_CLIENTS
            .iter()
            .map(|client| root.join(client.project_dir)),
    );
    directories.sort();
    directories.dedup();

    let mut grouped: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for directory in directories {
        for item in discover_project_skills(&directory) {
            let paths = grouped.entry(item.skill_id.clone()).or_default();
            if item.managed_by_himind {
                paths.0.push(item.path);
            } else {
                paths.1.push(item.path);
            }
        }
    }
    grouped
        .into_iter()
        .filter_map(|(skill_id, (mut managed_paths, mut native_paths))| {
            if managed_paths.is_empty() || native_paths.is_empty() {
                return None;
            }
            managed_paths.sort();
            native_paths.sort();
            Some(SkillConflict {
                skill_id,
                managed_paths,
                native_paths,
                reason: "同一 Skill ID 同时存在 HiMind 托管副本和项目原生副本".to_string(),
            })
        })
        .collect()
}

fn persisted_workspace() -> Option<PathBuf> {
    let content =
        std::fs::read_to_string(crate::store::paths::agent_home().join(WORKSPACE_CONFIG_FILE))
            .ok()?;
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()?
        .get("root")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

fn load_persisted_workspace() -> Option<PathBuf> {
    persisted_workspace().and_then(|path| canonical_workspace_root(&path).ok())
}

fn persist_workspace(value: Option<&str>) -> Result<(), Box<dyn Error>> {
    let path = crate::store::paths::agent_home().join(WORKSPACE_CONFIG_FILE);
    match value {
        Some(root) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&serde_json::json!({"root": root}))?,
            )?;
        }
        None => {
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn canonical_workspace_root(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("项目工作区不可访问: {} ({error})", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("项目工作区必须是目录: {}", canonical.display()).into());
    }
    if crate::extension_workspace::is_agent_managed_path(&canonical) {
        return Err("项目工作区不能位于 HiMind Agent 安装目录或数据目录".into());
    }
    Ok(canonical)
}

pub(crate) fn workspace_id(root: &Path) -> String {
    // Windows paths are case-insensitive.  Normalizing separators and case
    // keeps the identity stable when the same checkout is supplied in a
    // different spelling.  The prefix makes the value self-describing in
    // diagnostics and leaves room for future fingerprint algorithms.
    let normalized = root
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    format!("sha256:{:x}", Sha256::digest(normalized.as_bytes()))
}

/// Format a canonical path for display.  Windows canonicalization adds a
/// `\\?\` verbatim prefix that reads badly in the UI and is rejected by some
/// external tools, so strip it from user-facing strings while keeping the
/// canonical form for receipts, comparisons and workspace identity.
pub(crate) fn display_path(path: &Path) -> String {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.to_string()
}

/// The render mode that actually applies to a target.
///
/// Project projections are always real copies.  A symlink into the
/// machine-local Store breaks as soon as the repository is cloned, archived or
/// committed, and the project lock file describes content the project itself
/// owns.  Global projections keep honouring the user's copy/symlink setting.
pub(crate) fn effective_sync_mode(configured: &str, target: &SkillTarget) -> String {
    if target.is_workspace() {
        crate::skill::store::SKILL_SYNC_MODE_COPY.to_string()
    } else {
        configured.to_string()
    }
}

fn validate_relative_directory(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Err("Skill 客户端目录不能为空".into());
    }
    let path = PathBuf::from(&normalized);
    if path.is_absolute()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Skill 客户端目录必须是安全的相对路径: {value}").into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn workspace_target_has_stable_identity_and_native_root() {
        let root = test_workspace_root();
        fs::create_dir_all(&root).unwrap();
        let target = SkillTarget::workspace(&root, ".agents/skills", "workspace").unwrap();
        assert_eq!(target.target_kind, TARGET_KIND_WORKSPACE);
        assert_eq!(
            target.root,
            root.canonicalize().unwrap().join(".agents").join("skills")
        );
        assert!(target
            .workspace_id
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_absolute_and_parent_client_directories() {
        let root = test_workspace_root();
        fs::create_dir_all(&root).unwrap();
        assert!(SkillTarget::workspace(&root, "../skills", "workspace").is_err());
        assert!(SkillTarget::workspace(&root, "C:/skills", "workspace").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_project_owned_and_himind_managed_skills() {
        let root = test_workspace_root();
        let user_skill = root.join("project-rules");
        let managed_skill = root.join("managed-rules");
        fs::create_dir_all(&user_skill).unwrap();
        fs::create_dir_all(&managed_skill).unwrap();
        fs::write(
            user_skill.join("SKILL.md"),
            "---\nname: project-rules\ndescription: Repository conventions.\n---\n",
        )
        .unwrap();
        fs::write(
            managed_skill.join("SKILL.md"),
            "---\nname: managed-rules\ndescription: Managed conventions.\n---\n",
        )
        .unwrap();
        fs::write(managed_skill.join(".himind-render.json"), "{}").unwrap();

        let discovered = discover_project_skills(&root);
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].name, "managed-rules");
        assert!(discovered[0].managed_by_himind);
        assert_eq!(discovered[1].name, "project-rules");
        assert!(!discovered[1].managed_by_himind);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_lock_is_explicit_and_version_pinned() {
        let root = test_workspace_root();
        fs::create_dir_all(&root).unwrap();
        let target = SkillTarget::workspace(&root, ".agents/skills", "workspace").unwrap();
        let mut lock = SkillWorkspaceLock {
            schema_version: 1,
            skills: BTreeMap::new(),
        };
        lock.skills.insert(
            "demo-skill".to_string(),
            SkillWorkspaceLockEntry {
                version: "1.2.3".to_string(),
                sha256: "sha256:demo".to_string(),
                source: "local-zip".to_string(),
                management: MANAGEMENT_MODE_MANAGED.to_string(),
                enabled: true,
            },
        );
        write_workspace_lock(&root, &lock).unwrap();
        assert!(target_allows_record(&target, "demo-skill", Some("1.2.3")).unwrap());
        assert!(!target_allows_record(&target, "demo-skill", Some("1.2.4")).unwrap());
        set_workspace_skill_enabled(&root, "demo-skill", false).unwrap();
        assert!(!target_allows_record(&target, "demo-skill", Some("1.2.3")).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lock_entry_wins_over_legacy_deployment_pin() {
        let mut lock = SkillWorkspaceLock {
            schema_version: 1,
            skills: BTreeMap::new(),
        };
        lock.skills.insert(
            "demo-skill".to_string(),
            SkillWorkspaceLockEntry {
                version: "2.0.0".to_string(),
                sha256: "sha256:demo".to_string(),
                source: "himind-store".to_string(),
                management: MANAGEMENT_MODE_MANAGED.to_string(),
                enabled: true,
            },
        );
        let deployments = vec![deployment("demo-skill", "1.0.0", "workspace-a", "100")];
        assert_eq!(
            pinned_version_in(&lock, &deployments, "workspace-a", "demo-skill").as_deref(),
            Some("2.0.0")
        );
        assert!(record_allowed_in(
            &lock,
            &deployments,
            Some("workspace-a"),
            "demo-skill",
            Some("2.0.0")
        ));
        assert!(!record_allowed_in(
            &lock,
            &deployments,
            Some("workspace-a"),
            "demo-skill",
            Some("3.0.0")
        ));
    }

    #[test]
    fn legacy_deployments_stay_repairable_until_the_lock_exists() {
        let lock = SkillWorkspaceLock {
            schema_version: 1,
            skills: BTreeMap::new(),
        };
        let deployments = vec![
            deployment("demo-skill", "1.0.0", "workspace-a", "100"),
            deployment("demo-skill", "1.1.0", "workspace-a", "200"),
            deployment("demo-skill", "9.9.9", "workspace-b", "300"),
        ];
        assert_eq!(
            pinned_version_in(&lock, &deployments, "workspace-a", "demo-skill").as_deref(),
            Some("1.1.0")
        );
        assert!(record_allowed_in(
            &lock,
            &deployments,
            Some("workspace-a"),
            "demo-skill",
            Some("1.1.0")
        ));
        assert!(!record_allowed_in(
            &lock,
            &deployments,
            Some("workspace-c"),
            "demo-skill",
            Some("1.1.0")
        ));
        assert!(!record_allowed_in(
            &lock,
            &deployments,
            Some("workspace-a"),
            "unassigned-skill",
            None
        ));
    }

    #[test]
    fn disabled_or_native_lock_entries_are_not_synced() {
        let mut lock = SkillWorkspaceLock {
            schema_version: 1,
            skills: BTreeMap::new(),
        };
        lock.skills.insert(
            "disabled-skill".to_string(),
            SkillWorkspaceLockEntry {
                version: "1.0.0".to_string(),
                sha256: "sha256:demo".to_string(),
                source: "himind-store".to_string(),
                management: MANAGEMENT_MODE_MANAGED.to_string(),
                enabled: false,
            },
        );
        lock.skills.insert(
            "native-skill".to_string(),
            SkillWorkspaceLockEntry {
                version: "1.0.0".to_string(),
                sha256: "sha256:demo".to_string(),
                source: "repository".to_string(),
                management: MANAGEMENT_MODE_NATIVE.to_string(),
                enabled: true,
            },
        );
        assert!(!record_allowed_in(
            &lock,
            &[],
            Some("workspace-a"),
            "disabled-skill",
            Some("1.0.0")
        ));
        assert!(!record_allowed_in(
            &lock,
            &[],
            Some("workspace-a"),
            "native-skill",
            Some("1.0.0")
        ));
    }

    #[test]
    fn project_projections_always_render_copies() {
        let root = test_workspace_root();
        fs::create_dir_all(&root).unwrap();
        let workspace = SkillTarget::workspace(&root, ".agents/skills", "workspace").unwrap();
        let global = SkillTarget::global(root.join("global-skills"), "test", true);

        assert_eq!(
            effective_sync_mode(crate::skill::store::SKILL_SYNC_MODE_SYMLINK, &workspace),
            crate::skill::store::SKILL_SYNC_MODE_COPY
        );
        assert_eq!(
            effective_sync_mode(crate::skill::store::SKILL_SYNC_MODE_COPY, &workspace),
            crate::skill::store::SKILL_SYNC_MODE_COPY
        );
        assert_eq!(
            effective_sync_mode(crate::skill::store::SKILL_SYNC_MODE_SYMLINK, &global),
            crate::skill::store::SKILL_SYNC_MODE_SYMLINK
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detects_native_and_managed_copies_of_the_same_skill() {
        let root = test_workspace_root();
        let native = root.join(".agents").join("skills").join("demo-skill");
        let managed = root.join(".claude").join("skills").join("demo-skill");
        fs::create_dir_all(&native).unwrap();
        fs::create_dir_all(&managed).unwrap();
        let body = "---\nname: demo-skill\ndescription: Demo.\n---\n";
        fs::write(native.join("SKILL.md"), body).unwrap();
        fs::write(managed.join("SKILL.md"), body).unwrap();
        fs::write(managed.join(".himind-render.json"), "{}").unwrap();

        let conflicts = discover_project_skill_conflicts(&root);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].skill_id, "demo-skill");
        assert_eq!(conflicts[0].managed_paths.len(), 1);
        assert_eq!(conflicts[0].native_paths.len(), 1);

        fs::remove_dir_all(&native).unwrap();
        assert!(discover_project_skill_conflicts(&root).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    fn deployment(
        skill_id: &str,
        version: &str,
        workspace_id: &str,
        stamp: &str,
    ) -> SkillDeployment {
        SkillDeployment {
            skill_id: skill_id.to_string(),
            version: version.to_string(),
            client_id: "codex".to_string(),
            target_kind: TARGET_KIND_WORKSPACE.to_string(),
            workspace_root: Some(format!("C:/workspaces/{workspace_id}")),
            workspace_id: Some(workspace_id.to_string()),
            rendered_root: format!("C:/workspaces/{workspace_id}/.agents/skills/{skill_id}"),
            updated_at: stamp.to_string(),
            management_mode: MANAGEMENT_MODE_MANAGED.to_string(),
            source: "himind-store".to_string(),
            content_sha256: String::new(),
        }
    }

    fn test_workspace_root() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "himind-skill-workspace-{}-{stamp}",
            std::process::id()
        ))
    }
}
