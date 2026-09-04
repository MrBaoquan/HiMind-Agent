use crate::api::distribution::{PluginCatalogItem, SkillCatalogItem};
use crate::store::{atomic_file, paths};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SETTINGS_SCHEMA_VERSION: u32 = 1;
const CATALOG_SCHEMA_VERSION: u32 = 1;
const DEFAULT_CATALOG_PATH: &str = ".himind/catalog.json";
const OFFICIAL_EXTENSION_REPOSITORY: &str = "MrBaoquan/himind-extensions";
const SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(60);
pub(crate) const AUTHORING_FEATURE_ID: &str = "com.himind.feature.extension-authoring";
const AUTHORING_PLUGIN_ID: &str = "com.himind.extension-development-tools";
const AUTHORING_SKILL_IDS: [&str; 2] = [
    "com.himind.skill.develop-himind-plugins",
    "com.himind.skill.develop-himind-skills",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionSourceVerification {
    Required,
    Optional,
}

impl Default for ExtensionSourceVerification {
    fn default() -> Self {
        Self::Required
    }
}

impl ExtensionSourceVerification {
    pub(crate) fn requires_signature(&self) -> bool {
        matches!(self, Self::Required)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionSourceConfig {
    pub id: String,
    pub name: String,
    pub repository: String,
    pub reference: String,
    pub catalog_path: String,
    pub enabled: bool,
    pub auto_update: bool,
    #[serde(default)]
    pub verification: ExtensionSourceVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionSourceSettings {
    #[serde(default = "settings_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<ExtensionSourceConfig>,
}

impl Default for ExtensionSourceSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ExtensionFeaturePack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub plugin_ids: Vec<String>,
    #[serde(default)]
    pub skill_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionSourceCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub generation: String,
    #[serde(default)]
    pub plugins: Vec<PluginCatalogItem>,
    #[serde(default)]
    pub skills: Vec<SkillCatalogItem>,
    #[serde(default)]
    pub feature_packs: Vec<ExtensionFeaturePack>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionSourceStatus {
    pub source: ExtensionSourceConfig,
    pub state: String,
    pub plugin_count: usize,
    pub skill_count: usize,
    pub generation: String,
    pub using_cache: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ExtensionSourceSnapshot {
    pub plugins: Vec<PluginCatalogItem>,
    pub skills: Vec<SkillCatalogItem>,
    pub feature_packs: Vec<ExtensionFeaturePack>,
    pub sources: Vec<ExtensionSourceStatus>,
    #[serde(skip_serializing)]
    plugin_versions: Vec<PluginCatalogItem>,
    #[serde(skip_serializing)]
    skill_versions: Vec<SkillCatalogItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionProvenance {
    pub asset_kind: String,
    pub asset_key: String,
    pub version: String,
    pub source_id: String,
    pub repository: String,
    pub reference: String,
    pub catalog_path: String,
    pub artifact_url: String,
    pub sha256: String,
    pub signature_key_id: String,
    pub auto_update: bool,
}

pub(crate) fn settings() -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    settings_at(&settings_path())
}

pub(crate) fn add_github_source(
    name: &str,
    repository: &str,
    reference: &str,
    catalog_path: Option<&str>,
    verification: Option<&str>,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let source = github_source_config(name, repository, reference, catalog_path, verification)?;
    upsert_source(source)
}

/// Build a validated GitHub source without persisting it.
///
/// Importers use this before writing settings so a malformed catalog cannot
/// leave an unusable source behind when a one-click import fails.
pub(crate) fn github_source_config(
    name: &str,
    repository: &str,
    reference: &str,
    catalog_path: Option<&str>,
    verification: Option<&str>,
) -> Result<ExtensionSourceConfig, Box<dyn Error>> {
    let parsed = crate::app::github_source::parse_source_url(repository)?;
    let repository = parsed.repository;
    let reference = if !parsed.reference.is_empty() {
        parsed.reference
    } else {
        validate_reference(reference)?
    };
    let requested_catalog_path = catalog_path.unwrap_or(DEFAULT_CATALOG_PATH).trim();
    let catalog_path =
        if !parsed.subpath.is_empty() && requested_catalog_path == DEFAULT_CATALOG_PATH {
            let path = if parsed.subpath.ends_with(".json") {
                parsed.subpath
            } else {
                format!("{}/.himind/catalog.json", parsed.subpath)
            };
            validate_catalog_path(&path)?
        } else {
            validate_catalog_path(requested_catalog_path)?
        };
    let verification = source_verification(&repository, verification)?;
    let id = source_id(&repository, &reference, &catalog_path);
    Ok(ExtensionSourceConfig {
        id: id.clone(),
        name: if name.trim().is_empty() {
            repository.clone()
        } else {
            name.trim().chars().take(80).collect()
        },
        repository,
        reference,
        catalog_path,
        enabled: true,
        auto_update: false,
        verification,
    })
}

pub(crate) fn upsert_source(
    source: ExtensionSourceConfig,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let mut current = settings()?;
    let id = source.id.clone();
    if let Some(existing) = current.sources.iter_mut().find(|item| item.id == id) {
        *existing = source;
    } else {
        current.sources.push(source);
    }
    current
        .sources
        .sort_by(|left, right| left.id.cmp(&right.id));
    save_settings(&current)?;
    invalidate_snapshot_cache();
    Ok(current)
}

pub(crate) fn update_source(
    source_id: &str,
    enabled: bool,
    auto_update: bool,
    verification: Option<&str>,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let mut current = settings()?;
    let source = current
        .sources
        .iter_mut()
        .find(|item| item.id == source_id)
        .ok_or("扩展源不存在")?;
    source.enabled = enabled;
    source.auto_update = auto_update;
    if let Some(value) = verification {
        source.verification = source_verification(&source.repository, Some(value))?;
    }
    save_settings(&current)?;
    invalidate_snapshot_cache();
    Ok(current)
}

pub(crate) fn remove_source(source_id: &str) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let mut current = settings()?;
    let previous = current.sources.len();
    current.sources.retain(|source| source.id != source_id);
    if current.sources.len() == previous {
        return Err("扩展源不存在".into());
    }
    save_settings(&current)?;
    let _ = fs::remove_file(cache_path(source_id));
    invalidate_snapshot_cache();
    Ok(current)
}

pub(crate) fn snapshot() -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    snapshot_with_cache(false)
}

pub(crate) fn refresh_snapshot() -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    snapshot_with_cache(true)
}

fn snapshot_with_cache(force: bool) -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    if !force {
        if let Some((loaded_at, value)) = snapshot_cache()
            .lock()
            .map_err(|_| "扩展源内存缓存不可用")?
            .as_ref()
        {
            if loaded_at.elapsed() < SNAPSHOT_CACHE_TTL {
                return Ok(value.clone());
            }
        }
    }
    let value = load_snapshot()?;
    *snapshot_cache()
        .lock()
        .map_err(|_| "扩展源内存缓存不可用")? = Some((Instant::now(), value.clone()));
    Ok(value)
}

fn load_snapshot() -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    let mut result = ExtensionSourceSnapshot::default();
    let mut plugins = HashMap::<String, PluginCatalogItem>::new();
    let mut skills = HashMap::<String, SkillCatalogItem>::new();
    let mut feature_packs = HashMap::<String, ExtensionFeaturePack>::new();
    for source in settings()?.sources.into_iter().filter(|item| item.enabled) {
        let (catalog, using_cache, error) = match fetch_catalog(&source) {
            Ok(catalog) => {
                save_cached_catalog(&source.id, &catalog)?;
                (Some(catalog), false, String::new())
            }
            Err(error) => match load_cached_catalog(&source.id) {
                Ok(Some(catalog)) => match validate_catalog(&catalog, &source) {
                    Ok(()) => (Some(catalog), true, error.to_string()),
                    Err(cache_error) => {
                        (None, false, format!("{error}; 缓存不再可信: {cache_error}"))
                    }
                },
                Ok(None) => (None, false, error.to_string()),
                Err(cache_error) => (None, false, format!("{error}; 缓存读取失败: {cache_error}")),
            },
        };
        let mut status = ExtensionSourceStatus {
            source: source.clone(),
            state: if catalog.is_some() {
                "ready"
            } else {
                "unavailable"
            }
            .to_string(),
            plugin_count: 0,
            skill_count: 0,
            generation: String::new(),
            using_cache,
            error,
        };
        if let Some(mut catalog) = catalog {
            status.plugin_count = catalog.plugins.len();
            status.skill_count = catalog.skills.len();
            status.generation = catalog.generation.clone();
            for item in &mut catalog.plugins {
                normalize_plugin_item(item, &source)?;
                result.plugin_versions.push(item.clone());
                if let Some(existing) = plugins.get(&item.plugin_id) {
                    if existing.source != item.source {
                        return Err(format!(
                            "扩展 ID {} 同时来自多个 GitHub 源，请只保留一个可信来源",
                            item.plugin_id
                        )
                        .into());
                    }
                    if crate::skill::resolver::compare_versions(&item.version, &existing.version)
                        == std::cmp::Ordering::Greater
                    {
                        plugins.insert(item.plugin_id.clone(), item.clone());
                    }
                } else {
                    plugins.insert(item.plugin_id.clone(), item.clone());
                }
            }
            for item in &mut catalog.skills {
                normalize_skill_item(item, &source)?;
                result.skill_versions.push(item.clone());
                if let Some(existing) = skills.get(&item.skill_id) {
                    if existing.source != item.source {
                        return Err(format!(
                            "扩展 ID {} 同时来自多个 GitHub 源，请只保留一个可信来源",
                            item.skill_id
                        )
                        .into());
                    }
                    if crate::skill::resolver::compare_versions(&item.version, &existing.version)
                        == std::cmp::Ordering::Greater
                    {
                        skills.insert(item.skill_id.clone(), item.clone());
                    }
                } else {
                    skills.insert(item.skill_id.clone(), item.clone());
                }
            }
            for pack in catalog.feature_packs {
                validate_feature_pack(&pack)?;
                feature_packs.entry(pack.id.clone()).or_insert(pack);
            }
        }
        result.sources.push(status);
    }
    result.plugins = plugins.into_values().collect();
    result.skills = skills.into_values().collect();
    result.feature_packs = feature_packs.into_values().collect();
    result
        .plugins
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    result
        .skills
        .sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    result
        .feature_packs
        .sort_by(|left, right| left.id.cmp(&right.id));
    sort_plugin_versions(&mut result.plugin_versions);
    sort_skill_versions(&mut result.skill_versions);
    Ok(result)
}

