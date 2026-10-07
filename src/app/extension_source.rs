use crate::api::distribution::{
    ExpertCatalogItem, PluginCatalogItem, SkillCatalogItem, WorkflowCatalogItem,
};
use crate::extension_contracts::{normalize_distribution_targets, DistributionTarget};
use crate::store::{atomic_file, paths};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SETTINGS_SCHEMA_VERSION: u32 = 1;
const CATALOG_SCHEMA_VERSION: u32 = 1;
const DEFAULT_CATALOG_PATH: &str = ".himind/catalog.json";
const LOCAL_CATALOG_PATH: &str = "extensions.json";
const OFFICIAL_EXTENSION_REPOSITORY: &str = "MrBaoquan/himind-extensions";
const SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(60);
const DEFAULT_DISTRIBUTION_CHANNEL: &str = "stable";
const DEFAULT_CATALOG_ID: &str = "public";
pub(crate) const AUTHORING_FEATURE_ID: &str = "com.himind.feature.extension-authoring";
const AUTHORING_PLUGIN_ID: &str = "com.himind.extension-development-tools";
/// 「扩展创作」能力集 = 1 个插件 + 4 个技能。缺任何一项，AI 都不知道
/// 自己该按什么规范创作对应类型的扩展，所以这里必须成套校验。
const AUTHORING_SKILL_IDS: [&str; 4] = [
    "com.himind.skill.develop-himind-plugins",
    "com.himind.skill.develop-himind-skills",
    "com.himind.skill.develop-himind-workflows",
    "com.himind.skill.develop-himind-conventions",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionSourceKind {
    Github,
    Local,
}

impl Default for ExtensionSourceKind {
    fn default() -> Self {
        Self::Github
    }
}

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

/// 同一分发单元内从哪一侧取用扩展。本地工作区是开发期权威来源，
/// 因此默认取本地；切到远端用于验证「客户端按仓库地址安装」这条分发链路。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionSourceAcquisition {
    Local,
    Remote,
}

impl Default for ExtensionSourceAcquisition {
    fn default() -> Self {
        Self::Local
    }
}

/// 分发单元：归一化地址相同的一组来源记录。本地工作区源与其对应的
/// GitHub 分发源属于同一单元，在列表里只呈现一行，由 `acquisition`
/// 决定从哪一侧取用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionDistributionUnit {
    pub unit_key: String,
    pub name: String,
    pub distribution_id: String,
    pub channel: String,
    pub catalog_id: String,
    pub acquisition: ExtensionSourceAcquisition,
    #[serde(default)]
    pub local_source_id: Option<String>,
    #[serde(default)]
    pub remote_source_id: Option<String>,
    pub repository: String,
    #[serde(default)]
    pub local_root: Option<String>,
    pub plugin_count: usize,
    pub skill_count: usize,
    pub workflow_count: usize,
    #[serde(default)]
    pub expert_count: usize,
    pub state: String,
    #[serde(default)]
    pub plugin_ids: Vec<String>,
    #[serde(default)]
    pub skill_ids: Vec<String>,
    #[serde(default)]
    pub workflow_ids: Vec<String>,
    #[serde(default)]
    pub expert_ids: Vec<String>,
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub installed: Vec<ExtensionUnitInstallation>,
    /// 取用侧目录当前提供的制品版本，用于与 `installed` 对比得出可更新项。
    #[serde(default)]
    pub assets: Vec<ExtensionUnitAsset>,
    /// 非取用侧的情况。取用侧是本地时这里描述 GitHub 发布，反之描述本地源码；
    /// 单元只有一侧来源时为 `None`。
    #[serde(default)]
    pub other_side: Option<ExtensionUnitOtherSide>,
}

