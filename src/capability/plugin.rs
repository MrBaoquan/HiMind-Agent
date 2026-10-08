use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static ACTIVE_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_PLUGIN_INVOCATIONS: usize = 4;
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// 单条插件响应的传输上限。
///
/// 这里刻意给得比"业务上想内联多少"更宽：合法能力（例如把产物 base64 内联给插件自己的
/// 预览界面）本来就可能是几 MB。1 MiB 会把这种正常调用判成插件故障，进而阻塞全部依赖者。
/// 默认 16 MiB，并可用 `HIMIND_AGENT_MAX_PLUGIN_RESPONSE_BYTES` 覆盖；真正的超大传输应
/// 由能力改成分页或返回引用，而不是继续抬高上限。
const DEFAULT_MAX_PLUGIN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PLUGIN_STDERR_BYTES: usize = 64 * 1024;
const PLUGIN_EXIT_GRACE_PERIOD: Duration = Duration::from_secs(2);

struct ActiveInvocationGuard;

impl Drop for ActiveInvocationGuard {
    fn drop(&mut self) {
        ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::Release);
    }
}

/// "最近失败"作为降级信号的有效窗口。
///
/// 健康记录是历史事实，不该永久污染依赖者：一次超限响应或一次超时会在几周后仍然让
/// 所有依赖它的技能显示"部分功能不可用"。超过这个窗口的失败只留在插件卡片里供排障，
/// 不再影响技能状态；窗口内再次失败会刷新时间戳。
pub(crate) const PLUGIN_FAILURE_RECENCY_SECONDS: u64 = 24 * 60 * 60;

/// 插件调用的失败分类。
///
/// 关键区别是「这是插件坏了，还是这次调用不行」：只有进程/协议层面的故障才计入插件健康
/// 并可能触发熔断；能力层错误与超出传输上限的响应属于这次调用的问题，返回给调用方即可，
/// 不应让插件的所有依赖者一起不可用。
#[derive(Debug)]
pub(crate) enum PluginInvocationError {
    /// 进程或 JSON-RPC 传输失败：计入插件健康。
    Transport(String),
    /// 插件正常应答但返回 error：不计入插件健康，也不触发熔断。
    Capability { capability: String, message: String },
    /// 单条响应超过传输上限：不计入插件健康，提示该能力改成分页或返回引用。
    ResponseTooLarge {
        plugin: String,
        capability: String,
        observed_bytes: usize,
        limit: usize,
    },
}

impl PluginInvocationError {
    /// 是否应计入插件健康（进而可能熔断）。
    pub(crate) fn records_health(&self) -> bool {
        matches!(self, Self::Transport(_))
    }
}

impl std::fmt::Display for PluginInvocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(formatter, "{message}"),
            Self::Capability {
                capability,
                message,
            } => write!(formatter, "plugin capability {capability} failed: {message}"),
            Self::ResponseTooLarge {
                plugin,
                capability,
                observed_bytes,
                limit,
            } => write!(
                formatter,
                "plugin {plugin} 的 {capability} 响应超过单条上限：已读到 {observed_bytes} 字节，上限 {limit} 字节。该能力应改为分页或返回引用（可用 HIMIND_AGENT_MAX_PLUGIN_RESPONSE_BYTES 调整上限）。"
            ),
        }
    }
}

impl std::error::Error for PluginInvocationError {}