fn snapshot_cache() -> &'static Mutex<Option<(Instant, ExtensionSourceSnapshot)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, ExtensionSourceSnapshot)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn invalidate_snapshot_cache() {
    if let Ok(mut value) = snapshot_cache().lock() {
        *value = None;
    }
}

pub(crate) fn plugin_versions(plugin_id: &str) -> Result<Vec<PluginCatalogItem>, Box<dyn Error>> {
    Ok(snapshot()?
        .plugin_versions
        .into_iter()
        .filter(|item| item.plugin_id == plugin_id)
        .collect())
}

pub(crate) fn skill_versions(skill_id: &str) -> Result<Vec<SkillCatalogItem>, Box<dyn Error>> {
    Ok(snapshot()?
        .skill_versions
        .into_iter()
        .filter(|item| item.skill_id == skill_id)
        .collect())
}

fn sort_plugin_versions(items: &mut [PluginCatalogItem]) {
    items.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| crate::skill::resolver::compare_versions(&right.version, &left.version))
    });
}

fn sort_skill_versions(items: &mut [SkillCatalogItem]) {
    items.sort_by(|left, right| {
        left.skill_id
            .cmp(&right.skill_id)
            .then_with(|| crate::skill::resolver::compare_versions(&right.version, &left.version))
    });
}