/// 单元里「另一侧」的可用性与版本落差。
///
/// 取用侧决定市场和安装看到的内容，另一侧的更高版本不会自动顶上来。UI 用这里的
/// 计数提示开发者「远端已经有更新，可以切换取用侧」，避免本地预览掩盖线上版本。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionUnitOtherSide {
    /// `local` 或 `remote`。
    pub side: String,
    /// 该侧目录是否已加载。来源停用或读取失败时为 false。
    pub available: bool,
    /// 该侧存在更高版本的制品数量。
    pub newer_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionUnitAsset {
    pub asset_kind: String,
    pub asset_id: String,
    pub name: String,
    pub version: String,
    /// 来源绑定字段。安装请求必须使用同一来源和版本，不能仅按 ID 反查。
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub artifact_url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub signature_key_id: String,
    #[serde(default)]
    pub signature_algorithm: String,
    #[serde(default)]
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionUnitInstallation {
    pub asset_kind: String,
    pub asset_id: String,
    pub version: String,
    pub source_id: String,
    #[serde(default)]
    pub sha256: String,
    /// `local` / `remote` 表示该安装来自本单元取用侧，`foreign` 表示来自
    /// 其它来源（含免安装的开发挂载与本地文件安装）。
    pub side: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct ExtensionSourceConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ExtensionSourceKind,
    pub repository: String,
    pub reference: String,
    pub catalog_path: String,
    pub enabled: bool,
    pub auto_update: bool,
    #[serde(default)]
    pub verification: ExtensionSourceVerification,
    /// GitHub 上游仓库（owner/repo）。本地目录源用于关联它对应的分发源，
    /// GitHub 源为空字符串。
    /// 派生字段：每次读取时由工作区 `extensions.json` / git remote 现算，
    /// 并在 `save_settings` 落盘前清空，避免配置里残留过期值。
    #[serde(default)]
    pub upstream_repository: String,
    /// Stable product identity shared by local, GitHub and Dashboard sources.
    /// Older settings omit it and are migrated from the repository/upstream.
    #[serde(default)]
    pub distribution_id: String,
    /// Release lane, for example `stable`, `beta` or `dev`.
    #[serde(default = "default_distribution_channel")]
    pub channel: String,
    /// Catalog namespace within a distribution, normally `public`.
    #[serde(default = "default_catalog_id")]
    pub catalog_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionSourceSettings {
    #[serde(default = "settings_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<ExtensionSourceConfig>,
    /// 分发单元取用模式：`unit_key` → `acquisition`。仅保存与默认值不同的
    /// 单元，未登记的单元按 `Local` 处理。
    #[serde(default)]
    pub acquisitions: BTreeMap<String, ExtensionSourceAcquisition>,
    /// 分发单元默认分发目标：`unit_key` → 目标集合。仅保存显式设置过的
    /// 单元，未登记的单元按「仅工作台」处理。项目级覆盖优先于这里的默认值。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub distribution_targets: BTreeMap<String, Vec<DistributionTarget>>,
}

impl Default for ExtensionSourceSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            sources: Vec::new(),
            acquisitions: BTreeMap::new(),
            distribution_targets: BTreeMap::new(),
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
    #[serde(default)]
    pub agent_preset_ids: Vec<String>,
    #[serde(skip)]
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentPresetCatalogItem {
    pub preset_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ExtensionAgentPreset {
    pub preset_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtensionSourceCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub generation: String,
    /// Explicit identity prevents an accidentally matching repository URL from
    /// merging unrelated local and remote sources.
    #[serde(default)]
    pub distribution_id: String,
    #[serde(default = "default_distribution_channel")]
    pub channel: String,
    #[serde(default = "default_catalog_id")]
    pub catalog_id: String,
    #[serde(default)]
    pub plugins: Vec<PluginCatalogItem>,
    #[serde(default)]
    pub skills: Vec<SkillCatalogItem>,
    #[serde(default)]
    pub workflows: Vec<WorkflowCatalogItem>,
    #[serde(default)]
    pub experts: Vec<ExpertCatalogItem>,
    #[serde(default)]
    pub feature_packs: Vec<ExtensionFeaturePack>,
    #[serde(default)]
    pub agent_presets: Vec<AgentPresetCatalogItem>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionSourceStatus {
    pub source: ExtensionSourceConfig,
    pub state: String,
    pub plugin_count: usize,
    pub skill_count: usize,
    pub workflow_count: usize,
    #[serde(default)]
    pub expert_count: usize,
    pub generation: String,
    pub using_cache: bool,
    pub error: String,
    #[serde(default)]
    pub versions: Vec<ExtensionSourceVersion>,
    /// Local source provenance. Empty for GitHub sources.
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub source_tree: String,
    #[serde(default)]
    pub source_dirty: bool,
    /// 合并说明（如同名扩展被其他来源优先采用），不影响来源可用状态。
    pub notices: Vec<ExtensionSourceNotice>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionSourceVersion {
    pub asset_kind: String,
    pub asset_id: String,
    pub version: String,
}

/// 同名扩展未参与合并的说明：按原因分组，避免把每一项拼成一句长文本。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionSourceNotice {
    pub reason: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ExtensionSourceSnapshot {
    pub plugins: Vec<PluginCatalogItem>,
    pub skills: Vec<SkillCatalogItem>,
    pub workflows: Vec<WorkflowCatalogItem>,
    #[serde(default)]
    pub experts: Vec<ExpertCatalogItem>,
    pub feature_packs: Vec<ExtensionFeaturePack>,
    pub agent_presets: Vec<ExtensionAgentPreset>,
    pub sources: Vec<ExtensionSourceStatus>,
    #[serde(default)]
    pub units: Vec<ExtensionDistributionUnit>,
    #[serde(skip_serializing)]
    plugin_versions: Vec<PluginCatalogItem>,
    #[serde(skip_serializing)]
    skill_versions: Vec<SkillCatalogItem>,
    #[serde(skip_serializing)]
    workflow_versions: Vec<WorkflowCatalogItem>,
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
    let distribution_id = normalize_repository_key(&repository);
    Ok(ExtensionSourceConfig {
        id: id.clone(),
        name: if name.trim().is_empty() {
            repository.clone()
        } else {
            name.trim().chars().take(80).collect()
        },
        kind: ExtensionSourceKind::Github,
        repository,
        reference,
        catalog_path,
        enabled: true,
        auto_update: false,
        verification,
        upstream_repository: String::new(),
        distribution_id,
        channel: DEFAULT_DISTRIBUTION_CHANNEL.to_string(),
        catalog_id: DEFAULT_CATALOG_ID.to_string(),
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

pub(crate) fn add_local_source(
    name: &str,
    root: &str,
    catalog_path: Option<&str>,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let root_path = Path::new(root.trim());
    if !root_path.is_dir() {
        return Err("本地扩展源必须是目录".into());
    }
    let root = root_path.canonicalize()?;
    let catalog_path = validate_catalog_path(catalog_path.unwrap_or(LOCAL_CATALOG_PATH))?;
    let catalog_file = root.join(&catalog_path);
    if !catalog_file.is_file() {
        return Err(format!("本地扩展源缺少目录文件: {catalog_path}").into());
    }
    let content = fs::read_to_string(&catalog_file)?;
    let aggregate: LocalAggregateCatalog = serde_json::from_str(&content)
        .map_err(|error| format!("本地扩展源目录文件格式无效: {error}"))?;
    aggregate.validate()?;
    for entry in &aggregate.extensions {
        let dir = safe_local_child(&root, &entry.path)?;
        let manifest_name = match entry.kind.as_str() {
            "plugin" => "plugin.json",
            "skill" => "skill.json",
            "workflow" => "workflow.json",
            "expert" => "expert.json",
            _ => unreachable!(),
        };
        if !dir.join(manifest_name).is_file() {
            return Err(format!("扩展目录缺少 {manifest_name}: {}", entry.path).into());
        }
        let manifest_id = fs::read_to_string(dir.join(manifest_name))
            .ok()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        if manifest_id.as_deref() != Some(entry.id.trim()) {
            return Err(format!("扩展清单 ID 与 manifest 不一致: {}", entry.path).into());
        }
    }
    let root_display = crate::extension_workspace::display_path(&root);
    let id = local_source_id(&root_display, &catalog_path);
    let mut current = settings()?;
    let source = ExtensionSourceConfig {
        id: id.clone(),
        name: if name.trim().is_empty() {
            root_display.clone()
        } else {
            name.trim().chars().take(80).collect()
        },
        kind: ExtensionSourceKind::Local,
        repository: root_display,
        reference: String::new(),
        catalog_path,
        enabled: true,
        auto_update: false,
        verification: ExtensionSourceVerification::Optional,
        upstream_repository: local_upstream_repository(&root, &aggregate.repository),
        distribution_id: aggregate_distribution_id(&aggregate, &root),
        channel: aggregate_channel(&aggregate),
        catalog_id: aggregate_catalog_id(&aggregate),
    };
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
    apply_local_upstreams(&mut current);
    // 第一个本地目录源自动成为当前开发工作区，避免“已添加工作区但无法开发”的断点。
    if !crate::extension_workspace::settings().configured {
        let _ = crate::extension_workspace::select(&root);
    }
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
        if source.kind == ExtensionSourceKind::Local {
            // 本地目录源固定使用用户自定义制品校验，忽略传入的校验策略。
            source.verification = ExtensionSourceVerification::Optional;
        } else {
            source.verification = source_verification(&source.repository, Some(value))?;
        }
    }
    save_settings(&current)?;
    invalidate_snapshot_cache();
    Ok(current)
}

pub(crate) fn remove_source(source_id: &str) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let mut current = settings()?;
    let previous = current.sources.len();
    let removed = current
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .cloned();
    current.sources.retain(|source| source.id != source_id);
    if current.sources.len() == previous {
        return Err("扩展源不存在".into());
    }
    save_settings(&current)?;
    remove_cached_catalog(source_id);
    // 扩展源被移除后，它留下的来源记录已经无处可查，留着只会挡住其他来源接管同名资产。
    remove_provenance_for_source(source_id);
    invalidate_snapshot_cache();
    if let Some(removed) = removed {
        reconcile_workspace_after_removal(&removed, &current.sources);
    }
    Ok(current)
}

enum WorkspaceAfterRemoval {
    Keep,
    Select(String),
    Clear,
}

fn reconcile_workspace_after_removal(
    removed: &ExtensionSourceConfig,
    remaining: &[ExtensionSourceConfig],
) {
    let workspace = crate::extension_workspace::settings();
    let current_root = (workspace.configured && !workspace.root.trim().is_empty())
        .then(|| workspace.root.as_str());
    match workspace_after_removal(removed, remaining, current_root) {
        WorkspaceAfterRemoval::Keep => {}
        WorkspaceAfterRemoval::Select(root) => {
            let _ = crate::extension_workspace::select(Path::new(&root));
        }
        WorkspaceAfterRemoval::Clear => {
            let _ = crate::extension_workspace::clear();
        }
    }
}

/// 移除本地目录源后，当前开发工作区不能指向已解绑的目录。
fn workspace_after_removal(
    removed: &ExtensionSourceConfig,
    remaining: &[ExtensionSourceConfig],
    current_root: Option<&str>,
) -> WorkspaceAfterRemoval {
    let Some(current_root) = current_root else {
        return WorkspaceAfterRemoval::Keep;
    };
    if removed.kind != ExtensionSourceKind::Local || !paths_equal(current_root, &removed.repository)
    {
        return WorkspaceAfterRemoval::Keep;
    }
    match remaining
        .iter()
        .find(|source| source.kind == ExtensionSourceKind::Local)
    {
        Some(next) => WorkspaceAfterRemoval::Select(next.repository.clone()),
        None => WorkspaceAfterRemoval::Clear,
    }
}

fn paths_equal(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

pub(crate) fn snapshot() -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    snapshot_with_cache(false)
}

pub(crate) fn refresh_snapshot() -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    snapshot_with_cache(true)
}

fn snapshot_with_cache(force: bool) -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    if !force {
        if let Some(value) = fresh_cached_snapshot() {
            return Ok(value);
        }
    }
    // 打开「市场」时会同时发起插件/技能/工作流/来源四路请求，冷缓存下它们都会重建
    // 快照并重写同一份来源缓存文件。并发重建会让 Windows 的 MoveFileExW 以
    // access denied 失败，界面上看起来就是"来源刷新失败，请检查权限后重试"这种假
    // 权限错误。重建本身只是读磁盘 + 写缓存，串行化没有副作用。
    let _rebuild = snapshot_rebuild_gate()
        .lock()
        .map_err(|_| "扩展源刷新锁不可用")?;
    if !force {
        // 等锁期间可能已经有线程完成了重建，直接用它的结果。
        if let Some(value) = fresh_cached_snapshot() {
            return Ok(value);
        }
    }
    let value = load_snapshot(force)?;
    store_snapshot_cache(&value)?;
    Ok(value)
}

fn fresh_cached_snapshot() -> Option<ExtensionSourceSnapshot> {
    let cache = snapshot_cache().lock().ok()?;
    match cache.as_ref() {
        Some((loaded_at, value)) if loaded_at.elapsed() < SNAPSHOT_CACHE_TTL => Some(value.clone()),
        _ => None,
    }
}

fn store_snapshot_cache(value: &ExtensionSourceSnapshot) -> Result<(), Box<dyn Error>> {
    *snapshot_cache()
        .lock()
        .map_err(|_| "扩展源内存缓存不可用")? = Some((Instant::now(), value.clone()));
    Ok(())
}

fn load_snapshot(refresh_remote: bool) -> Result<ExtensionSourceSnapshot, Box<dyn Error>> {
    let mut result = ExtensionSourceSnapshot::default();
    let mut plugins = HashMap::<String, PluginCatalogItem>::new();
    let mut skills = HashMap::<String, SkillCatalogItem>::new();
    let mut workflows = HashMap::<String, WorkflowCatalogItem>::new();
    let mut experts = HashMap::<String, ExpertCatalogItem>::new();
    let mut feature_packs = HashMap::<String, ExtensionFeaturePack>::new();
    let mut agent_presets = HashMap::<String, ExtensionAgentPreset>::new();
    let mut conflicts = Vec::<(String, String, String)>::new();
    let mut source_catalogs = HashMap::<String, SourceCatalogAssets>::new();
    let mut settings = settings()?;
    // 添加来源时用户只能填仓库、分支和清单路径，通道与目录身份只能从远端清单读回来。
    // 这里纠正一次并落盘，本地源码源与 GitHub 发布源才会合并成同一个分发单元。
    if adopt_github_distribution_identities(&mut settings, refresh_remote) {
        save_settings(&settings)?;
    }
    let acquisitions = settings.acquisitions.clone();
    let enabled = settings
        .sources
        .iter()
        .filter(|item| item.enabled)
        .cloned()
        .collect::<Vec<_>>();
    let unit_of = unit_source_ids(&enabled);
    let same_unit = |left: &str, right: &str| match (unit_of.get(left), unit_of.get(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    };
    for source in ordered_sources(enabled.clone(), &acquisitions) {
        // 本地源目录读取是廉价且确定的，每次都按磁盘重读，保证开发者刚改完就能
        // 被取用；网络刷新只由 refresh_snapshot 触发，避免打开页面卡在 GitHub 可用性上。
        let refresh = refresh_remote || source.kind == ExtensionSourceKind::Local;
        let (catalog, using_cache, error) = if refresh {
            match fetch_catalog(&source) {
                Ok(catalog) => {
                    cache_catalog(&source, &catalog);
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
                    Err(cache_error) => {
                        (None, false, format!("{error}; 缓存读取失败: {cache_error}"))
                    }
                },
            }
        } else {
            // 远端源沿用缓存，网络刷新属于 refresh_snapshot 与后台对账。
            match load_cached_catalog(&source.id) {
                Ok(Some(catalog)) => match validate_catalog(&catalog, &source) {
                    Ok(()) => (Some(catalog), true, String::new()),
                    Err(cache_error) => (None, false, format!("缓存不再可信: {cache_error}")),
                },
                Ok(None) => (None, false, "扩展源尚未同步".to_string()),
                Err(cache_error) => (None, false, format!("缓存读取失败: {cache_error}")),
            }
        };
        let local_revision = if source.kind == ExtensionSourceKind::Local {
            local_source_revision(Path::new(&source.repository))
        } else {
            (String::new(), String::new(), false)
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
            workflow_count: 0,
            expert_count: 0,
            generation: String::new(),
            using_cache,
            error,
            versions: Vec::new(),
            source_commit: local_revision.0,
            source_tree: local_revision.1,
            source_dirty: local_revision.2,
            notices: Vec::new(),
        };
        if let Some(mut catalog) = catalog {
            status.plugin_count = catalog.plugins.len();
            status.skill_count = catalog.skills.len();
            status.workflow_count = catalog.workflows.len();
            status.expert_count = catalog.experts.len();
            status.generation = catalog.generation.clone();
            status.versions = catalog
                .plugins
                .iter()
                .map(|item| ExtensionSourceVersion {
                    asset_kind: "plugin".to_string(),
                    asset_id: item.plugin_id.clone(),
                    version: item.version.clone(),
                })
                .chain(catalog.skills.iter().map(|item| ExtensionSourceVersion {
                    asset_kind: "skill".to_string(),
                    asset_id: item.skill_id.clone(),
                    version: item.version.clone(),
                }))
                .chain(catalog.workflows.iter().map(|item| ExtensionSourceVersion {
                    asset_kind: "workflow".to_string(),
                    asset_id: item.workflow_id.clone(),
                    version: item.version.clone(),
                }))
                .chain(catalog.experts.iter().map(|item| ExtensionSourceVersion {
                    asset_kind: "expert".to_string(),
                    asset_id: item.expert_id.clone(),
                    version: item.version.clone(),
                }))
                .collect();
            let identity = source_identity(&source);
            let mut assets = SourceCatalogAssets::default();
            for item in &mut catalog.plugins {
                normalize_plugin_item(item, &source)?;
                assets.plugins.push((
                    item.plugin_id.clone(),
                    item.name.clone(),
                    item.version.clone(),
                ));
                assets.asset_details.insert(
                    format!("plugin:{}", item.plugin_id),
                    ExtensionUnitAsset {
                        asset_kind: "plugin".to_string(),
                        asset_id: item.plugin_id.clone(),
                        name: item.name.clone(),
                        version: item.version.clone(),
                        source_id: source.id.clone(),
                        source_kind: match source.kind {
                            ExtensionSourceKind::Local => "local".to_string(),
                            ExtensionSourceKind::Github => "github".to_string(),
                        },
                        artifact_url: item.download_url.clone(),
                        sha256: item.sha256.clone(),
                        signature_key_id: item.signature_key_id.clone(),
                        signature_algorithm: item.signature_algorithm.clone(),
                        channel: item.channel.clone(),
                    },
                );
                result.plugin_versions.push(item.clone());
                let existing = plugins
                    .get(&item.plugin_id)
                    .map(|value| (value.source.clone(), value.version.clone()));
                match existing {
                    Some((existing_source, _)) if existing_source != item.source => {
                        if same_unit(&item.source, &existing_source) {
                            // 同一分发单元内已按取用模式排好序，落选侧静默让位。
                        } else if source_outranks(&item.source, &existing_source) {
                            conflicts.push(conflict(
                                source_id_of(&existing_source),
                                "插件",
                                &item.plugin_id,
                                &item.source,
                            ));
                            plugins.insert(item.plugin_id.clone(), item.clone());
                        } else {
                            conflicts.push(conflict(
                                &source.id,
                                "插件",
                                &item.plugin_id,
                                &existing_source,
                            ));
                        }
                    }
                    Some((_, existing_version)) => {
                        if crate::skill::resolver::compare_versions(
                            &item.version,
                            &existing_version,
                        ) == std::cmp::Ordering::Greater
                        {
                            plugins.insert(item.plugin_id.clone(), item.clone());
                        }
                    }
                    None => {
                        plugins.insert(item.plugin_id.clone(), item.clone());
                    }
                }
            }
            for item in &mut catalog.skills {
                normalize_skill_item(item, &source)?;
                assets.skills.push((
                    item.skill_id.clone(),
                    item.name.clone(),
                    item.version.clone(),
                ));
                assets.asset_details.insert(
                    format!("skill:{}", item.skill_id),
                    ExtensionUnitAsset {
                        asset_kind: "skill".to_string(),
                        asset_id: item.skill_id.clone(),
                        name: item.name.clone(),
                        version: item.version.clone(),
                        source_id: source.id.clone(),
                        source_kind: match source.kind {
                            ExtensionSourceKind::Local => "local".to_string(),
                            ExtensionSourceKind::Github => "github".to_string(),
                        },
                        artifact_url: item.download_url.clone(),
                        sha256: item.sha256.clone(),
                        signature_key_id: item.signature_key_id.clone(),
                        signature_algorithm: item.signature_algorithm.clone(),
                        channel: item.channel.clone(),
                    },
                );
                result.skill_versions.push(item.clone());
                let existing = skills
                    .get(&item.skill_id)
                    .map(|value| (value.source.clone(), value.version.clone()));
                match existing {
                    Some((existing_source, _)) if existing_source != item.source => {
                        if same_unit(&item.source, &existing_source) {
                            // 同一分发单元内已按取用模式排好序，落选侧静默让位。
                        } else if source_outranks(&item.source, &existing_source) {
                            conflicts.push(conflict(
                                source_id_of(&existing_source),
                                "Skill",
                                &item.skill_id,
                                &item.source,
                            ));
                            skills.insert(item.skill_id.clone(), item.clone());
                        } else {
                            conflicts.push(conflict(
                                &source.id,
                                "Skill",
                                &item.skill_id,
                                &existing_source,
                            ));
                        }
                    }
                    Some((_, existing_version)) => {
                        if crate::skill::resolver::compare_versions(
                            &item.version,
                            &existing_version,
                        ) == std::cmp::Ordering::Greater
                        {
                            skills.insert(item.skill_id.clone(), item.clone());
                        }
                    }
                    None => {
                        skills.insert(item.skill_id.clone(), item.clone());
                    }
                }
            }
            for item in &mut catalog.workflows {
                normalize_workflow_item(item, &source)?;
                assets.workflows.push((
                    item.workflow_id.clone(),
                    item.name.clone(),
                    item.version.clone(),
                ));
                assets.asset_details.insert(
                    format!("workflow:{}", item.workflow_id),
                    ExtensionUnitAsset {
                        asset_kind: "workflow".to_string(),
                        asset_id: item.workflow_id.clone(),
                        name: item.name.clone(),
                        version: item.version.clone(),
                        source_id: source.id.clone(),
                        source_kind: match source.kind {
                            ExtensionSourceKind::Local => "local".to_string(),
                            ExtensionSourceKind::Github => "github".to_string(),
                        },
                        artifact_url: item.download_url.clone(),
                        sha256: item.sha256.clone(),
                        signature_key_id: item.signature_key_id.clone(),
                        signature_algorithm: item.signature_algorithm.clone(),
                        channel: item.channel.clone(),
                    },
                );
                result.workflow_versions.push(item.clone());
                let existing = workflows
                    .get(&item.workflow_id)
                    .map(|value| (value.source.clone(), value.version.clone()));
                match existing {
                    Some((existing_source, _)) if existing_source != item.source => {
                        if same_unit(&item.source, &existing_source) {
                            // 同一分发单元内已按取用模式排好序，落选侧静默让位。
                        } else if source_outranks(&item.source, &existing_source) {
                            conflicts.push(conflict(
                                source_id_of(&existing_source),
                                "Workflow",
                                &item.workflow_id,
                                &item.source,
                            ));
                            workflows.insert(item.workflow_id.clone(), item.clone());
                        } else {
                            conflicts.push(conflict(
                                &source.id,
                                "Workflow",
                                &item.workflow_id,
                                &existing_source,
                            ));
                        }
                    }
                    Some((_, existing_version)) => {
                        if crate::skill::resolver::compare_versions(
                            &item.version,
                            &existing_version,
                        ) == std::cmp::Ordering::Greater
                        {
                            workflows.insert(item.workflow_id.clone(), item.clone());
                        }
                    }
                    None => {
                        workflows.insert(item.workflow_id.clone(), item.clone());
                    }
                }
            }
            for item in &mut catalog.experts {
                normalize_expert_item(item, &source)?;
                assets.experts.push((
                    item.expert_id.clone(),
                    item.name.clone(),
                    item.version.clone(),
                ));
                assets.asset_details.insert(
                    format!("expert:{}", item.expert_id),
                    ExtensionUnitAsset {
                        asset_kind: "expert".to_string(),
                        asset_id: item.expert_id.clone(),
                        name: item.name.clone(),
                        version: item.version.clone(),
                        source_id: source.id.clone(),
                        source_kind: match source.kind {
                            ExtensionSourceKind::Local => "local".to_string(),
                            ExtensionSourceKind::Github => "github".to_string(),
                        },
                        artifact_url: item.download_url.clone(),
                        sha256: item.sha256.clone(),
                        signature_key_id: item.signature_key_id.clone(),
                        signature_algorithm: item.signature_algorithm.clone(),
                        channel: String::new(),
                    },
                );
                let existing = experts
                    .get(&item.expert_id)
                    .map(|value| (value.source.clone(), value.version.clone()));
                match existing {
                    Some((existing_source, _)) if existing_source != item.source => {
                        if same_unit(&item.source, &existing_source) {
                        } else if source_outranks(&item.source, &existing_source) {
                            conflicts.push(conflict(
                                source_id_of(&existing_source),
                                "专家",
                                &item.expert_id,
                                &item.source,
                            ));
                            experts.insert(item.expert_id.clone(), item.clone());
                        } else {
                            conflicts.push(conflict(
                                &source.id,
                                "专家",
                                &item.expert_id,
                                &existing_source,
                            ));
                        }
                    }
                    Some((_, existing_version)) => {
                        if crate::skill::resolver::compare_versions(
                            &item.version,
                            &existing_version,
                        ) == std::cmp::Ordering::Greater
                        {
                            experts.insert(item.expert_id.clone(), item.clone());
                        }
                    }
                    None => {
                        experts.insert(item.expert_id.clone(), item.clone());
                    }
                }
            }
            for mut pack in catalog.feature_packs {
                validate_feature_pack(&pack)?;
                pack.source_id = source.id.clone();
                let pack_identity = source_identity(&source);
                let existing = feature_packs
                    .get(&pack.id)
                    .map(|value| value.source_id.clone());
                match existing {
                    Some(existing_source) if existing_source != pack.source_id => {
                        let existing_identity = format!("github:{existing_source}");
                        if same_unit(&pack_identity, &existing_identity) {
                            // 同一分发单元内已按取用模式排好序，落选侧静默让位。
                        } else if source_outranks(&pack_identity, &existing_identity) {
                            conflicts.push(conflict(
                                &existing_source,
                                "功能包",
                                &pack.id,
                                &pack_identity,
                            ));
                            feature_packs.insert(pack.id.clone(), pack);
                        } else {
                            conflicts.push(conflict(
                                &pack.source_id,
                                "功能包",
                                &pack.id,
                                &existing_identity,
                            ));
                        }
                    }
                    Some(_) => {}
                    None => {
                        feature_packs.insert(pack.id.clone(), pack);
                    }
                }
            }
            for item in catalog.agent_presets {
                validate_agent_preset(&item)?;
                let identity = source_identity(&source);
                let normalized = ExtensionAgentPreset {
                    preset_id: item.preset_id,
                    name: item.name,
                    description: item.description,
                    version: item.version,
                    path: item.path,
                    sha256: item.sha256,
                    source: identity.clone(),
                };
                let existing = agent_presets
                    .get(&normalized.preset_id)
                    .map(|value| value.source.clone());
                match existing {
                    Some(existing_source) if existing_source != normalized.source => {
                        if same_unit(&identity, &existing_source) {
                            // 同一分发单元内已按取用模式排好序，落选侧静默让位。
                        } else if source_outranks(&identity, &existing_source) {
                            conflicts.push(conflict(
                                source_id_of(&existing_source),
                                "DSH preset",
                                &normalized.preset_id,
                                &identity,
                            ));
                            agent_presets.insert(normalized.preset_id.clone(), normalized);
                        } else {
                            conflicts.push(conflict(
                                &source.id,
                                "DSH preset",
                                &normalized.preset_id,
                                &existing_source,
                            ));
                        }
                    }
                    Some(_) => {}
                    None => {
                        agent_presets.insert(normalized.preset_id.clone(), normalized);
                    }
                }
            }
            source_catalogs.insert(identity, assets);
        }
        result.sources.push(status);
    }
    for (loser_source_id, reason, item) in conflicts {
        if let Some(status) = result
            .sources
            .iter_mut()
            .find(|status| status.source.id == loser_source_id)
        {
            match status
                .notices
                .iter_mut()
                .find(|notice| notice.reason == reason)
            {
                Some(notice) => notice.items.push(item),
                None => status.notices.push(ExtensionSourceNotice {
                    reason,
                    items: vec![item],
                }),
            }
        }
    }
    // 停用的来源也要保留单元行，否则界面上会直接消失、无法重新启用。
    let mut units = build_units(&settings.sources, &acquisitions, &source_catalogs);
    attach_unit_installations(&mut units);
    attach_unit_projects(&mut units);
    result.units = units;
    result.plugins = plugins.into_values().collect();
    result.skills = skills.into_values().collect();
    result.workflows = workflows.into_values().collect();
    result.experts = experts.into_values().collect();
    result.feature_packs = feature_packs.into_values().collect();
    result.agent_presets = agent_presets.into_values().collect();
    result
        .plugins
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    result
        .skills
        .sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    result
        .workflows
        .sort_by(|left, right| left.workflow_id.cmp(&right.workflow_id));
    result
        .experts
        .sort_by(|left, right| left.expert_id.cmp(&right.expert_id));
    result
        .feature_packs
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .agent_presets
        .sort_by(|left, right| left.preset_id.cmp(&right.preset_id));
    sort_plugin_versions(&mut result.plugin_versions);
    sort_skill_versions(&mut result.skill_versions);
    sort_workflow_versions(&mut result.workflow_versions);
    Ok(result)
}

/// 反查每个分发单元的安装情况，让界面能回答两个不同的问题：
/// 「本机现在有没有这个制品」和「它是不是来自本单元的取用侧」。
///
/// 只用 `extension.lock` 的 `source_id` 判断会产生严重的漏报：本地文件安装会写
/// `local`、手工导入写 `adhoc`，这些都是「安装方式」而不是来源 ID，无法归属到单元。
/// 因此这里以本机资产注册表（插件/技能/工作流）作为「已安装」的事实来源，
/// 产权归属仍按台账判断，归属不上的记为 `foreign`（与结构体注释的定义一致）。
fn attach_unit_installations(units: &mut [ExtensionDistributionUnit]) {
    let lock = crate::app::extension_lock::load().unwrap_or_default();
    let development_plugins = crate::capability::plugin::development_plugin_entries();
    let development_skills = crate::skill::development::entries();
    // 注册表口径与「插件」「技能」「工作流」页一致，避免同一事实在不同页面显示不同。
    let installed_plugins = installed_plugin_versions();
    let installed_skills = installed_skill_versions();
    let installed_workflows = installed_workflow_versions();
    let installed_experts = installed_expert_versions();
    for unit in units.iter_mut() {
        let unit_plugins = unit.plugin_ids.clone();
        let unit_skills = unit.skill_ids.clone();
        let unit_workflows = unit.workflow_ids.clone();
        for entry in lock.entries.values() {
            let side = if Some(&entry.source_id) == unit.local_source_id.as_ref() {
                "local"
            } else if Some(&entry.source_id) == unit.remote_source_id.as_ref() {
                "remote"
            } else {
                continue;
            };
            unit.installed.push(ExtensionUnitInstallation {
                asset_kind: entry.asset_kind.clone(),
                asset_id: entry.asset_id.clone(),
                version: entry.version.clone(),
                source_id: entry.source_id.clone(),
                sha256: entry.sha256.clone(),
                side: side.to_string(),
            });
        }
        // 已安装但无法归属到本单元的制品：补上版本，标记为另一来源。
        for plugin_id in &unit_plugins {
            let Some(version) = installed_plugins.get(plugin_id) else {
                continue;
            };
            unit.installed
                .push(foreign_installation("plugin", plugin_id, version));
        }
        for skill_id in &unit_skills {
            let Some(version) = installed_skills.get(skill_id) else {
                continue;
            };
            unit.installed
                .push(foreign_installation("skill", skill_id, version));
        }
        for workflow_id in &unit_workflows {
            let Some(version) = installed_workflows.get(workflow_id) else {
                continue;
            };
            unit.installed
                .push(foreign_installation("workflow", workflow_id, version));
        }
        for expert_id in &unit.expert_ids {
            if let Some(version) = installed_experts.get(expert_id) {
                unit.installed
                    .push(foreign_installation("expert", expert_id, version));
            }
        }
        for (plugin_id, path) in &development_plugins {
            if !unit_plugins.contains(plugin_id) {
                continue;
            }
            unit.installed.push(ExtensionUnitInstallation {
                asset_kind: "plugin".to_string(),
                asset_id: plugin_id.clone(),
                version: plugin_manifest_version(path).unwrap_or_default(),
                source_id: "development".to_string(),
                sha256: String::new(),
                side: "development".to_string(),
            });
        }
        for (skill_id, path) in &development_skills {
            if !unit_skills.contains(skill_id) {
                continue;
            }
            unit.installed.push(ExtensionUnitInstallation {
                asset_kind: "skill".to_string(),
                asset_id: skill_id.clone(),
                version: skill_manifest_version(path).unwrap_or_default(),
                source_id: "development".to_string(),
                sha256: String::new(),
                side: "development".to_string(),
            });
        }
        unit.installed.sort_by(|left, right| {
            (
                left.asset_kind.as_str(),
                left.asset_id.as_str(),
                side_priority(&left.side),
            )
                .cmp(&(
                    right.asset_kind.as_str(),
                    right.asset_id.as_str(),
                    side_priority(&right.side),
                ))
        });
        unit.installed.dedup_by(|left, right| {
            left.asset_kind == right.asset_kind && left.asset_id == right.asset_id
        });
    }
}

fn foreign_installation(kind: &str, id: &str, version: &str) -> ExtensionUnitInstallation {
    ExtensionUnitInstallation {
        asset_kind: kind.to_string(),
        asset_id: id.to_string(),
        version: version.to_string(),
        source_id: String::new(),
        sha256: String::new(),
        side: "foreign".to_string(),
    }
}

/// 本机已安装的插件版本（与「插件」页同源）。
fn installed_plugin_versions() -> BTreeMap<String, String> {
    crate::capability::plugin::scan_plugins()
        .unwrap_or_default()
        .into_iter()
        .map(|item| (item.id, item.version))
        .collect()
}

/// 本机已安装的技能版本（与「技能」页同源）。
fn installed_skill_versions() -> BTreeMap<String, String> {
    crate::skill::store::SkillStore::new()
        .list_records()
        .unwrap_or_default()
        .into_iter()
        .map(|record| (record.manifest.id, record.manifest.version))
        .collect()
}

/// 本机已安装的工作流版本（与「工作流」页同源）。
fn installed_workflow_versions() -> BTreeMap<String, String> {
    crate::workflow::WorkflowStore::open_default()
        .and_then(|store| store.list())
        .unwrap_or_default()
        .into_iter()
        .map(|installed| (installed.package.id, installed.package.version))
        .collect()
}

fn installed_expert_versions() -> BTreeMap<String, String> {
    crate::expert::list()
        .unwrap_or_default()
        .into_iter()
        .map(|item| (item.id, item.version))
        .collect()
}

/// 开发直挂是运行时最高优先级的生效来源，排序时排在安装台账之前。
fn side_priority(side: &str) -> u8 {
    match side {
        "development" => 0,
        "local" => 1,
        "remote" => 2,
        // 归属不明的本机安装排最后：它只用于「本机是否有」的计数，不代表来源。
        _ => 3,
    }
}

fn plugin_manifest_version(root: &Path) -> Option<String> {
    let content = fs::read_to_string(root.join("plugin.json")).ok()?;
    crate::capability::plugin::parse_plugin_manifest(content.trim_start_matches('\u{feff}'))
        .ok()
        .map(|manifest| manifest.version)
}

fn skill_manifest_version(root: &Path) -> Option<String> {
    let content = fs::read_to_string(root.join("skill.json")).ok()?;
    serde_json::from_str::<crate::skill::types::SkillManifest>(
        content.trim_start_matches('\u{feff}'),
    )
    .ok()
    .map(|manifest| manifest.version)
}

fn snapshot_cache() -> &'static Mutex<Option<(Instant, ExtensionSourceSnapshot)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, ExtensionSourceSnapshot)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 只允许一个线程同时重建扩展源快照，避免并发重写同一份来源缓存（见
/// `snapshot_with_cache`）。
fn snapshot_rebuild_gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
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

pub(crate) fn workflow_versions(
    workflow_id: &str,
) -> Result<Vec<WorkflowCatalogItem>, Box<dyn Error>> {
    Ok(snapshot()?
        .workflow_versions
        .into_iter()
        .filter(|item| item.workflow_id == workflow_id)
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

fn sort_workflow_versions(items: &mut [WorkflowCatalogItem]) {
    items.sort_by(|left, right| {
        left.workflow_id
            .cmp(&right.workflow_id)
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
        // 原子写会留一份 `.json.bak`，格式与正文完全一样；不排掉就会出现
        // 同一个资产的重复来源记录。
        if !entry.path().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
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
    install_plugin_bound(plugin_id, version, None, None)
}

pub(crate) fn install_plugin_bound(
    plugin_id: &str,
    version: Option<&str>,
    expected_source_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let plugin_versions = snapshot.plugin_versions.clone();
    let selected_source = plugin_versions
        .iter()
        .find(|item| {
            item.plugin_id == plugin_id
                && version.map(|value| value == item.version).unwrap_or(true)
                && expected_source_id
                    .map(|source_id| item.source.ends_with(&format!(":{source_id}")))
                    .unwrap_or(true)
        })
        .map(|item| item.source.clone())
        .ok_or_else(|| format!("扩展源中未找到插件: {plugin_id}"))?;
    let mut order = Vec::new();
    let mut visiting = HashSet::new();
    resolve_plugin_order(
        &plugin_versions,
        plugin_id,
        version,
        &selected_source,
        &mut visiting,
        &mut order,
    )?;
    if let Some(root) = order.iter().find(|item| item.plugin_id == plugin_id) {
        if let Some(source_id) = expected_source_id {
            if !root.source.ends_with(&format!(":{source_id}")) {
                return Err(format!("插件 {} 未匹配请求来源 {}", plugin_id, source_id).into());
            }
        }
        if let Some(sha256) = expected_sha256.filter(|value| !value.trim().is_empty()) {
            if !root.sha256.eq_ignore_ascii_case(sha256) {
                return Err(format!("插件 {} 制品摘要与请求不一致", plugin_id).into());
            }
        }
    }
    let mut changes = Vec::new();
    let mut reference_changes = Vec::new();
    let mut provenance_changes = Vec::new();
    let mut lock_changes = Vec::new();
    for item in &order {
        let before = crate::app::plugin_manager::local_status(&item.plugin_id);
        let previous_lock = crate::app::extension_lock::read("plugin", &item.plugin_id)?;
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
        // 版本相同不足以跳过：只有已装的那一份确实出自本次目标来源时才认为
        // 无需重装。用户在来源管理里切换取用侧后再点安装，就是要覆盖制品并把
        // 来源记录改过来；这里若只看版本，界面提示的「重新安装会切换来源」会
        // 变成一句做不到的承诺。
        // 本地目录源的 catalog 项没有制品摘要（sha256 恒为空），「版本相同」也就
        // 代表不了「内容相同」：同一个 1.2.0 里改过 UI，磁盘上留着的还是上一份，
        // 界面却会报安装成功。技能与工作流的安装本就不做这层跳过，插件这里对齐。
        if source.kind != ExtensionSourceKind::Local
            && before.current_version == item.version
            && before.enabled
            && installed_provenance_matches(
                "plugin",
                &item.plugin_id,
                source,
                &item.version,
                &item.sha256,
            )
        {
            continue;
        }
        lock_changes.push(("plugin".to_string(), item.plugin_id.clone(), previous_lock));
        let install_result = if source.kind == ExtensionSourceKind::Local {
            let dir = local_item_dir(&item.download_url)?;
            crate::app::plugin_manager::install_local_package_from_source(
                &dir,
                &source_identity(source),
            )
        } else {
            crate::app::plugin_manager::install_public_catalog_item(
                item,
                source.verification.requires_signature(),
            )
        };
        if let Err(error) = install_result {
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

/// 本机装着的这一份，是不是就是 `source` 这次要装的东西。
///
/// 判断依据是来源记录：来源 ID 和版本都要对得上，目录里给了摘要时还要对得上
/// 摘要（本地来源不记摘要，跳过这一项）。记录缺失或读不出来时一律返回 false，
/// 走正常安装，避免"文件坏了但记录还在"时被误判成已是最新。
fn installed_provenance_matches(
    kind: &str,
    key: &str,
    source: &ExtensionSourceConfig,
    version: &str,
    sha256: &str,
) -> bool {
    let Ok(Some(record)) = read_provenance(kind, key) else {
        return false;
    };
    record.source_id == source.id
        && record.version == version
        && (sha256.trim().is_empty() || record.sha256.eq_ignore_ascii_case(sha256))
}

/// 删除某个资产的本机来源记录（含原子写留下的 `.bak` 备份）。
///
/// 记录的意义是"已装的这一份是从哪个扩展源的哪个制品来的"，资产被卸载之后
/// 它就直接过期了：留着不但占位，还会让自动更新把已经卸掉的资产当成待更新项。
pub(crate) fn remove_provenance(kind: &str, key: &str) {
    remove_provenance_at(&paths::agent_home().join("data"), kind, key);
}

/// 在指定状态根上删除来源记录。退役与卸载清理跑在 `SkillStore` 自己的状态根上，
/// 用全局 `agent_home` 会在测试或非默认 profile 里删到别人的记录。
pub(crate) fn remove_provenance_at(state_root: &Path, kind: &str, key: &str) {
    let Ok(path) = provenance_path_at(state_root, kind, key) else {
        return;
    };
    let _ = fs::remove_file(atomic_file::backup_path(&path));
    let _ = fs::remove_file(&path);
}

/// 删除某个扩展源留下的全部来源记录，用于移除扩展源时收尾。
pub(crate) fn remove_provenance_for_source(source_id: &str) {
    let Ok(records) = list_provenance() else {
        return;
    };
    for record in records {
        if record.source_id == source_id {
            remove_provenance(&record.asset_kind, &record.asset_key);
        }
    }
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
    plan_plugin_bound(plugin_id, version, None, None)
}

pub(crate) fn plan_plugin_bound(
    plugin_id: &str,
    version: Option<&str>,
    expected_source_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<crate::app::plugin_manager::PluginInstallPlan, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let plugin_versions = snapshot.plugin_versions.clone();
    let selected_source = plugin_versions
        .iter()
        .find(|item| {
            item.plugin_id == plugin_id
                && version.map(|value| value == item.version).unwrap_or(true)
                && expected_source_id
                    .map(|source_id| item.source.ends_with(&format!(":{source_id}")))
                    .unwrap_or(true)
        })
        .map(|item| item.source.clone())
        .ok_or_else(|| format!("扩展源中未找到插件: {plugin_id}"))?;
    let mut order = Vec::new();
    resolve_plugin_order(
        &plugin_versions,
        plugin_id,
        version,
        &selected_source,
        &mut HashSet::new(),
        &mut order,
    )?;
    let plugin = order
        .iter()
        .find(|item| item.plugin_id == plugin_id)
        .cloned()
        .ok_or("扩展源中未找到插件")?;
    if let Some(source_id) = expected_source_id {
        if !plugin.source.ends_with(&format!(":{source_id}")) {
            return Err(format!("插件 {} 未匹配请求来源 {}", plugin_id, source_id).into());
        }
    }
    if let Some(sha256) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        if !plugin.sha256.eq_ignore_ascii_case(sha256) {
            return Err(format!("插件 {} 制品摘要与请求不一致", plugin_id).into());
        }
    }
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
                reason: "扩展源依赖".to_string(),
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
    install_skill_bound(skill_id, version, None, None)
}

pub(crate) fn install_skill_bound(
    skill_id: &str,
    version: Option<&str>,
    expected_source_id: Option<&str>,
    expected_sha256: Option<&str>,
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
                && expected_source_id
                    .map(|source_id| item.source.ends_with(&format!(":{source_id}")))
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到 Skill: {skill_id}"))?;
    if let Some(sha256) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        if !item.sha256.eq_ignore_ascii_case(sha256) {
            return Err(format!("Skill {} 制品摘要与请求不一致", skill_id).into());
        }
    }
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
        if let Err(error) =
            install_plugin_bound(&dependency.plugin_id, None, Some(&source.id), None)
        {
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
    let record = match install_skill_catalog_item(&item, source) {
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

pub(crate) fn install_workflow(
    workflow_id: &str,
    version: Option<&str>,
) -> Result<(WorkflowCatalogItem, crate::workflow::InstalledWorkflow), Box<dyn Error>> {
    install_workflow_bound(workflow_id, version, None, None)
}

pub(crate) fn install_workflow_bound(
    workflow_id: &str,
    version: Option<&str>,
    expected_source_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<(WorkflowCatalogItem, crate::workflow::InstalledWorkflow), Box<dyn Error>> {
    let snapshot = snapshot()?;
    let item = snapshot
        .workflow_versions
        .iter()
        .find(|item| {
            item.workflow_id == workflow_id
                && version
                    .map(|requested| requested == item.version)
                    .unwrap_or(true)
                && expected_source_id
                    .map(|source_id| item.source.ends_with(&format!(":{source_id}")))
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到 Workflow: {workflow_id}"))?;
    if let Some(sha256) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        if !item.sha256.eq_ignore_ascii_case(sha256) {
            return Err(format!("Workflow {} 制品摘要与请求不一致", workflow_id).into());
        }
    }
    let source = source_for_catalog_item(&snapshot, &item.source)?;
    let store = crate::workflow::WorkflowStore::open_default()?;
    let previous_installation = store
        .list()?
        .into_iter()
        .find(|installed| installed.package.id == item.workflow_id);
    let previous_provenance = read_provenance("workflow", &item.workflow_id)?;
    let require_signature = source.verification.requires_signature();
    let installed = if source.kind == ExtensionSourceKind::Local {
        crate::app::workflow_manager::install_local_catalog_item(&item, require_signature)?
    } else {
        crate::app::workflow_manager::install_public_catalog_item(&item, require_signature)?
    };
    if let Err(error) = save_provenance(
        source,
        "workflow",
        &item.workflow_id,
        &item.version,
        &item.download_url,
        &item.sha256,
        &item.signature_key_id,
    ) {
        compensate_workflow_installation(&store, &item.workflow_id, previous_installation.as_ref());
        return Err(error);
    }
    if let Err(error) = crate::app::extension_lock::record_source_workflow(source, &item) {
        restore_provenance_changes(&[(
            "workflow".to_string(),
            item.workflow_id.clone(),
            previous_provenance,
        )]);
        compensate_workflow_installation(&store, &item.workflow_id, previous_installation.as_ref());
        return Err(error);
    }
    Ok((item, installed))
}

fn compensate_workflow_installation(
    store: &crate::workflow::WorkflowStore,
    workflow_id: &str,
    previous: Option<&crate::workflow::InstalledWorkflow>,
) {
    let current = store.list().ok().and_then(|items| {
        items
            .into_iter()
            .find(|item| item.package.id == workflow_id)
    });
    match (previous, current) {
        (Some(previous), Some(current)) if previous.package.version != current.package.version => {
            let _ = store.rollback(workflow_id);
        }
        (None, Some(_)) => {
            let _ = store.remove(workflow_id);
        }
        _ => {}
    }
}

/// 只切换某个来源的启用状态，保留它已有的自动更新与校验策略。
///
/// CLI 与 UI 共用同一条更新路径，避免脚本化验收时出现“命令行改了配置但没落盘”
/// 这类分叉。
pub(crate) fn set_source_enabled(
    source_id: &str,
    enabled: bool,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let auto_update = settings()?
        .sources
        .iter()
        .find(|item| item.id == source_id)
        .map(|item| item.auto_update)
        .ok_or("扩展源不存在")?;
    update_source(source_id, enabled, auto_update, None)
}

/// 切换某个分发单元的取用侧。`Local` 是默认值，显式选回本地时不落盘。
pub(crate) fn set_unit_acquisition(
    unit_key: &str,
    acquisition: ExtensionSourceAcquisition,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let path = settings_path();
    let updated = set_acquisition_at(&path, unit_key, acquisition)?;
    Ok(updated)
}

fn set_acquisition_at(
    path: &Path,
    unit_key: &str,
    acquisition: ExtensionSourceAcquisition,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let mut current = settings_at(path)?;
    let known = current
        .sources
        .iter()
        .map(unit_key_of)
        .collect::<HashSet<_>>();
    if !known.contains(unit_key) {
        return Err(format!("扩展分发单元不存在: {unit_key}").into());
    }
    match acquisition {
        // 本地是默认取用侧，落盘时不留冗余键。
        ExtensionSourceAcquisition::Local => {
            current.acquisitions.remove(unit_key);
        }
        ExtensionSourceAcquisition::Remote => {
            current
                .acquisitions
                .insert(unit_key.to_string(), acquisition);
        }
    }
    atomic_file::atomic_write(
        path,
        &serde_json::to_vec_pretty(&persisted_settings(&current))?,
    )?;
    settings_at(path)
}

/// 设置某个分发单元的默认分发目标，`None` 表示回到继承（按清单声明或出厂默认）。
///
/// 只接受已登记单元的 `unit_key`，避免配置里出现无法归属的目标记录。`["workbench"]`
/// 既可能是「继承」，也可能是管理员对声明了 GitHub 的扩展做出的收窄，两者裁剪结果
/// 不同，因此显式选择一律落盘，只有 `inherit` 才删除记录。
pub(crate) fn set_unit_distribution_targets(
    unit_key: &str,
    targets: Option<&[DistributionTarget]>,
) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    let path = settings_path();
    let mut current = settings_at(&path)?;
    let known = current
        .sources
        .iter()
        .map(unit_key_of)
        .collect::<HashSet<_>>();
    if !known.contains(unit_key) {
        return Err(format!("扩展分发单元不存在: {unit_key}").into());
    }
    match targets {
        Some(targets) => {
            let normalized = normalize_distribution_targets(targets)?;
            current
                .distribution_targets
                .insert(unit_key.to_string(), normalized);
        }
        None => {
            current.distribution_targets.remove(unit_key);
        }
    }
    atomic_file::atomic_write(
        &path,
        &serde_json::to_vec_pretty(&persisted_settings(&current))?,
    )?;
    invalidate_snapshot_cache();
    settings_at(&path)
}

/// 读取分发单元显式设置的默认目标；未设置时返回 `None`，表示继承。
pub(crate) fn unit_distribution_targets_setting(
    settings: &ExtensionSourceSettings,
    unit_key: &str,
) -> Option<Vec<DistributionTarget>> {
    if unit_key.trim().is_empty() {
        return None;
    }
    settings
        .distribution_targets
        .get(unit_key)
        .filter(|targets| !targets.is_empty())
        .cloned()
}

/// 读取分发单元默认目标；未登记时回落到「仅工作台」。
pub(crate) fn unit_distribution_targets(
    settings: &ExtensionSourceSettings,
    unit_key: &str,
) -> Vec<DistributionTarget> {
    unit_distribution_targets_setting(settings, unit_key)
        .unwrap_or_else(crate::extension_contracts::default_distribution_targets)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionUnitInstallReport {
    pub unit_key: String,
    pub acquisition: ExtensionSourceAcquisition,
    pub plugins: Vec<ExtensionUnitAsset>,
    pub skills: Vec<ExtensionUnitAsset>,
    pub workflows: Vec<ExtensionUnitAsset>,
    #[serde(default)]
    pub experts: Vec<ExtensionUnitAsset>,
    pub errors: Vec<String>,
    pub failures: Vec<ExtensionUnitInstallFailure>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionUnitInstallFailure {
    pub asset_kind: String,
    pub asset_id: String,
    pub message: String,
    pub retryable: bool,
}

/// 按取用侧把整个分发单元安装/更新到本机。先完成全量预检，再写入制品。
pub(crate) fn install_unit(unit_key: &str) -> Result<ExtensionUnitInstallReport, Box<dyn Error>> {
    install_unit_bound(unit_key, None)
}

/// 安装分发单元的来源锁定入口。UI 会传入当前快照里的 source_id；旧的
/// CLI 调用可以省略该字段，但仍会拒绝不可用首选来源和混合来源制品。
pub(crate) fn install_unit_bound(
    unit_key: &str,
    expected_source_id: Option<&str>,
) -> Result<ExtensionUnitInstallReport, Box<dyn Error>> {
    // 本地取用侧每次都从磁盘重读，无需网络刷新；远端取用侧需要最新的发布目录。
    let snapshot = if acquisition_for(&settings()?.acquisitions, unit_key)
        == ExtensionSourceAcquisition::Local
    {
        snapshot()?
    } else {
        refresh_snapshot()?
    };
    let unit = snapshot
        .units
        .iter()
        .find(|unit| unit.unit_key == unit_key)
        .cloned()
        .ok_or_else(|| format!("扩展分发单元不存在: {unit_key}"))?;
    if unit.state != "ready" {
        return Err(format!("扩展分发单元当前不可安装: {}", unit.state).into());
    }
    let source_id = match unit.acquisition {
        ExtensionSourceAcquisition::Local => unit.local_source_id.clone(),
        ExtensionSourceAcquisition::Remote => unit.remote_source_id.clone(),
    }
    .ok_or_else(|| "当前取用模式没有可用来源".to_string())?;
    if let Some(expected) = expected_source_id.filter(|value| !value.trim().is_empty()) {
        if expected != source_id {
            return Err(format!(
                "扩展来源已变化，请刷新后重试（请求 {expected}，当前 {source_id}）"
            )
            .into());
        }
    }
    if unit.assets.iter().any(|asset| asset.source_id != source_id) {
        return Err("扩展单元包含未绑定当前来源的制品，已阻止安装".into());
    }
    // Installation mutates plugin/skill/workflow stores. Resolve every
    // artifact and dependency first so a bad item cannot leave a half-installed
    // distribution unit behind.
    preflight_unit_installation(&unit, &snapshot)?;
    let mut report = ExtensionUnitInstallReport {
        unit_key: unit.unit_key.clone(),
        acquisition: unit.acquisition.clone(),
        plugins: Vec::new(),
        skills: Vec::new(),
        workflows: Vec::new(),
        experts: Vec::new(),
        errors: Vec::new(),
        failures: Vec::new(),
        retryable: false,
    };
    for asset in &unit.assets {
        match asset.asset_kind.as_str() {
            "plugin" => match install_plugin_bound(
                &asset.asset_id,
                Some(&asset.version),
                Some(&asset.source_id),
                Some(&asset.sha256),
            ) {
                Ok(item) => report.plugins.push(ExtensionUnitAsset {
                    asset_kind: "plugin".to_string(),
                    asset_id: item.plugin_id,
                    name: item.name,
                    version: item.version,
                    source_id: asset.source_id.clone(),
                    source_kind: asset.source_kind.clone(),
                    artifact_url: asset.artifact_url.clone(),
                    sha256: asset.sha256.clone(),
                    signature_key_id: asset.signature_key_id.clone(),
                    signature_algorithm: asset.signature_algorithm.clone(),
                    channel: asset.channel.clone(),
                }),
                Err(error) => {
                    record_install_failure(&mut report, "plugin", &asset.asset_id, error, true)
                }
            },
            "skill" => match install_skill_bound(
                &asset.asset_id,
                Some(&asset.version),
                Some(&asset.source_id),
                Some(&asset.sha256),
            ) {
                Ok((item, _)) => report.skills.push(ExtensionUnitAsset {
                    asset_kind: "skill".to_string(),
                    asset_id: item.skill_id,
                    name: item.name,
                    version: item.version,
                    source_id: asset.source_id.clone(),
                    source_kind: asset.source_kind.clone(),
                    artifact_url: asset.artifact_url.clone(),
                    sha256: asset.sha256.clone(),
                    signature_key_id: asset.signature_key_id.clone(),
                    signature_algorithm: asset.signature_algorithm.clone(),
                    channel: asset.channel.clone(),
                }),
                Err(error) => {
                    record_install_failure(&mut report, "skill", &asset.asset_id, error, true)
                }
            },
            "workflow" => match install_workflow_bound(
                &asset.asset_id,
                Some(&asset.version),
                Some(&asset.source_id),
                Some(&asset.sha256),
            ) {
                Ok((item, _)) => report.workflows.push(ExtensionUnitAsset {
                    asset_kind: "workflow".to_string(),
                    asset_id: item.workflow_id,
                    name: item.name,
                    version: item.version,
                    source_id: asset.source_id.clone(),
                    source_kind: asset.source_kind.clone(),
                    artifact_url: asset.artifact_url.clone(),
                    sha256: asset.sha256.clone(),
                    signature_key_id: asset.signature_key_id.clone(),
                    signature_algorithm: asset.signature_algorithm.clone(),
                    channel: asset.channel.clone(),
                }),
                Err(error) => {
                    record_install_failure(&mut report, "workflow", &asset.asset_id, error, true)
                }
            },
            "expert" => {
                match install_expert_bound(&asset.asset_id, &asset.version, &asset.source_id) {
                    Ok(item) => report.experts.push(ExtensionUnitAsset {
                        asset_kind: "expert".to_string(),
                        asset_id: item.id,
                        name: item.name,
                        version: item.version,
                        source_id: asset.source_id.clone(),
                        source_kind: asset.source_kind.clone(),
                        artifact_url: asset.artifact_url.clone(),
                        sha256: asset.sha256.clone(),
                        signature_key_id: asset.signature_key_id.clone(),
                        signature_algorithm: asset.signature_algorithm.clone(),
                        channel: asset.channel.clone(),
                    }),
                    Err(error) => {
                        record_install_failure(&mut report, "expert", &asset.asset_id, error, true)
                    }
                }
            }
            other => record_install_failure(
                &mut report,
                other,
                &asset.asset_id,
                format!("不支持的扩展类型: {other}").into(),
                false,
            ),
        }
    }
    if !report.plugins.is_empty() {
        crate::capability::service::invalidate_capability_discovery();
    }
    Ok(report)
}

pub(crate) fn install_expert_bound(
    expert_id: &str,
    version: &str,
    source_id: &str,
) -> Result<crate::expert::ExpertSummary, Box<dyn Error>> {
    let settings = settings()?;
    let source = settings
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .ok_or("专家来源不存在")?;
    if source.kind != ExtensionSourceKind::Local {
        return Err("远端来源的专家请从市场安装".into());
    }
    let catalog = build_local_catalog(source)?;
    let item = catalog
        .experts
        .into_iter()
        .find(|item| item.expert_id == expert_id && item.version == version)
        .ok_or_else(|| format!("本地来源中未找到专家: {expert_id}@{version}"))?;
    let dir = local_item_dir(&item.download_url)?;
    let definition: crate::expert::ExpertDefinition =
        serde_json::from_slice(&fs::read(dir.join("expert.json"))?)?;
    crate::expert::install_local_definition(definition)
}

fn record_install_failure(
    report: &mut ExtensionUnitInstallReport,
    asset_kind: &str,
    asset_id: &str,
    error: Box<dyn Error>,
    retryable: bool,
) {
    let message = error.to_string();
    report.errors.push(format!(
        "{} {} 安装失败: {message}",
        match asset_kind {
            "plugin" => "插件",
            "skill" => "Skill",
            "workflow" => "Workflow",
            _ => "扩展",
        },
        asset_id
    ));
    report.failures.push(ExtensionUnitInstallFailure {
        asset_kind: asset_kind.to_string(),
        asset_id: asset_id.to_string(),
        message,
        retryable,
    });
    report.retryable |= retryable;
}

fn preflight_unit_installation(
    unit: &ExtensionDistributionUnit,
    snapshot: &ExtensionSourceSnapshot,
) -> Result<(), Box<dyn Error>> {
    for asset in &unit.assets {
        let source = snapshot
            .sources
            .iter()
            .map(|status| &status.source)
            .find(|source| source.id == asset.source_id)
            .ok_or_else(|| format!("制品 {} 的来源不存在", asset.asset_id))?;
        validate_asset_identity(&asset.asset_kind, &asset.asset_id)?;
        if source.kind == ExtensionSourceKind::Local {
            let dir = local_item_dir(&asset.artifact_url)?;
            let manifest = match asset.asset_kind.as_str() {
                "plugin" => "plugin.json",
                "skill" => "skill.json",
                "workflow" => "workflow.json",
                "expert" => "expert.json",
                other => return Err(format!("不支持的扩展类型: {other}").into()),
            };
            if !dir.join(manifest).is_file() {
                return Err(format!("本地扩展 {} 缺少 {}", asset.asset_id, manifest).into());
            }
        } else {
            validate_artifact(&source.repository, &asset.artifact_url, 1, &asset.sha256)?;
            // The complete signature was checked while loading the catalog;
            // keep the unit-level preflight strict about the metadata needed
            // by the installer without duplicating the catalog payload.
            if source.verification.requires_signature()
                && (asset.signature_key_id.trim().is_empty()
                    || asset.signature_algorithm.trim().is_empty())
            {
                return Err("远端制品缺少签名元数据".into());
            }
        }
        match asset.asset_kind.as_str() {
            "plugin" => {
                plan_plugin_bound(
                    &asset.asset_id,
                    Some(&asset.version),
                    Some(&asset.source_id),
                    Some(&asset.sha256),
                )?;
            }
            "skill" => {
                plan_skill_bound(
                    &asset.asset_id,
                    Some(&asset.version),
                    Some(&asset.source_id),
                    Some(&asset.sha256),
                )?;
            }
            "workflow" => {
                let workflow = snapshot
                    .workflow_versions
                    .iter()
                    .find(|item| {
                        item.workflow_id == asset.asset_id
                            && item.version == asset.version
                            && item.source.ends_with(&format!(":{}", asset.source_id))
                    })
                    .ok_or_else(|| format!("扩展源中未找到 Workflow: {}", asset.asset_id))?;
                if !asset.sha256.trim().is_empty()
                    && !workflow.sha256.eq_ignore_ascii_case(&asset.sha256)
                {
                    return Err(format!("Workflow {} 制品摘要与请求不一致", asset.asset_id).into());
                }
            }
            "expert" => {
                let expert = snapshot
                    .experts
                    .iter()
                    .find(|item| {
                        item.expert_id == asset.asset_id
                            && item.version == asset.version
                            && item.source.ends_with(&format!(":{}", asset.source_id))
                    })
                    .ok_or_else(|| format!("扩展源中未找到专家: {}", asset.asset_id))?;
                if !asset.sha256.trim().is_empty()
                    && !expert.sha256.eq_ignore_ascii_case(&asset.sha256)
                {
                    return Err(format!("专家 {} 制品摘要与请求不一致", asset.asset_id).into());
                }
            }
            other => return Err(format!("不支持的扩展类型: {other}").into()),
        }
    }
    Ok(())
}

pub(crate) fn plan_skill(
    skill_id: &str,
    version: Option<&str>,
) -> Result<crate::app::skill_manager::SkillInstallPlan, Box<dyn Error>> {
    plan_skill_bound(skill_id, version, None, None)
}

pub(crate) fn plan_skill_bound(
    skill_id: &str,
    version: Option<&str>,
    expected_source_id: Option<&str>,
    expected_sha256: Option<&str>,
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
                && expected_source_id
                    .map(|source_id| item.source.ends_with(&format!(":{source_id}")))
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("扩展源中未找到 Skill: {skill_id}"))?;
    if let Some(sha256) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        if !skill.sha256.eq_ignore_ascii_case(sha256) {
            return Err(format!("Skill {} 制品摘要与请求不一致", skill_id).into());
        }
    }
    let mut actions = Vec::new();
    let mut blocked_reasons = Vec::new();
    for dependency in &skill.plugin_dependencies {
        let local = crate::app::plugin_manager::local_status(&dependency.plugin_id);
        let source_item = snapshot
            .plugin_versions
            .iter()
            .find(|item| item.plugin_id == dependency.plugin_id && item.source == skill.source);
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
            reason: "扩展源依赖".to_string(),
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

pub(crate) fn plan_workflow(
    workflow_id: &str,
    version: Option<&str>,
) -> Result<WorkflowCatalogItem, Box<dyn Error>> {
    workflow_versions(workflow_id)?
        .into_iter()
        .find(|item| {
            version
                .map(|requested| requested == item.version)
                .unwrap_or(true)
        })
        .ok_or_else(|| format!("扩展源中未找到 Workflow: {workflow_id}").into())
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
            agent_preset_ids: Vec::new(),
            source_id: String::new(),
        });
    if !plugin_ready {
        if !snapshot
            .plugins
            .iter()
            .any(|item| item.plugin_id == AUTHORING_PLUGIN_ID)
        {
            return Err("扩展创作组件尚未安装，且已配置的扩展源未提供扩展开发工具".into());
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
        // 自动更新只消费远端 Release；本地工作区即使被标记为可用，也
        // 不能在后台覆盖开发者的 local_preview。
        .filter(|status| {
            status.source.enabled
                && status.source.auto_update
                && status.source.kind == ExtensionSourceKind::Github
        })
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
        // 溯源记录在卸载后会长期保留，因此不能把「来源里有记录」当成
        // 「本机仍然安装」。自动更新只覆盖已安装能力，否则来源一旦开启
        // 自动更新，用户主动卸载的扩展会在下次刷新时被自动装回来。
        if provenance.asset_kind == "plugin" {
            let Some(item) = snapshot.plugins.iter().find(|item| {
                item.plugin_id == provenance.asset_key
                    && item.source.ends_with(&format!(":{}", provenance.source_id))
            }) else {
                continue;
            };
            let local = crate::app::plugin_manager::local_status(&provenance.asset_key);
            if local.current_version.trim().is_empty() {
                continue;
            }
            if crate::skill::resolver::compare_versions(&item.version, &local.current_version)
                == std::cmp::Ordering::Greater
            {
                install_plugin(&provenance.asset_key, Some(&item.version))?;
                updated.push(format!("plugin:{}@{}", item.plugin_id, item.version));
            }
        } else if provenance.asset_kind == "skill" {
            let Some(item) = snapshot.skills.iter().find(|item| {
                item.skill_id == provenance.asset_key
                    && item.source.ends_with(&format!(":{}", provenance.source_id))
            }) else {
                continue;
            };
            let current = crate::skill::store::SkillStore::new()
                .get_record(&provenance.asset_key)?
                .map(|record| record.manifest.version)
                .unwrap_or_default();
            if current.trim().is_empty() {
                continue;
            }
            if crate::skill::resolver::compare_versions(&item.version, &current)
                == std::cmp::Ordering::Greater
            {
                let (_, record) = install_skill(&provenance.asset_key, Some(&item.version))?;
                crate::skill::sync_record_to_supported_clients(&record, crate::VERSION, &[])?;
                updated.push(format!("skill:{}@{}", item.skill_id, item.version));
            }
        } else if provenance.asset_kind == "workflow" {
            let Some(item) = snapshot.workflow_versions.iter().find(|item| {
                item.workflow_id == provenance.asset_key
                    && item.source.ends_with(&format!(":{}", provenance.source_id))
            }) else {
                continue;
            };
            let store = crate::workflow::WorkflowStore::open_default()?;
            let current = store
                .list()?
                .into_iter()
                .find(|workflow| workflow.package.id == provenance.asset_key)
                .map(|workflow| workflow.package.version)
                .unwrap_or_default();
            if current.trim().is_empty() {
                continue;
            }
            if crate::skill::resolver::compare_versions(&item.version, &current)
                == std::cmp::Ordering::Greater
            {
                install_workflow(&provenance.asset_key, Some(&item.version))?;
                updated.push(format!("workflow:{}@{}", item.workflow_id, item.version));
            }
        }
    }
    Ok(updated)
}

/// Reconcile DSH's user preset directory from extension-source catalogs.
///
/// Presets are source-managed only when the feature pack that owns them has a
/// real local plugin or Skill installation. User-authored presets are never
/// overwritten because they have no source marker.
pub(crate) fn reconcile_dsh_presets(home: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let snapshot = refresh_snapshot()?;
    let root = home.join(".agent-presets");
    let mut desired = HashMap::<String, (&ExtensionAgentPreset, &ExtensionSourceConfig)>::new();
    for pack in &snapshot.feature_packs {
        if pack.agent_preset_ids.is_empty() || !feature_pack_ready(pack) {
            continue;
        }
        let source = snapshot
            .sources
            .iter()
            .map(|status| &status.source)
            .find(|source| source.id == pack.source_id);
        let Some(source) = source else { continue };
        for preset_id in &pack.agent_preset_ids {
            let Some(preset) = snapshot.agent_presets.iter().find(|item| {
                item.preset_id == *preset_id && item.source == format!("github:{}", source.id)
            }) else {
                continue;
            };
            desired.entry(preset_id.clone()).or_insert((preset, source));
        }
    }

    let mut changed = Vec::new();
    let desired_ids = desired.keys().cloned().collect::<HashSet<_>>();
    let unavailable_sources = snapshot
        .sources
        .iter()
        .filter(|status| status.state == "unavailable")
        .map(|status| status.source.id.clone())
        .collect::<HashSet<_>>();
    for (preset_id, (preset, source)) in desired {
        let directory = root.join(&preset_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.file_type().is_dir() => {
                // Never follow a file, junction, or symlink supplied by the user.
                continue;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        fs::create_dir_all(&root)?;
        let marker_path = directory.join(".himind-source-preset.json");
        if directory.is_dir() && !marker_path.is_file() {
            // A user-owned preset wins over an extension source with the same id.
            continue;
        }
        let Ok(composition) = fetch_preset_file(source, &preset.path) else {
            continue;
        };
        let digest = format!("{:x}", Sha256::digest(&composition));
        if !digest.eq_ignore_ascii_case(&preset.sha256) {
            eprintln!("DSH preset {} 的 SHA-256 校验失败，跳过本次同步", preset_id);
            continue;
        }
        let metadata_path = preset
            .path
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/preset.yml"))
            .unwrap_or_else(|| "preset.yml".to_string());
        let metadata = fetch_preset_file(source, &metadata_path).unwrap_or_default();
        fs::create_dir_all(&directory)?;
        atomic_file::atomic_write(&directory.join("agent.cordis.yml"), &composition)?;
        if metadata.is_empty() {
            let _ = fs::remove_file(directory.join("preset.yml"));
        } else {
            atomic_file::atomic_write(&directory.join("preset.yml"), &metadata)?;
        }
        let marker = serde_json::json!({
            "schema_version": 1,
            "source_id": source.id,
            "preset_id": preset.preset_id,
            "version": preset.version,
            "sha256": preset.sha256,
        });
        atomic_file::atomic_write(&marker_path, &serde_json::to_vec_pretty(&marker)?)?;
        changed.push(format!("preset:{}@{}", preset_id, preset.version));
    }

    if root.is_dir() {
        for entry in fs::read_dir(&root)?.flatten() {
            let directory = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&directory) else {
                continue;
            };
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let marker_path = directory.join(".himind-source-preset.json");
            if !marker_path.is_file() {
                continue;
            }
            let Ok(marker) = serde_json::from_slice::<serde_json::Value>(&fs::read(&marker_path)?)
            else {
                continue;
            };
            let preset_id = marker
                .get("preset_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let source_id = marker
                .get("source_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if !should_remove_managed_preset(
                preset_id,
                source_id,
                &desired_ids,
                &unavailable_sources,
            ) {
                continue;
            }
            fs::remove_dir_all(&directory)?;
            changed.push(format!("preset:{}:removed", preset_id));
        }
    }
    Ok(changed)
}

pub(crate) fn reconcile_dsh_presets_now() -> Result<Vec<String>, Box<dyn Error>> {
    let home = crate::runtime::builtin::interactive_home_path()
        .map_err(|error| format!("DSH 用户目录不可用: {error}"))?;
    reconcile_dsh_presets(&home)
}

fn feature_pack_ready(pack: &ExtensionFeaturePack) -> bool {
    let plugin_ready = pack.plugin_ids.iter().any(|id| {
        let local = crate::app::plugin_manager::local_status(id);
        if local.current_version.is_empty() || !local.enabled {
            return false;
        }
        provenance_matches_or_local("plugin", id, &pack.source_id)
    });
    let store = crate::skill::store::SkillStore::new();
    let skill_ready = pack.skill_ids.iter().any(|id| {
        let installed = store.get_record(id).ok().flatten().is_some();
        installed && provenance_matches_or_local("skill", id, &pack.source_id)
    });
    (pack.plugin_ids.is_empty() && pack.skill_ids.is_empty()) || plugin_ready || skill_ready
}

fn provenance_matches_or_local(kind: &str, key: &str, source_id: &str) -> bool {
    match read_provenance(kind, key) {
        Ok(Some(record)) => record.source_id == source_id,
        Ok(None) => true,
        Err(_) => false,
    }
}

fn fetch_preset_file(
    source: &ExtensionSourceConfig,
    path: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let normalized = path.trim().replace('\\', "/");
    let safe = Path::new(&normalized);
    if normalized.is_empty()
        || safe.is_absolute()
        || safe
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("DSH preset 路径无效".into());
    }
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        source.repository, source.reference, normalized
    );
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("HiMind-Agent")
        .build()?
        .get(url)
        .send()?
        .error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

fn resolve_plugin_order(
    catalog: &[PluginCatalogItem],
    plugin_id: &str,
    version: Option<&str>,
    source: &str,
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
                && item.source == source
                && version
                    .map(|requested| requested == item.version)
                    .unwrap_or(true)
        })
        .cloned()
        .ok_or_else(|| format!("来源 {source} 中未找到插件: {plugin_id}"))?;
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
            resolve_plugin_order(
                catalog,
                &dependency.plugin_id,
                None,
                source,
                visiting,
                order,
            )?;
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
        .or_else(|| source.strip_prefix("local:"))
        .ok_or("扩展目录项缺少来源身份")?;
    snapshot
        .sources
        .iter()
        .map(|status| &status.source)
        .find(|config| config.id == source_id)
        .ok_or_else(|| "扩展目录项对应的来源配置不存在".into())
}

fn fetch_catalog(source: &ExtensionSourceConfig) -> Result<ExtensionSourceCatalog, Box<dyn Error>> {
    if source.kind == ExtensionSourceKind::Local {
        let catalog = build_local_catalog(source)?;
        validate_catalog(&catalog, source)?;
        return Ok(catalog);
    }
    let catalog = fetch_remote_catalog(source)?;
    validate_catalog(&catalog, source)?;
    Ok(catalog)
}

/// 只把远端清单取回来，不校验来源绑定，供「按远端清单纠正来源身份」使用。
fn fetch_remote_catalog(
    source: &ExtensionSourceConfig,
) -> Result<ExtensionSourceCatalog, Box<dyn Error>> {
    let url = catalog_url(source)?;
    let catalog = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("HiMind-Agent")
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .json::<ExtensionSourceCatalog>()?;
    Ok(catalog)
}

/// GitHub 发布源的通道与目录身份由远端清单自己声明：用户在「添加来源」里只能填
/// 仓库、分支和清单路径，没有可选项。拿到清单后按它的声明纠正一次并落盘，本地
/// 源码源与该 GitHub 发布源才会落到同一个分发单元，重启后取用键也保持稳定。
fn adopt_github_distribution_identities(
    settings: &mut ExtensionSourceSettings,
    refresh_remote: bool,
) -> bool {
    let mut changed = false;
    for source in settings.sources.iter_mut() {
        if source.kind != ExtensionSourceKind::Github {
            continue;
        }
        let catalog = if refresh_remote {
            match fetch_remote_catalog(source) {
                Ok(catalog) => catalog,
                Err(_) => continue,
            }
        } else {
            match load_cached_catalog(&source.id) {
                Ok(Some(catalog)) => catalog,
                _ => continue,
            }
        };
        if adopt_catalog_distribution_identity(source, &catalog) {
            changed = true;
        }
    }
    changed
}

fn adopt_catalog_distribution_identity(
    source: &mut ExtensionSourceConfig,
    catalog: &ExtensionSourceCatalog,
) -> bool {
    let distribution_id = normalize_distribution_id(&catalog.distribution_id);
    let channel = normalize_channel(&catalog.channel);
    let catalog_id = normalize_catalog_id(&catalog.catalog_id);
    let mut changed = false;
    if !distribution_id.is_empty() && distribution_id != source.distribution_id {
        source.distribution_id = distribution_id;
        changed = true;
    }
    if !channel.is_empty() && channel != source.channel {
        source.channel = channel;
        changed = true;
    }
    if !catalog_id.is_empty() && catalog_id != source.catalog_id {
        source.catalog_id = catalog_id;
        changed = true;
    }
    changed
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
    if !catalog.distribution_id.trim().is_empty() {
        validate_distribution_field(&catalog.distribution_id, "目录分发 ID", true)?;
        if !source.distribution_id.trim().is_empty()
            && normalize_distribution_id(&catalog.distribution_id)
                != normalize_distribution_id(&source.distribution_id)
        {
            return Err("扩展源目录分发 ID 与来源配置不一致".into());
        }
    }
    validate_distribution_field(&catalog.channel, "目录分发通道", false)?;
    if !source.channel.trim().is_empty()
        && normalize_channel(&catalog.channel) != normalize_channel(&source.channel)
    {
        return Err("扩展源目录分发通道与来源配置不一致".into());
    }
    validate_distribution_field(&catalog.catalog_id, "目录 ID", false)?;
    if !source.catalog_id.trim().is_empty()
        && normalize_catalog_id(&catalog.catalog_id) != normalize_catalog_id(&source.catalog_id)
    {
        return Err("扩展源目录 ID 与来源配置不一致".into());
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
        if source.kind == ExtensionSourceKind::Local {
            validate_local_item_identity("plugin", &item.plugin_id)?;
        } else {
            validate_artifact(
                &source.repository,
                &item.download_url,
                item.file_size,
                &item.sha256,
            )?;
        }
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
        if source.kind == ExtensionSourceKind::Local {
            validate_local_item_identity("skill", &item.skill_id)?;
        } else {
            validate_artifact(
                &source.repository,
                &item.download_url,
                item.file_size,
                &item.sha256,
            )?;
        }
        validate_catalog_signature(
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            &source.verification,
        )?;
    }
    for item in &catalog.workflows {
        if !identities.insert(format!("workflow:{}:{}", item.workflow_id, item.version)) {
            return Err(format!(
                "扩展源包含重复 Workflow 版本: {} {}",
                item.workflow_id, item.version
            )
            .into());
        }
        if source.kind == ExtensionSourceKind::Local {
            validate_local_item_identity("workflow", &item.workflow_id)?;
        } else {
            validate_artifact(
                &source.repository,
                &item.download_url,
                item.file_size,
                &item.sha256,
            )?;
        }
        validate_catalog_signature(
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            &source.verification,
        )?;
    }
    for item in &catalog.experts {
        if !identities.insert(format!("expert:{}:{}", item.expert_id, item.version)) {
            return Err(format!(
                "扩展源包含重复专家版本: {} {}",
                item.expert_id, item.version
            )
            .into());
        }
        if source.kind == ExtensionSourceKind::Local {
            validate_local_item_identity("expert", &item.expert_id)?;
        } else {
            validate_artifact(
                &source.repository,
                &item.download_url,
                item.file_size,
                &item.sha256,
            )?;
        }
        validate_catalog_signature(
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            &source.verification,
        )?;
    }
    for pack in &catalog.feature_packs {
        if !identities.insert(format!("feature_pack:{}", pack.id)) {
            return Err(format!("扩展源包含重复功能包: {}", pack.id).into());
        }
        validate_feature_pack(pack)?;
    }
    for item in &catalog.agent_presets {
        if !identities.insert(format!("agent_preset:{}", item.preset_id)) {
            return Err(format!("扩展源包含重复 DSH preset: {}", item.preset_id).into());
        }
        validate_agent_preset(item)?;
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
    item.source = format!("{}:{}", source_kind_prefix(source.kind), source.id);
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
    item.source = format!("{}:{}", source_kind_prefix(source.kind), source.id);
    item.assignment = "optional".to_string();
    item.management = "user_managed".to_string();
    item.install_mode = "prompt".to_string();
    item.organization_reason.clear();
    item.managed = false;
    item.allow_disable = true;
    item.allow_uninstall = true;
    Ok(())
}

fn normalize_workflow_item(
    item: &mut WorkflowCatalogItem,
    source: &ExtensionSourceConfig,
) -> Result<(), Box<dyn Error>> {
    validate_asset_identity("workflow", &item.workflow_id)?;
    item.source = format!("{}:{}", source_kind_prefix(source.kind), source.id);
    item.assignment = "optional".to_string();
    item.management = "user_managed".to_string();
    item.install_mode = "prompt".to_string();
    item.organization_reason.clear();
    item.managed = false;
    item.allow_disable = true;
    item.allow_uninstall = true;
    Ok(())
}

fn normalize_expert_item(
    item: &mut ExpertCatalogItem,
    source: &ExtensionSourceConfig,
) -> Result<(), Box<dyn Error>> {
    validate_asset_identity("expert", &item.expert_id)?;
    item.source = format!("{}:{}", source_kind_prefix(source.kind), source.id);
    item.assignment = "optional".to_string();
    item.management = "user_managed".to_string();
    item.install_mode = "prompt".to_string();
    item.managed = false;
    item.allow_disable = true;
    item.allow_uninstall = true;
    Ok(())
}

fn source_kind_prefix(kind: ExtensionSourceKind) -> &'static str {
    match kind {
        ExtensionSourceKind::Github => "github",
        ExtensionSourceKind::Local => "local",
    }
}

fn source_identity(source: &ExtensionSourceConfig) -> String {
    format!("{}:{}", source_kind_prefix(source.kind), source.id)
}

fn source_id_of(identity: &str) -> &str {
    identity
        .split_once(':')
        .map(|(_, id)| id)
        .unwrap_or(identity)
}

// 本地开发源与它对应的 GitHub 分发源必然携带同一批扩展 ID，两者并存是本机开发
// 的正常形态：本地源优先，保证开发者看到的是本机构建；其余重复按配置顺序先到先得。
fn source_outranks(candidate: &str, existing: &str) -> bool {
    candidate.starts_with("local:") && !existing.starts_with("local:")
}

/// 归一化仓库地址，用于判断两个来源是否指向同一份分发内容。
fn normalize_repository_key(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn normalize_path_key(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(['\\', '/'])
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// 分发单元键。本地工作区源优先用它声明的上游仓库作为键，这样它才能与对应的
/// GitHub 分发源落到同一单元；没有上游仓库时退化为本地路径键。
pub(crate) fn unit_key_of(source: &ExtensionSourceConfig) -> String {
    let distribution_id = normalize_distribution_id(&source.distribution_id);
    let channel = normalize_channel(&source.channel);
    let catalog_id = normalize_catalog_id(&source.catalog_id);
    if !distribution_id.is_empty() && !channel.is_empty() && !catalog_id.is_empty() {
        return format!("{distribution_id}#{channel}#{catalog_id}");
    }
    // Compatibility for in-memory fixtures and pre-migration callers. Persisted
    // settings are normalized by `settings_at` before they reach this path.
    match source.kind {
        ExtensionSourceKind::Local => {
            let upstream = normalize_repository_key(&source.upstream_repository);
            if upstream.is_empty() {
                format!("local:{}", normalize_path_key(&source.repository))
            } else {
                format!("remote:{upstream}")
            }
        }
        ExtensionSourceKind::Github => {
            format!("remote:{}", normalize_repository_key(&source.repository))
        }
    }
}

fn acquisition_for(
    acquisitions: &BTreeMap<String, ExtensionSourceAcquisition>,
    unit_key: &str,
) -> ExtensionSourceAcquisition {
    acquisitions.get(unit_key).copied().unwrap_or_default()
}

/// 按分发单元聚合后展开来源顺序：单元之间保持配置顺序，单元内部按取用模式
/// 排列，让「先到先得」的合并规则直接体现取用侧。
fn ordered_sources(
    sources: Vec<ExtensionSourceConfig>,
    acquisitions: &BTreeMap<String, ExtensionSourceAcquisition>,
) -> Vec<ExtensionSourceConfig> {
    let mut units = Vec::<String>::new();
    let mut grouped = HashMap::<String, Vec<ExtensionSourceConfig>>::new();
    for source in sources {
        let key = unit_key_of(&source);
        if !grouped.contains_key(&key) {
            units.push(key.clone());
        }
        grouped.entry(key).or_default().push(source);
    }
    let mut ordered = Vec::new();
    for key in units {
        let mut members = grouped.remove(&key).unwrap_or_default();
        // 取用侧优先：合并目录里同一 ID 的首个命中就是取用来源，安装与依赖解析
        // 都按首个命中取值，所以这里必须让取用侧排在同单元其它成员之前。
        let local_first = acquisition_for(acquisitions, &key) == ExtensionSourceAcquisition::Local;
        members.sort_by_key(|source| {
            u8::from((source.kind == ExtensionSourceKind::Local) != local_first)
        });
        ordered.extend(members);
    }
    ordered
}

fn unit_source_ids(sources: &[ExtensionSourceConfig]) -> HashMap<String, String> {
    sources
        .iter()
        .map(|source| (source_identity(source), unit_key_of(source)))
        .collect()
}

/// 单个来源目录提供的制品清单，用于按取用侧汇总分发单元内容。
#[derive(Debug, Default, Clone)]
struct SourceCatalogAssets {
    plugins: Vec<(String, String, String)>,
    skills: Vec<(String, String, String)>,
    workflows: Vec<(String, String, String)>,
    experts: Vec<(String, String, String)>,
    /// 完整制品元数据，键为 `kind:id`。旧的测试 fixture 只提供前三个
    /// 三元组时，build_units 会回退到兼容字段并补齐来源信息。
    #[allow(dead_code)]
    asset_details: HashMap<String, ExtensionUnitAsset>,
}

fn build_units(
    sources: &[ExtensionSourceConfig],
    acquisitions: &BTreeMap<String, ExtensionSourceAcquisition>,
    catalogs: &HashMap<String, SourceCatalogAssets>,
) -> Vec<ExtensionDistributionUnit> {
    let mut order = Vec::<String>::new();
    let mut grouped = HashMap::<String, Vec<&ExtensionSourceConfig>>::new();
    for source in sources {
        let key = unit_key_of(source);
        if !grouped.contains_key(&key) {
            order.push(key.clone());
        }
        grouped.entry(key).or_default().push(source);
    }
    let mut units = Vec::new();
    for key in order {
        let members = grouped.remove(&key).unwrap_or_default();
        let local = members
            .iter()
            .find(|source| source.kind == ExtensionSourceKind::Local)
            .copied();
        let remote = members
            .iter()
            .find(|source| source.kind == ExtensionSourceKind::Github)
            .copied();
        let acquisition = acquisition_for(acquisitions, &key);
        let preferred = match acquisition {
            ExtensionSourceAcquisition::Local => local,
            ExtensionSourceAcquisition::Remote => remote,
        };
        // 发现阶段可以展示“首选来源不可用”，但不能把另一来源伪装成首选
        // 来源。安装会携带 source_id，避免本地预览意外安装线上制品。
        let primary = preferred;
        let source_available = primary
            .map(|source| catalogs.contains_key(&source_identity(source)))
            .unwrap_or(false);
        let repository = remote
            .map(|source| source.repository.clone())
            .or_else(|| {
                local
                    .map(|source| source.upstream_repository.clone())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default();
        let name = local
            .map(|source| source.name.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| remote.map(|source| source.name.clone()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                if repository.is_empty() {
                    local
                        .map(|source| source.repository.clone())
                        .unwrap_or_default()
                } else {
                    repository.clone()
                }
            });
        let primary_identity = primary.map(source_identity);
        let assets = primary
            .and_then(|source| catalogs.get(&source_identity(source)))
            .cloned()
            .unwrap_or_default();
        // 另一侧只用于提示，不参与单元内容；它没加载时 newer_count 保持 0，
        // 界面据此只显示可用性，不显示虚假的“可更新”。
        let other = match acquisition {
            ExtensionSourceAcquisition::Local => remote,
            ExtensionSourceAcquisition::Remote => local,
        };
        let other_side = other.map(|_| ExtensionUnitOtherSide {
            side: match acquisition {
                ExtensionSourceAcquisition::Local => "remote",
                ExtensionSourceAcquisition::Remote => "local",
            }
            .to_string(),
            available: other
                .map(|source| catalogs.contains_key(&source_identity(source)))
                .unwrap_or(false),
            newer_count: other
                .and_then(|source| catalogs.get(&source_identity(source)))
                .map(|other| count_newer_assets(&assets, other))
                .unwrap_or_default(),
        });
        let mut plugin_ids = assets
            .plugins
            .iter()
            .map(|item| item.0.clone())
            .collect::<Vec<_>>();
        let mut skill_ids = assets
            .skills
            .iter()
            .map(|item| item.0.clone())
            .collect::<Vec<_>>();
        let mut workflow_ids = assets
            .workflows
            .iter()
            .map(|item| item.0.clone())
            .collect::<Vec<_>>();
        let mut expert_ids = assets
            .experts
            .iter()
            .map(|item| item.0.clone())
            .collect::<Vec<_>>();
        plugin_ids.sort();
        skill_ids.sort();
        workflow_ids.sort();
        expert_ids.sort();
        let plugin_count = plugin_ids.len();
        let skill_count = skill_ids.len();
        let workflow_count = workflow_ids.len();
        let expert_count = expert_ids.len();
        let mut catalog_assets = assets
            .plugins
            .iter()
            .map(|(asset_id, name, version)| {
                assets
                    .asset_details
                    .get(&format!("plugin:{asset_id}"))
                    .cloned()
                    .unwrap_or_else(|| ExtensionUnitAsset {
                        asset_kind: "plugin".to_string(),
                        asset_id: asset_id.clone(),
                        name: name.clone(),
                        version: version.clone(),
                        source_id: primary_identity
                            .as_deref()
                            .map(source_id_of)
                            .unwrap_or_default()
                            .to_string(),
                        source_kind: if primary
                            .map(|s| s.kind == ExtensionSourceKind::Local)
                            .unwrap_or(false)
                        {
                            "local"
                        } else {
                            "github"
                        }
                        .to_string(),
                        artifact_url: String::new(),
                        sha256: String::new(),
                        signature_key_id: String::new(),
                        signature_algorithm: String::new(),
                        channel: String::new(),
                    })
            })
            .chain(assets.skills.iter().map(|(asset_id, name, version)| {
                assets
                    .asset_details
                    .get(&format!("skill:{asset_id}"))
                    .cloned()
                    .unwrap_or_else(|| ExtensionUnitAsset {
                        asset_kind: "skill".to_string(),
                        asset_id: asset_id.clone(),
                        name: name.clone(),
                        version: version.clone(),
                        source_id: primary_identity
                            .as_deref()
                            .map(source_id_of)
                            .unwrap_or_default()
                            .to_string(),
                        source_kind: if primary
                            .map(|s| s.kind == ExtensionSourceKind::Local)
                            .unwrap_or(false)
                        {
                            "local"
                        } else {
                            "github"
                        }
                        .to_string(),
                        artifact_url: String::new(),
                        sha256: String::new(),
                        signature_key_id: String::new(),
                        signature_algorithm: String::new(),
                        channel: String::new(),
                    })
            }))
            .chain(assets.workflows.iter().map(|(asset_id, name, version)| {
                assets
                    .asset_details
                    .get(&format!("workflow:{asset_id}"))
                    .cloned()
                    .unwrap_or_else(|| ExtensionUnitAsset {
                        asset_kind: "workflow".to_string(),
                        asset_id: asset_id.clone(),
                        name: name.clone(),
                        version: version.clone(),
                        source_id: primary_identity
                            .as_deref()
                            .map(source_id_of)
                            .unwrap_or_default()
                            .to_string(),
                        source_kind: if primary
                            .map(|s| s.kind == ExtensionSourceKind::Local)
                            .unwrap_or(false)
                        {
                            "local"
                        } else {
                            "github"
                        }
                        .to_string(),
                        artifact_url: String::new(),
                        sha256: String::new(),
                        signature_key_id: String::new(),
                        signature_algorithm: String::new(),
                        channel: String::new(),
                    })
            }))
            .chain(assets.experts.iter().map(|(asset_id, name, version)| {
                assets
                    .asset_details
                    .get(&format!("expert:{asset_id}"))
                    .cloned()
                    .unwrap_or_else(|| ExtensionUnitAsset {
                        asset_kind: "expert".to_string(),
                        asset_id: asset_id.clone(),
                        name: name.clone(),
                        version: version.clone(),
                        source_id: primary_identity
                            .as_deref()
                            .map(source_id_of)
                            .unwrap_or_default()
                            .to_string(),
                        source_kind: if primary
                            .map(|s| s.kind == ExtensionSourceKind::Local)
                            .unwrap_or(false)
                        {
                            "local"
                        } else {
                            "github"
                        }
                        .to_string(),
                        artifact_url: String::new(),
                        sha256: String::new(),
                        signature_key_id: String::new(),
                        signature_algorithm: String::new(),
                        channel: String::new(),
                    })
            }))
            .collect::<Vec<_>>();
        catalog_assets.sort_by(|left, right| {
            (left.asset_kind.as_str(), left.asset_id.as_str())
                .cmp(&(right.asset_kind.as_str(), right.asset_id.as_str()))
        });
        units.push(ExtensionDistributionUnit {
            unit_key: key.clone(),
            name,
            distribution_id: preferred
                .map(|source| source.distribution_id.clone())
                .filter(|value| !value.is_empty())
                .or_else(|| local.map(|source| source.distribution_id.clone()))
                .or_else(|| remote.map(|source| source.distribution_id.clone()))
                .unwrap_or_default(),
            channel: preferred
                .map(|source| source.channel.clone())
                .filter(|value| !value.is_empty())
                .or_else(|| local.map(|source| source.channel.clone()))
                .or_else(|| remote.map(|source| source.channel.clone()))
                .unwrap_or_else(|| DEFAULT_DISTRIBUTION_CHANNEL.to_string()),
            catalog_id: preferred
                .map(|source| source.catalog_id.clone())
                .filter(|value| !value.is_empty())
                .or_else(|| local.map(|source| source.catalog_id.clone()))
                .or_else(|| remote.map(|source| source.catalog_id.clone()))
                .unwrap_or_else(|| DEFAULT_CATALOG_ID.to_string()),
            acquisition,
            local_source_id: local.map(|source| source.id.clone()),
            remote_source_id: remote.map(|source| source.id.clone()),
            repository,
            local_root: local.map(|source| source.repository.clone()),
            plugin_count,
            skill_count,
            workflow_count,
            expert_count,
            state: if !source_available {
                "unavailable".to_string()
            } else if plugin_count + skill_count + workflow_count + expert_count == 0 {
                "empty".to_string()
            } else {
                "ready".to_string()
            },
            plugin_ids,
            skill_ids,
            workflow_ids,
            expert_ids,
            project_ids: Vec::new(),
            installed: Vec::new(),
            assets: catalog_assets,
            other_side,
        });
    }
    units
}

/// 统计另一侧目录里版本更高的制品数量。版本比较沿用安装解析的同一个比较器，
/// 避免界面提示和实际解析出现两套“更高版本”的语义。
fn count_newer_assets(current: &SourceCatalogAssets, other: &SourceCatalogAssets) -> usize {
    let mut count = 0;
    for (current_items, other_items) in [
        (&current.plugins, &other.plugins),
        (&current.skills, &other.skills),
        (&current.workflows, &other.workflows),
        (&current.experts, &other.experts),
    ] {
        let current_versions = current_items
            .iter()
            .map(|(id, _, version)| (id.as_str(), version.as_str()))
            .collect::<HashMap<_, _>>();
        for (id, _, version) in other_items {
            let newer = current_versions.get(id.as_str()).is_some_and(|current| {
                crate::skill::resolver::compare_versions(version, current)
                    == std::cmp::Ordering::Greater
            });
            if newer {
                count += 1;
            }
        }
    }
    count
}

fn conflict_reason(winner: &str) -> &'static str {
    if winner.starts_with("local:") {
        "由本地开发源优先提供"
    } else {
        "由配置顺序更早的来源优先提供"
    }
}

fn conflict(
    loser_source_id: &str,
    label: &str,
    key: &str,
    winner: &str,
) -> (String, String, String) {
    (
        loser_source_id.to_string(),
        conflict_reason(winner).to_string(),
        format!("{label} {key}"),
    )
}

fn local_item_dir(download_url: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = download_url
        .strip_prefix("local:")
        .ok_or("本地扩展目录项缺少本地路径")?;
    if path.trim().is_empty() {
        return Err("本地扩展目录项缺少本地路径".into());
    }
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return Err(format!("本地扩展目录不存在: {}", path.display()).into());
    }
    Ok(path)
}

fn install_skill_catalog_item(
    item: &SkillCatalogItem,
    source: &ExtensionSourceConfig,
) -> Result<crate::skill::types::SkillRecord, Box<dyn Error>> {
    if source.kind == ExtensionSourceKind::Local {
        let dir = local_item_dir(&item.download_url)?;
        return crate::app::skill_manager::install_local_package_from_source(
            &dir,
            &source_identity(source),
        );
    }
    crate::app::skill_manager::install_public_catalog_item(
        item,
        source.verification.requires_signature(),
    )
}

fn validate_local_item_identity(kind: &str, key: &str) -> Result<(), Box<dyn Error>> {
    validate_asset_identity(kind, key)
}

fn validate_feature_pack(pack: &ExtensionFeaturePack) -> Result<(), Box<dyn Error>> {
    validate_asset_key(&pack.id)?;
    for key in pack
        .plugin_ids
        .iter()
        .chain(pack.skill_ids.iter())
        .chain(pack.agent_preset_ids.iter())
    {
        validate_asset_key(key)?;
    }
    Ok(())
}

fn validate_agent_preset(item: &AgentPresetCatalogItem) -> Result<(), Box<dyn Error>> {
    if !is_valid_dsh_preset_id(&item.preset_id) {
        return Err(format!(
            "DSH preset {} 的 ID 必须匹配小写字母、数字和连字符",
            item.preset_id
        )
        .into());
    }
    if item.version.trim().is_empty() {
        return Err(format!("DSH preset {} 缺少版本", item.preset_id).into());
    }
    if item.sha256.len() != 64 || !item.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("DSH preset {} 的 SHA-256 无效", item.preset_id).into());
    }
    let normalized = item.path.trim().replace('\\', "/");
    let path = Path::new(&normalized);
    if normalized.is_empty()
        || !(normalized == "agent.cordis.yml" || normalized.ends_with("/agent.cordis.yml"))
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "DSH preset {} 必须指向仓库内的 agent.cordis.yml",
            item.preset_id
        )
        .into());
    }
    Ok(())
}

fn is_valid_dsh_preset_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
        })
}

fn should_remove_managed_preset(
    preset_id: &str,
    source_id: &str,
    desired_ids: &HashSet<String>,
    unavailable_sources: &HashSet<String>,
) -> bool {
    !preset_id.is_empty()
        && !desired_ids.contains(preset_id)
        && !unavailable_sources.contains(source_id)
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
    // GitHub 的 owner/repo 不区分大小写，目录里的制品地址常为全小写，
    // 与用户填写的仓库大小写并不总是一致，这里按大小写不敏感比较。
    if !url
        .path()
        .to_ascii_lowercase()
        .starts_with(&expected.to_ascii_lowercase())
    {
        return Err("扩展源制品地址不属于配置的 GitHub 仓库".into());
    }
    Ok(())
}

fn settings_at(path: &Path) -> Result<ExtensionSourceSettings, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(ExtensionSourceSettings::default());
    }
    let mut value: ExtensionSourceSettings = serde_json::from_slice(&fs::read(path)?)?;
    if value.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(format!("扩展源配置版本不受支持: {}", value.schema_version).into());
    }
    let mut ids = HashSet::new();
    for source in &mut value.sources {
        normalize_source_distribution_identity(source);
        validate_distribution_field(&source.distribution_id, "分发 ID", true)?;
        validate_distribution_field(&source.channel, "分发通道", false)?;
        validate_distribution_field(&source.catalog_id, "目录 ID", false)?;
        let expected = match source.kind {
            ExtensionSourceKind::Github => source_id(
                &normalize_repository(&source.repository)?,
                &validate_reference(&source.reference)?,
                &validate_catalog_path(&source.catalog_path)?,
            ),
            ExtensionSourceKind::Local => {
                let catalog_path = validate_catalog_path(&source.catalog_path)?;
                if source.repository.trim().is_empty() {
                    return Err(format!("本地扩展源缺少目录: {}", source.name).into());
                }
                local_source_id(&source.repository, &catalog_path)
            }
        };
        if source.id != expected {
            return Err(format!("扩展源配置身份无效: {}", source.name).into());
        }
        if !ids.insert(source.id.clone()) {
            return Err(format!("扩展源配置重复: {}", source.id).into());
        }
    }
    apply_local_upstreams(&mut value);
    migrate_acquisition_keys(&mut value.acquisitions, &value.sources);
    Ok(value)
}

/// Fill identity fields for settings written before the explicit distribution
/// identity was introduced. This is intentionally deterministic and does not
/// use a remote response, so a source keeps the same unit key across restarts.
fn normalize_source_distribution_identity(source: &mut ExtensionSourceConfig) {
    if source.kind == ExtensionSourceKind::Local {
        let root = Path::new(source.repository.trim());
        if let Ok(content) = fs::read_to_string(root.join(&source.catalog_path)) {
            if let Ok(aggregate) = serde_json::from_str::<LocalAggregateCatalog>(&content) {
                source.distribution_id = aggregate_distribution_id(&aggregate, root);
                source.channel = aggregate_channel(&aggregate);
                source.catalog_id = aggregate_catalog_id(&aggregate);
                return;
            }
        }
    }
    if source.distribution_id.trim().is_empty() {
        source.distribution_id = match source.kind {
            ExtensionSourceKind::Github => normalize_repository_key(&source.repository),
            ExtensionSourceKind::Local => {
                let root = Path::new(source.repository.trim());
                let aggregate = fs::read_to_string(root.join(&source.catalog_path))
                    .ok()
                    .and_then(|content| {
                        serde_json::from_str::<LocalAggregateCatalog>(&content).ok()
                    });
                aggregate
                    .as_ref()
                    .map(|value| aggregate_distribution_id(value, root))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        let upstream =
                            resolve_local_upstream(&source.repository, &source.catalog_path);
                        if upstream.is_empty() {
                            local_distribution_fallback(&source.repository)
                        } else {
                            upstream
                        }
                    })
            }
        };
    } else {
        source.distribution_id = normalize_distribution_id(&source.distribution_id);
    }
    if source.channel.trim().is_empty() {
        source.channel = DEFAULT_DISTRIBUTION_CHANNEL.to_string();
    } else {
        source.channel = normalize_channel(&source.channel);
    }
    if source.catalog_id.trim().is_empty() {
        source.catalog_id = DEFAULT_CATALOG_ID.to_string();
    } else {
        source.catalog_id = normalize_catalog_id(&source.catalog_id);
    }
}

/// Old settings used `remote:owner/repo` or a local path as the acquisition
/// key. Move those entries to the explicit identity key while preserving a
/// user's selected remote side.
fn migrate_acquisition_keys(
    acquisitions: &mut BTreeMap<String, ExtensionSourceAcquisition>,
    sources: &[ExtensionSourceConfig],
) {
    let mut aliases = BTreeMap::new();
    for source in sources {
        let new_key = unit_key_of(source);
        let old_key = match source.kind {
            ExtensionSourceKind::Github => {
                format!("remote:{}", normalize_repository_key(&source.repository))
            }
            ExtensionSourceKind::Local => {
                let upstream = normalize_repository_key(&source.upstream_repository);
                if upstream.is_empty() {
                    format!("local:{}", normalize_path_key(&source.repository))
                } else {
                    format!("remote:{upstream}")
                }
            }
        };
        if old_key != new_key {
            aliases.insert(old_key, new_key);
        }
    }
    for (old_key, new_key) in aliases {
        if let Some(value) = acquisitions.remove(&old_key) {
            acquisitions.entry(new_key).or_insert(value);
        }
    }
}

fn apply_local_upstreams(settings: &mut ExtensionSourceSettings) {
    for source in settings.sources.iter_mut() {
        source.upstream_repository = if source.kind == ExtensionSourceKind::Local {
            resolve_local_upstream(&source.repository, &source.catalog_path)
        } else {
            String::new()
        };
    }
}

fn resolve_local_upstream(root: &str, catalog_path: &str) -> String {
    let root = Path::new(root.trim());
    if !root.is_dir() {
        return String::new();
    }
    let declared = fs::read_to_string(root.join(catalog_path))
        .ok()
        .and_then(|content| serde_json::from_str::<LocalAggregateCatalog>(&content).ok())
        .map(|aggregate| aggregate.repository)
        .unwrap_or_default();
    local_upstream_repository(root, &declared)
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

fn persisted_settings(settings: &ExtensionSourceSettings) -> ExtensionSourceSettings {
    let mut persisted = settings.clone();
    let valid = persisted
        .sources
        .iter()
        .map(unit_key_of)
        .collect::<HashSet<String>>();
    persisted.acquisitions.retain(|key, _| valid.contains(key));
    persisted
        .distribution_targets
        .retain(|key, _| valid.contains(key));
    for source in persisted.sources.iter_mut() {
        source.upstream_repository = String::new();
    }
    persisted
}

fn save_settings(settings: &ExtensionSourceSettings) -> Result<(), Box<dyn Error>> {
    atomic_file::atomic_write(
        &settings_path(),
        &serde_json::to_vec_pretty(&persisted_settings(settings))?,
    )?;
    Ok(())
}

fn save_cached_catalog(
    source_id: &str,
    catalog: &ExtensionSourceCatalog,
) -> Result<(), Box<dyn Error>> {
    let path = cache_path(source_id);
    // 回写缓存时加文件锁：进程内由重建闸门串行化，CLI 与 GUI 这类多进程同时刷新
    // 同一来源时，锁是唯一的互斥手段。
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(catalog)?)?;
    Ok(())
}

/// 回写来源缓存。缓存只服务于"下次离线也能读"，写失败不影响本次已经取到的目录，
/// 所以降级成事件日志，而不是让整个来源刷新以假权限错误失败。
fn cache_catalog(source: &ExtensionSourceConfig, catalog: &ExtensionSourceCatalog) {
    if let Err(error) = save_cached_catalog(&source.id, catalog) {
        crate::app::crash::record_event(
            "warn",
            &format!("扩展源缓存写入失败: {} ({error})", source.id),
        );
    }
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

/// 缓存以 `<id>.json` 为主体，旁边还有写入时产生的 `.bak` 与 `.lock`。
/// 只删主文件会把已移除来源的残渣留在缓存目录里。
fn remove_cached_catalog(source_id: &str) {
    let cache = cache_path(source_id);
    let _ = fs::remove_file(&cache);
    for suffix in ["bak", "lock"] {
        let _ = fs::remove_file(cache.with_extension(format!("json.{suffix}")));
    }
}

fn provenance_root() -> PathBuf {
    paths::agent_home().join("data/extension-provenance")
}

fn provenance_path(kind: &str, key: &str) -> Result<PathBuf, Box<dyn Error>> {
    provenance_path_at(&paths::agent_home().join("data"), kind, key)
}

fn provenance_path_at(state_root: &Path, kind: &str, key: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_asset_identity(kind, key)?;
    Ok(state_root
        .join("extension-provenance")
        .join(format!("{kind}-{key}.json")))
}

fn catalog_url(source: &ExtensionSourceConfig) -> Result<url::Url, Box<dyn Error>> {
    if source.kind == ExtensionSourceKind::Local {
        return Err("本地扩展源不使用仓库 URL".into());
    }
    Ok(url::Url::parse(&format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        source.repository, source.reference, source.catalog_path
    ))?)
}

fn source_id(repository: &str, reference: &str, catalog_path: &str) -> String {
    let digest = Sha256::digest(format!("{repository}\n{reference}\n{catalog_path}").as_bytes());
    format!("github-{:x}", digest)[..23].to_string()
}

fn local_source_id(repository: &str, catalog_path: &str) -> String {
    let digest = Sha256::digest(format!("local:{repository}\n{catalog_path}").as_bytes());
    format!("local-{:x}", digest)[..23].to_string()
}

// 本地目录源统一读取聚合仓库的聚合清单（默认 extensions.json），
// 并从各扩展目录的 plugin.json / skill.json manifest 动态构建 catalog。
fn build_local_catalog(
    source: &ExtensionSourceConfig,
) -> Result<ExtensionSourceCatalog, Box<dyn Error>> {
    let root = PathBuf::from(&source.repository);
    if !root.is_dir() {
        return Err(format!("本地扩展源目录不存在: {}", source.repository).into());
    }
    let aggregate_path = root.join(&source.catalog_path);
    let content = fs::read_to_string(&aggregate_path)
        .map_err(|error| format!("本地扩展源缺少聚合清单 {}: {error}", source.catalog_path))?;
    let aggregate: LocalAggregateCatalog = serde_json::from_str(&content)
        .map_err(|error| format!("extensions.json 格式无效: {error}"))?;
    aggregate.validate()?;
    let mut catalog = ExtensionSourceCatalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        source_id: source.id.clone(),
        generation: String::new(),
        distribution_id: aggregate_distribution_id(&aggregate, &root),
        channel: aggregate_channel(&aggregate),
        catalog_id: aggregate_catalog_id(&aggregate),
        plugins: Vec::new(),
        skills: Vec::new(),
        workflows: Vec::new(),
        experts: Vec::new(),
        feature_packs: Vec::new(),
        agent_presets: Vec::new(),
    };
    for entry in &aggregate.extensions {
        let dir = safe_local_child(&root, &entry.path)?;
        match entry.kind.as_str() {
            "plugin" => {
                let item = build_local_plugin_item(&dir, source)?;
                if item.plugin_id != entry.id {
                    return Err(format!("扩展清单 ID 与 manifest 不一致: {}", entry.path).into());
                }
                catalog.plugins.push(item);
            }
            "skill" => {
                let item = build_local_skill_item(&dir, source)?;
                if item.skill_id != entry.id {
                    return Err(format!("扩展清单 ID 与 manifest 不一致: {}", entry.path).into());
                }
                catalog.skills.push(item);
            }
            "workflow" => {
                let item = build_local_workflow_item(&dir, source)?;
                if item.workflow_id != entry.id {
                    return Err(format!("扩展清单 ID 与 manifest 不一致: {}", entry.path).into());
                }
                catalog.workflows.push(item);
            }
            "expert" => {
                let item = build_local_expert_item(&dir, source)?;
                if item.expert_id != entry.id {
                    return Err(format!("扩展清单 ID 与 manifest 不一致: {}", entry.path).into());
                }
                catalog.experts.push(item);
            }
            _ => {
                return Err(format!("extensions.json 包含不支持的扩展类型: {}", entry.kind).into())
            }
        }
    }
    Ok(catalog)
}

fn build_local_plugin_item(
    dir: &Path,
    source: &ExtensionSourceConfig,
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let content = fs::read_to_string(dir.join("plugin.json"))?;
    let manifest =
        crate::capability::plugin::parse_plugin_manifest(content.trim_start_matches('\u{feff}'))?;
    let capability_ids: Vec<String> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    let mut categories = manifest.categories.clone();
    // plugin.json 声明了 categories，catalog 项必须原样保留；旧清单没有声明时
    // 才按 Capability 命名空间兜底，否则扩展市场里插件只能落进「未分类」。
    crate::extension_category::fill_missing_categories(&capability_ids, &mut categories);
    Ok(PluginCatalogItem {
        plugin_id: manifest.id.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        author_name: manifest.author.clone(),
        categories,
        review_status: "local".to_string(),
        governance: "optional".to_string(),
        version: manifest.version.clone(),
        release_notes: manifest.release_notes.clone(),
        published_at: String::new(),
        min_agent_version: manifest.min_agent_version.clone(),
        channel: "local".to_string(),
        artifact_id: String::new(),
        file_name: String::new(),
        file_size: 0,
        sha256: String::new(),
        signature: String::new(),
        signature_key_id: String::new(),
        signature_algorithm: String::new(),
        download_url: format!("local:{}", dir.display()),
        source: format!("local:{}", source.id),
        assignment: "optional".to_string(),
        management: "user_managed".to_string(),
        install_mode: "prompt".to_string(),
        organization_reason: String::new(),
        managed: false,
        allow_disable: true,
        allow_uninstall: true,
        capability_ids,
        permissions: manifest.permissions.clone(),
        view_count: manifest.contributes.views.len(),
        plugin_dependencies: manifest
            .plugin_dependencies
            .iter()
            .map(
                |dependency| crate::api::distribution::SkillPluginDependency {
                    plugin_id: dependency.plugin_id.clone(),
                    required: dependency.required,
                    min_version: dependency.min_version.clone(),
                },
            )
            .collect(),
    })
}

fn build_local_skill_item(
    dir: &Path,
    source: &ExtensionSourceConfig,
) -> Result<SkillCatalogItem, Box<dyn Error>> {
    let manifest = crate::skill::manifest::load_skill_manifest(dir)?;
    let capability_ids: Vec<String> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    let mut categories = manifest.categories.clone();
    // 同插件：显式分类优先，缺失时按 Capability 命名空间兜底。
    crate::extension_category::fill_missing_categories(&capability_ids, &mut categories);
    Ok(SkillCatalogItem {
        skill_id: manifest.id.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        author_name: manifest.author.clone(),
        categories,
        version: manifest.version.clone(),
        release_notes: manifest.release_notes.clone(),
        published_at: String::new(),
        min_agent_version: manifest.min_agent_version.clone(),
        supported_clients: manifest.supported_clients.clone(),
        capability_ids,
        plugin_dependencies: manifest
            .plugin_dependencies
            .iter()
            .map(
                |dependency| crate::api::distribution::SkillPluginDependency {
                    plugin_id: dependency.plugin_id.clone(),
                    required: dependency.required,
                    min_version: dependency.min_version.clone().unwrap_or_default(),
                },
            )
            .collect(),
        risk_summary: manifest.risk_summary.clone(),
        channel: "local".to_string(),
        artifact_id: String::new(),
        file_name: String::new(),
        file_size: 0,
        sha256: String::new(),
        signature: String::new(),
        signature_key_id: String::new(),
        signature_algorithm: String::new(),
        download_url: format!("local:{}", dir.display()),
        source: format!("local:{}", source.id),
        assignment: "optional".to_string(),
        management: "user_managed".to_string(),
        install_mode: "prompt".to_string(),
        organization_reason: String::new(),
        managed: false,
        allow_disable: true,
        allow_uninstall: true,
    })
}

fn build_local_workflow_item(
    dir: &Path,
    source: &ExtensionSourceConfig,
) -> Result<WorkflowCatalogItem, Box<dyn Error>> {
    let package = crate::workflow::load_from_directory(dir)?;
    // Workflow Package 不声明分类，catalog 项按 Capability 命名空间补齐，
    // 否则工作流在整个扩展市场里只能落进「未分类」。
    let categories = crate::extension_category::infer_categories(&package.capabilities);
    Ok(WorkflowCatalogItem {
        workflow_id: package.id.clone(),
        name: package.name.clone(),
        description: package.description.clone(),
        author_name: String::new(),
        categories,
        version: package.version.clone(),
        // 与插件、技能一样取自清单：本地源是开发者的预览视图，更新说明要能
        // 在发布前就看见，否则「写没写说明」只能等到发到市场才知道。
        release_notes: package.release_notes.clone(),
        published_at: String::new(),
        min_agent_version: package.min_agent_version.clone(),
        capability_ids: package.capabilities.clone(),
        channel: "local".to_string(),
        artifact_id: String::new(),
        file_name: String::new(),
        file_size: 0,
        sha256: String::new(),
        signature: String::new(),
        signature_key_id: String::new(),
        signature_algorithm: String::new(),
        download_url: format!("local:{}", dir.display()),
        source: format!("local:{}", source.id),
        assignment: "optional".to_string(),
        management: "user_managed".to_string(),
        install_mode: "prompt".to_string(),
        organization_reason: String::new(),
        managed: false,
        allow_disable: true,
        allow_uninstall: true,
        extension_lock: None,
    })
}

fn build_local_expert_item(
    dir: &Path,
    source: &ExtensionSourceConfig,
) -> Result<ExpertCatalogItem, Box<dyn Error>> {
    let definition: crate::expert::ExpertDefinition =
        serde_json::from_slice(&fs::read(dir.join("expert.json"))?)?;
    crate::expert::validate_definition(&definition)?;
    let instructions = fs::read(dir.join("EXPERT.md"))?;
    if definition.instructions.as_bytes() != instructions {
        return Err("expert.json 与 EXPERT.md 内容不一致".into());
    }
    Ok(ExpertCatalogItem {
        expert_id: definition.id,
        name: definition.name,
        description: definition.description,
        author_name: definition.author,
        categories: definition.categories,
        version: definition.version,
        release_notes: definition.release_notes,
        published_at: String::new(),
        min_agent_version: definition.min_agent_version,
        supported_clients: definition.supported_clients,
        product_id: String::new(),
        release_id: String::new(),
        artifact_id: String::new(),
        file_name: String::new(),
        file_size: 0,
        sha256: String::new(),
        signature: String::new(),
        signature_key_id: String::new(),
        signature_algorithm: String::new(),
        download_url: format!("local:{}", dir.display()),
        source: format!("local:{}", source.id),
        assignment: "optional".to_string(),
        management: "user_managed".to_string(),
        install_mode: "prompt".to_string(),
        managed: false,
        allow_disable: true,
        allow_uninstall: true,
    })
}

/// 反查分发单元关联的开发项目，让界面把「源码工作区 → 本机生效」串起来。
fn attach_unit_projects(units: &mut [ExtensionDistributionUnit]) {
    let projects = crate::extension_projects::list().unwrap_or_default();
    for unit in units.iter_mut() {
        unit.project_ids = projects
            .iter()
            .filter(|project| project.source_unit_key == unit.unit_key)
            .map(|project| project.id.clone())
            .collect();
        unit.project_ids.sort();
    }
}

/// 已启用本地扩展源声明的扩展源码目录。
///
/// 这是「开发项目绑定哪个目录」的唯一权威来源：`extensions.json` 里的 `path`
/// 就是开发者真正编辑、构建、提交的源码工作区。项目登记不得再退回到
/// Agent 内部的 draft / test-package 目录，否则「用 AI 开发」会改到产物副本，
/// 下一次构建即被覆盖。
pub(crate) fn local_source_workspaces() -> Vec<LocalSourceWorkspace> {
    authoritative_local_sources()
        .into_iter()
        .flat_map(|snapshot| snapshot.workspaces)
        .collect()
}

/// 已成功读取出清单的本地扩展源快照。
///
/// 只有这里返回的源才有资格宣告「某个扩展已经不存在」：目录不可读或清单解析失败
/// 时不能据此删除项目登记，否则一次临时拔盘就会丢掉开发者的项目绑定。
pub(crate) fn authoritative_local_sources() -> Vec<LocalSourceSnapshot> {
    let Ok(current) = settings() else {
        return Vec::new();
    };
    let mut snapshots = Vec::new();
    // Source enablement controls market discovery and installation only. A
    // developer must still be able to open and build a registered workspace
    // while its distribution side is paused, so authoring discovery includes
    // every configured local source and validates it independently below.
    for source in current
        .sources
        .iter()
        .filter(|source| source.kind == ExtensionSourceKind::Local)
    {
        let root = PathBuf::from(&source.repository);
        let Ok(content) = fs::read_to_string(root.join(&source.catalog_path)) else {
            continue;
        };
        let Ok(aggregate) = serde_json::from_str::<LocalAggregateCatalog>(&content) else {
            continue;
        };
        let mut workspaces = Vec::new();
        for entry in &aggregate.extensions {
            let Ok(path) = safe_local_child(&root, &entry.path) else {
                continue;
            };
            workspaces.push(LocalSourceWorkspace {
                kind: entry.kind.clone(),
                extension_id: entry.id.clone(),
                path,
                repository: normalize_repository_key(&source.upstream_repository),
                subdirectory: entry.path.replace('\\', "/"),
                unit_key: unit_key_of(source),
            });
        }
        snapshots.push(LocalSourceSnapshot {
            root_key: normalize_path_key(&path_key_of(Path::new(&source.repository))),
            repository: normalize_repository_key(&source.upstream_repository),
            default_branch: default_branch_of(&aggregate),
            workspaces,
        });
    }
    snapshots
}

#[derive(Debug, Clone)]
pub(crate) struct LocalSourceSnapshot {
    /// 源根目录的归一化键，用于按路径判断某条项目登记属于这个源。
    root_key: String,
    /// 源声明的上游仓库键，用于跨机器比对同一条项目登记。
    repository: String,
    /// 源声明的默认分支，用于补登记时填写发布分支，避免空分支阻断发布。
    pub default_branch: String,
    pub workspaces: Vec<LocalSourceWorkspace>,
}

impl LocalSourceSnapshot {
    /// 项目登记是否属于这个源。
    ///
    /// 两边都声明了仓库时以仓库键为准：登记可能来自另一台机器，本地路径对不上，
    /// 但上游仓库一致，仍属同一个源；反过来，仓库不同就一定不是这个源的项目，
    /// 避免同一目录下并存的多仓库被误删。只有至少一侧没有仓库信息时，
    /// 才退化成「目录是否落在源根之下」。
    pub fn owns(&self, repository: &str, workspace_path: &Path) -> bool {
        let registry_key = normalize_repository_key(repository);
        if !self.repository.is_empty() && !registry_key.is_empty() {
            return self.repository == registry_key;
        }
        let path_key = normalize_path_key(&path_key_of(workspace_path));
        !self.root_key.is_empty()
            && (path_key == self.root_key || path_key.starts_with(&format!("{}/", self.root_key)))
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        root: &Path,
        repository: &str,
        default_branch: &str,
        workspaces: Vec<LocalSourceWorkspace>,
    ) -> Self {
        Self {
            root_key: normalize_path_key(&path_key_of(root)),
            repository: normalize_repository_key(repository),
            default_branch: default_branch.to_string(),
            workspaces,
        }
    }
}

/// 统一成可比较的路径文本：登记里的路径经过 canonicalize，是 Windows 的
/// `\\?\` 扩展前缀写法，而设置里的源根目录是普通写法，两边必须先对齐。
fn path_key_of(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy().to_string();
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text)
}

#[derive(Debug, Clone)]
pub(crate) struct LocalSourceWorkspace {
    pub kind: String,
    pub extension_id: String,
    pub path: PathBuf,
    pub repository: String,
    pub subdirectory: String,
    pub unit_key: String,
}

fn safe_local_child(root: &Path, relative: &str) -> Result<PathBuf, Box<dyn Error>> {
    let relative_path = Path::new(relative);
    if relative.trim().is_empty() || relative_path.is_absolute() || relative.contains('\\') {
        return Err(format!("扩展目录路径无效: {relative}").into());
    }
    let candidate = root.join(relative_path);
    let canonical = candidate.canonicalize()?;
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("扩展目录路径越界: {relative}").into());
    }
    Ok(canonical)
}