/// 单条插件响应的实际上限，允许用环境变量覆盖以便排障。
fn max_plugin_response_bytes() -> usize {
    std::env::var("HIMIND_AGENT_MAX_PLUGIN_RESPONSE_BYTES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_PLUGIN_RESPONSE_BYTES)
}
const PLUGIN_FAILURE_THRESHOLD: u32 = 3;

/// 熔断后的冷却窗口。
///
/// 熔断的目的不是永久封禁插件，而是避免连续失败继续拖垮调用方：冷却结束后进入半开
/// 状态，允许下一次调用去验证插件是否已经恢复——成功就清空健康记录，失败则重新计时。
/// 少了这一步，被熔断的插件再也不会被调用，也就永远没有自愈的机会。
const PLUGIN_BREAKER_COOLDOWN: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct PluginHealth {
    #[serde(default)]
    failure_count: u32,
    #[serde(default)]
    last_failure_at: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
}

/// 熔断是否仍然生效（处于冷却窗口内）。
///
/// 超过冷却窗口的失败只作为排障信息保留在插件卡片里，不再阻止调用：插件的健康记录
/// 是历史事实，但"历史失败"不能变成永久不可用。
fn circuit_is_open(health: &PluginHealth) -> bool {
    if health.failure_count < PLUGIN_FAILURE_THRESHOLD {
        return false;
    }
    match health.last_failure_at {
        Some(at) => unix_now().saturating_sub(at) < PLUGIN_BREAKER_COOLDOWN.as_secs(),
        None => true,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginCapabilityManifest {
    pub id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default = "default_risk_level")]
    pub risk_level: String,
    #[serde(default)]
    pub availability: String,
    #[serde(default = "default_plugin_capability_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_plugin_author")]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub release_notes: String,
    pub version: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub min_agent_version: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default = "default_plugin_governance")]
    pub governance: String,
    #[serde(default)]
    pub capabilities: Vec<PluginCapabilityManifest>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub plugin_dependencies: Vec<PluginDependencyManifest>,
    #[serde(default)]
    pub contributes: PluginContributions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginDependencyManifest {
    pub plugin_id: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub min_version: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct PluginContributions {
    #[serde(default)]
    pub views: Vec<PluginViewContribution>,
    #[serde(default)]
    pub commands: Vec<PluginCommandContribution>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginViewContribution {
    pub id: String,
    pub title: String,
    /// Compact label for host quick-launch surfaces. Full title remains used
    /// for the plugin window title and accessibility text.
    #[serde(default)]
    pub short_title: String,
    /// Semantic icon key resolved by the host. Unknown keys use app-window.
    #[serde(default = "default_view_icon")]
    pub icon: String,
    /// Whether this view is shown in the Agent quick-tools surface.
    #[serde(default = "default_true")]
    pub quick_access: bool,
    #[serde(default)]
    pub order: i32,
    #[serde(default = "default_view_location")]
    pub location: String,
    pub entry: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginCommandContribution {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PluginRegistryItem {
    pub id: String,
    pub name: String,
    pub author_name: String,
    pub description: String,
    pub release_notes: String,
    pub version: String,
    pub runtime: String,
    pub min_agent_version: String,
    pub governance: String,
    #[serde(default)]
    pub availability: String,
    #[serde(default)]
    pub source: String,
    pub status: String,
    pub enabled: bool,
    pub path: String,
    pub development: bool,
    pub entry: String,
    pub entry_modified_at: Option<u64>,
    pub entry_size: Option<u64>,
    pub previous_version: Option<String>,
    pub rollback_available: bool,
    /// 本机开发登记（免安装直挂）接管了同名已安装副本时才有的字段，记录被接管的
    /// 已安装版本。界面据此回答"我本来装的是哪个版本、现在跑的又是哪个版本"，
    /// 而不是让一条开发草稿顶掉已安装条目、把版本号和操作入口一起带偏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_installed_version: Option<String>,
    pub capabilities: Vec<PluginCapabilityManifest>,
    pub permissions: Vec<String>,
    pub plugin_dependencies: Vec<PluginDependencyManifest>,
    pub views: Vec<PluginViewContribution>,
    pub commands: Vec<PluginCommandContribution>,
    pub error: Option<String>,
    pub failure_count: u32,
    pub circuit_open: bool,
    /// 最近一次失败的时间（epoch 秒）。用于判断"最近失败"是否仍在有效窗口内。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<u64>,
}

pub(crate) fn plugin_registry_dir() -> PathBuf {
    crate::store::paths::agent_home().join("plugins")
}

fn development_registry_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_PLUGIN_DEVELOPMENT_REGISTRY") {
        return PathBuf::from(path);
    }
    plugin_registry_dir()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("plugin-development.json")
}

pub(crate) fn register_development_plugin(
    path: &std::path::Path,
) -> Result<String, Box<dyn Error>> {
    register_development_plugin_at(path, &development_registry_path())
}

fn register_development_plugin_at(
    path: &std::path::Path,
    registry_path: &std::path::Path,
) -> Result<String, Box<dyn Error>> {
    let root = path.canonicalize()?;
    let content = fs::read_to_string(root.join("plugin.json"))?;
    let manifest = parse_plugin_manifest(content.trim_start_matches('\u{feff}'))?;
    validate_manifest_contributions(&root, &manifest)?;
    validate_development_entry(&root, &manifest)?;
    // 多个工作区会话可以同时登记各自的开发插件，读改写必须整体串行化，
    // 否则后登记的会话会把先登记的条目整表覆盖掉。
    let _lock = crate::store::atomic_file::lock(registry_path)?;
    let mut entries = development_plugins_at(registry_path);
    entries.retain(|entry| entry.id != manifest.id);
    entries.push(DevelopmentPlugin {
        id: manifest.id.clone(),
        path: root.to_string_lossy().to_string(),
    });
    write_development_plugins_at(registry_path, &entries)?;
    // A development registration changes the discoverable capability set, and
    // carrying a previous health record forward would hide the new build.
    clear_plugin_health(
        &registry_path
            .with_file_name("plugin-development-health")
            .join(format!("{}.json", manifest.id)),
    );
    crate::capability::service::invalidate_capability_discovery();
    Ok(manifest.id)
}

pub(crate) fn unregister_development_plugin(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    unregister_development_plugin_at(plugin_id, &development_registry_path())
}

fn unregister_development_plugin_at(
    plugin_id: &str,
    registry_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _lock = crate::store::atomic_file::lock(registry_path)?;
    let mut entries = development_plugins_at(registry_path);
    let original_len = entries.len();
    entries.retain(|entry| entry.id != plugin_id);
    let changed = entries.len() != original_len;
    write_development_plugins_at(registry_path, &entries)?;
    if changed {
        crate::capability::service::invalidate_capability_discovery();
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct DevelopmentPlugin {
    id: String,
    path: String,
}

fn development_plugins() -> Vec<DevelopmentPlugin> {
    development_plugins_at(&development_registry_path())
}

/// 版本比较统一走技能解析器里的语义化比较，避免插件侧再写一套规则。
fn compare_plugin_versions(left: &str, right: &str) -> std::cmp::Ordering {
    crate::skill::resolver::compare_versions(left, right)
}

/// 免安装直挂的插件及其源码/产物目录，供分发单元状态展示。
pub(crate) fn development_plugin_entries() -> Vec<(String, PathBuf)> {
    development_plugins()
        .into_iter()
        .map(|entry| (entry.id, PathBuf::from(entry.path)))
        .collect()
}

fn development_plugins_at(path: &std::path::Path) -> Vec<DevelopmentPlugin> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn write_development_plugins_at(
    path: &std::path::Path,
    entries: &[DevelopmentPlugin],
) -> Result<(), Box<dyn Error>> {
    // 原子替换：并发读永远看到完整内容，也不会因为多个会话共用同一个
    // `.json.tmp` 而互相截断。调用方负责持有注册表锁。
    crate::store::atomic_file::atomic_write(path, &serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

/// 把本机开发登记合并进已安装清单。
///
/// 两条规则合起来才叫"单一真源"：
/// 1. 已安装副本是"我拥有的能力"，默认它就是这条目的全部事实；
/// 2. 开发登记只有在版本严格更高时才接管条目——它代表运行态真的换成了本机开发版本，
///    此时条目必须留下被接管的已安装版本（`overrides_installed_version`），
///    否则用户会看到"已安装"的版本号突然倒退回一条草稿，或者以为插件被降级了。
///
/// 版本相同或更低的开发登记不产生任何接管：它仍是「扩展开发」里的草稿，
/// 但不参与能力列表，免得旧草稿静默盖住用户真正安装的版本。
fn merge_development_items(
    items: &mut Vec<PluginRegistryItem>,
    development_items: Vec<PluginRegistryItem>,
) {
    for development_item in development_items {
        let mut installed_versions = items
            .iter()
            .filter(|item| item.id == development_item.id)
            .map(|item| item.version.clone());
        let Some(installed_version) = installed_versions.next() else {
            // 没有已安装副本：这条开发登记就是该插件在本机的唯一存在形式。
            items.push(development_item);
            continue;
        };
        let installed_version = installed_versions.fold(installed_version, |current, other| {
            if compare_plugin_versions(&other, &current) == std::cmp::Ordering::Greater {
                other
            } else {
                current
            }
        });
        if compare_plugin_versions(&development_item.version, &installed_version)
            != std::cmp::Ordering::Greater
        {
            continue;
        }
        let Some(index) = items.iter().position(|item| item.id == development_item.id) else {
            // 条目可能在扫描期间被移除；把这次开发登记视为独立条目，
            // 不让一个目录竞争把 Agent 线程打崩。
            items.push(development_item);
            continue;
        };
        let mut taken_over = development_item;
        taken_over.overrides_installed_version = Some(installed_version);
        items[index] = taken_over;
    }
}

pub(crate) fn scan_plugins() -> Result<Vec<PluginRegistryItem>, Box<dyn Error>> {
    let mut items = builtin_plugin_items();
    let root = plugin_registry_dir();
    if root.exists() {
        for entry in fs::read_dir(&root)?.flatten() {
            let path = entry.path();
            if !is_plugin_install_directory(&path) {
                continue;
            }
            items.push(read_plugin_item(path, false));
        }
    }
    // 开发登记（免安装直挂）是"我正在开发的那份制品"，不是"我拥有的能力"。
    // 它一旦无条件覆盖同名已安装副本，界面就会出现"已安装"却挂着更低版本号，
    // 运行态也会静默跑起旧草稿（例如 v0.3.11 盖住已安装的 v0.3.13）。
    // 只有开发版本严格更高时才接管，否则已安装副本是唯一真源。
    let development_items = development_plugins()
        .into_iter()
        .map(|entry| PathBuf::from(entry.path))
        // 目录已被删除或移动时登记已失效，直接跳过，不要用一条读不出来的记录
        // 去顶掉用户真正安装在 plugins/ 下的副本。
        .filter(|path| path.is_dir())
        .map(|path| read_plugin_item(path, true))
        .collect();
    merge_development_items(&mut items, development_items);
    items.sort_by(|a, b| a.id.cmp(&b.id));
    for (plugin_id, issue) in plugin_dependency_cycle_issues(&items) {
        if let Some(item) = items
            .iter_mut()
            .find(|candidate| candidate.id == plugin_id && candidate.error.is_none())
        {
            item.status = "blocked".to_string();
            item.enabled = false;
            item.error = Some(issue);
        }
    }
    // Resolve plugin dependencies after all sources have been discovered. A
    // plugin with an unsatisfied required dependency remains visible for
    // diagnostics, but is blocked from the Capability Registry and MCP.
    loop {
        let snapshot = items.clone();
        let mut changed = false;
        for item in &mut items {
            let issues = plugin_dependency_issues(item, &snapshot);
            if !issues.is_empty() && item.error.is_none() {
                item.status = "blocked".to_string();
                item.enabled = false;
                item.error = Some(issues.join("; "));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(items)
}

fn plugin_dependency_cycle_issues(installed: &[PluginRegistryItem]) -> HashMap<String, String> {
    fn visit(
        plugin_id: &str,
        installed: &[PluginRegistryItem],
        index: &HashMap<String, usize>,
        visited: &mut HashSet<String>,
        active: &mut HashMap<String, usize>,
        stack: &mut Vec<String>,
        issues: &mut HashMap<String, String>,
    ) {
        if let Some(start) = active.get(plugin_id).copied() {
            let mut cycle = stack[start..].to_vec();
            cycle.push(plugin_id.to_string());
            let message = format!("插件依赖存在循环: {}", cycle.join(" -> "));
            for member in &stack[start..] {
                issues
                    .entry(member.clone())
                    .or_insert_with(|| message.clone());
            }
            return;
        }
        if visited.contains(plugin_id) {
            return;
        }
        let Some(item) = index
            .get(plugin_id)
            .and_then(|position| installed.get(*position))
        else {
            return;
        };
        active.insert(plugin_id.to_string(), stack.len());
        stack.push(plugin_id.to_string());
        for dependency in item
            .plugin_dependencies
            .iter()
            .filter(|dependency| dependency.required)
        {
            if index.contains_key(&dependency.plugin_id) {
                visit(
                    &dependency.plugin_id,
                    installed,
                    index,
                    visited,
                    active,
                    stack,
                    issues,
                );
            }
        }
        stack.pop();
        active.remove(plugin_id);
        visited.insert(plugin_id.to_string());
    }

    let index = installed
        .iter()
        .enumerate()
        .map(|(position, item)| (item.id.clone(), position))
        .collect::<HashMap<_, _>>();
    let mut visited = HashSet::new();
    let mut active = HashMap::new();
    let mut stack = Vec::new();
    let mut issues = HashMap::new();
    for item in installed {
        visit(
            &item.id,
            installed,
            &index,
            &mut visited,
            &mut active,
            &mut stack,
            &mut issues,
        );
    }
    issues
}

/// Returns required dependency failures for an installed plugin. Optional
/// dependencies are intentionally omitted: their absence must not prevent a
/// plugin from being used, but can be surfaced by management UIs later.
pub(crate) fn plugin_dependency_issues(
    plugin: &PluginRegistryItem,
    installed: &[PluginRegistryItem],
) -> Vec<String> {
    plugin
        .plugin_dependencies
        .iter()
        .filter(|dependency| dependency.required)
        .filter_map(|dependency| {
            let Some(provider) = installed
                .iter()
                .find(|item| item.id == dependency.plugin_id)
            else {
                return Some(format!("缺少必需插件 {}", dependency.plugin_id));
            };
            // 只有真的不可用（被停用或已熔断）才阻断依赖者。仅仅"最近一次调用失败"
            // 不算不可用——否则一次超时/一次超限响应就会永久阻断所有依赖它的插件，
            // 而且被阻断后它再也不会被调用，无法自愈。
            if !provider.enabled {
                return Some(format!(
                    "必需插件 {} {}",
                    dependency.plugin_id,
                    if provider.circuit_open {
                        "连续失败已熔断"
                    } else {
                        "当前已停用"
                    }
                ));
            }
            if !dependency.min_version.trim().is_empty()
                && crate::skill::resolver::compare_versions(
                    &provider.version,
                    &dependency.min_version,
                ) == std::cmp::Ordering::Less
            {
                return Some(format!(
                    "插件 {} 版本低于 {}",
                    dependency.plugin_id, dependency.min_version
                ));
            }
            None
        })
        .collect()
}

/// Resolves dependencies for a manifest before it is installed as a
/// development candidate. This keeps candidate tests aligned with the same
/// runtime gate used by the live registry.
pub(crate) fn plugin_manifest_dependency_issues(manifest: &PluginManifest) -> Vec<String> {
    match scan_plugins() {
        Ok(installed) => {
            let candidate = PluginRegistryItem {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                author_name: manifest.author.clone(),
                description: manifest.description.clone(),
                release_notes: manifest.release_notes.clone(),
                version: manifest.version.clone(),
                runtime: manifest.runtime.clone(),
                min_agent_version: manifest.min_agent_version.clone(),
                governance: manifest.governance.clone(),
                availability: "local".to_string(),
                source: "candidate".to_string(),
                status: "installed".to_string(),
                enabled: true,
                path: String::new(),
                development: true,
                entry: manifest.entry.clone(),
                entry_modified_at: None,
                entry_size: None,
                previous_version: None,
                rollback_available: false,
                overrides_installed_version: None,
                capabilities: manifest.capabilities.clone(),
                permissions: manifest.permissions.clone(),
                plugin_dependencies: manifest.plugin_dependencies.clone(),
                views: manifest.contributes.views.clone(),
                commands: manifest.contributes.commands.clone(),
                error: None,
                failure_count: 0,
                circuit_open: false,
                last_failure_at: None,
            };
            plugin_dependency_issues(&candidate, &installed)
        }
        Err(error) => vec![format!("读取插件依赖失败: {error}")],
    }
}

fn is_plugin_install_directory(path: &std::path::Path) -> bool {
    path.is_dir()
        && (path.join("plugin.json").is_file()
            || path.join("current").join("plugin.json").is_file())
}

pub(crate) fn is_builtin_plugin(plugin_id: &str) -> bool {
    matches!(
        plugin_id,
        "com.himind.builtin.svn"
            | "com.himind.builtin.smb"
            | "com.himind.builtin.inner-admin"
            | "com.himind.dashboard-business"
            | "com.himind.knowledge"
    )
}

fn builtin_plugin_items() -> Vec<PluginRegistryItem> {
    vec![
        builtin_plugin(
            "com.himind.builtin.svn",
            "SVN 个人账号与工作区",
            "管理当前用户自己的 SVN 凭据和本机展项工作区。",
            &[
                "svn.connection.list",
                "svn.connection.test",
                "exhibit.workspace.checkout",
                "exhibit.workspace.status",
                "exhibit.migration_source.scan",
                "exhibit.workspace.update",
                "exhibit.workspace.open",
                "exhibit.repository.import_local",
            ],
            &["secret.svn.broker", "network.svn", "process.tortoisesvn"],
        ),
        builtin_plugin(
            "com.himind.builtin.smb",
            "SMB 共享资源",
            "由 Dashboard 任务编排的受控共享目录读取与资源上传模块。",
            &[],
            &["fs.smb.broker", "network.internal"],
        ),
        builtin_plugin(
            "com.himind.builtin.inner-admin",
            "内网交付与上传",
            "内网工程同步、代码预处理、分片上传和占位说明交付模块。",
            &["inner_admin.login_status"],
            &[
                "secret.inner_admin.broker",
                "network.internal",
                "artifact.read",
            ],
        ),
        builtin_plugin(
            "com.himind.dashboard-business",
            "项目业务助手",
            "以当前用户身份读取 Dashboard 项目、展项、需求、IP 和我的工作聚合事实。",
            &[
                "context.resolve",
                "project.context.get",
                "exhibit.context.get",
                "work.my_summary",
            ],
            &["dashboard.business.read", "oauth.delegated_user"],
        ),
        builtin_plugin(
            "com.himind.knowledge",
            "知识检索",
            "以当前用户身份检索获准向外部 AI 工具开放的 Dashboard 知识空间。",
            &["knowledge.search.v1"],
            &["dashboard.knowledge.search", "oauth.delegated_user"],
        ),
    ]
}

fn builtin_plugin(
    id: &str,
    name: &str,
    description: &str,
    capability_ids: &[&str],
    permissions: &[&str],
) -> PluginRegistryItem {
    let availability = if matches!(
        id,
        "com.himind.dashboard-business" | "com.himind.knowledge" | "com.himind.builtin.smb"
    ) {
        "control_plane"
    } else {
        "local"
    };
    PluginRegistryItem {
        id: id.to_string(),
        name: name.to_string(),
        author_name: "马宝全".to_string(),
        description: description.to_string(),
        release_notes: description.to_string(),
        version: crate::VERSION.to_string(),
        runtime: "builtin".to_string(),
        min_agent_version: crate::VERSION.to_string(),
        governance: "required".to_string(),
        availability: availability.to_string(),
        source: "builtin".to_string(),
        status: "installed".to_string(),
        enabled: true,
        path: String::new(),
        development: false,
        entry: String::new(),
        entry_modified_at: None,
        entry_size: None,
        previous_version: None,
        rollback_available: false,
        overrides_installed_version: None,
        capabilities: capability_ids
            .iter()
            .map(|id| PluginCapabilityManifest {
                id: (*id).to_string(),
                description: description.to_string(),
                input_schema: Value::Null,
                risk_level: "builtin_policy".to_string(),
                availability: "local".to_string(),
                timeout_seconds: default_plugin_capability_timeout(),
            })
            .collect(),
        permissions: permissions
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        plugin_dependencies: Vec::new(),
        views: Vec::new(),
        commands: Vec::new(),
        error: None,
        failure_count: 0,
        circuit_open: false,
        last_failure_at: None,
    }
}

pub(crate) fn find_plugin(plugin_id: &str) -> Result<Option<PluginRegistryItem>, Box<dyn Error>> {
    Ok(scan_plugins()?
        .into_iter()
        .find(|item| item.id == plugin_id))
}

pub(crate) fn plugin_view_entry(
    plugin_id: &str,
    view_id: &str,
) -> Result<Option<(PluginRegistryItem, PluginViewContribution, PathBuf)>, Box<dyn Error>> {
    let Some(plugin) = find_plugin(plugin_id)? else {
        return Ok(None);
    };
    if !plugin.enabled {
        return Err(format!(
            "plugin is unavailable: {}: {}",
            plugin.id,
            plugin.error.as_deref().unwrap_or("disabled")
        )
        .into());
    }
    let Some(view) = plugin.views.iter().find(|view| view.id == view_id).cloned() else {
        return Ok(None);
    };
    let root = plugin_execution_dir(&plugin).canonicalize()?;
    let entry = relative_plugin_resource(&root, &view.entry)?;
    if entry.extension().and_then(|value| value.to_str()) != Some("html") {
        return Err(format!("plugin view entry must be an HTML file: {}", view.entry).into());
    }
    Ok(Some((plugin, view, entry)))
}

pub(crate) fn resolve_plugin_ui_resource(
    url: &url::Url,
) -> Result<(PathBuf, &'static str), Box<dyn Error>> {
    if !is_plugin_ui_origin(url) {
        return Err("invalid plugin UI resource origin".into());
    }
    let segments: Vec<&str> = url
        .path_segments()
        .ok_or("plugin UI resource path is missing")?
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 3 {
        return Err("plugin UI resource path is incomplete".into());
    }
    let plugin_id = segments[0];
    let view_id = segments[1];
    let relative = segments[2..].join("/");
    let Some((plugin, _view, _entry)) = plugin_view_entry(plugin_id, view_id)? else {
        return Err("plugin UI view is unavailable".into());
    };
    let root = plugin_execution_dir(&plugin).canonicalize()?;
    let resource = relative_plugin_resource(&root, &relative)?;
    let content_type = match resource.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    Ok((resource, content_type))
}

pub(crate) fn is_plugin_ui_origin(url: &url::Url) -> bool {
    (url.scheme() == "plugin-ui" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https")
            && url.host_str() == Some("plugin-ui.localhost"))
}

pub(crate) fn is_plugin_ui_navigation(url: &url::Url) -> bool {
    url.as_str() == "about:blank" || is_plugin_ui_origin(url)
}

fn read_plugin_item(path: PathBuf, development: bool) -> PluginRegistryItem {
    let manifest_path = path.join("current").join("plugin.json");
    let fallback_manifest_path = path.join("plugin.json");
    let manifest_path = if manifest_path.exists() {
        manifest_path
    } else {
        fallback_manifest_path
    };
    let default_id = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| error.to_string())
        .and_then(|content| parse_plugin_manifest(&content).map_err(|error| error.to_string()));

    match manifest {
        Ok(manifest) => {
            let validation = validate_manifest_contributions(&path, &manifest);
            let entry_metadata = development_entry_metadata(&path, &manifest.entry);
            let governance = fs::read_to_string(path.join("current").join("policy.json"))
                .ok()
                .and_then(|content| serde_json::from_str::<Value>(&content).ok())
                .and_then(|value| {
                    value
                        .get("governance")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| manifest.governance.clone());
            let disabled = path.join("disabled").exists();
            let health_root = plugin_health_root(&path, development, &manifest.id);
            let health = read_plugin_health(&health_root);
            let circuit_open = circuit_is_open(&health);
            let source = fs::read_to_string(path.join("current").join("policy.json"))
                .ok()
                .and_then(|content| serde_json::from_str::<Value>(&content).ok())
                .and_then(|value| {
                    value
                        .get("source")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| {
                    if development {
                        "development".to_string()
                    } else {
                        "local".to_string()
                    }
                });
            let availability = manifest_availability(&manifest);
            // 上一版本与当前版本相同时不算"可回滚"：互换只会让版本号原地不动。
            let previous_version = previous_plugin_version(&path);
            let rollback_available = previous_version.as_deref().is_some_and(|previous| {
                crate::skill::resolver::compare_versions(previous, &manifest.version)
                    != std::cmp::Ordering::Equal
            });
            PluginRegistryItem {
                id: manifest.id,
                name: manifest.name,
                author_name: manifest.author,
                description: manifest.description,
                release_notes: manifest.release_notes,
                version: manifest.version,
                runtime: manifest.runtime,
                min_agent_version: manifest.min_agent_version,
                governance,
                availability,
                source,
                status: validation
                    .as_ref()
                    .map(|_| {
                        if circuit_open {
                            "failed".to_string()
                        } else {
                            "installed".to_string()
                        }
                    })
                    .unwrap_or_else(|_| "failed".to_string()),
                enabled: validation.is_ok() && !disabled && !circuit_open,
                path: path.to_string_lossy().to_string(),
                development,
                entry: manifest.entry,
                entry_modified_at: entry_metadata.as_ref().and_then(|metadata| metadata.0),
                entry_size: entry_metadata.map(|metadata| metadata.1),
                previous_version,
                rollback_available,
                overrides_installed_version: None,
                capabilities: manifest.capabilities,
                permissions: manifest.permissions,
                plugin_dependencies: manifest.plugin_dependencies,
                views: manifest.contributes.views,
                commands: manifest.contributes.commands,
                error: validation
                    .err()
                    .map(|error| error.to_string())
                    .or(health.last_error),
                failure_count: health.failure_count,
                circuit_open,
                last_failure_at: health.last_failure_at,
            }
        }
        Err(error) => PluginRegistryItem {
            id: default_id.clone(),
            name: default_id,
            author_name: "未知作者".to_string(),
            description: String::new(),
            release_notes: String::new(),
            version: String::new(),
            runtime: String::new(),
            min_agent_version: String::new(),
            governance: "optional".to_string(),
            availability: "local".to_string(),
            source: "local".to_string(),
            status: "failed".to_string(),
            enabled: false,
            path: path.to_string_lossy().to_string(),
            development,
            entry: String::new(),
            entry_modified_at: None,
            entry_size: None,
            previous_version: None,
            rollback_available: false,
            overrides_installed_version: None,
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            views: Vec::new(),
            commands: Vec::new(),
            error: Some(error.to_string()),
            failure_count: 0,
            circuit_open: false,
            last_failure_at: None,
        },
    }
}

fn manifest_availability(manifest: &PluginManifest) -> String {
    let dashboard_permission = manifest
        .permissions
        .iter()
        .any(|permission| permission == "network.dashboard.public");
    let mut has_local = false;
    let mut has_network = false;
    let mut has_control_plane = false;
    for capability in &manifest.capabilities {
        match capability.availability.trim().to_ascii_lowercase().as_str() {
            "control_plane" | "dashboard" => has_control_plane = true,
            "network_service" | "network" => has_network = true,
            "local" => has_local = true,
            _ if dashboard_permission => has_control_plane = true,
            _ => has_local = true,
        }
    }
    if has_local {
        "local".to_string()
    } else if has_network {
        "network_service".to_string()
    } else if has_control_plane || dashboard_permission {
        "control_plane".to_string()
    } else {
        "local".to_string()
    }
}

fn previous_plugin_version(root: &std::path::Path) -> Option<String> {
    let content = fs::read_to_string(root.join("previous/plugin.json")).ok()?;
    parse_plugin_manifest(&content)
        .ok()
        .map(|manifest| manifest.version)
}

fn development_entry_metadata(root: &std::path::Path, entry: &str) -> Option<(Option<u64>, u64)> {
    let execution_dir = {
        let current = root.join("current");
        if current.join("plugin.json").exists() {
            current
        } else {
            root.to_path_buf()
        }
    };
    let metadata = fs::metadata(execution_dir.join(entry)).ok()?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64);
    Some((modified_at, metadata.len()))
}

pub(crate) fn registry_json() -> Result<Value, Box<dyn Error>> {
    registry_json_for_control_plane(true)
}

pub(crate) fn registry_json_for_control_plane(
    control_plane_enabled: bool,
) -> Result<Value, Box<dyn Error>> {
    let items = scan_plugins()?
        .into_iter()
        .filter(|item| control_plane_enabled || item.availability != "control_plane")
        .collect::<Vec<_>>();
    Ok(json!({
        "items": items,
        "total": items.len(),
        "registry_ready": true,
        "registry_dir": plugin_registry_dir().to_string_lossy().to_string(),
        "external_runtime": "process-jsonrpc-stdio"
    }))
}

fn health_path(root: &std::path::Path) -> PathBuf {
    if root.extension().and_then(|value| value.to_str()) == Some("json") {
        root.to_path_buf()
    } else {
        root.join("health.json")
    }
}

fn plugin_health_root(path: &std::path::Path, development: bool, plugin_id: &str) -> PathBuf {
    if development {
        development_registry_path()
            .with_file_name("plugin-development-health")
            .join(format!("{plugin_id}.json"))
    } else {
        path.to_path_buf()
    }
}

fn read_plugin_health(root: &std::path::Path) -> PluginHealth {
    fs::read_to_string(health_path(root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn write_plugin_health(
    root: &std::path::Path,
    health: &PluginHealth,
) -> Result<(), Box<dyn Error>> {
    let path = health_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", next_request_id()));
    fs::write(&temporary, serde_json::to_vec_pretty(health)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn record_plugin_failure(root: &std::path::Path, error: &str) {
    let mut health = read_plugin_health(root);
    let was_unhealthy = health.failure_count >= PLUGIN_FAILURE_THRESHOLD;
    health.failure_count = health
        .failure_count
        .saturating_add(1)
        .min(PLUGIN_FAILURE_THRESHOLD);
    health.last_failure_at = Some(unix_now());
    health.last_error = Some(error.chars().take(2048).collect());
    let _ = write_plugin_health(root, &health);
    if !was_unhealthy && health.failure_count >= PLUGIN_FAILURE_THRESHOLD {
        // The plugin just crossed into the unavailable state.
        crate::capability::service::invalidate_capability_discovery();
    }
}

fn clear_plugin_health(root: &std::path::Path) {
    let _ = fs::remove_file(health_path(root));
}

pub(crate) fn reset_plugin_health(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    let root = plugin_registry_dir().join(plugin_id);
    clear_plugin_health(&root);
    clear_plugin_health(
        &development_registry_path()
            .with_file_name("plugin-development-health")
            .join(format!("{plugin_id}.json")),
    );
    crate::capability::service::invalidate_capability_discovery();
    Ok(())
}

pub(crate) fn invoke_plugin_capability(
    capability_id: &str,
    input: Value,
    trusted_dashboard_url: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let plugin = scan_plugins()?
        .into_iter()
        .find(|item| {
            item.enabled
                && item.runtime == "process-jsonrpc-stdio"
                && item
                    .capabilities
                    .iter()
                    .any(|capability| capability.id == capability_id)
        })
        .ok_or_else(|| format!("plugin capability not found: {capability_id}"))?;

    invoke_plugin_capability_for_item(&plugin, capability_id, input, trusted_dashboard_url)
}

pub(crate) fn invoke_plugin_capability_for_plugin(
    plugin_id: &str,
    capability_id: &str,
    input: Value,
    trusted_dashboard_url: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let plugin = scan_plugins()?
        .into_iter()
        .find(|item| item.id == plugin_id && item.enabled)
        .ok_or_else(|| format!("plugin not found or unavailable: {plugin_id}"))?;
    invoke_plugin_capability_for_item(&plugin, capability_id, input, trusted_dashboard_url)
}

fn invoke_plugin_capability_for_item(
    plugin: &PluginRegistryItem,
    capability_id: &str,
    input: Value,
    trusted_dashboard_url: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let capability = plugin
        .capabilities
        .iter()
        .find(|capability| capability.id == capability_id)
        .ok_or_else(|| format!("plugin capability not found: {capability_id}"))?;
    validate_input_schema(&capability.input_schema, &input)?;
    let timeout = plugin_invocation_timeout(capability, &input);
    let active = ACTIVE_INVOCATIONS.fetch_add(1, Ordering::AcqRel);
    if active >= MAX_PLUGIN_INVOCATIONS {
        ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::Release);
        return Err("plugin invocation limit reached".into());
    }
    let _active_invocation = ActiveInvocationGuard;

    let result = invoke_plugin_process(
        &plugin,
        capability,
        capability_id,
        input,
        trusted_dashboard_url,
        timeout,
    );
    let health_root = plugin_health_root(
        std::path::Path::new(&plugin.path),
        plugin.development,
        &plugin.id,
    );
    if result.is_ok() {
        clear_plugin_health(&health_root);
    } else if let Err(error) = &result {
        // 只有进程/协议故障才算插件不健康；能力层错误与超限响应只属于这次调用。
        let records = error
            .downcast_ref::<PluginInvocationError>()
            .map(PluginInvocationError::records_health)
            .unwrap_or(true);
        if records {
            record_plugin_failure(&health_root, &error.to_string());
        }
    }
    result
}

fn invoke_plugin_process(
    plugin: &PluginRegistryItem,
    capability: &PluginCapabilityManifest,
    capability_id: &str,
    input: Value,
    trusted_dashboard_url: Option<&str>,
    timeout: Duration,
) -> Result<Value, Box<dyn Error>> {
    if plugin.status != "installed" {
        return Err(format!("plugin is not installed: {}", plugin.id).into());
    }
    if plugin.runtime != "process-jsonrpc-stdio" {
        return Err(format!("unsupported plugin runtime: {}", plugin.runtime).into());
    }

    let entry = plugin_manifest_entry(&plugin)?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": next_request_id(),
        "method": capability_id,
        "params": input,
    });

    let mut command = crate::runtime::process::hidden_command(entry);
    command
        .current_dir(plugin_execution_dir(&plugin))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("HIMIND_CONTROL_PLANE_URL")
        .env_remove("HIMIND_DASHBOARD_URL")
        .env_remove("HIMIND_API_BASE");
    if let Some(url) = trusted_plugin_dashboard_url(plugin, capability, trusted_dashboard_url) {
        // The Agent owns the trusted endpoint. Plugins never select or persist
        // an arbitrary control-plane URL supplied by the AI caller.
        command
            .env("HIMIND_CONTROL_PLANE_URL", url)
            .env("HIMIND_DASHBOARD_URL", url);
    }
    // Extensions own their runtime state. The Agent only guarantees a stable,
    // writable directory that survives upgrades, so an extension UI can read the
    // same data the workflow produced without the workflow carrying storage policy.
    let data_dir = plugin_data_dir(&plugin.id);
    if let Err(error) = fs::create_dir_all(&data_dir) {
        return Err(format!(
            "failed to prepare plugin data directory for {}: {error}",
            plugin.id
        )
        .into());
    }
    command.env("HIMIND_PLUGIN_DATA_ROOT", &data_dir);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start plugin {}: {error}", plugin.id))?;

    // The Agent launches one short-lived plugin process per capability call.
    // Close the request pipe after the single JSON-RPC message so a normal
    // stdio server can observe EOF and exit after writing its response. If we
    // leave `ChildStdin` attached to the child, the response is readable but
    // `child.wait()` blocks until the plugin timeout because the server keeps
    // waiting for another request.
    {
        let Some(mut stdin) = child.stdin.take() else {
            terminate_plugin_child(&mut child);
            return Err(format!("plugin stdin unavailable: {}", plugin.id).into());
        };
        if let Err(error) = writeln!(stdin, "{}", request) {
            terminate_plugin_child(&mut child);
            return Err(error.into());
        }
    }

    let Some(stdout) = child.stdout.take() else {
        terminate_plugin_child(&mut child);
        return Err(format!("plugin stdout unavailable: {}", plugin.id).into());
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_plugin_child(&mut child);
        return Err(format!("plugin stderr unavailable: {}", plugin.id).into());
    };
    let (response_tx, response_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        let result = reader
            .by_ref()
            .take((max_plugin_response_bytes() + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map(|_| bytes);
        let _ = response_tx.send(result);
    });
    thread::spawn(move || {
        let mut reader = stderr.take((MAX_PLUGIN_STDERR_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
    });

    let response_bytes = match response_rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            terminate_plugin_child(&mut child);
            return Err(error.into());
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_plugin_child(&mut child);
            return Err(format!(
                "plugin timed out after {} seconds: {}",
                timeout.as_secs(),
                plugin.id
            )
            .into());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            terminate_plugin_child(&mut child);
            return Err(format!("plugin output channel closed: {}", plugin.id).into());
        }
    };

    let status = match wait_plugin_child(&mut child) {
        Ok(Some(status)) => status,
        Ok(None) => {
            terminate_plugin_child(&mut child);
            return Err(format!(
                "plugin did not exit after returning a response: {}",
                plugin.id
            )
            .into());
        }
        Err(error) => {
            terminate_plugin_child(&mut child);
            return Err(error.into());
        }
    };
    if !status.success() {
        return Err(format!("plugin exited with status: {status}").into());
    }
    let response_limit = max_plugin_response_bytes();
    if response_bytes.len() > response_limit {
        return Err(PluginInvocationError::ResponseTooLarge {
            plugin: plugin.id.clone(),
            capability: capability_id.to_string(),
            observed_bytes: response_bytes.len(),
            limit: response_limit,
        }
        .into());
    }
    let response_line = String::from_utf8(response_bytes)?;
    if response_line.trim().is_empty() {
        return Err(format!("plugin returned empty response: {}", plugin.id).into());
    }

    let response: Value = serde_json::from_str(response_line.trim())?;
    if let Some(error) = response.get("error") {
        // 插件正常应答、只是这次能力调用报了错（例如参数不合法或业务前置不满足）：
        // 这是能力层结果，不属于插件健康问题，不能据此把插件和它的依赖者一起判死。
        return Err(PluginInvocationError::Capability {
            capability: capability_id.to_string(),
            message: error.to_string(),
        }
        .into());
    }
    Ok(response.get("result").cloned().unwrap_or(response))
}

fn terminate_plugin_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_plugin_child(
    child: &mut std::process::Child,
) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
    let deadline = Instant::now() + PLUGIN_EXIT_GRACE_PERIOD;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn trusted_plugin_dashboard_url<'a>(
    plugin: &PluginRegistryItem,
    capability: &PluginCapabilityManifest,
    trusted_dashboard_url: Option<&'a str>,
) -> Option<&'a str> {
    let control_plane_capability = matches!(
        capability.availability.trim().to_ascii_lowercase().as_str(),
        "control_plane" | "dashboard"
    );
    let dashboard_permission = plugin
        .permissions
        .iter()
        .any(|permission| permission == "network.dashboard.public");
    trusted_dashboard_url
        .map(str::trim)
        .filter(|url| !url.is_empty() && control_plane_capability && dashboard_permission)
}

fn validate_input_schema(schema: &Value, input: &Value) -> Result<(), Box<dyn Error>> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        let object = input.as_object().ok_or("plugin input must be an object")?;
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(format!("plugin input is missing required property: {name}").into());
                }
            }
        }
        if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                if let Some(unknown) = object.keys().find(|key| !properties.contains_key(*key)) {
                    return Err(format!("plugin input contains unknown property: {unknown}").into());
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, property_schema) in properties {
                if let Some(value) = object.get(name) {
                    validate_json_type(name, property_schema, value)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_json_type(name: &str, schema: &Value, value: &Value) -> Result<(), Box<dyn Error>> {
    let expected = schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let valid = match expected {
        "string" => value.is_string(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "" => true,
        _ => true,
    };
    if !valid {
        return Err(format!("plugin input property has invalid type: {name}").into());
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value
            .as_f64()
            .map(|number| number < minimum)
            .unwrap_or(false)
        {
            return Err(format!("plugin input property is below minimum: {name}").into());
        }
    }
    Ok(())
}

fn plugin_manifest_entry(plugin: &PluginRegistryItem) -> Result<PathBuf, Box<dyn Error>> {
    let root = plugin_execution_dir(plugin);
    let manifest_path = root.join("plugin.json");
    let manifest_content = fs::read_to_string(manifest_path)?;
    let manifest = parse_plugin_manifest(&manifest_content)?;
    if manifest.entry.trim().is_empty() {
        return Err(format!("plugin entry is required: {}", plugin.id).into());
    }
    let entry = PathBuf::from(manifest.entry);
    if entry.is_absolute() {
        return Err(format!("plugin entry must be relative: {}", plugin.id).into());
    }
    let root = root.canonicalize()?;
    let entry = root.join(entry).canonicalize()?;
    if !entry.starts_with(&root) {
        return Err(format!(
            "plugin entry must stay inside plugin directory: {}",
            plugin.id
        )
        .into());
    }
    Ok(entry)
}

pub(crate) fn plugin_execution_dir(plugin: &PluginRegistryItem) -> PathBuf {
    let root = PathBuf::from(&plugin.path);
    let current = root.join("current");
    if current.join("plugin.json").exists() {
        current
    } else {
        root
    }
}

/// 依赖锁校验用的插件内容目录：已安装副本取 `versions/<当前版本>`，开发直挂取源码目录。
///
/// 不能直接用 `path`（插件产品根）：那个目录里还躺着 `current/`、`previous/`、
/// `versions/` 和安装期写进去的 `policy.json`（记录来源、治理、授权等本机状态），
/// 同一个版本在不同机器上装一次摘要就变一次，跨机器校验必然对不上。版本目录是
/// 打包产物的原样落点，只有它才和发布侧算出来的摘要一致。
pub(crate) fn plugin_content_dir(plugin: &PluginRegistryItem) -> PathBuf {
    let root = PathBuf::from(&plugin.path);
    let version = plugin.version.trim();
    if validate_plugin_version(version).is_ok() {
        let version_dir = root.join("versions").join(version);
        if version_dir.is_dir() {
            return version_dir;
        }
    }
    // 开发直挂（免安装登记）的插件目录本身就是内容根，没有 `versions/<版本>` 这一层。
    root
}

/// Returns the private, per-plugin data directory.
///
/// Extensions own their runtime state (archives, caches, indexes) and the Agent
/// only guarantees a stable, writable location that survives plugin upgrades and
/// rollbacks. The path is derived from the plugin id instead of the manifest, so
/// a plugin can never declare itself into another extension's directory.
pub(crate) fn plugin_data_dir(plugin_id: &str) -> PathBuf {
    let safe_id = if is_safe_resource_segment(plugin_id) {
        plugin_id.to_string()
    } else {
        let sanitized: String = plugin_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        // 清洗解决不了 `..`、以点号结尾这类名字（点号本来就在字符集里），
        // 它们拼进路径会被文件系统解释成别的目录，所以再加一个固定前缀。
        if crate::path_guard::is_safe_dir_name(&sanitized) {
            sanitized
        } else {
            format!("plugin-{}", sanitized.trim_end_matches(['.', ' ']))
        }
    };
    if let Some(root) = env::var_os("HIMIND_PLUGIN_DATA_ROOT") {
        let root = PathBuf::from(root);
        if !root.as_os_str().is_empty() {
            return root.join(&safe_id);
        }
    }
    crate::store::paths::agent_home()
        .join("plugin-data")
        .join(safe_id)
}

pub(crate) fn validate_manifest_contributions(
    plugin_path: &std::path::Path,
    manifest: &PluginManifest,
) -> Result<(), Box<dyn Error>> {
    let execution_dir = {
        let current = plugin_path.join("current");
        if current.join("plugin.json").exists() {
            current
        } else {
            plugin_path.to_path_buf()
        }
    };
    let root = execution_dir.canonicalize()?;
    if !is_safe_resource_segment(&manifest.id) {
        return Err(format!("invalid plugin id: {}", manifest.id).into());
    }
    // 版本号会被拼成 `versions/<版本>`。它不参与任何"用户可见的名字"，却直接
    // 决定安装落点，所以必须和 ID 用同一把尺子量。
    validate_plugin_version(&manifest.version)?;
    validate_independent_capability_contract(manifest)?;
    let mut dependency_ids = std::collections::HashSet::new();
    for dependency in &manifest.plugin_dependencies {
        if !is_safe_resource_segment(&dependency.plugin_id) {
            return Err(format!("invalid plugin dependency id: {}", dependency.plugin_id).into());
        }
        if dependency.plugin_id == manifest.id {
            return Err("plugin cannot depend on itself".into());
        }
        if !dependency_ids.insert(dependency.plugin_id.as_str()) {
            return Err(format!("duplicate plugin dependency: {}", dependency.plugin_id).into());
        }
        if !dependency.min_version.trim().is_empty()
            && semver::Version::parse(&dependency.min_version).is_err()
        {
            return Err(format!(
                "invalid plugin dependency version: {}",
                dependency.min_version
            )
            .into());
        }
    }
    let mut view_ids = std::collections::HashSet::new();
    for view in &manifest.contributes.views {
        if !is_safe_resource_segment(&view.id) || view.title.trim().is_empty() {
            return Err("plugin view id and title are required".into());
        }
        if !view_ids.insert(view.id.as_str()) {
            return Err(format!("duplicate plugin view id: {}", view.id).into());
        }
        if !matches!(view.location.as_str(), "plugin_navigation" | "host_panel") {
            return Err(format!("unsupported plugin view location: {}", view.location).into());
        }
        let entry = relative_plugin_resource(&root, &view.entry)?;
        if entry.extension().and_then(|value| value.to_str()) != Some("html") {
            return Err(format!("plugin view entry must be an HTML file: {}", view.entry).into());
        }
    }
    let mut command_ids = std::collections::HashSet::new();
    for command in &manifest.contributes.commands {
        if command.id.trim().is_empty() || command.title.trim().is_empty() {
            return Err("plugin command id and title are required".into());
        }
        if !command_ids.insert(command.id.as_str()) {
            return Err(format!("duplicate plugin command id: {}", command.id).into());
        }
    }
    Ok(())
}

/// Short-video creation is a local production workflow. Keep that boundary
/// enforced by the Agent as well as by the extension repository so a future
/// manifest edit cannot accidentally turn it into a Dashboard capability.
fn validate_independent_capability_contract(
    manifest: &PluginManifest,
) -> Result<(), Box<dyn Error>> {
    if !manifest
        .capabilities
        .iter()
        .any(|capability| capability.id.starts_with("short.video."))
    {
        return Ok(());
    }
    if manifest
        .permissions
        .iter()
        .any(|permission| permission == "network.dashboard.public")
    {
        return Err(
            "short.video.* capabilities cannot request network.dashboard.public; the workflow must remain independent"
                .into(),
        );
    }
    for capability in manifest
        .capabilities
        .iter()
        .filter(|capability| capability.id.starts_with("short.video."))
    {
        if capability.availability.trim().to_ascii_lowercase() != "local" {
            return Err(format!(
                "short.video.* capability {} must declare availability=local",
                capability.id
            )
            .into());
        }
    }
    Ok(())
}

pub(crate) fn validate_development_entry(
    plugin_path: &std::path::Path,
    manifest: &PluginManifest,
) -> Result<(), Box<dyn Error>> {
    if manifest.runtime != "process-jsonrpc-stdio" {
        return Err(format!(
            "unsupported development plugin runtime: {}",
            manifest.runtime
        )
        .into());
    }
    if manifest.entry.trim().is_empty() {
        return Err("development plugin entry is required".into());
    }
    let root = plugin_path.canonicalize()?;
    let relative = PathBuf::from(&manifest.entry);
    if relative.is_absolute() {
        return Err("development plugin entry must be relative".into());
    }
    let entry = root.join(relative).canonicalize()?;
    if !entry.starts_with(&root) || !entry.is_file() {
        return Err("development plugin entry must be a file inside the project directory".into());
    }
    Ok(())
}

fn is_safe_resource_segment(value: &str) -> bool {
    !value.is_empty()
        && crate::path_guard::is_safe_dir_name(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// 插件版本号会原样拼进 `versions/<版本>` 目录名，因此比"任意字符串"更严：
/// 字符集受限之外，还必须是文件系统会当成普通名字的那种（不是 `.`、`..`、
/// 也不是以点号结尾的名字，后者在 Windows 上会被规范化掉）。
///
/// 这里不强制三段式语义化版本：老包可能用 `1.0` 这类写法，装不上会变成
/// 用户侧的功能回归；真正要挡住的是"版本号把安装落点挪出插件目录"。
pub(crate) fn validate_plugin_version(version: &str) -> Result<(), Box<dyn Error>> {
    let trimmed = version.trim();
    if !crate::path_guard::is_safe_dir_name(trimmed)
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
    {
        return Err(format!("invalid plugin version: {version}").into());
    }
    Ok(())
}

fn relative_plugin_resource(
    root: &std::path::Path,
    relative: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let relative_path = PathBuf::from(relative);
    if relative.trim().is_empty() || relative_path.is_absolute() {
        return Err(format!("plugin resource entry must be relative: {relative}").into());
    }
    let resolved = root.join(relative_path).canonicalize()?;
    if !resolved.starts_with(root) {
        return Err(format!("plugin resource entry escapes plugin directory: {relative}").into());
    }
    Ok(resolved)
}

fn next_request_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("req_{millis}_{sequence}")
}

pub(crate) fn parse_plugin_manifest(content: &str) -> Result<PluginManifest, serde_json::Error> {
    serde_json::from_str(content.trim_start_matches('\u{feff}'))
}

fn default_risk_level() -> String {
    "read_only".to_string()
}

fn default_plugin_capability_timeout() -> u64 {
    PLUGIN_TIMEOUT.as_secs()
}

fn plugin_invocation_timeout(capability: &PluginCapabilityManifest, input: &Value) -> Duration {
    let maximum = capability.timeout_seconds.clamp(1, 60 * 60);
    let requested = input
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(maximum)
        .clamp(1, maximum);
    Duration::from_secs(requested)
}

fn default_true() -> bool {
    true
}

fn default_plugin_governance() -> String {
    "optional".to_string()
}

fn default_plugin_author() -> String {
    "未知作者".to_string()
}

fn default_view_location() -> String {
    "plugin_navigation".to_string()
}

fn default_view_icon() -> String {
    "app-window".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn plugin_data_directory_is_derived_from_the_plugin_id() {
        let root = std::env::temp_dir().join(format!("agent-plugin-data-{}", next_request_id()));
        let previous = env::var_os("HIMIND_PLUGIN_DATA_ROOT");
        env::set_var("HIMIND_PLUGIN_DATA_ROOT", &root);

        let path = plugin_data_dir("com.himind.tech-radar");
        assert_eq!(path, root.join("com.himind.tech-radar"));
        // A hostile or legacy manifest id must not escape the plugin data root.
        let escaped = plugin_data_dir("../../production");
        assert!(escaped.starts_with(&root));
        assert_eq!(
            escaped.file_name().and_then(|value| value.to_str()),
            Some(".._.._production")
        );

        match previous {
            Some(value) => env::set_var("HIMIND_PLUGIN_DATA_ROOT", value),
            None => env::remove_var("HIMIND_PLUGIN_DATA_ROOT"),
        }
    }

    #[test]
    fn capability_timeout_respects_the_declared_upper_bound() {
        let capability = PluginCapabilityManifest {
            id: "example.long".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            risk_level: "process".to_string(),
            availability: "local".to_string(),
            timeout_seconds: 1800,
        };
        assert_eq!(
            plugin_invocation_timeout(&capability, &serde_json::json!({})).as_secs(),
            1800
        );
        assert_eq!(
            plugin_invocation_timeout(&capability, &serde_json::json!({"timeout_seconds": 600}))
                .as_secs(),
            600
        );
        assert_eq!(
            plugin_invocation_timeout(&capability, &serde_json::json!({"timeout_seconds": 7200}))
                .as_secs(),
            1800
        );
    }

    #[test]
    fn ignores_registry_support_directories_without_plugin_manifest() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-scan-test-{}", next_request_id()));
        let candidates = root.join("candidates");
        let installed = root.join("com.himind.example").join("current");
        fs::create_dir_all(&candidates).unwrap();
        fs::create_dir_all(&installed).unwrap();
        fs::write(installed.join("plugin.json"), "{}").unwrap();

        assert!(!is_plugin_install_directory(&candidates));
        assert!(is_plugin_install_directory(installed.parent().unwrap()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_plugin_item_includes_release_notes() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-release-notes-{}", next_request_id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("plugin.json"),
            r#"{"id":"com.himind.release-notes-test","name":"更新说明测试","version":"1.2.3","release_notes":"新增本机详情更新说明。"}"#,
        )
        .unwrap();

        let item = read_plugin_item(root.clone(), false);
        assert_eq!(item.version, "1.2.3");
        assert_eq!(item.release_notes, "新增本机详情更新说明。");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn required_plugin_dependency_blocks_missing_or_old_provider() {
        let dependent = builtin_plugin("com.example.dependent", "依赖测试", "依赖测试", &[], &[]);
        let mut dependent = dependent;
        dependent.plugin_dependencies = vec![PluginDependencyManifest {
            plugin_id: "com.example.provider".to_string(),
            required: true,
            min_version: "2.0.0".to_string(),
        }];

        let missing = plugin_dependency_issues(&dependent, &[]);
        assert_eq!(missing, vec!["缺少必需插件 com.example.provider"]);

        let mut old = builtin_plugin("com.example.provider", "能力提供者", "能力提供者", &[], &[]);
        old.version = "1.0.0".to_string();
        let outdated = plugin_dependency_issues(&dependent, &[old]);
        assert_eq!(outdated, vec!["插件 com.example.provider 版本低于 2.0.0"]);
    }

    #[test]
    fn optional_plugin_dependency_does_not_block_runtime() {
        let mut dependent = builtin_plugin(
            "com.example.dependent",
            "可选依赖测试",
            "可选依赖测试",
            &[],
            &[],
        );
        dependent.plugin_dependencies = vec![PluginDependencyManifest {
            plugin_id: "com.example.optional".to_string(),
            required: false,
            min_version: "1.0.0".to_string(),
        }];

        assert!(plugin_dependency_issues(&dependent, &[]).is_empty());
    }

    #[test]
    fn required_plugin_dependency_cycle_blocks_every_cycle_member() {
        let mut first = builtin_plugin("com.example.first", "第一个插件", "测试", &[], &[]);
        first.plugin_dependencies = vec![PluginDependencyManifest {
            plugin_id: "com.example.second".to_string(),
            required: true,
            min_version: "1.0.0".to_string(),
        }];
        let mut second = builtin_plugin("com.example.second", "第二个插件", "测试", &[], &[]);
        second.plugin_dependencies = vec![PluginDependencyManifest {
            plugin_id: "com.example.first".to_string(),
            required: true,
            min_version: "1.0.0".to_string(),
        }];
        let mut upstream = builtin_plugin("com.example.upstream", "上游插件", "测试", &[], &[]);
        upstream.plugin_dependencies = vec![PluginDependencyManifest {
            plugin_id: "com.example.first".to_string(),
            required: true,
            min_version: "1.0.0".to_string(),
        }];

        let issues = plugin_dependency_cycle_issues(&[first, second, upstream]);
        assert_eq!(issues.len(), 2);
        assert!(issues.contains_key("com.example.first"));
        assert!(issues.contains_key("com.example.second"));
        assert!(!issues.contains_key("com.example.upstream"));
        assert!(issues["com.example.first"]
            .contains("com.example.first -> com.example.second -> com.example.first"));
    }

    #[test]
    fn mixed_plugin_remains_visible_when_it_has_local_capabilities() {
        let manifest = parse_plugin_manifest(
            r#"{
            "id":"com.himind.mixed-test",
            "name":"混合能力测试",
            "version":"1.0.0",
            "permissions":["network.dashboard.public"],
            "capabilities":[
                {"id":"mixed.inspect","availability":"local"},
                {"id":"mixed.publish","availability":"control_plane"}
            ]
        }"#,
        )
        .unwrap();
        assert_eq!(manifest_availability(&manifest), "local");
    }

    #[test]
    fn short_video_manifest_is_required_to_be_local_only() {
        let local = parse_plugin_manifest(
            r#"{
            "id":"com.himind.short-video-test",
            "name":"短视频测试",
            "version":"1.0.0",
            "capabilities":[{"id":"short.video.project.create","availability":"local"}]
        }"#,
        )
        .unwrap();
        assert!(validate_independent_capability_contract(&local).is_ok());

        let dashboard = parse_plugin_manifest(
            r#"{
            "id":"com.himind.short-video-dashboard-test",
            "name":"短视频测试",
            "version":"1.0.0",
            "capabilities":[{"id":"short.video.project.create","availability":"control_plane"}]
        }"#,
        )
        .unwrap();
        assert!(validate_independent_capability_contract(&dashboard).is_err());

        let permission = parse_plugin_manifest(
            r#"{
            "id":"com.himind.short-video-permission-test",
            "name":"短视频测试",
            "version":"1.0.0",
            "permissions":["network.dashboard.public"],
            "capabilities":[{"id":"short.video.project.create","availability":"local"}]
        }"#,
        )
        .unwrap();
        assert!(validate_independent_capability_contract(&permission).is_err());
    }

    #[test]
    fn dashboard_only_plugin_is_control_plane() {
        let manifest = parse_plugin_manifest(
            r#"{
            "id":"com.himind.dashboard-test",
            "name":"控制面能力测试",
            "version":"1.0.0",
            "permissions":["network.dashboard.public"],
            "capabilities":[{"id":"dashboard.read","availability":"control_plane"}]
        }"#,
        )
        .unwrap();
        assert_eq!(manifest_availability(&manifest), "control_plane");
    }

    #[test]
    fn dashboard_url_is_injected_only_for_permitted_control_plane_capabilities() {
        let root = std::env::temp_dir().join(format!(
            "agent-plugin-dashboard-environment-{}",
            next_request_id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("plugin.json"),
            r#"{
                "id":"com.himind.dashboard-environment-test",
                "name":"控制面环境测试",
                "version":"1.0.0",
                "runtime":"process-jsonrpc-stdio",
                "permissions":["network.dashboard.public"],
                "capabilities":[
                    {"id":"distribution.resolve","availability":"control_plane"},
                    {"id":"distribution.inspect","availability":"local"}
                ]
            }"#,
        )
        .unwrap();
        let plugin = read_plugin_item(root.clone(), false);
        let resolve = plugin
            .capabilities
            .iter()
            .find(|item| item.id == "distribution.resolve")
            .unwrap();
        let inspect = plugin
            .capabilities
            .iter()
            .find(|item| item.id == "distribution.inspect")
            .unwrap();

        assert_eq!(
            trusted_plugin_dashboard_url(&plugin, resolve, Some("https://dashboard.example")),
            Some("https://dashboard.example")
        );
        assert_eq!(
            trusted_plugin_dashboard_url(&plugin, inspect, Some("https://dashboard.example")),
            None
        );
        assert_eq!(trusted_plugin_dashboard_url(&plugin, resolve, None), None);

        let mut unpermitted = plugin.clone();
        unpermitted.permissions.clear();
        assert_eq!(
            trusted_plugin_dashboard_url(&unpermitted, resolve, Some("https://dashboard.example")),
            None
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_registry_hides_control_plane_builtins() {
        let registry = registry_json_for_control_plane(false).unwrap();
        let items = registry["items"].as_array().unwrap();
        assert!(items
            .iter()
            .any(|item| item["id"] == "com.himind.builtin.svn"));
        assert!(!items
            .iter()
            .any(|item| item["id"] == "com.himind.dashboard-business"));
        assert!(!items
            .iter()
            .any(|item| item["id"] == "com.himind.knowledge"));
        assert_eq!(registry["total"].as_u64().unwrap(), items.len() as u64);
    }

    #[test]
    fn plugin_health_opens_at_threshold_and_clears_on_success() {
        let root = std::env::temp_dir().join(format!("agent-plugin-health-{}", next_request_id()));
        fs::create_dir_all(&root).unwrap();

        record_plugin_failure(&root, "first failure");
        record_plugin_failure(&root, "second failure");
        assert!(read_plugin_health(&root).failure_count < PLUGIN_FAILURE_THRESHOLD);

        record_plugin_failure(&root, "third failure");
        let health = read_plugin_health(&root);
        assert_eq!(health.failure_count, PLUGIN_FAILURE_THRESHOLD);
        assert!(health.last_error.as_deref() == Some("third failure"));

        clear_plugin_health(&root);
        assert_eq!(read_plugin_health(&root).failure_count, 0);
        let _ = fs::remove_dir_all(root);
    }

    /// 熔断是"冷却"而不是"封禁"：冷却窗口内拒绝调用，窗口结束后重新放行，
    /// 让下一次调用有机会成功并把插件带回可用状态。
    #[test]
    fn plugin_breaker_reopens_after_cooldown() {
        let cooling = PluginHealth {
            failure_count: PLUGIN_FAILURE_THRESHOLD,
            last_failure_at: Some(unix_now()),
            last_error: Some("plugin timed out after 30 seconds".to_string()),
        };
        assert!(circuit_is_open(&cooling));

        let cooled_down = PluginHealth {
            last_failure_at: Some(unix_now() - PLUGIN_BREAKER_COOLDOWN.as_secs() - 1),
            ..cooling.clone()
        };
        assert!(!circuit_is_open(&cooled_down));

        let below_threshold = PluginHealth {
            failure_count: PLUGIN_FAILURE_THRESHOLD - 1,
            ..cooling
        };
        assert!(!circuit_is_open(&below_threshold));
    }

    #[test]
    fn legacy_plugin_view_defaults_to_quick_access_metadata() {
        let manifest = parse_plugin_manifest(
            r#"{
                "id":"com.himind.legacy-view",
                "name":"旧版视图插件",
                "version":"1.0.0",
                "contributes": {
                    "views": [{
                        "id":"legacy.main",
                        "title":"旧版工具",
                        "entry":"ui/index.html"
                    }]
                }
            }"#,
        )
        .unwrap();

        let view = manifest.contributes.views.first().unwrap();
        assert_eq!(view.short_title, "");
        assert_eq!(view.icon, "app-window");
        assert!(view.quick_access);
        assert_eq!(view.order, 0);
        assert_eq!(view.location, "plugin_navigation");
    }

    #[test]
    fn plugin_view_can_opt_out_of_quick_access() {
        let manifest = parse_plugin_manifest(
            r#"{
                "id":"com.himind.hidden-view",
                "name":"隐藏快捷入口插件",
                "version":"1.0.0",
                "contributes": {
                    "views": [{
                        "id":"hidden.main",
                        "title":"隐藏工具",
                        "short_title":"隐藏",
                        "icon":"video",
                        "quick_access":false,
                        "order":42,
                        "location":"host_panel",
                        "entry":"ui/index.html"
                    }]
                }
            }"#,
        )
        .unwrap();

        let view = manifest.contributes.views.first().unwrap();
        assert_eq!(view.short_title, "隐藏");
        assert_eq!(view.icon, "video");
        assert!(!view.quick_access);
        assert_eq!(view.order, 42);
        assert_eq!(view.location, "host_panel");
    }

    #[test]
    fn accepts_html_view_inside_plugin_root() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-view-test-{}", next_request_id()));
        fs::create_dir_all(root.join("ui")).unwrap();
        fs::write(root.join("ui/index.html"), "<html></html>").unwrap();
        let manifest = PluginManifest {
            id: "demo.view".to_string(),
            name: "Demo".to_string(),
            author: "测试作者".to_string(),
            description: String::new(),
            release_notes: "测试插件视图。".to_string(),
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            categories: Vec::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
                    short_title: String::new(),
                    icon: default_view_icon(),
                    quick_access: true,
                    order: 0,
                    location: "plugin_navigation".to_string(),
                    entry: "ui/index.html".to_string(),
                }],
                commands: Vec::new(),
            },
        };

        assert!(validate_manifest_contributions(&root, &manifest).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    /// 版本号会被拼成 `versions/<版本>`：`..` 之类会改变落点的写法必须在清单
    /// 校验阶段就被拒，而不是等到拼接路径时才被文件系统解释成上级目录。
    #[test]
    fn rejects_version_that_escapes_the_version_directory() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-version-test-{}", next_request_id()));
        fs::create_dir_all(&root).unwrap();
        let mut manifest = PluginManifest {
            id: "demo.version".to_string(),
            name: "Demo".to_string(),
            author: "测试作者".to_string(),
            description: String::new(),
            release_notes: "测试插件版本号。".to_string(),
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            categories: Vec::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions::default(),
        };
        assert!(validate_manifest_contributions(&root, &manifest).is_ok());

        for version in [".", "..", "...", "1.0.0.", "..\\..\\escaped", "../escaped"] {
            manifest.version = version.to_string();
            assert!(
                validate_plugin_version(version).is_err(),
                "{version:?} 不该通过版本号校验"
            );
            assert!(
                validate_manifest_contributions(&root, &manifest).is_err(),
                "{version:?} 不该通过插件清单校验"
            );
        }
        manifest.version = "0.0.0+sha.abcdef123456".to_string();
        assert!(
            validate_manifest_contributions(&root, &manifest).is_ok(),
            "标准包用内容摘要生成的版本号必须继续可用"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_non_html_view_entry() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-view-test-{}", next_request_id()));
        fs::create_dir_all(root.join("ui")).unwrap();
        fs::write(root.join("ui/index.js"), "console.log(1)").unwrap();
        let manifest = PluginManifest {
            id: "demo.view".to_string(),
            name: "Demo".to_string(),
            author: "测试作者".to_string(),
            description: String::new(),
            release_notes: "测试插件视图。".to_string(),
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            categories: Vec::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
                    short_title: String::new(),
                    icon: default_view_icon(),
                    quick_access: true,
                    order: 0,
                    location: "plugin_navigation".to_string(),
                    entry: "ui/index.js".to_string(),
                }],
                commands: Vec::new(),
            },
        };

        assert!(validate_manifest_contributions(&root, &manifest).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_view_path_escape() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-view-test-{}", next_request_id()));
        fs::create_dir_all(&root).unwrap();
        let outside = root.parent().unwrap().join(format!(
            "agent-plugin-view-outside-{}.html",
            next_request_id()
        ));
        fs::write(&outside, "<html></html>").unwrap();
        let manifest = PluginManifest {
            id: "demo.view".to_string(),
            name: "Demo".to_string(),
            author: "测试作者".to_string(),
            description: String::new(),
            release_notes: "测试插件视图。".to_string(),
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            categories: Vec::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
                    short_title: String::new(),
                    icon: default_view_icon(),
                    quick_access: true,
                    order: 0,
                    location: "plugin_navigation".to_string(),
                    entry: format!("../{}", outside.file_name().unwrap().to_string_lossy()),
                }],
                commands: Vec::new(),
            },
        };

        assert!(validate_manifest_contributions(&root, &manifest).is_err());
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_windows_plugin_ui_origin() {
        for scheme in ["http", "https"] {
            let url = url::Url::parse(&format!(
                "{scheme}://plugin-ui.localhost/demo.multi-cap/demo.multi-cap.overview/ui/index.html"
            ))
            .unwrap();
            assert!(is_plugin_ui_origin(&url));
        }
    }

    #[test]
    fn rejects_unrelated_plugin_ui_origin() {
        let url = url::Url::parse(
            "https://example.com/demo.multi-cap/demo.multi-cap.overview/ui/index.html",
        )
        .unwrap();
        assert!(!is_plugin_ui_origin(&url));
    }

    #[test]
    fn allows_webview_initial_blank_navigation() {
        let url = url::Url::parse("about:blank").unwrap();
        assert!(is_plugin_ui_navigation(&url));
    }

    #[test]
    fn validates_required_and_unknown_plugin_input_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "page": { "type": "integer", "minimum": 1 }
            },
            "required": ["query"],
            "additionalProperties": false
        });

        assert!(validate_input_schema(&schema, &json!({ "query": "rust", "page": 1 })).is_ok());
        assert!(validate_input_schema(&schema, &json!({})).is_err());
        assert!(
            validate_input_schema(&schema, &json!({ "query": "rust", "extra": true })).is_err()
        );
        assert!(validate_input_schema(&schema, &json!({ "query": "rust", "page": 0 })).is_err());
    }

    #[test]
    fn rejects_plugin_input_with_wrong_property_type() {
        let schema = json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        });

        assert!(validate_input_schema(&schema, &json!({ "query": 42 })).is_err());
    }

    #[test]
    fn registers_and_unregisters_development_plugin_without_deleting_source() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-dev-test-{}", next_request_id()));
        let project = root.join("project");
        let registry = root.join("plugin-development.json");
        fs::create_dir_all(project.join("bin")).unwrap();
        fs::write(project.join("bin/demo.exe"), "test executable").unwrap();
        fs::write(
            project.join("plugin.json"),
            r#"{"id":"demo.development","name":"Demo","version":"1.0.0","runtime":"process-jsonrpc-stdio","entry":"bin/demo.exe"}"#,
        )
        .unwrap();

        assert_eq!(
            register_development_plugin_at(&project, &registry).unwrap(),
            "demo.development"
        );
        assert_eq!(development_plugins_at(&registry).len(), 1);
        let item = read_plugin_item(project.clone(), true);
        assert_eq!(item.entry, "bin/demo.exe");
        assert_eq!(item.entry_size, Some(15));
        assert!(item.entry_modified_at.is_some());
        unregister_development_plugin_at("demo.development", &registry).unwrap();
        assert!(development_plugins_at(&registry).is_empty());
        assert!(project.join("plugin.json").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// 一个 Agent 可以同时服务多个工作区会话，每个会话都在登记自己的开发插件。
    /// 并发登记不允许丢条目，也不允许出现读不到的中间态。
    #[test]
    fn concurrent_development_plugin_registrations_lose_nothing() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-dev-concurrent-{}", next_request_id()));
        let registry = root.join("plugin-development.json");
        let projects = (0..6)
            .map(|index| {
                let project = root.join(format!("project-{index}"));
                fs::create_dir_all(project.join("bin")).unwrap();
                fs::write(project.join("bin/demo.exe"), "test executable").unwrap();
                fs::write(
                    project.join("plugin.json"),
                    format!(
                        r#"{{"id":"demo.concurrent.{index}","name":"Demo {index}","version":"1.0.0","runtime":"process-jsonrpc-stdio","entry":"bin/demo.exe"}}"#
                    ),
                )
                .unwrap();
                project
            })
            .collect::<Vec<_>>();

        let barrier = std::sync::Barrier::new(projects.len());
        std::thread::scope(|scope| {
            for project in &projects {
                let registry = registry.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    register_development_plugin_at(project, &registry).unwrap();
                });
            }
        });

        let mut ids = development_plugins_at(&registry)
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            (0..6)
                .map(|index| format!("demo.concurrent.{index}"))
                .collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unbuilt_or_escaping_development_entry() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-dev-test-{}", next_request_id()));
        let project = root.join("project");
        let registry = root.join("plugin-development.json");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("plugin.json"),
            r#"{"id":"demo.unbuilt","name":"Demo","version":"1.0.0","runtime":"process-jsonrpc-stdio","entry":"bin/missing.exe"}"#,
        )
        .unwrap();
        assert!(register_development_plugin_at(&project, &registry).is_err());

        fs::write(root.join("outside.exe"), "test executable").unwrap();
        fs::write(
            project.join("plugin.json"),
            r#"{"id":"demo.escape","name":"Demo","version":"1.0.0","runtime":"process-jsonrpc-stdio","entry":"../outside.exe"}"#,
        )
        .unwrap();
        assert!(register_development_plugin_at(&project, &registry).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_copy_stays_authoritative_until_development_version_is_newer() {
        let root =
            std::env::temp_dir().join(format!("agent-plugin-merge-test-{}", next_request_id()));
        let installed_dir = root.join("plugins/demo.merge");
        let development_dir = root.join("draft/demo.merge");
        let write_package = |dir: &std::path::Path, version: &str| {
            fs::create_dir_all(dir.join("bin")).unwrap();
            fs::write(dir.join("bin/demo.exe"), "test executable").unwrap();
            fs::write(
                dir.join("plugin.json"),
                format!(
                    r#"{{"id":"demo.merge","name":"Demo","version":"{version}","runtime":"process-jsonrpc-stdio","entry":"bin/demo.exe"}}"#
                ),
            )
            .unwrap();
        };
        write_package(&installed_dir, "0.3.13");

        // 旧草稿（更低版本）不接管：这就是用户报的"已安装却挂着 v0.3.11"。
        write_package(&development_dir, "0.3.11");
        let mut items = vec![read_plugin_item(installed_dir.clone(), false)];
        merge_development_items(
            &mut items,
            vec![read_plugin_item(development_dir.clone(), true)],
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].version, "0.3.13");
        assert!(!items[0].development);
        assert!(items[0].overrides_installed_version.is_none());

        // 版本相同也不接管：重建一份同版本草稿不该让已安装条目变成"开发中"。
        write_package(&development_dir, "0.3.13");
        let mut items = vec![read_plugin_item(installed_dir.clone(), false)];
        merge_development_items(
            &mut items,
            vec![read_plugin_item(development_dir.clone(), true)],
        );
        assert_eq!(items[0].version, "0.3.13");
        assert!(!items[0].development);

        // 更高版本接管，但必须留下被接管的已安装版本，界面上两个版本都要说清楚。
        write_package(&development_dir, "0.3.14");
        let mut items = vec![read_plugin_item(installed_dir.clone(), false)];
        merge_development_items(
            &mut items,
            vec![read_plugin_item(development_dir.clone(), true)],
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].version, "0.3.14");
        assert!(items[0].development);
        assert_eq!(
            items[0].overrides_installed_version.as_deref(),
            Some("0.3.13")
        );

        // 没有已安装副本时，开发登记就是这条插件在本机的唯一存在形式。
        let mut items: Vec<PluginRegistryItem> = Vec::new();
        merge_development_items(
            &mut items,
            vec![read_plugin_item(development_dir.clone(), true)],
        );
        assert_eq!(items.len(), 1);
        assert!(items[0].development);
        assert!(items[0].overrides_installed_version.is_none());

        let _ = fs::remove_dir_all(root);
    }
}