pub(crate) fn save_provenance(
    source: &ExtensionSourceConfig,
    kind: &str,
    key: &str,
    version: &str,
    artifact_url: &str,
    sha256: &str,
    signature_key_id: &str,
) -> Result<ExtensionProvenance, Box<dyn Error>> {
    validate_asset_identity(kind, key)?;
    let record = ExtensionProvenance {
        asset_kind: kind.to_string(),
        asset_key: key.to_string(),
        version: version.to_string(),
        source_id: source.id.clone(),
        repository: source.repository.clone(),
        reference: source.reference.clone(),
        catalog_path: source.catalog_path.clone(),
        artifact_url: artifact_url.to_string(),
        sha256: sha256.to_ascii_lowercase(),
        signature_key_id: signature_key_id.to_string(),
        auto_update: source.auto_update,
    };
    let path = provenance_path(kind, key)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
    Ok(record)
}

pub(crate) fn list_provenance() -> Result<Vec<ExtensionProvenance>, Box<dyn Error>> {
    let root = provenance_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<ExtensionProvenance>(&fs::read(entry.path())?) {
            result.push(value);
        }
    }
    result.sort_by(|left, right| {
        left.asset_kind
            .cmp(&right.asset_kind)
            .then_with(|| left.asset_key.cmp(&right.asset_key))
    });
    Ok(result)
}

pub(crate) fn install_plugin(
    plugin_id: &str,
    version: Option<&str>,
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let mut order = Vec::new();
    let mut visiting = HashSet::new();
    resolve_plugin_order(
        &snapshot.plugin_versions,
        plugin_id,
        version,
        &mut visiting,
        &mut order,
    )?;
    let mut changes = Vec::new();
    let mut reference_changes = Vec::new();
    let mut provenance_changes = Vec::new();
    let mut lock_changes = Vec::new();
    for item in &order {
        let before = crate::app::plugin_manager::local_status(&item.plugin_id);
        let previous_lock = crate::app::extension_lock::read("plugin", &item.plugin_id)?;
        if before.current_version == item.version && before.enabled {
            continue;
        }
        let source = match source_for_catalog_item(&snapshot, &item.source) {
            Ok(source) => source,
            Err(error) => {
                restore_plugin_install_state(&reference_changes);
                restore_provenance_changes(&provenance_changes);
                restore_lock_changes(&lock_changes);
                compensate_plugin_changes(&changes);
                return Err(error);
            }
        };
        lock_changes.push(("plugin".to_string(), item.plugin_id.clone(), previous_lock));
        if let Err(error) = crate::app::plugin_manager::install_public_catalog_item(
            item,
            source.verification.requires_signature(),
        ) {
            restore_plugin_install_state(&reference_changes);
            restore_provenance_changes(&provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&changes);
            return Err(error);
        }
        changes.push((item.plugin_id.clone(), before));
        let previous_provenance = match read_provenance("plugin", &item.plugin_id) {
            Ok(value) => value,
            Err(error) => {
                restore_plugin_install_state(&reference_changes);
                compensate_plugin_changes(&changes);
                restore_lock_changes(&lock_changes);
                return Err(error);
            }
        };
        provenance_changes.push((
            "plugin".to_string(),
            item.plugin_id.clone(),
            previous_provenance,
        ));
        if let Err(error) = save_provenance(
            source,
            "plugin",
            &item.plugin_id,
            &item.version,
            &item.download_url,
            &item.sha256,
            &item.signature_key_id,
        ) {
            restore_plugin_install_state(&reference_changes);
            restore_provenance_changes(&provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&changes);
            return Err(error);
        }
        let direct_dependencies = item
            .plugin_dependencies
            .iter()
            .filter(|dependency| dependency.required)
            .map(|dependency| dependency.plugin_id.clone())
            .collect::<Vec<_>>();
        let owner = format!("plugin:{}", item.plugin_id);
        let previous_references = crate::app::plugin_manager::owner_dependency_ids(&owner);
        if let Err(error) =
            crate::app::plugin_manager::set_owner_references(&owner, &direct_dependencies)
        {
            restore_plugin_install_state(&reference_changes);
            restore_provenance_changes(&provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&changes);
            return Err(error);
        }
        reference_changes.push((owner, previous_references));
        if let Err(error) = crate::app::extension_lock::record_source_plugin(source, item) {
            restore_plugin_install_state(&reference_changes);
            restore_provenance_changes(&provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&changes);
            return Err(error);
        }
    }
    order
        .into_iter()
        .find(|item| item.plugin_id == plugin_id)
        .ok_or_else(|| "扩展源中未找到插件".into())
}

fn restore_plugin_install_state(reference_changes: &[(String, Vec<String>)]) {
    for (owner, previous) in reference_changes.iter().rev() {
        let _ = crate::app::plugin_manager::set_owner_references(owner, previous);
    }
}