#[derive(Debug, Deserialize)]
struct LocalAggregateCatalog {
    #[serde(default)]
    repository: String,
    /// Stable product identity shared with the published catalog. When omitted
    /// the repository (or Git origin) is used for backwards compatibility.
    #[serde(default)]
    distribution_id: String,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    catalog_id: String,
    /// 源声明的默认分支；缺省时按 `main` 兜底，与 `extensions.json` 的既有写法一致。
    #[serde(default)]
    default_branch: String,
    extensions: Vec<LocalAggregateExtension>,
}

fn default_branch_of(aggregate: &LocalAggregateCatalog) -> String {
    let declared = aggregate.default_branch.trim();
    if !declared.is_empty() {
        return declared.to_string();
    }
    "main".to_string()
}

#[derive(Debug, Deserialize)]
struct LocalAggregateExtension {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    path: String,
}

impl LocalAggregateCatalog {
    fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.extensions.is_empty() {
            return Err("extensions.json 未声明任何扩展".into());
        }
        let mut ids = std::collections::HashSet::new();
        for item in &self.extensions {
            if item.id.trim().is_empty() || item.path.trim().is_empty() {
                return Err("extensions.json 包含空的扩展 ID 或目录".into());
            }
            if !matches!(
                item.kind.as_str(),
                "plugin" | "skill" | "workflow" | "expert"
            ) {
                return Err(format!("extensions.json 包含不支持的扩展类型: {}", item.kind).into());
            }
            if !ids.insert(format!("{}:{}", item.kind, item.id.trim())) {
                return Err(format!("extensions.json 包含重复扩展 ID: {}", item.id).into());
            }
        }
        Ok(())
    }
}