fn read_provenance(kind: &str, key: &str) -> Result<Option<ExtensionProvenance>, Box<dyn Error>> {
    let path = provenance_path(kind, key)?;
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn restore_provenance_changes(changes: &[(String, String, Option<ExtensionProvenance>)]) {
    for (kind, key, previous) in changes.iter().rev() {
        let Ok(path) = provenance_path(kind, key) else {
            continue;
        };
        match previous {
            Some(record) => {
                let _ = atomic_file::atomic_write(
                    &path,
                    &serde_json::to_vec_pretty(record).unwrap_or_default(),
                );
            }
            None => {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn restore_lock_changes(
    changes: &[(
        String,
        String,
        Option<crate::app::extension_lock::ExtensionLockEntry>,
    )],
) {
    for (kind, key, previous) in changes.iter().rev() {
        let _ = crate::app::extension_lock::restore(kind, key, previous.clone());
    }
}

pub(crate) fn plan_plugin(
    plugin_id: &str,
    version: Option<&str>,
) -> Result<crate::app::plugin_manager::PluginInstallPlan, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let mut order = Vec::new();
    resolve_plugin_order(
        &snapshot.plugin_versions,
        plugin_id,
        version,
        &mut HashSet::new(),
        &mut order,
    )?;
    let plugin = order
        .iter()
        .find(|item| item.plugin_id == plugin_id)
        .cloned()
        .ok_or("扩展源中未找到插件")?;
    let dependency_actions = order
        .into_iter()
        .filter(|item| item.plugin_id != plugin_id)
        .map(|item| {
            let local = crate::app::plugin_manager::local_status(&item.plugin_id);
            let action = if local.current_version.is_empty() {
                "install"
            } else if crate::skill::resolver::compare_versions(
                &item.version,
                &local.current_version,
            ) == std::cmp::Ordering::Greater
            {
                "update"
            } else {
                "satisfied"
            };
            crate::app::plugin_manager::PluginDependencyAction {
                plugin_id: item.plugin_id.clone(),
                plugin_name: item.name.clone(),
                plugin_description: item.description.clone(),
                required: true,
                current_version: local.current_version,
                target_version: item.version,
                action: action.to_string(),
                reason: "GitHub 扩展源依赖".to_string(),
                requested_by: plugin.name.clone(),
            }
        })
        .collect();
    Ok(crate::app::plugin_manager::PluginInstallPlan {
        plugin,
        dependency_actions,
        blocked_reasons: Vec::new(),
        ready: true,
    })
}

pub(crate) fn install_skill(
    skill_id: &str,
    version: Option<&str>,
) -> Result<(SkillCatalogItem, crate::skill::types::SkillRecord), Box<dyn Error>> {
    let snapshot = snapshot()?;
    let item = snapshot
        .skill_versions
        .iter()
        .find(|item| {
            item.skill_id == skill_id
                && version
                    .map(|requested| requested == item.version)
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到 Skill: {skill_id}"))?;
    // Resolve the source before mutating any local dependency state so a
    // malformed catalog cannot leave a partially installed dependency set.
    let source = source_for_catalog_item(&snapshot, &item.source)?;
    let previous_provenance = read_provenance("skill", &item.skill_id)?;
    let mut plugin_changes = Vec::new();
    let mut plugin_provenance_changes = Vec::new();
    let mut lock_changes = Vec::new();
    let previous_references =
        crate::app::plugin_manager::owner_dependency_ids(&format!("skill:{skill_id}"));
    for dependency in item
        .plugin_dependencies
        .iter()
        .filter(|dependency| dependency.required)
    {
        let before = crate::app::plugin_manager::local_status(&dependency.plugin_id);
        let previous_lock = crate::app::extension_lock::read("plugin", &dependency.plugin_id)?;
        let previous_plugin_provenance = match read_provenance("plugin", &dependency.plugin_id) {
            Ok(value) => value,
            Err(error) => {
                restore_provenance_changes(&plugin_provenance_changes);
                restore_lock_changes(&lock_changes);
                compensate_plugin_changes(&plugin_changes);
                return Err(error);
            }
        };
        let satisfied = !before.current_version.is_empty()
            && (dependency.min_version.is_empty()
                || crate::skill::resolver::compare_versions(
                    &before.current_version,
                    &dependency.min_version,
                ) != std::cmp::Ordering::Less);
        if satisfied {
            continue;
        }
        if let Err(error) = install_plugin(&dependency.plugin_id, None) {
            restore_provenance_changes(&plugin_provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&plugin_changes);
            return Err(format!("安装 Skill 依赖 {} 失败: {error}", dependency.plugin_id).into());
        }
        plugin_changes.push((dependency.plugin_id.clone(), before));
        plugin_provenance_changes.push((
            "plugin".to_string(),
            dependency.plugin_id.clone(),
            previous_plugin_provenance,
        ));
        lock_changes.push((
            "plugin".to_string(),
            dependency.plugin_id.clone(),
            previous_lock,
        ));
    }
    let dependency_ids = item
        .plugin_dependencies
        .iter()
        .filter(|dependency| dependency.required)
        .map(|dependency| dependency.plugin_id.clone())
        .collect::<Vec<_>>();
    let owner = format!("skill:{skill_id}");
    if let Err(error) = crate::app::plugin_manager::set_owner_references(&owner, &dependency_ids) {
        restore_provenance_changes(&plugin_provenance_changes);
        compensate_plugin_changes(&plugin_changes);
        return Err(format!("记录 Skill 插件依赖失败: {error}").into());
    }
    let record = match crate::app::skill_manager::install_public_catalog_item(
        &item,
        source.verification.requires_signature(),
    ) {
        Ok(record) => record,
        Err(error) => {
            let _ = crate::app::plugin_manager::set_owner_references(&owner, &previous_references);
            restore_provenance_changes(&plugin_provenance_changes);
            restore_lock_changes(&lock_changes);
            compensate_plugin_changes(&plugin_changes);
            return Err(error);
        }
    };
    if let Err(error) = save_provenance(
        source,
        "skill",
        &item.skill_id,
        &item.version,
        &item.download_url,
        &item.sha256,
        &item.signature_key_id,
    ) {
        let _ = crate::app::plugin_manager::set_owner_references(&owner, &previous_references);
        restore_provenance_changes(&[(
            "skill".to_string(),
            item.skill_id.clone(),
            previous_provenance,
        )]);
        restore_provenance_changes(&plugin_provenance_changes);
        restore_lock_changes(&lock_changes);
        compensate_plugin_changes(&plugin_changes);
        return Err(error);
    }
    let previous_lock = crate::app::extension_lock::read("skill", &item.skill_id)?;
    lock_changes.push(("skill".to_string(), item.skill_id.clone(), previous_lock));
    if let Err(error) = crate::app::extension_lock::record_source_skill(source, &item) {
        let _ = crate::app::plugin_manager::set_owner_references(&owner, &previous_references);
        restore_provenance_changes(&[(
            "skill".to_string(),
            item.skill_id.clone(),
            previous_provenance,
        )]);
        restore_provenance_changes(&plugin_provenance_changes);
        restore_lock_changes(&lock_changes);
        compensate_plugin_changes(&plugin_changes);
        return Err(error);
    }
    Ok((item, record))
}

pub(crate) fn plan_skill(
    skill_id: &str,
    version: Option<&str>,
) -> Result<crate::app::skill_manager::SkillInstallPlan, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let skill = snapshot
        .skill_versions
        .iter()
        .find(|item| {
            item.skill_id == skill_id
                && version
                    .map(|requested| requested == item.version)
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到 Skill: {skill_id}"))?;
    let mut actions = Vec::new();
    let mut blocked_reasons = Vec::new();
    for dependency in &skill.plugin_dependencies {
        let local = crate::app::plugin_manager::local_status(&dependency.plugin_id);
        let source_item = snapshot
            .plugins
            .iter()
            .find(|item| item.plugin_id == dependency.plugin_id);
        let satisfied = !local.current_version.is_empty()
            && (dependency.min_version.is_empty()
                || crate::skill::resolver::compare_versions(
                    &local.current_version,
                    &dependency.min_version,
                ) != std::cmp::Ordering::Less);
        if dependency.required && !satisfied && source_item.is_none() {
            blocked_reasons.push(format!("缺少必需插件 {}", dependency.plugin_id));
        }
        actions.push(crate::app::plugin_manager::PluginDependencyAction {
            plugin_id: dependency.plugin_id.clone(),
            plugin_name: source_item
                .map(|item| item.name.clone())
                .unwrap_or_else(|| dependency.plugin_id.clone()),
            plugin_description: source_item
                .map(|item| item.description.clone())
                .unwrap_or_default(),
            required: dependency.required,
            current_version: local.current_version,
            target_version: source_item
                .map(|item| item.version.clone())
                .unwrap_or_default(),
            action: if satisfied {
                "satisfied".to_string()
            } else if source_item.is_some() {
                "install".to_string()
            } else {
                "unavailable".to_string()
            },
            reason: "GitHub 扩展源依赖".to_string(),
            requested_by: skill.name.clone(),
        });
    }
    Ok(crate::app::skill_manager::SkillInstallPlan {
        skill,
        plugin_actions: actions,
        ready: blocked_reasons.is_empty(),
        blocked_reasons,
    })
}

pub(crate) fn ensure_authoring_feature() -> Result<(), Box<dyn Error>> {
    let plugin_ready = crate::app::plugin_manager::local_status(AUTHORING_PLUGIN_ID).enabled;
    let store = crate::skill::store::SkillStore::new();
    let skills_ready = AUTHORING_SKILL_IDS
        .iter()
        .all(|skill_id| store.get_record(skill_id).ok().flatten().is_some());
    if plugin_ready && skills_ready {
        return Ok(());
    }
    let snapshot = snapshot()?;
    let pack = snapshot
        .feature_packs
        .iter()
        .find(|pack| pack.id == AUTHORING_FEATURE_ID)
        .cloned()
        .unwrap_or_else(|| ExtensionFeaturePack {
            id: AUTHORING_FEATURE_ID.to_string(),
            name: "扩展创作".to_string(),
            plugin_ids: vec![AUTHORING_PLUGIN_ID.to_string()],
            skill_ids: AUTHORING_SKILL_IDS
                .iter()
                .map(|value| value.to_string())
                .collect(),
        });
    if !plugin_ready {
        if !snapshot
            .plugins
            .iter()
            .any(|item| item.plugin_id == AUTHORING_PLUGIN_ID)
        {
            return Err("扩展创作组件尚未安装，且已配置的扩展源未提供 AI 扩展开发工具".into());
        }
        install_plugin(AUTHORING_PLUGIN_ID, None)?;
    }
    for skill_id in &pack.skill_ids {
        if store.get_record(skill_id)?.is_none() {
            if !snapshot
                .skills
                .iter()
                .any(|item| item.skill_id == *skill_id)
            {
                return Err(format!("扩展创作组件缺少 Skill: {skill_id}").into());
            }
            let (_, record) = install_skill(skill_id, None)?;
            crate::skill::sync_record_to_supported_clients(&record, crate::VERSION, &[])?;
        }
    }
    Ok(())
}

pub(crate) fn reconcile_auto_updates() -> Result<Vec<String>, Box<dyn Error>> {
    let snapshot = refresh_snapshot()?;
    let auto_sources = snapshot
        .sources
        .iter()
        .filter(|status| status.source.enabled && status.source.auto_update)
        .map(|status| status.source.id.as_str())
        .collect::<HashSet<_>>();
    if auto_sources.is_empty() {
        return Ok(Vec::new());
    }
    let mut updated = Vec::new();
    for provenance in list_provenance()? {
        if !auto_sources.contains(provenance.source_id.as_str()) {
            continue;
        }
        if provenance.asset_kind == "plugin" {
            let Some(item) = snapshot.plugins.iter().find(|item| {
                item.plugin_id == provenance.asset_key
                    && item.source == format!("github:{}", provenance.source_id)
            }) else {
                continue;
            };
            let local = crate::app::plugin_manager::local_status(&provenance.asset_key);
            if crate::skill::resolver::compare_versions(&item.version, &local.current_version)
                == std::cmp::Ordering::Greater
            {
                install_plugin(&provenance.asset_key, Some(&item.version))?;
                updated.push(format!("plugin:{}@{}", item.plugin_id, item.version));
            }
        } else if provenance.asset_kind == "skill" {
            let Some(item) = snapshot.skills.iter().find(|item| {
                item.skill_id == provenance.asset_key
                    && item.source == format!("github:{}", provenance.source_id)
            }) else {
                continue;
            };
            let current = crate::skill::store::SkillStore::new()
                .get_record(&provenance.asset_key)?
                .map(|record| record.manifest.version)
                .unwrap_or_default();
            if crate::skill::resolver::compare_versions(&item.version, &current)
                == std::cmp::Ordering::Greater
            {
                let (_, record) = install_skill(&provenance.asset_key, Some(&item.version))?;
                crate::skill::sync_record_to_supported_clients(&record, crate::VERSION, &[])?;
                updated.push(format!("skill:{}@{}", item.skill_id, item.version));
            }
        }
    }
    Ok(updated)
}

fn resolve_plugin_order(
    catalog: &[PluginCatalogItem],
    plugin_id: &str,
    version: Option<&str>,
    visiting: &mut HashSet<String>,
    order: &mut Vec<PluginCatalogItem>,
) -> Result<(), Box<dyn Error>> {
    if order.iter().any(|item| item.plugin_id == plugin_id) {
        return Ok(());
    }
    if !visiting.insert(plugin_id.to_string()) {
        return Err(format!("插件依赖存在循环: {plugin_id}").into());
    }
    let item = catalog
        .iter()
        .find(|item| {
            item.plugin_id == plugin_id
                && version
                    .map(|requested| requested == item.version)
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到插件: {plugin_id}"))?;
    for dependency in item
        .plugin_dependencies
        .iter()
        .filter(|dependency| dependency.required)
    {
        let installed = crate::app::plugin_manager::local_status(&dependency.plugin_id);
        let satisfied = !installed.current_version.is_empty()
            && (dependency.min_version.is_empty()
                || crate::skill::resolver::compare_versions(
                    &installed.current_version,
                    &dependency.min_version,
                ) != std::cmp::Ordering::Less);
        if !satisfied {
            resolve_plugin_order(catalog, &dependency.plugin_id, None, visiting, order)?;
            let resolved = order
                .iter()
                .find(|candidate| candidate.plugin_id == dependency.plugin_id)
                .ok_or("插件依赖解析结果缺失")?;
            if !dependency.min_version.is_empty()
                && crate::skill::resolver::compare_versions(
                    &resolved.version,
                    &dependency.min_version,
                ) == std::cmp::Ordering::Less
            {
                return Err(format!(
                    "插件依赖 {} 需要 v{} 及以上",
                    dependency.plugin_id, dependency.min_version
                )
                .into());
            }
        }
    }
    visiting.remove(plugin_id);
    order.push(item);
    Ok(())
}

fn compensate_plugin_changes(changes: &[(String, crate::app::plugin_manager::LocalPluginStatus)]) {
    for (plugin_id, before) in changes.iter().rev() {
        let current = crate::app::plugin_manager::local_status(plugin_id);
        if before.current_version.is_empty() {
            let _ = crate::app::plugin_manager::remove_for_policy(plugin_id);
        } else if current.current_version != before.current_version {
            let _ = crate::app::plugin_manager::rollback(plugin_id);
        }
        if !before.enabled {
            let _ = crate::app::plugin_manager::set_enabled(plugin_id, false);
        }
    }
}

fn source_for_catalog_item<'a>(
    snapshot: &'a ExtensionSourceSnapshot,
    source: &str,
) -> Result<&'a ExtensionSourceConfig, Box<dyn Error>> {
    let source_id = source
        .strip_prefix("github:")
        .ok_or("扩展目录项缺少 GitHub 来源身份")?;
    snapshot
        .sources
        .iter()
        .map(|status| &status.source)
        .find(|config| config.id == source_id)
        .ok_or_else(|| "扩展目录项对应的来源配置不存在".into())
}

fn fetch_catalog(source: &ExtensionSourceConfig) -> Result<ExtensionSourceCatalog, Box<dyn Error>> {
    let url = catalog_url(source)?;
    let catalog = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("HiMind-Agent")
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .json::<ExtensionSourceCatalog>()?;
    validate_catalog(&catalog, source)?;
    Ok(catalog)
}

pub(crate) fn validate_catalog(
    catalog: &ExtensionSourceCatalog,
    source: &ExtensionSourceConfig,
) -> Result<(), Box<dyn Error>> {
    if catalog.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(format!("扩展源目录版本不受支持: {}", catalog.schema_version).into());
    }
    if !catalog.source_id.is_empty() && catalog.source_id != source.id {
        return Err("扩展源目录身份与本机配置不一致".into());
    }
    let mut identities = HashSet::new();
    for item in &catalog.plugins {
        if !identities.insert(format!("plugin:{}:{}", item.plugin_id, item.version)) {
            return Err(format!(
                "扩展源包含重复插件版本: {} {}",
                item.plugin_id, item.version
            )
            .into());
        }
        validate_artifact(
            &source.repository,
            &item.download_url,
            item.file_size,
            &item.sha256,
        )?;
        validate_catalog_signature(
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            &source.verification,
        )?;
    }
    for item in &catalog.skills {
        if !identities.insert(format!("skill:{}:{}", item.skill_id, item.version)) {
            return Err(format!(
                "扩展源包含重复 Skill 版本: {} {}",
                item.skill_id, item.version
            )
            .into());
        }
        validate_artifact(
            &source.repository,
            &item.download_url,
            item.file_size,
            &item.sha256,
        )?;
        validate_catalog_signature(
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            &source.verification,
        )?;
    }
    Ok(())
}

fn validate_catalog_signature(
    signature: &str,
    key_id: &str,
    algorithm: &str,
    verification: &ExtensionSourceVerification,
) -> Result<(), Box<dyn Error>> {
    crate::app::system::validate_signature_metadata(
        signature,
        key_id,
        algorithm,
        verification.requires_signature(),
    )?;
    if !signature.is_empty() {
        crate::app::system::trusted_signing_public_key(key_id)?;
    }
    Ok(())
}

fn normalize_plugin_item(
    item: &mut PluginCatalogItem,
    source: &ExtensionSourceConfig,
) -> Result<(), Box<dyn Error>> {
    validate_asset_identity("plugin", &item.plugin_id)?;
    item.governance = "optional".to_string();
    item.source = format!("github:{}", source.id);
    item.assignment = "optional".to_string();
    item.management = "user_managed".to_string();
    item.install_mode = "prompt".to_string();
    item.organization_reason.clear();
    item.managed = false;
    item.allow_disable = true;
    item.allow_uninstall = true;
    Ok(())
}

fn normalize_skill_item(
    item: &mut SkillCatalogItem,
    source: &ExtensionSourceConfig,
) -> Result<(), Box<dyn Error>> {
    validate_asset_identity("skill", &item.skill_id)?;
    item.source = format!("github:{}", source.id);
    item.assignment = "optional".to_string();
    item.management = "user_managed".to_string();
    item.install_mode = "prompt".to_string();
    item.organization_reason.clear();
    item.managed = false;
    item.allow_disable = true;
    item.allow_uninstall = true;
    Ok(())
}

fn validate_feature_pack(pack: &ExtensionFeaturePack) -> Result<(), Box<dyn Error>> {
    validate_asset_key(&pack.id)?;
    for key in pack.plugin_ids.iter().chain(pack.skill_ids.iter()) {
        validate_asset_key(key)?;
    }
    Ok(())
}

fn validate_artifact(
    repository: &str,
    download_url: &str,
    file_size: u64,
    sha256: &str,
) -> Result<(), Box<dyn Error>> {
    if file_size == 0 || sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("扩展源制品大小或 SHA-256 无效".into());
    }
    let url = url::Url::parse(download_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err("GitHub 扩展源制品必须使用 github.com 的 HTTPS Release 地址".into());
    }
    let expected = format!("/{repository}/releases/download/");
    if !url.path().starts_with(&expected) {
        return Err("扩展源制品地址不属于配置的 GitHub 仓库".into());
    }
    Ok(())
}

fn settings_at(path: &Path) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(ExtensionSourceSettings::default());
    }
    let value: ExtensionSourceSettings = serde_json::from_slice(&fs::read(path)?)?;
    if value.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(format!("扩展源配置版本不受支持: {}", value.schema_version).into());
    }
    let mut ids = HashSet::new();
    for source in &value.sources {
        if source.id
            != source_id(
                &normalize_repository(&source.repository)?,
                &validate_reference(&source.reference)?,
                &validate_catalog_path(&source.catalog_path)?,
            )
        {
            return Err(format!("扩展源配置身份无效: {}", source.name).into());
        }
        if !ids.insert(source.id.clone()) {
            return Err(format!("扩展源配置重复: {}", source.id).into());
        }
    }
    Ok(value)
}

fn parse_verification(value: Option<&str>) -> Result<ExtensionSourceVerification, Box<dyn Error>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(ExtensionSourceVerification::Required),
        Some("required") => Ok(ExtensionSourceVerification::Required),
        Some("optional") => Ok(ExtensionSourceVerification::Optional),
        Some(value) => Err(format!("扩展源来源校验策略不受支持: {value}").into()),
    }
}

fn source_verification(
    repository: &str,
    value: Option<&str>,
) -> Result<ExtensionSourceVerification, Box<dyn Error>> {
    let verification = parse_verification(value)?;
    if repository.eq_ignore_ascii_case(OFFICIAL_EXTENSION_REPOSITORY)
        && !verification.requires_signature()
    {
        return Err("HiMind 官方扩展源必须使用可信签名校验".into());
    }
    Ok(verification)
}

fn save_settings(settings: &ExtensionSourceSettings) -> Result<(), Box<dyn Error>> {
    atomic_file::atomic_write(&settings_path(), &serde_json::to_vec_pretty(settings)?)?;
    Ok(())
}

fn save_cached_catalog(
    source_id: &str,
    catalog: &ExtensionSourceCatalog,
) -> Result<(), Box<dyn Error>> {
    atomic_file::atomic_write(&cache_path(source_id), &serde_json::to_vec_pretty(catalog)?)?;
    Ok(())
}

fn load_cached_catalog(source_id: &str) -> Result<Option<ExtensionSourceCatalog>, Box<dyn Error>> {
    let path = cache_path(source_id);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn settings_path() -> PathBuf {
    paths::agent_home().join("data/extension-sources.json")
}

fn cache_path(source_id: &str) -> PathBuf {
    paths::agent_home()
        .join("data/extension-source-cache")
        .join(format!("{source_id}.json"))
}

fn provenance_root() -> PathBuf {
    paths::agent_home().join("data/extension-provenance")
}

fn provenance_path(kind: &str, key: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_asset_identity(kind, key)?;
    Ok(provenance_root().join(format!("{kind}-{key}.json")))
}

fn catalog_url(source: &ExtensionSourceConfig) -> Result<url::Url, Box<dyn Error>> {
    Ok(url::Url::parse(&format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        source.repository, source.reference, source.catalog_path
    ))?)
}