fn aggregate_distribution_id(aggregate: &LocalAggregateCatalog, root: &Path) -> String {
    let declared = normalize_distribution_id(&aggregate.distribution_id);
    if !declared.is_empty() {
        return declared;
    }
    let upstream = local_upstream_repository(root, &aggregate.repository);
    if !upstream.is_empty() {
        return normalize_distribution_id(&upstream);
    }
    local_distribution_fallback(&root.display().to_string())
}

fn local_distribution_fallback(value: &str) -> String {
    let digest = Sha256::digest(normalize_path_key(value).as_bytes());
    format!(
        "local-{:.16x}",
        u64::from_be_bytes(digest[..8].try_into().unwrap())
    )
}

fn aggregate_channel(aggregate: &LocalAggregateCatalog) -> String {
    let channel = normalize_channel(&aggregate.channel);
    if channel.is_empty() {
        DEFAULT_DISTRIBUTION_CHANNEL.to_string()
    } else {
        channel
    }
}

fn aggregate_catalog_id(aggregate: &LocalAggregateCatalog) -> String {
    let catalog_id = normalize_catalog_id(&aggregate.catalog_id);
    if catalog_id.is_empty() {
        DEFAULT_CATALOG_ID.to_string()
    } else {
        catalog_id
    }
}

fn normalize_repository(value: &str) -> Result<String, Box<dyn Error>> {
    Ok(crate::app::github_source::parse_source_url(value)?.repository)
}