fn source_id(repository: &str, reference: &str, catalog_path: &str) -> String {
    let digest = Sha256::digest(format!("{repository}\n{reference}\n{catalog_path}").as_bytes());
    format!("github-{:x}", digest)[..23].to_string()
}

fn normalize_repository(value: &str) -> Result<String, Box<dyn Error>> {
    Ok(crate::app::github_source::parse_source_url(value)?.repository)
}

fn validate_reference(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.'))
    {
        return Err("GitHub ref 必须是固定 tag、branch 或 commit，且不能包含路径穿越".into());
    }
    Ok(value.to_string())
}

fn validate_catalog_path(value: &str) -> Result<String, Box<dyn Error>> {
    let normalized = value.trim().replace('\\', "/");
    let path = Path::new(&normalized);
    if normalized.is_empty()
        || !normalized.ends_with(".json")
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("扩展源目录路径必须是仓库内的 JSON 文件".into());
    }
    Ok(normalized)
}

fn validate_asset_identity(kind: &str, key: &str) -> Result<(), Box<dyn Error>> {
    if !matches!(kind, "plugin" | "skill") {
        return Err("扩展类型无效".into());
    }
    validate_asset_key(key)
}

fn validate_asset_key(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("扩展 ID 无效".into());
    }
    Ok(())
}