fn normalize_distribution_id(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn normalize_channel(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_catalog_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn default_distribution_channel() -> String {
    DEFAULT_DISTRIBUTION_CHANNEL.to_string()
}

fn default_catalog_id() -> String {
    DEFAULT_CATALOG_ID.to_string()
}

fn validate_distribution_field(
    value: &str,
    label: &str,
    allow_slash: bool,
) -> Result<(), Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 160
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-')
                || (allow_slash && byte == b'/')
        })
    {
        return Err(format!("{label}格式无效").into());
    }
    Ok(())
}

/// 解析本地聚合目录对应的 GitHub 上游仓库（owner/repo）。
/// 优先使用 `extensions.json` 的 `repository` 声明，其次回退到该目录的 git remote origin。
fn local_upstream_repository(root: &Path, declared: &str) -> String {
    let declared = declared.trim();
    if !declared.is_empty() {
        if let Ok(repository) = normalize_repository(declared) {
            return repository;
        }
    }
    git_origin_repository(root).unwrap_or_default()
}

fn git_origin_repository(root: &Path) -> Option<String> {
    let mut command = crate::runtime::process::hidden_command("git");
    command
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"]);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    normalize_git_remote(&value)
}

/// Read immutable Git provenance for a local source without treating an
/// uncommitted checkout as a publishable artifact. The UI uses `dirty` to make
/// that distinction explicit; failures simply mean the directory is not a Git
/// working tree and are represented by empty commit/tree values.
fn local_source_revision(root: &Path) -> (String, String, bool) {
    let commit = git_revision_output(root, &["rev-parse", "HEAD"]);
    let tree = git_revision_output(root, &["rev-parse", "HEAD^{tree}"]);
    let dirty = {
        let mut command = crate::runtime::process::hidden_command("git");
        command
            .arg("-C")
            .arg(root)
            .args(["status", "--porcelain", "--untracked-files=all"]);
        command
            .output()
            .map(|output| output.status.success() && !output.stdout.is_empty())
            .unwrap_or(false)
    };
    (commit, tree, dirty)
}