fn settings_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_item(url: &str) -> PluginCatalogItem {
        PluginCatalogItem {
            plugin_id: "com.himind.test".to_string(),
            name: "测试".to_string(),
            description: String::new(),
            author_name: String::new(),
            categories: vec![],
            review_status: String::new(),
            governance: "required".to_string(),
            version: "1.0.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: "0.3.0".to_string(),
            channel: "stable".to_string(),
            artifact_id: String::new(),
            file_name: "test.hmpkg".to_string(),
            file_size: 8,
            sha256: "a".repeat(64),
            signature: "c2ln".to_string(),
            signature_key_id: "test".to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
            download_url: url.to_string(),
            source: String::new(),
            assignment: String::new(),
            management: String::new(),
            install_mode: String::new(),
            organization_reason: String::new(),
            managed: true,
            allow_disable: false,
            allow_uninstall: false,
            capability_ids: vec![],
            permissions: vec![],
            view_count: 0,
            plugin_dependencies: vec![],
        }
    }

    #[test]
    fn validates_source_identity_and_paths() {
        assert_eq!(
            normalize_repository("https://github.com/Owner/repo.git").unwrap(),
            "Owner/repo"
        );
        assert_eq!(
            normalize_repository("https://github.com/Owner/repo.git?path=/extensions#v1.0.0")
                .unwrap(),
            "Owner/repo"
        );
        assert!(normalize_repository("https://example.com/Owner/repo").is_err());
        assert!(validate_reference("v1.0.0").is_ok());
        assert!(validate_reference("../main").is_err());
        assert!(validate_catalog_path(".himind/catalog.json").is_ok());
        assert!(validate_catalog_path("../catalog.json").is_err());
    }

    #[test]
    fn catalog_artifacts_must_be_repository_release_assets() {
        assert!(validate_artifact(
            "Owner/repo",
            "https://github.com/Owner/repo/releases/download/v1/test.hmpkg",
            8,
            &"a".repeat(64)
        )
        .is_ok());
        assert!(validate_artifact(
            "Owner/repo",
            "https://github.com/Other/repo/releases/download/v1/test.hmpkg",
            8,
            &"a".repeat(64)
        )
        .is_err());
        assert!(validate_artifact(
            "Owner/repo",
            "https://example.com/test.hmpkg",
            8,
            &"a".repeat(64)
        )
        .is_err());
    }

    #[test]
    fn github_catalog_cannot_assign_organization_policy() {
        let source = ExtensionSourceConfig {
            id: "github-test".to_string(),
            name: "测试".to_string(),
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: ".himind/catalog.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Required,
        };
        let mut item = plugin_item("https://github.com/Owner/repo/releases/download/v1/test.hmpkg");
        normalize_plugin_item(&mut item, &source).unwrap();
        assert_eq!(item.governance, "optional");
        assert_eq!(item.management, "user_managed");
        assert!(!item.managed);
        assert!(item.allow_disable && item.allow_uninstall);
    }

    #[test]
    fn plugin_version_history_is_sorted_newest_first() {
        let mut older =
            plugin_item("https://github.com/Owner/repo/releases/download/v1/test.hmpkg");
        older.version = "1.4.0".to_string();
        let mut newer = older.clone();
        newer.version = "2.0.0".to_string();
        let mut versions = vec![older, newer];
        sort_plugin_versions(&mut versions);
        assert_eq!(versions[0].version, "2.0.0");
        assert_eq!(versions[1].version, "1.4.0");
    }

    #[test]
    fn settings_round_trip_without_profile_globals() {
        let root = std::env::temp_dir().join(format!(
            "himind-extension-source-test-{}",
            std::process::id()
        ));
        let path = root.join("extension-sources.json");
        let repository = "Owner/repo";
        let reference = "main";
        let catalog_path = ".himind/catalog.json";
        let value = ExtensionSourceSettings {
            schema_version: 1,
            sources: vec![ExtensionSourceConfig {
                id: source_id(repository, reference, catalog_path),
                name: "测试".to_string(),
                repository: repository.to_string(),
                reference: reference.to_string(),
                catalog_path: catalog_path.to_string(),
                enabled: true,
                auto_update: false,
                verification: ExtensionSourceVerification::Required,
            }],
        };
        fs::create_dir_all(&root).unwrap();
        atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert_eq!(settings_at(&path).unwrap().sources, value.sources);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_verification_defaults_to_required_and_accepts_optional() {
        let required: ExtensionSourceConfig = serde_json::from_value(serde_json::json!({
            "id": "github-test",
            "name": "测试",
            "repository": "Owner/repo",
            "reference": "main",
            "catalog_path": ".himind/catalog.json",
            "enabled": true,
            "auto_update": false
        }))
        .unwrap();
        assert_eq!(required.verification, ExtensionSourceVerification::Required);
        assert!(required.verification.requires_signature());
        assert_eq!(
            parse_verification(Some("optional")).unwrap(),
            ExtensionSourceVerification::Optional
        );
        assert!(parse_verification(Some("disabled")).is_err());
        assert!(source_verification(OFFICIAL_EXTENSION_REPOSITORY, Some("optional")).is_err());
        assert_eq!(
            source_verification("Owner/custom", Some("optional")).unwrap(),
            ExtensionSourceVerification::Optional
        );
    }

    #[test]
    fn optional_source_allows_unsigned_catalog_but_rejects_partial_metadata() {
        assert!(
            validate_catalog_signature("", "", "", &ExtensionSourceVerification::Optional).is_ok()
        );
        assert!(validate_catalog_signature(
            "c2ln",
            "",
            "rsa-pss-sha256",
            &ExtensionSourceVerification::Optional
        )
        .is_err());
        assert!(
            validate_catalog_signature("", "", "", &ExtensionSourceVerification::Required).is_err()
        );
    }
}