fn git_revision_output(root: &Path, args: &[&str]) -> String {
    let mut command = crate::runtime::process::hidden_command("git");
    command.arg("-C").arg(root).args(args);
    let Ok(output) = command.output() else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn normalize_git_remote(value: &str) -> Option<String> {
    let value = value.trim();
    let value = if value.contains("://") {
        value
    } else if let Some((_, path)) = value.rsplit_once(':') {
        path
    } else {
        value
    };
    normalize_repository(value).ok()
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
    if !matches!(kind, "plugin" | "skill" | "workflow" | "expert") {
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

// ---------------------------------------------------------------------------
// 一键批量更新
//
// 市场与「我的能力」都只比较版本号来判断「可更新」，但安装路径是按来源绑定的：
// 同一个扩展在多个来源里都有更高版本时，只有本机安装台账能证明哪个来源才是
// 当初安装它的来源。批量更新如果选错来源，轻则整批报错，重则把组织策略下发的
// 版本覆盖成本地开发版本。因此这里先把候选分成三组：
//   ready   —— 台账来源与本次更新来源一致，可安全批量更新；
//   review  —— 无台账或来源已变更，必须由用户显式确认后才更新；
//   managed —— 组织直接管理版本，不参与手动批量更新。
// ---------------------------------------------------------------------------

/// 批量更新分组：来源已核对，可直接更新。
pub(crate) const EXTENSION_UPDATE_GROUP_READY: &str = "ready";
/// 批量更新分组：来源无法核对，需要用户显式确认。
pub(crate) const EXTENSION_UPDATE_GROUP_REVIEW: &str = "review";
/// 批量更新分组：跟随组织策略，不参与批量更新。
pub(crate) const EXTENSION_UPDATE_GROUP_MANAGED: &str = "managed";

/// 组织下发来源的固定标识，出现在安装台账里。
const ORGANIZATION_SOURCE_ID: &str = "organization";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionUpdateCandidate {
    pub asset_kind: String,
    pub asset_id: String,
    pub name: String,
    pub installed_version: String,
    pub target_version: String,
    pub source_id: String,
    pub source_name: String,
    pub channel: String,
    pub sha256: String,
    pub artifact_id: String,
    pub group: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExtensionUpdateTarget {
    pub asset_kind: String,
    pub asset_id: String,
    pub version: String,
    pub source_id: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub artifact_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionUpdateOutcome {
    pub asset_kind: String,
    pub asset_id: String,
    pub name: String,
    pub from_version: String,
    pub to_version: String,
    /// updated | failed | cancelled
    pub status: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionBatchUpdateReport {
    pub outcomes: Vec<ExtensionUpdateOutcome>,
    pub updated_count: usize,
    pub failed_count: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionUpdateProgress {
    pub index: usize,
    pub total: usize,
    pub asset_kind: String,
    pub asset_id: String,
    pub name: String,
    pub from_version: String,
    pub to_version: String,
    /// running | updated | failed | cancelled
    pub status: String,
    pub message: String,
}

/// 本机安装台账里登记过的来源。台账是「这个扩展当初从哪来」的唯一凭据：
/// 来源台账（provenance）优先，它随每次来源安装写入并带制品摘要。
struct UpdateLedger {
    source_id: String,
}

fn update_ledger(
    provenance: &[ExtensionProvenance],
    lock: &crate::app::extension_lock::ExtensionLockFile,
    asset_kind: &str,
    asset_id: &str,
) -> UpdateLedger {
    let recorded = provenance
        .iter()
        .find(|item| item.asset_kind == asset_kind && item.asset_key == asset_id)
        .map(|item| item.source_id.trim().to_string())
        .filter(|value| !value.is_empty());
    let locked = lock
        .entries
        .get(&format!("{asset_kind}:{asset_id}"))
        .map(|item| item.source_id.trim().to_string())
        .filter(|value| !value.is_empty());
    UpdateLedger {
        source_id: recorded.or(locked).unwrap_or_default(),
    }
}

/// 目录项里的 `local:xxx` / `github:xxx` 前缀剥掉，得到与台账同构的来源 ID。
fn catalog_source_id(source: &str) -> String {
    source
        .strip_prefix("github:")
        .or_else(|| source.strip_prefix("local:"))
        .unwrap_or(source)
        .trim()
        .to_string()
}

fn update_source_name(snapshot: &ExtensionSourceSnapshot, source: &str) -> String {
    let source_id = catalog_source_id(source);
    let resolved = snapshot
        .sources
        .iter()
        .map(|status| &status.source)
        .find(|config| config.id == source_id)
        .map(|config| config.name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or(source_id);
    friendly_source_label(&resolved)
}

/// 本地目录来源没起名字时，来源配置里存的就是绝对路径，直接显示会把
/// `F:\WebProjects\himind-extensions` 这种本机路径带到用户面前。这里按市场页
/// `friendlySourceName` 的同一口径收敛成目录名，非路径来源（如 `owner/repo`）原样保留。
fn friendly_source_label(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let bytes = trimmed.as_bytes();
    let drive_path = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    let unc_path = trimmed.contains("\\\\");
    if !drive_path && !unc_path {
        return trimmed.to_string();
    }
    let cleaned = trimmed.trim_end_matches(['\\', '/']);
    cleaned
        .rsplit(['\\', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(cleaned)
        .to_string()
}

/// 从候选里挑出要安装的目标：优先沿用台账登记过的来源，其次取最高版本。
fn pick_update_target<T>(candidates: &[(String, String, T)], ledger_source: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_same_source: Option<usize> = None;
    for (index, (version, source_id, _)) in candidates.iter().enumerate() {
        let newer = |current: usize| {
            crate::skill::resolver::compare_versions(version, &candidates[current].0)
                == std::cmp::Ordering::Greater
        };
        if !ledger_source.is_empty()
            && source_id == ledger_source
            && best_same_source.map_or(true, newer)
        {
            best_same_source = Some(index);
        }
        if best.map_or(true, newer) {
            best = Some(index);
        }
    }
    best_same_source.or(best)
}

fn classify_update(
    ledger_source: &str,
    target_source: &str,
    target_source_name: &str,
    managed: bool,
) -> (&'static str, String) {
    if managed {
        // 分组标题已经写清了组织策略，逐项再重复一遍只会把清单撑长。
        return (EXTENSION_UPDATE_GROUP_MANAGED, String::new());
    }
    if target_source.is_empty() {
        return (
            EXTENSION_UPDATE_GROUP_REVIEW,
            "这次更新没有可核对的来源".to_string(),
        );
    }
    if ledger_source.is_empty() {
        return (
            EXTENSION_UPDATE_GROUP_REVIEW,
            "本机没有安装来源记录".to_string(),
        );
    }
    if ledger_source != target_source {
        return (
            EXTENSION_UPDATE_GROUP_REVIEW,
            format!("本机安装来源与本次更新来源（{target_source_name}）不一致"),
        );
    }
    (EXTENSION_UPDATE_GROUP_READY, String::new())
}

/// 列出所有「版本可更新」的扩展，并给出批量更新的安全分组。
pub(crate) fn plan_extension_updates() -> Result<Vec<ExtensionUpdateCandidate>, Box<dyn Error>> {
    let snapshot = snapshot()?;
    let lock = crate::app::extension_lock::load()?;
    let provenance = list_provenance()?;
    let mut candidates = Vec::new();
    candidates.extend(plan_plugin_updates(&snapshot, &lock, &provenance));
    candidates.extend(plan_skill_updates(&snapshot, &lock, &provenance));
    candidates.extend(plan_workflow_updates(&snapshot, &lock, &provenance));
    candidates.sort_by(|left, right| {
        left.asset_kind
            .cmp(&right.asset_kind)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(candidates)
}

fn plan_plugin_updates(
    snapshot: &ExtensionSourceSnapshot,
    lock: &crate::app::extension_lock::ExtensionLockFile,
    provenance: &[ExtensionProvenance],
) -> Vec<ExtensionUpdateCandidate> {
    let mut by_id: BTreeMap<&str, Vec<&PluginCatalogItem>> = BTreeMap::new();
    for item in &snapshot.plugins {
        by_id.entry(item.plugin_id.as_str()).or_default().push(item);
    }
    let mut out = Vec::new();
    for (plugin_id, items) in by_id {
        let installed = crate::app::plugin_manager::local_status(plugin_id).current_version;
        if installed.trim().is_empty() {
            continue;
        }
        let available = items
            .iter()
            .filter(|item| {
                crate::skill::resolver::compare_versions(&item.version, &installed)
                    == std::cmp::Ordering::Greater
            })
            .map(|item| (item.version.clone(), catalog_source_id(&item.source), *item))
            .collect::<Vec<_>>();
        let ledger = update_ledger(provenance, lock, "plugin", plugin_id);
        let Some(index) = pick_update_target(&available, &ledger.source_id) else {
            continue;
        };
        let (version, source_id, item) = &available[index];
        let source_name = update_source_name(snapshot, &item.source);
        let managed = matches!(
            crate::app::plugin_manager::local_governance(plugin_id).as_str(),
            "managed" | "required"
        ) || ledger.source_id == ORGANIZATION_SOURCE_ID
            || item.managed
            || matches!(item.governance.as_str(), "managed" | "required")
            || item.assignment == "required";
        let (group, reason) = classify_update(&ledger.source_id, source_id, &source_name, managed);
        out.push(ExtensionUpdateCandidate {
            asset_kind: "plugin".to_string(),
            asset_id: plugin_id.to_string(),
            name: if item.name.trim().is_empty() {
                plugin_id.to_string()
            } else {
                item.name.clone()
            },
            installed_version: installed,
            target_version: version.clone(),
            source_id: source_id.clone(),
            source_name,
            channel: item.channel.clone(),
            sha256: item.sha256.clone(),
            artifact_id: item.artifact_id.clone(),
            group: group.to_string(),
            reason,
        });
    }
    out
}

fn plan_skill_updates(
    snapshot: &ExtensionSourceSnapshot,
    lock: &crate::app::extension_lock::ExtensionLockFile,
    provenance: &[ExtensionProvenance],
) -> Vec<ExtensionUpdateCandidate> {
    let store = crate::skill::store::SkillStore::new();
    let mut by_id: BTreeMap<&str, Vec<&SkillCatalogItem>> = BTreeMap::new();
    for item in &snapshot.skills {
        by_id.entry(item.skill_id.as_str()).or_default().push(item);
    }
    let mut out = Vec::new();
    for (skill_id, items) in by_id {
        let installed = store
            .get_record(skill_id)
            .ok()
            .flatten()
            .map(|record| record.manifest.version)
            .unwrap_or_default();
        if installed.trim().is_empty() {
            continue;
        }
        let available = items
            .iter()
            .filter(|item| {
                crate::skill::resolver::compare_versions(&item.version, &installed)
                    == std::cmp::Ordering::Greater
            })
            .map(|item| (item.version.clone(), catalog_source_id(&item.source), *item))
            .collect::<Vec<_>>();
        let ledger = update_ledger(provenance, lock, "skill", skill_id);
        let Some(index) = pick_update_target(&available, &ledger.source_id) else {
            continue;
        };
        let (version, source_id, item) = &available[index];
        let source_name = update_source_name(snapshot, &item.source);
        let managed = ledger.source_id == ORGANIZATION_SOURCE_ID
            || item.managed
            || item.assignment == "required"
            || item.management != "user_managed";
        let (group, reason) = classify_update(&ledger.source_id, source_id, &source_name, managed);
        out.push(ExtensionUpdateCandidate {
            asset_kind: "skill".to_string(),
            asset_id: skill_id.to_string(),
            name: if item.name.trim().is_empty() {
                skill_id.to_string()
            } else {
                item.name.clone()
            },
            installed_version: installed,
            target_version: version.clone(),
            source_id: source_id.clone(),
            source_name,
            channel: item.channel.clone(),
            sha256: item.sha256.clone(),
            artifact_id: item.artifact_id.clone(),
            group: group.to_string(),
            reason,
        });
    }
    out
}

fn plan_workflow_updates(
    snapshot: &ExtensionSourceSnapshot,
    lock: &crate::app::extension_lock::ExtensionLockFile,
    provenance: &[ExtensionProvenance],
) -> Vec<ExtensionUpdateCandidate> {
    let installed_versions = crate::workflow::WorkflowStore::open_default()
        .and_then(|store| store.list())
        .map(|workflows| {
            workflows
                .into_iter()
                .map(|workflow| (workflow.package.id, workflow.package.version))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut by_id: BTreeMap<&str, Vec<&WorkflowCatalogItem>> = BTreeMap::new();
    for item in &snapshot.workflows {
        by_id
            .entry(item.workflow_id.as_str())
            .or_default()
            .push(item);
    }
    let mut out = Vec::new();
    for (workflow_id, items) in by_id {
        let Some(installed) = installed_versions.get(workflow_id).cloned() else {
            continue;
        };
        if installed.trim().is_empty() {
            continue;
        }
        let available = items
            .iter()
            .filter(|item| {
                crate::skill::resolver::compare_versions(&item.version, &installed)
                    == std::cmp::Ordering::Greater
            })
            .map(|item| (item.version.clone(), catalog_source_id(&item.source), *item))
            .collect::<Vec<_>>();
        let ledger = update_ledger(provenance, lock, "workflow", workflow_id);
        let Some(index) = pick_update_target(&available, &ledger.source_id) else {
            continue;
        };
        let (version, source_id, item) = &available[index];
        let source_name = update_source_name(snapshot, &item.source);
        let managed = ledger.source_id == ORGANIZATION_SOURCE_ID
            || item.managed
            || item.assignment == "required"
            || item.management != "user_managed";
        let (group, reason) = classify_update(&ledger.source_id, source_id, &source_name, managed);
        out.push(ExtensionUpdateCandidate {
            asset_kind: "workflow".to_string(),
            asset_id: workflow_id.to_string(),
            name: if item.name.trim().is_empty() {
                workflow_id.to_string()
            } else {
                item.name.clone()
            },
            installed_version: installed,
            target_version: version.clone(),
            source_id: source_id.clone(),
            source_name,
            channel: item.channel.clone(),
            sha256: item.sha256.clone(),
            artifact_id: item.artifact_id.clone(),
            group: group.to_string(),
            reason,
        });
    }
    out
}

fn installed_extension_version(asset_kind: &str, asset_id: &str) -> String {
    match asset_kind {
        "plugin" => crate::app::plugin_manager::local_status(asset_id).current_version,
        "skill" => crate::skill::store::SkillStore::new()
            .get_record(asset_id)
            .ok()
            .flatten()
            .map(|record| record.manifest.version)
            .unwrap_or_default(),
        "workflow" => crate::workflow::WorkflowStore::open_default()
            .and_then(|store| store.list())
            .map(|workflows| {
                workflows
                    .into_iter()
                    .find(|workflow| workflow.package.id == asset_id)
                    .map(|workflow| workflow.package.version)
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// 在最新快照里复核一个更新目标：版本、来源、制品摘要都必须与预览时一致，
/// 否则说明来源目录在用户确认前后发生了变化，按失败处理而不是照旧安装。
fn resolve_update_target(
    snapshot: &ExtensionSourceSnapshot,
    target: &ExtensionUpdateTarget,
) -> Result<String, Box<dyn Error>> {
    let matches = |source: &str, version: &str| {
        version == target.version && catalog_source_id(source) == target.source_id
    };
    let (name, source, sha256, artifact_id) = match target.asset_kind.as_str() {
        "plugin" => snapshot
            .plugins
            .iter()
            .find(|item| item.plugin_id == target.asset_id && matches(&item.source, &item.version))
            .map(|item| {
                (
                    item.name.clone(),
                    item.source.clone(),
                    item.sha256.clone(),
                    item.artifact_id.clone(),
                )
            }),
        "skill" => snapshot
            .skills
            .iter()
            .find(|item| item.skill_id == target.asset_id && matches(&item.source, &item.version))
            .map(|item| {
                (
                    item.name.clone(),
                    item.source.clone(),
                    item.sha256.clone(),
                    item.artifact_id.clone(),
                )
            }),
        "workflow" => snapshot
            .workflows
            .iter()
            .find(|item| {
                item.workflow_id == target.asset_id && matches(&item.source, &item.version)
            })
            .map(|item| {
                (
                    item.name.clone(),
                    item.source.clone(),
                    item.sha256.clone(),
                    item.artifact_id.clone(),
                )
            }),
        other => return Err(format!("不支持的扩展类型: {other}").into()),
    }
    .ok_or_else(|| {
        format!(
            "{} v{} 已不在来源目录中，请刷新市场后重试",
            target.asset_id, target.version
        )
    })?;
    let expected_sha = target.sha256.trim();
    if !expected_sha.is_empty() && !sha256.eq_ignore_ascii_case(expected_sha) {
        return Err(format!("{} 的制品摘要已变化，请刷新市场后重试", target.asset_id).into());
    }
    let expected_artifact = target.artifact_id.trim();
    if !expected_artifact.is_empty() && artifact_id != expected_artifact {
        return Err(format!("{} 的制品已变更，请刷新市场后重试", target.asset_id).into());
    }
    source_for_catalog_item(snapshot, &source)?;
    Ok(if name.trim().is_empty() {
        target.asset_id.clone()
    } else {
        name
    })
}

fn installed_sha256_of(target: &ExtensionUpdateTarget) -> Option<&str> {
    let value = target.sha256.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 执行批量更新。逐项独立成败：某项失败不影响已成功的项，也不会留半装状态，
/// 因为每个扩展的安装本身仍然走按来源绑定的补偿式安装。
pub(crate) fn apply_extension_updates(
    targets: &[ExtensionUpdateTarget],
    cancel: &std::sync::atomic::AtomicBool,
    mut on_progress: impl FnMut(ExtensionUpdateProgress),
) -> Result<ExtensionBatchUpdateReport, Box<dyn Error>> {
    let snapshot = snapshot()?;
    // 插件先于技能：技能的插件依赖在插件更新后就能满足，避免依赖校验先行失败。
    let mut ordered: Vec<&ExtensionUpdateTarget> = Vec::new();
    for kind in ["plugin", "skill", "workflow"] {
        ordered.extend(targets.iter().filter(|target| target.asset_kind == kind));
    }
    let total = ordered.len();
    let mut report = ExtensionBatchUpdateReport {
        outcomes: Vec::new(),
        updated_count: 0,
        failed_count: 0,
        cancelled: false,
    };
    let mut plugin_touched = false;
    for (offset, target) in ordered.iter().enumerate() {
        let from_version = installed_extension_version(&target.asset_kind, &target.asset_id);
        let cancelled = cancel.load(std::sync::atomic::Ordering::SeqCst);
        let mut progress = ExtensionUpdateProgress {
            index: offset + 1,
            total,
            asset_kind: target.asset_kind.clone(),
            asset_id: target.asset_id.clone(),
            name: target.asset_id.clone(),
            from_version: from_version.clone(),
            to_version: target.version.clone(),
            status: if cancelled { "cancelled" } else { "running" }.to_string(),
            message: if cancelled {
                "已取消，未执行".to_string()
            } else {
                String::new()
            },
        };
        if cancelled {
            report.cancelled = true;
            report.outcomes.push(ExtensionUpdateOutcome {
                asset_kind: target.asset_kind.clone(),
                asset_id: target.asset_id.clone(),
                name: target.asset_id.clone(),
                from_version,
                to_version: target.version.clone(),
                status: "cancelled".to_string(),
                message: "已取消，未执行".to_string(),
                retryable: true,
            });
            on_progress(progress);
            continue;
        }
        let resolved = resolve_update_target(&snapshot, target);
        let result = match resolved {
            Err(error) => Err(error),
            Ok(name) => {
                progress.name = name;
                on_progress(progress.clone());
                match target.asset_kind.as_str() {
                    "plugin" => install_plugin_bound(
                        &target.asset_id,
                        Some(&target.version),
                        Some(&target.source_id),
                        installed_sha256_of(target),
                    )
                    .map(|_| ()),
                    "skill" => install_skill_bound(
                        &target.asset_id,
                        Some(&target.version),
                        Some(&target.source_id),
                        installed_sha256_of(target),
                    )
                    .map(|_| ()),
                    "workflow" => install_workflow_bound(
                        &target.asset_id,
                        Some(&target.version),
                        Some(&target.source_id),
                        installed_sha256_of(target),
                    )
                    .map(|_| ()),
                    other => Err(format!("不支持的扩展类型: {other}").into()),
                }
            }
        };
        match result {
            Ok(()) => {
                if target.asset_kind == "plugin" {
                    plugin_touched = true;
                }
                report.updated_count += 1;
                report.outcomes.push(ExtensionUpdateOutcome {
                    asset_kind: target.asset_kind.clone(),
                    asset_id: target.asset_id.clone(),
                    name: progress.name.clone(),
                    from_version,
                    to_version: target.version.clone(),
                    status: "updated".to_string(),
                    message: String::new(),
                    retryable: false,
                });
                progress.status = "updated".to_string();
            }
            Err(error) => {
                let message = error.to_string();
                report.failed_count += 1;
                report.outcomes.push(ExtensionUpdateOutcome {
                    asset_kind: target.asset_kind.clone(),
                    asset_id: target.asset_id.clone(),
                    name: progress.name.clone(),
                    from_version,
                    to_version: target.version.clone(),
                    status: "failed".to_string(),
                    message: message.clone(),
                    retryable: true,
                });
                progress.status = "failed".to_string();
                progress.message = message;
            }
        }
        on_progress(progress);
    }
    if plugin_touched {
        crate::capability::service::invalidate_capability_discovery();
    }
    Ok(report)
}

/// 批量更新的协作式取消标志。一次只允许一个批量任务在跑，因此用进程内单例
/// 就够了；它只影响「还没开始安装的项」，不打断正在写入的单个扩展。
static BATCH_UPDATE_CANCEL: OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
    OnceLock::new();

fn batch_update_cancel() -> &'static std::sync::atomic::AtomicBool {
    BATCH_UPDATE_CANCEL
        .get_or_init(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
}

pub(crate) fn reset_extension_update_cancel() {
    batch_update_cancel().store(false, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn cancel_extension_updates() {
    batch_update_cancel().store(true, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn extension_update_cancel_flag() -> &'static std::sync::atomic::AtomicBool {
    batch_update_cancel()
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

    fn workflow_item(url: &str) -> WorkflowCatalogItem {
        WorkflowCatalogItem {
            workflow_id: "com.himind.workflow.test".to_string(),
            name: "Workflow Test".to_string(),
            description: String::new(),
            author_name: String::new(),
            categories: Vec::new(),
            version: "1.0.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: "0.3.0".to_string(),
            capability_ids: vec!["system.health".to_string()],
            channel: "stable".to_string(),
            artifact_id: String::new(),
            file_name: "test.hmwf".to_string(),
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
            extension_lock: None,
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
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: ".himind/catalog.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Required,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let mut item = plugin_item("https://github.com/Owner/repo/releases/download/v1/test.hmpkg");
        normalize_plugin_item(&mut item, &source).unwrap();
        assert_eq!(item.governance, "optional");
        assert_eq!(item.management, "user_managed");
        assert!(!item.managed);
        assert!(item.allow_disable && item.allow_uninstall);
    }

    #[test]
    fn github_workflow_catalog_item_is_validated_and_normalized() {
        let source = ExtensionSourceConfig {
            id: "github-test".to_string(),
            name: "测试".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: ".himind/catalog.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let mut item =
            workflow_item("https://github.com/Owner/repo/releases/download/v1/test.hmwf");
        item.signature.clear();
        item.signature_key_id.clear();
        item.signature_algorithm.clear();
        let catalog = ExtensionSourceCatalog {
            schema_version: CATALOG_SCHEMA_VERSION,
            source_id: source.id.clone(),
            generation: String::new(),
            plugins: Vec::new(),
            skills: Vec::new(),
            workflows: vec![item.clone()],
            experts: Vec::new(),
            feature_packs: Vec::new(),
            agent_presets: Vec::new(),
            distribution_id: String::new(),
            channel: DEFAULT_DISTRIBUTION_CHANNEL.to_string(),
            catalog_id: DEFAULT_CATALOG_ID.to_string(),
        };
        validate_catalog(&catalog, &source).unwrap();
        normalize_workflow_item(&mut item, &source).unwrap();
        assert_eq!(item.source, format!("github:{}", source.id));
        assert_eq!(item.management, "user_managed");
        assert!(!item.managed);
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
    fn plugin_dependencies_stay_on_the_selected_source() {
        let mut root = plugin_item("https://github.com/Owner/a/releases/download/v1/root.hmpkg");
        root.plugin_id = "com.himind.root".to_string();
        root.source = "github:source-a".to_string();
        root.plugin_dependencies = vec![crate::api::distribution::SkillPluginDependency {
            plugin_id: "com.himind.dependency".to_string(),
            required: true,
            min_version: "1.0.0".to_string(),
        }];
        let mut wrong_source =
            plugin_item("https://github.com/Owner/b/releases/download/v2/dependency.hmpkg");
        wrong_source.plugin_id = "com.himind.dependency".to_string();
        wrong_source.version = "2.0.0".to_string();
        wrong_source.source = "github:source-b".to_string();
        let mut selected_source = wrong_source.clone();
        selected_source.version = "1.0.0".to_string();
        selected_source.source = "github:source-a".to_string();
        let catalog = vec![wrong_source, root, selected_source];
        let mut order = Vec::new();

        resolve_plugin_order(
            &catalog,
            "com.himind.root",
            Some("1.0.0"),
            "github:source-a",
            &mut HashSet::new(),
            &mut order,
        )
        .unwrap();

        assert_eq!(order.len(), 2);
        assert!(order.iter().all(|item| item.source == "github:source-a"));
        assert_eq!(order[0].plugin_id, "com.himind.dependency");
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
                kind: ExtensionSourceKind::Github,
                repository: repository.to_string(),
                reference: reference.to_string(),
                catalog_path: catalog_path.to_string(),
                enabled: true,
                auto_update: false,
                verification: ExtensionSourceVerification::Required,
                upstream_repository: String::new(),
                ..Default::default()
            }],
            acquisitions: BTreeMap::new(),
            distribution_targets: BTreeMap::new(),
        };
        fs::create_dir_all(&root).unwrap();
        atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let loaded = settings_at(&path).unwrap();
        assert_eq!(loaded.sources.len(), 1);
        assert_eq!(loaded.sources[0].distribution_id, "owner/repo");
        assert_eq!(loaded.sources[0].channel, DEFAULT_DISTRIBUTION_CHANNEL);
        assert_eq!(loaded.sources[0].catalog_id, DEFAULT_CATALOG_ID);
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

    #[test]
    fn dsh_preset_validation_matches_official_directory_id_rules() {
        assert!(is_valid_dsh_preset_id("himind-short-video"));
        assert!(is_valid_dsh_preset_id("preset2"));
        assert!(!is_valid_dsh_preset_id("Himind-short-video"));
        assert!(!is_valid_dsh_preset_id("himind_short_video"));
        assert!(!is_valid_dsh_preset_id("himind.short.video"));
        assert!(!is_valid_dsh_preset_id("-short-video"));
    }

    #[test]
    fn dsh_preset_catalog_item_requires_safe_path_and_digest() {
        let valid = AgentPresetCatalogItem {
            preset_id: "himind-short-video".to_string(),
            name: "短视频创作".to_string(),
            description: String::new(),
            version: "0.1.0".to_string(),
            path: "dsh/agent-presets/himind-short-video/agent.cordis.yml".to_string(),
            sha256: "a".repeat(64),
        };
        assert!(validate_agent_preset(&valid).is_ok());

        let mut root_preset = valid.clone();
        root_preset.path = "agent.cordis.yml".to_string();
        assert!(validate_agent_preset(&root_preset).is_ok());

        let mut invalid = valid.clone();
        invalid.path = "../agent.cordis.yml".to_string();
        assert!(validate_agent_preset(&invalid).is_err());

        let mut invalid = valid;
        invalid.sha256 = "not-a-sha256".to_string();
        assert!(validate_agent_preset(&invalid).is_err());
    }

    #[test]
    fn catalog_validation_checks_preset_entries_before_snapshot_merge() {
        let source = ExtensionSourceConfig {
            id: "github-test".to_string(),
            name: "测试".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: ".himind/catalog.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let mut catalog: ExtensionSourceCatalog = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "plugins": [],
            "skills": [],
            "feature_packs": [],
            "agent_presets": [{
                "preset_id": "BadPreset",
                "name": "错误",
                "version": "1.0.0",
                "path": "dsh/agent-presets/BadPreset/agent.cordis.yml",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }))
        .unwrap();
        assert!(validate_catalog(&catalog, &source).is_err());
        catalog.agent_presets[0].preset_id = "good-preset".to_string();
        catalog.agent_presets[0].path =
            "dsh/agent-presets/good-preset/agent.cordis.yml".to_string();
        assert!(validate_catalog(&catalog, &source).is_ok());
    }

    #[test]
    fn feature_pack_rejects_unsafe_asset_ids() {
        let pack = ExtensionFeaturePack {
            id: "com.himind.feature.short-video".to_string(),
            name: "短视频创作".to_string(),
            plugin_ids: vec!["com.himind.short-video-creation".to_string()],
            skill_ids: vec![],
            agent_preset_ids: vec!["himind-short-video".to_string()],
            source_id: String::new(),
        };
        assert!(validate_feature_pack(&pack).is_ok());

        let mut invalid = pack;
        invalid.agent_preset_ids = vec!["../short-video".to_string()];
        assert!(validate_feature_pack(&invalid).is_err());
    }

    #[test]
    fn unavailable_source_keeps_last_known_preset() {
        let desired = HashSet::new();
        let unavailable = HashSet::from(["github-short-video".to_string()]);
        assert!(!should_remove_managed_preset(
            "himind-short-video",
            "github-short-video",
            &desired,
            &unavailable,
        ));
        assert!(should_remove_managed_preset(
            "himind-short-video",
            "github-short-video",
            &desired,
            &HashSet::new(),
        ));
    }

    fn local_repo_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-local-source-test-{}-{}",
            std::process::id(),
            tag
        ))
    }

    fn write_local_aggregate(root: &Path, extension: &str) {
        fs::create_dir_all(root.join("plugins/demo")).unwrap();
        fs::write(
            root.join("extensions.json"),
            format!(
                r#"{{"schema_version":1,"extensions":[{{"type":"plugin","id":"com.himind.demo","path":"{extension}"}}]}}"#
            ),
        )
        .unwrap();
        fs::write(
            root.join("plugins/demo/plugin.json"),
            r#"{"id":"com.himind.demo","name":"演示","description":"测试","version":"1.0.0","min_agent_version":"0.3.0","capabilities":[],"permissions":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn unit_acquisition_persists_remote_and_drops_redundant_local() {
        let root = std::env::temp_dir().join(format!(
            "himind-unit-acquisition-test-{}",
            std::process::id()
        ));
        let path = root.join("extension-sources.json");
        let repository = "Owner/repo";
        let reference = "main";
        let catalog_path = ".himind/catalog.json";
        let source = ExtensionSourceConfig {
            id: source_id(repository, reference, catalog_path),
            name: "测试".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: repository.to_string(),
            reference: reference.to_string(),
            catalog_path: catalog_path.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Required,
            upstream_repository: String::new(),
            distribution_id: "owner/repo".to_string(),
            channel: DEFAULT_DISTRIBUTION_CHANNEL.to_string(),
            catalog_id: DEFAULT_CATALOG_ID.to_string(),
            ..Default::default()
        };
        let unit_key = unit_key_of(&source);
        let value = ExtensionSourceSettings {
            schema_version: 1,
            sources: vec![source],
            acquisitions: BTreeMap::new(),
            distribution_targets: BTreeMap::new(),
        };
        fs::create_dir_all(&root).unwrap();
        atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let updated =
            set_acquisition_at(&path, &unit_key, ExtensionSourceAcquisition::Remote).unwrap();
        assert_eq!(
            updated.acquisitions.get(&unit_key),
            Some(&ExtensionSourceAcquisition::Remote)
        );
        assert_eq!(
            settings_at(&path).unwrap().acquisitions.get(&unit_key),
            Some(&ExtensionSourceAcquisition::Remote),
            "远端取用必须落盘，重启后仍生效"
        );

        let updated =
            set_acquisition_at(&path, &unit_key, ExtensionSourceAcquisition::Local).unwrap();
        assert!(updated.acquisitions.is_empty());
        assert!(settings_at(&path).unwrap().acquisitions.is_empty());

        assert!(
            set_acquisition_at(
                &path,
                "remote:owner/unknown",
                ExtensionSourceAcquisition::Remote
            )
            .is_err(),
            "未知分发单元必须拒绝写入"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn distribution_units_group_local_and_remote_sources_of_the_same_repository() {
        let local = ExtensionSourceConfig {
            id: "local-1".to_string(),
            name: "本地工作区".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"F:\repo".to_string(),
            reference: String::new(),
            catalog_path: LOCAL_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: "Owner/repo".to_string(),
            ..Default::default()
        };
        let remote = ExtensionSourceConfig {
            id: "github-1".to_string(),
            name: "GitHub 分发源".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: DEFAULT_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let local_identity = source_identity(&local);
        let remote_identity = source_identity(&remote);
        let mut catalogs = HashMap::new();
        catalogs.insert(
            local_identity.clone(),
            SourceCatalogAssets {
                plugins: vec![(
                    "com.himind.local-only".to_string(),
                    "本地".to_string(),
                    "2.0.0".to_string(),
                )],
                skills: Vec::new(),
                workflows: Vec::new(),
                experts: Vec::new(),
                asset_details: HashMap::new(),
            },
        );
        catalogs.insert(
            remote_identity,
            SourceCatalogAssets {
                plugins: vec![(
                    "com.himind.shared".to_string(),
                    "远端".to_string(),
                    "1.0.0".to_string(),
                )],
                skills: Vec::new(),
                workflows: Vec::new(),
                experts: Vec::new(),
                asset_details: HashMap::new(),
            },
        );

        let units = build_units(
            &[local.clone(), remote.clone()],
            &BTreeMap::new(),
            &catalogs,
        );
        assert_eq!(units.len(), 1, "同址来源必须合并为一个分发单元");
        let unit = &units[0];
        assert_eq!(unit.acquisition, ExtensionSourceAcquisition::Local);
        assert_eq!(unit.local_source_id.as_deref(), Some("local-1"));
        assert_eq!(unit.remote_source_id.as_deref(), Some("github-1"));
        assert_eq!(unit.repository, "Owner/repo");
        // 默认取用本地侧，单元内容就是本地目录当前提供的制品。
        assert_eq!(unit.plugin_ids, vec!["com.himind.local-only".to_string()]);
        assert_eq!(unit.assets.len(), 1);
        assert_eq!(unit.assets[0].version, "2.0.0");

        let mut remote_first = BTreeMap::new();
        remote_first.insert(unit.unit_key.clone(), ExtensionSourceAcquisition::Remote);
        let units = build_units(&[local, remote], &remote_first, &catalogs);
        assert_eq!(
            units[0].acquisition,
            ExtensionSourceAcquisition::Remote,
            "显式选择远端时单元内容切换到 GitHub 侧"
        );
        assert_eq!(units[0].plugin_ids, vec!["com.himind.shared".to_string()]);
    }

    fn unit_other_side_fixture() -> (ExtensionSourceConfig, ExtensionSourceConfig) {
        let local = ExtensionSourceConfig {
            id: "local-1".to_string(),
            name: "本地工作区".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"F:\repo".to_string(),
            reference: String::new(),
            catalog_path: LOCAL_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: "Owner/repo".to_string(),
            ..Default::default()
        };
        let remote = ExtensionSourceConfig {
            id: "github-1".to_string(),
            name: "GitHub 分发源".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: DEFAULT_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Required,
            upstream_repository: String::new(),
            ..Default::default()
        };
        (local, remote)
    }

    fn catalog_assets(items: Vec<(&str, &str)>) -> SourceCatalogAssets {
        SourceCatalogAssets {
            plugins: items
                .into_iter()
                .map(|(id, version)| (id.to_string(), id.to_string(), version.to_string()))
                .collect(),
            skills: Vec::new(),
            workflows: Vec::new(),
            experts: Vec::new(),
            asset_details: HashMap::new(),
        }
    }

    #[test]
    fn unit_reports_newer_versions_only_from_the_other_side() {
        let (local, remote) = unit_other_side_fixture();
        let mut catalogs = HashMap::new();
        catalogs.insert(
            "local:local-1".to_string(),
            catalog_assets(vec![("com.example.a", "1.0.0"), ("com.example.b", "2.0.0")]),
        );
        catalogs.insert(
            "github:github-1".to_string(),
            catalog_assets(vec![
                // 远端更高，应计一次。
                ("com.example.a", "1.2.0"),
                // 远端更低，不能算成“可切换更新”。
                ("com.example.b", "1.9.0"),
            ]),
        );

        let units = build_units(
            &[local.clone(), remote.clone()],
            &BTreeMap::new(),
            &catalogs,
        );
        assert_eq!(units.len(), 1);
        let other = units[0]
            .other_side
            .as_ref()
            .expect("两侧来源齐全时必须给出另一侧信息");
        assert_eq!(other.side, "remote");
        assert!(other.available);
        assert_eq!(other.newer_count, 1);

        // 切到远端后，“另一侧”变成本地。提示是双向的：此时本地目录里的
        // com.example.b 2.0.0 高于线上的 1.9.0，所以同样提示“本地更新”。
        let mut remote_first = BTreeMap::new();
        remote_first.insert(
            units[0].unit_key.clone(),
            ExtensionSourceAcquisition::Remote,
        );
        let units = build_units(&[local, remote], &remote_first, &catalogs);
        assert_eq!(units[0].acquisition, ExtensionSourceAcquisition::Remote);
        let other = units[0].other_side.as_ref().unwrap();
        assert_eq!(other.side, "local");
        assert!(other.available);
        assert_eq!(other.newer_count, 1);
    }

    #[test]
    fn unit_reports_other_side_as_unavailable_when_it_is_not_loaded() {
        let (local, remote) = unit_other_side_fixture();
        let mut catalogs = HashMap::new();
        catalogs.insert(
            "local:local-1".to_string(),
            catalog_assets(vec![("com.example.a", "1.0.0")]),
        );
        // GitHub 源被停用或读取失败时没有目录，此时不得提示“可更新”。
        let units = build_units(&[local, remote], &BTreeMap::new(), &catalogs);
        let other = units[0]
            .other_side
            .as_ref()
            .expect("单元仍有另一侧来源配置");
        assert_eq!(other.side, "remote");
        assert!(!other.available);
        assert_eq!(other.newer_count, 0);
    }

    #[test]
    fn distribution_units_keep_unrelated_sources_separate() {
        let first = ExtensionSourceConfig {
            id: "local-1".to_string(),
            name: "工作区 A".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"F:\a".to_string(),
            reference: String::new(),
            catalog_path: LOCAL_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: "Owner/a".to_string(),
            ..Default::default()
        };
        let second = ExtensionSourceConfig {
            id: "local-2".to_string(),
            name: "工作区 B".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"F:\b".to_string(),
            reference: String::new(),
            catalog_path: LOCAL_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: "Owner/b".to_string(),
            ..Default::default()
        };
        let units = build_units(&[first, second], &BTreeMap::new(), &HashMap::new());
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].state, "unavailable");
        assert_ne!(units[0].unit_key, units[1].unit_key);
    }

    #[test]
    fn explicit_distribution_identity_keeps_channels_separate() {
        let stable = ExtensionSourceConfig {
            id: "github-stable".to_string(),
            name: "稳定版".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: DEFAULT_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            distribution_id: "owner/repo".to_string(),
            channel: "stable".to_string(),
            catalog_id: DEFAULT_CATALOG_ID.to_string(),
        };
        let beta = ExtensionSourceConfig {
            id: "github-beta".to_string(),
            name: "预览版".to_string(),
            channel: "beta".to_string(),
            ..stable.clone()
        };
        let units = build_units(&[stable, beta], &BTreeMap::new(), &HashMap::new());
        assert_eq!(units.len(), 2);
        assert_ne!(units[0].unit_key, units[1].unit_key);
        assert!(units
            .iter()
            .any(|unit| unit.unit_key.ends_with("#stable#public")));
        assert!(units
            .iter()
            .any(|unit| unit.unit_key.ends_with("#beta#public")));
    }

    #[test]
    fn ordered_sources_put_the_acquisition_side_first_within_a_unit() {
        let remote = ExtensionSourceConfig {
            id: "github-1".to_string(),
            name: "GitHub 分发源".to_string(),
            kind: ExtensionSourceKind::Github,
            repository: "Owner/repo".to_string(),
            reference: "main".to_string(),
            catalog_path: DEFAULT_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let local = ExtensionSourceConfig {
            id: "local-1".to_string(),
            name: "本地工作区".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"F:\repo".to_string(),
            reference: String::new(),
            catalog_path: LOCAL_CATALOG_PATH.to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: "Owner/repo".to_string(),
            ..Default::default()
        };
        // 配置顺序里 GitHub 源在前：真实工作区就是这样登记的。
        let sources = vec![remote.clone(), local.clone()];
        let ordered = ordered_sources(sources.clone(), &BTreeMap::new());
        assert_eq!(
            ordered[0].id, "local-1",
            "本地取用时本地源必须先命中，安装与依赖解析才会落到本地制品"
        );
        let mut remote_acquisition = BTreeMap::new();
        remote_acquisition.insert(unit_key_of(&remote), ExtensionSourceAcquisition::Remote);
        let ordered = ordered_sources(sources, &remote_acquisition);
        assert_eq!(ordered[0].id, "github-1");
    }

    #[test]
    fn local_source_outranks_github_distribution_source_on_duplicate_ids() {
        assert!(source_outranks("local:local-1a2b", "github:github-3c4d"));
        assert!(!source_outranks("github:github-3c4d", "local:local-1a2b"));
        assert!(!source_outranks("github:github-3c4d", "github:github-5e6f"));
        assert!(!source_outranks("local:local-1a2b", "local:local-7g8h"));
        assert_eq!(conflict_reason("local:local-1a2b"), "由本地开发源优先提供");
        assert_eq!(
            conflict_reason("github:github-3c4d"),
            "由配置顺序更早的来源优先提供"
        );
        assert_eq!(
            conflict("github-3c4d", "插件", "com.himind.demo", "local:local-1a2b"),
            (
                "github-3c4d".to_string(),
                "由本地开发源优先提供".to_string(),
                "插件 com.himind.demo".to_string()
            )
        );
        assert_eq!(source_id_of("local:local-1a2b"), "local-1a2b");
        assert_eq!(source_id_of("github-3c4d"), "github-3c4d");
    }

    #[test]
    fn local_source_id_is_stable_and_prefixed() {
        let first = local_source_id(r"C:\extensions", "extensions.json");
        let second = local_source_id(r"C:\extensions", "extensions.json");
        let different = local_source_id(r"D:\extensions", "extensions.json");
        assert_eq!(first, second);
        assert_ne!(first, different);
        assert!(first.starts_with("local-"));
        assert_eq!(first.len(), 23);
    }

    #[test]
    fn build_local_catalog_reads_aggregate_and_manifests() {
        let root = local_repo_root("build");
        write_local_aggregate(&root, "plugins/demo");
        let source = ExtensionSourceConfig {
            id: local_source_id(
                &crate::extension_workspace::display_path(&root),
                "extensions.json",
            ),
            name: "本地".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: crate::extension_workspace::display_path(&root),
            reference: String::new(),
            catalog_path: "extensions.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let catalog = build_local_catalog(&source).unwrap();
        assert_eq!(catalog.plugins.len(), 1);
        assert_eq!(catalog.plugins[0].plugin_id, "com.himind.demo");
        assert_eq!(catalog.plugins[0].version, "1.0.0");
        assert!(catalog.plugins[0].download_url.starts_with("local:"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn local_source_rejects_missing_aggregate() {
        let root = local_repo_root("missing");
        fs::create_dir_all(&root).unwrap();
        let source = ExtensionSourceConfig {
            id: "local-test".to_string(),
            name: "本地".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: crate::extension_workspace::display_path(&root),
            reference: String::new(),
            catalog_path: "extensions.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        assert!(build_local_catalog(&source).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn local_source_normalize_uses_local_prefix() {
        let source = ExtensionSourceConfig {
            id: "local-test".to_string(),
            name: "本地".to_string(),
            kind: ExtensionSourceKind::Local,
            repository: r"C:\extensions".to_string(),
            reference: String::new(),
            catalog_path: "extensions.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        };
        let mut item = plugin_item("https://github.com/Owner/repo/releases/download/v1/test.hmpkg");
        normalize_plugin_item(&mut item, &source).unwrap();
        assert_eq!(item.source, "local:local-test");
        assert_eq!(item.governance, "optional");
        assert_eq!(item.management, "user_managed");
    }

    #[test]
    fn normalize_git_remote_accepts_https_and_ssh() {
        assert_eq!(
            normalize_git_remote("https://github.com/MrBaoquan/himind-extensions.git").as_deref(),
            Some("MrBaoquan/himind-extensions")
        );
        assert_eq!(
            normalize_git_remote("git@github.com:MrBaoquan/himind-extensions.git").as_deref(),
            Some("MrBaoquan/himind-extensions")
        );
        assert_eq!(normalize_git_remote("https://gitlab.com/owner/repo"), None);
    }

    #[test]
    fn local_upstream_repository_prefers_declared_catalog_value() {
        assert_eq!(
            local_upstream_repository(
                Path::new(r"C:\missing-workspace"),
                "https://github.com/Owner/repo.git"
            ),
            "Owner/repo"
        );
        assert_eq!(
            local_upstream_repository(Path::new(r"C:\missing-workspace"), "Owner/repo"),
            "Owner/repo"
        );
        assert_eq!(
            local_upstream_repository(Path::new(r"C:\missing-workspace"), "not a repository"),
            String::new()
        );
    }

    fn workspace_source(
        id: &str,
        kind: ExtensionSourceKind,
        repository: &str,
    ) -> ExtensionSourceConfig {
        ExtensionSourceConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind,
            repository: repository.to_string(),
            reference: String::new(),
            catalog_path: "extensions.json".to_string(),
            enabled: true,
            auto_update: false,
            verification: ExtensionSourceVerification::Optional,
            upstream_repository: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn removal_keeps_workspace_when_removed_source_is_unrelated() {
        let local = workspace_source("local-a", ExtensionSourceKind::Local, r"C:\extensions\a");
        let github = workspace_source("github-a", ExtensionSourceKind::Github, "Owner/repo");
        assert!(matches!(
            workspace_after_removal(
                &github,
                std::slice::from_ref(&local),
                Some(r"C:\extensions\a")
            ),
            WorkspaceAfterRemoval::Keep
        ));
        assert!(matches!(
            workspace_after_removal(&local, &[], None),
            WorkspaceAfterRemoval::Keep
        ));
        let other = workspace_source("local-b", ExtensionSourceKind::Local, r"C:\extensions\b");
        assert!(matches!(
            workspace_after_removal(
                &local,
                std::slice::from_ref(&other),
                Some(r"C:\extensions\b")
            ),
            WorkspaceAfterRemoval::Keep
        ));
    }

    #[test]
    fn removal_falls_back_to_remaining_local_source() {
        let removed = workspace_source("local-a", ExtensionSourceKind::Local, r"C:\extensions\a");
        let next = workspace_source("local-b", ExtensionSourceKind::Local, r"C:\extensions\b");
        let github = workspace_source("github-a", ExtensionSourceKind::Github, "Owner/repo");
        match workspace_after_removal(&removed, &[github, next.clone()], Some("c:/EXTENSIONS/a/")) {
            WorkspaceAfterRemoval::Select(root) => assert_eq!(root, next.repository),
            _ => panic!("应回退到剩余本地源"),
        }
    }

    #[test]
    fn removal_clears_workspace_without_remaining_local_source() {
        let removed = workspace_source("local-a", ExtensionSourceKind::Local, r"C:\extensions\a");
        let github = workspace_source("github-a", ExtensionSourceKind::Github, "Owner/repo");
        assert!(matches!(
            workspace_after_removal(&removed, &[github], Some(r"C:\extensions\a")),
            WorkspaceAfterRemoval::Clear
        ));
    }

    #[test]
    fn settings_derive_local_upstream_repository_without_persisting_it() {
        let root =
            std::env::temp_dir().join(format!("himind-local-upstream-test-{}", std::process::id()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("extensions.json"),
            r#"{"schema_version":1,"repository":"https://github.com/Owner/repo.git","extensions":[{"type":"plugin","id":"demo","path":"plugins/demo"}]}"#,
        )
        .unwrap();
        let workspace_display = crate::extension_workspace::display_path(&workspace);
        let catalog_path = "extensions.json";
        let mut value = ExtensionSourceSettings {
            schema_version: 1,
            sources: vec![workspace_source(
                &local_source_id(&workspace_display, catalog_path),
                ExtensionSourceKind::Local,
                &workspace_display,
            )],
            acquisitions: BTreeMap::new(),
            distribution_targets: BTreeMap::new(),
        };
        let path = root.join("extension-sources.json");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let loaded = settings_at(&path).unwrap();
        assert_eq!(loaded.sources[0].upstream_repository, "Owner/repo");
        // 命令响应必须带派生字段，前端才能显示上游仓库并预填分发源。
        assert_eq!(
            serde_json::to_value(&loaded).unwrap()["sources"][0]["upstream_repository"],
            "Owner/repo"
        );
        // 落盘必须清空派生字段，避免本地仓库改了声明后残留过期值。
        let persisted = serde_json::to_value(persisted_settings(&loaded)).unwrap();
        assert_eq!(persisted["sources"][0]["upstream_repository"], "");
        // 旧配置里若已写入派生值，读取时也必须被重新派生覆盖。
        value.sources[0].upstream_repository = "Stale/repo".to_string();
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert_eq!(
            settings_at(&path).unwrap().sources[0].upstream_repository,
            "Owner/repo"
        );
        let _ = fs::remove_dir_all(root);
    }
}
