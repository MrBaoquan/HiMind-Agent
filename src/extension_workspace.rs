use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "extension-workspace.json";
const BINDING_FILE: &str = "extension-workspace-binding.json";
const CATALOG_FILE: &str = "extensions.json";
/// 绑定文件 v2 结构版本。v1 是单值 `{"root": "..."}`，v2 是集合
/// `{"version": 2, "roots": [...]}`。两者都能读，写回统一用 v2。
const BINDING_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionWorkspaceSettings {
    pub configured: bool,
    pub valid: bool,
    pub root: String,
    pub catalog_path: String,
    pub repository: String,
    pub default_branch: String,
    pub extension_count: usize,
    pub error: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredExtension {
    pub kind: String,
    pub id: String,
    pub path: PathBuf,
    pub source_repository: String,
    pub source_default_branch: String,
    pub source_subdirectory: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WorkspaceConfig {
    root: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct WorkspaceBindings {
    #[serde(default = "binding_schema_version")]
    version: u32,
    #[serde(default)]
    roots: Vec<String>,
}

fn binding_schema_version() -> u32 {
    BINDING_SCHEMA_VERSION
}

#[derive(Debug, Clone, Deserialize)]
struct Catalog {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    extensions: Vec<CatalogExtension>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogExtension {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    path: String,
}

pub(crate) fn settings() -> ExtensionWorkspaceSettings {
    let configured_root = configured_root();
    let Some(root) = configured_root else {
        let configured = env::var_os("HIMIND_EXTENSIONS_ROOT").is_some() || config_path().is_file();
        return ExtensionWorkspaceSettings {
            configured,
            valid: false,
            root: String::new(),
            catalog_path: String::new(),
            repository: String::new(),
            default_branch: String::new(),
            extension_count: 0,
            error: if configured {
                "扩展聚合仓库不可用，请重新选择包含 extensions.json 的目录。".to_string()
            } else {
                String::new()
            },
        };
    };
    let root_display = display_path(&root);
    let catalog_path = root.join(CATALOG_FILE);
    match read_catalog(&root) {
        Ok(catalog) => ExtensionWorkspaceSettings {
            configured: true,
            valid: true,
            root: root_display,
            catalog_path: display_path(&catalog_path),
            repository: catalog.repository,
            default_branch: catalog.default_branch,
            extension_count: catalog.extensions.len(),
            error: String::new(),
        },
        Err(error) => ExtensionWorkspaceSettings {
            configured: true,
            valid: false,
            root: root_display,
            catalog_path: display_path(&catalog_path),
            repository: String::new(),
            default_branch: String::new(),
            extension_count: 0,
            error: error.to_string(),
        },
    }
}

pub(crate) fn select(
    root: &Path,
) -> Result<ExtensionWorkspaceSettings, Box<dyn std::error::Error>> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err("扩展工作区必须是目录".into());
    }
    read_catalog(&root)?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(&WorkspaceConfig {
        root: display_path(&root),
    })?;
    fs::write(path, content)?;
    // Apply the selection immediately. This keeps the panel authoritative for
    // the running Agent even when a launcher supplied a temporary root override.
    env::set_var("HIMIND_EXTENSIONS_ROOT", &root);
    // Keep GUI selection and external MCP authoring on the same source of
    // truth. A separately launched MCP companion can reuse the selected root.
    bind(&root)?;
    Ok(settings())
}

pub(crate) fn clear() -> Result<ExtensionWorkspaceSettings, Box<dyn std::error::Error>> {
    // 只解除本次选择所占用的目录，其他 AI 会话记住的工作区不能被牵连。
    let previous = configured_root();
    let path = config_path();
    if path.is_file() {
        fs::remove_file(path)?;
    }
    unbind(previous.as_deref())?;
    env::remove_var("HIMIND_EXTENSIONS_ROOT");
    Ok(settings())
}

/// Bind the current AI authoring session to a local extension workspace.
/// The binding accepts an aggregate repository, a single extension project,
/// or an empty directory before a manifest exists. It is persisted per Agent
/// profile and never opens a folder or requires Dashboard.
///
/// 绑定是**集合**语义：同一个 Agent 进程可能同时服务多个 HiMind AI 会话，
/// 每个会话的工作区都要保留，后一个会话不能覆盖前一个。
pub(crate) fn bind(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let canonical = validate_authoring_root(&root.to_string_lossy())?;
    let _lock = crate::store::atomic_file::lock(&binding_path())?;
    let mut bindings = read_bindings();
    let display = display_path(&canonical);
    bindings.roots.retain(|item| item != &display);
    bindings.roots.push(display);
    write_bindings(&bindings)?;
    Ok(canonical)
}

/// 解除绑定。`None` 表示清除全部绑定（MCP `extension.workspace.clear`
/// 不带参数时的既有语义），`Some(root)` 只解除指定目录。
pub(crate) fn unbind(root: Option<&Path>) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let _lock = crate::store::atomic_file::lock(&binding_path())?;
    let mut bindings = read_bindings();
    let (removed, kept): (Vec<String>, Vec<String>) = match root {
        Some(root) => {
            let key = display_path(&root.canonicalize().unwrap_or_else(|_| root.to_path_buf()));
            bindings
                .roots
                .iter()
                .cloned()
                .partition(|item| item == &key)
        }
        None => (bindings.roots.clone(), Vec::new()),
    };
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    bindings.roots = kept;
    write_bindings(&bindings)?;
    Ok(removed.into_iter().map(PathBuf::from).collect())
}

/// 清除全部工作区绑定。
pub(crate) fn clear_binding() -> Result<(), Box<dyn std::error::Error>> {
    unbind(None)?;
    Ok(())
}

/// 所有已绑定的工作区，按绑定顺序返回。多会话并发时用它列出每个会话的工作区。
pub(crate) fn bound_roots() -> Vec<PathBuf> {
    read_bindings()
        .roots
        .into_iter()
        .map(PathBuf::from)
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir() && !is_agent_managed_path(path))
        .collect()
}

/// 最近一次绑定的工作区。绑定是兜底默认值，不是权威来源。
pub(crate) fn bound_root() -> Option<PathBuf> {
    read_bindings()
        .roots
        .into_iter()
        .rev()
        .map(PathBuf::from)
        .find_map(|path| {
            let path = path.canonicalize().ok()?;
            (path.is_dir() && !is_agent_managed_path(&path)).then_some(path)
        })
}

fn read_bindings() -> WorkspaceBindings {
    let Ok(content) = fs::read_to_string(binding_path()) else {
        return WorkspaceBindings::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return WorkspaceBindings::default();
    };
    if let Some(roots) = value.get("roots").and_then(Value::as_array) {
        return WorkspaceBindings {
            version: BINDING_SCHEMA_VERSION,
            roots: roots
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        };
    }
    // v1 单值绑定文件。
    match value.get("root").and_then(Value::as_str) {
        Some(root) if !root.trim().is_empty() => WorkspaceBindings {
            version: BINDING_SCHEMA_VERSION,
            roots: vec![root.to_string()],
        },
        _ => WorkspaceBindings::default(),
    }
}

fn write_bindings(bindings: &WorkspaceBindings) -> Result<(), Box<dyn std::error::Error>> {
    let path = binding_path();
    if bindings.roots.is_empty() {
        if path.is_file() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut value = bindings.clone();
    value.version = BINDING_SCHEMA_VERSION;
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
    Ok(())
}

/// 校验一个由调用方显式传入的创作工作区。
///
/// 多会话并发下工作区由调用方按次传入，这里只保证它是一个真实存在、且不属于
/// Agent 安装目录或数据目录的目录，不再要求它等于某个全局工作区。
pub(crate) fn validate_authoring_root(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("workspace_root 不能为空".to_string());
    }
    let path = Path::new(trimmed)
        .canonicalize()
        .map_err(|error| format!("无法访问扩展工作区: {error}"))?;
    if !path.is_dir() {
        return Err("扩展工作区必须是目录".to_string());
    }
    if is_agent_managed_path(&path) {
        return Err("不能使用 Agent 安装目录或数据目录作为扩展工作区".to_string());
    }
    Ok(path)
}

/// Returns the effective authoring workspace and its provenance.
/// A valid explicit session workspace remains authoritative. When an external
/// MCP launcher starts in an Agent-managed directory, a persisted binding wins.
pub(crate) fn current_root() -> Result<(PathBuf, &'static str, bool), Box<dyn std::error::Error>> {
    let explicit = env::var_os("HIMIND_AI_WORKSPACE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let binding = bound_root();
    if let Some(path) = explicit.as_ref().and_then(|path| path.canonicalize().ok()) {
        if path.is_dir() && !is_agent_managed_path(&path) {
            return Ok((path, "session", false));
        }
    }
    if let Some(path) = binding {
        return Ok((path, "mcp_binding", true));
    }
    if let Some(path) = explicit.and_then(|path| path.canonicalize().ok()) {
        if path.is_dir() {
            return Ok((path, "session", false));
        }
    }
    Ok((
        env::current_dir()?.canonicalize()?,
        "process_current_dir",
        false,
    ))
}

/// 解析**单次调用**应该使用的扩展工作区。
///
/// 同一个 HiMind AI 进程里的多个工作区会话共用一个 MCP 伴生进程，环境变量
/// `HIMIND_AI_WORKSPACE` 只能承载一个值，谁最后启动谁覆盖。因此调用方显式传入的
/// `workspace_root` 才是唯一可靠的会话身份 —— 它是按次生效的，不会串到别的会话。
/// 只有调用方没传时才回落到会话环境变量、进程目录和历史绑定。
pub(crate) fn resolve_root(
    requested: Option<&str>,
) -> Result<(PathBuf, &'static str, bool), Box<dyn std::error::Error>> {
    if let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        let path = validate_authoring_root(raw)?;
        return Ok((path, "request", false));
    }
    current_root()
}

/// 本次调用已知的创作工作区集合：显式传入的排在最前，其次是会话环境变量指向的
/// 工作区，最后是历史绑定。
///
/// 只带 `package_path` 或扩展身份的调用（例如候选包保存、确认、提审）无法自己
/// 声明工作区，只能靠这个集合判断"这个制品确实来自开发者自己的工作区"。
pub(crate) fn known_authoring_roots(requested: Option<&str>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |candidate: PathBuf| {
        let Ok(path) = candidate.canonicalize() else {
            return;
        };
        if path.is_dir() && !is_agent_managed_path(&path) && !roots.contains(&path) {
            roots.push(path);
        }
    };
    if let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        push(PathBuf::from(raw));
    }
    if let Some(raw) = env::var_os("HIMIND_AI_WORKSPACE").filter(|value| !value.is_empty()) {
        push(PathBuf::from(raw));
    }
    for bound in bound_roots() {
        if !roots.contains(&bound) {
            roots.push(bound);
        }
    }
    roots
}

/// 运行期（技能、工作流）应该落在哪个目录干活。
///
/// 和创作期不同，这里没有调用方显式声明的目录可依赖，只能靠会话环境变量兜底。
/// 历史绑定是**集合**：多个会话各绑定一个目录时，"最近一次绑定"很可能属于别的
/// 会话，拿它当默认值会把 A 会话的任务跑进 B 会话的源码目录。所以只有在绑定唯一
/// 时才敢用它，否则退到进程目录。
pub(crate) fn session_root() -> Option<PathBuf> {
    if let Some(path) = env::var_os("HIMIND_AI_WORKSPACE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .and_then(|value| value.canonicalize().ok())
        .filter(|path| path.is_dir() && !is_agent_managed_path(path))
    {
        return Some(path);
    }
    let bindings = bound_roots();
    if bindings.len() == 1 {
        return bindings.into_iter().next();
    }
    let cwd = env::current_dir().ok()?.canonicalize().ok()?;
    (cwd.is_dir() && !is_agent_managed_path(&cwd)).then_some(cwd)
}

pub(crate) fn classify_path(path: &Path) -> &'static str {
    if path.join(CATALOG_FILE).is_file() {
        "aggregate"
    } else if path.join("plugin.json").is_file() {
        "plugin"
    } else if path.join("skill.json").is_file() {
        "skill"
    } else if path.join("workflow.json").is_file() {
        "workflow"
    } else {
        "directory"
    }
}

pub(crate) fn is_agent_managed_path(path: &Path) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    let data_dir = crate::store::paths::agent_home().canonicalize().ok();
    if data_dir
        .as_ref()
        .is_some_and(|root| canonical == *root || canonical.starts_with(root))
    {
        return true;
    }
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .ok();
    if source_root
        .iter()
        .any(|root| canonical == *root || canonical.starts_with(root))
    {
        return true;
    }
    let Ok(executable) = env::current_exe() else {
        return false;
    };
    let executable_root = executable
        .parent()
        .and_then(|parent| parent.canonicalize().ok());
    executable_root
        .iter()
        .any(|root| canonical == *root || canonical.starts_with(root))
}

pub(crate) fn discover() -> Vec<DiscoveredExtension> {
    let Some(root) = configured_root() else {
        return Vec::new();
    };
    let Ok(catalog) = read_catalog(&root) else {
        return Vec::new();
    };
    catalog
        .extensions
        .into_iter()
        .filter_map(|item| {
            if !matches!(item.kind.as_str(), "plugin" | "skill" | "workflow") {
                return None;
            }
            let path = safe_child_path(&root, &item.path).ok()?;
            let manifest_name = match item.kind.as_str() {
                "plugin" => "plugin.json",
                "skill" => "skill.json",
                "workflow" => "workflow.json",
                _ => return None,
            };
            if !path.join(manifest_name).is_file() {
                return None;
            }
            Some(DiscoveredExtension {
                kind: item.kind,
                id: item.id,
                path,
                source_repository: catalog.repository.clone(),
                source_default_branch: catalog.default_branch.clone(),
                source_subdirectory: item.path.replace('\\', "/"),
            })
        })
        .collect()
}

pub(crate) fn metadata_for_path(path: &Path) -> Option<(String, String, String)> {
    let canonical = path.canonicalize().ok()?;
    discover()
        .into_iter()
        .find(|item| item.path == canonical)
        .map(|item| {
            (
                item.source_repository,
                item.source_default_branch,
                item.source_subdirectory,
            )
        })
}

fn configured_root() -> Option<PathBuf> {
    // A stale launcher override must not mask a valid persisted MCP binding.
    let candidates = [
        env::var_os("HIMIND_EXTENSIONS_ROOT").map(PathBuf::from),
        fs::read_to_string(config_path())
            .ok()
            .and_then(|content| serde_json::from_str::<WorkspaceConfig>(&content).ok())
            .map(|value| PathBuf::from(value.root)),
        bound_root(),
    ];
    candidates.into_iter().flatten().find_map(|candidate| {
        let path = candidate.canonicalize().ok()?;
        (path.is_dir() && !is_agent_managed_path(&path)).then_some(path)
    })
}

fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSIONS_WORKSPACE_FILE") {
        return PathBuf::from(path);
    }
    crate::store::paths::agent_home().join(CONFIG_FILE)
}

fn binding_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSIONS_BINDING_FILE") {
        return PathBuf::from(path);
    }
    config_path()
        .parent()
        .map(|parent| parent.join(BINDING_FILE))
        .unwrap_or_else(|| PathBuf::from(BINDING_FILE))
}

pub(crate) fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    value.strip_prefix(r"\\?\").unwrap_or(&value).to_string()
}

fn read_catalog(root: &Path) -> Result<Catalog, Box<dyn std::error::Error>> {
    let path = root.join(CATALOG_FILE);
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("扩展工作区缺少 extensions.json: {error}"))?;
    let catalog = serde_json::from_str::<Catalog>(&content)
        .map_err(|error| format!("extensions.json 格式无效: {error}"))?;
    if catalog.repository.trim().is_empty() || catalog.default_branch.trim().is_empty() {
        return Err("extensions.json 缺少 repository 或 default_branch".into());
    }
    if catalog.extensions.is_empty() {
        return Err("extensions.json 未声明任何扩展".into());
    }
    let mut ids = std::collections::HashSet::new();
    for item in &catalog.extensions {
        if item.id.trim().is_empty() || item.path.trim().is_empty() {
            return Err("extensions.json 包含空的扩展 ID 或目录".into());
        }
        if item.kind != "plugin" && item.kind != "skill" && item.kind != "workflow" {
            return Err(format!("extensions.json 包含不支持的扩展类型: {}", item.kind).into());
        }
        if !ids.insert(format!("{}:{}", item.kind, item.id.trim())) {
            return Err(format!("extensions.json 包含重复扩展 ID: {}", item.id).into());
        }
        let path = safe_child_path(root, &item.path)?;
        let manifest_name = match item.kind.as_str() {
            "plugin" => "plugin.json",
            "skill" => "skill.json",
            "workflow" => "workflow.json",
            _ => unreachable!(),
        };
        if !path.join(manifest_name).is_file() {
            return Err(format!("扩展目录缺少 {manifest_name}: {}", item.path).into());
        }
        let manifest_id = fs::read_to_string(path.join(manifest_name))
            .ok()
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
            .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_string));
        if manifest_id.as_deref() != Some(item.id.trim()) {
            return Err(format!("扩展清单 ID 与 manifest 不一致: {}", item.path).into());
        }
    }
    Ok(catalog)
}

fn safe_child_path(root: &Path, relative: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let relative_path = Path::new(relative);
    if relative.trim().is_empty() || relative_path.is_absolute() || relative.contains('\\') {
        return Err(format!("扩展目录路径无效: {relative}").into());
    }
    let candidate = root.join(relative_path);
    let canonical = candidate.canonicalize()?;
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("扩展目录越出工作区: {relative}").into());
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// 工作区注册表（创作侧一等实体）
//
// `CONFIG_FILE` 记录"当前选中的聚合仓库"，`BINDING_FILE` 记录"AI 会话绑过哪些
// 目录"，两者都是运行期状态。开发者在界面里维护的开发目录清单是第三种东西：
// 集合语义、允许空目录、允许暂时不可用，只由用户增删。它单独落一个文件，
// 不改变上面两份状态的既有含义。
// ---------------------------------------------------------------------------

const REGISTRY_FILE: &str = "extension-workspaces.json";
const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// 界面里一行「工作区」。`available=false` 表示登记过但目录当前不可用（拔盘、
/// 目录被删）：仍然返回，用户才能把它移除。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionWorkspaceEntry {
    pub root: String,
    pub name: String,
    pub available: bool,
    /// 目录里有 `extensions.json`。没有清单只是"还没有可整体分发的扩展"，
    /// 不是错误；`valid` 才代表清单解析通过。
    pub has_catalog: bool,
    pub valid: bool,
    pub catalog_path: String,
    pub repository: String,
    pub default_branch: String,
    pub extension_count: usize,
    pub error: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct WorkspaceRegistry {
    #[serde(default = "registry_schema_version")]
    version: u32,
    #[serde(default)]
    roots: Vec<String>,
}

fn registry_schema_version() -> u32 {
    REGISTRY_SCHEMA_VERSION
}

fn registry_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSIONS_REGISTRY_FILE") {
        return PathBuf::from(path);
    }
    config_path()
        .parent()
        .map(|parent| parent.join(REGISTRY_FILE))
        .unwrap_or_else(|| PathBuf::from(REGISTRY_FILE))
}

fn read_registry() -> WorkspaceRegistry {
    let Ok(content) = fs::read_to_string(registry_path()) else {
        return WorkspaceRegistry::default();
    };
    let Ok(value) = serde_json::from_str::<WorkspaceRegistry>(&content) else {
        return WorkspaceRegistry::default();
    };
    WorkspaceRegistry {
        version: REGISTRY_SCHEMA_VERSION,
        roots: value
            .roots
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    }
}

fn write_registry(registry: &WorkspaceRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let path = registry_path();
    if registry.roots.is_empty() {
        if path.is_file() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let value = WorkspaceRegistry {
        version: REGISTRY_SCHEMA_VERSION,
        roots: registry.roots.clone(),
    };
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
    Ok(())
}

/// 两个路径是否指向同一个目录。目录不存在时退回字符串比较，避免因为拔盘
/// 而无法移除登记项。
pub(crate) fn same_root(left: &str, right: &str) -> bool {
    let key = |value: &str| match Path::new(value).canonicalize() {
        Ok(path) => display_path(&path),
        Err(_) => value.to_string(),
    };
    let normalize = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    normalize(&key(left)) == normalize(&key(right))
}

/// 登记一个开发目录。允许任何真实目录（不含 `extensions.json` 也可以）。
pub(crate) fn register_root(raw: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let canonical = validate_authoring_root(raw)?;
    let display = display_path(&canonical);
    let _lock = crate::store::atomic_file::lock(&registry_path())?;
    let mut registry = read_registry();
    registry.roots.retain(|item| !same_root(item, &display));
    registry.roots.push(display);
    write_registry(&registry)?;
    Ok(canonical)
}

pub(crate) fn unregister_root(raw: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = crate::store::atomic_file::lock(&registry_path())?;
    let mut registry = read_registry();
    let before = registry.roots.len();
    registry.roots.retain(|item| !same_root(item, raw));
    if registry.roots.len() == before {
        return Ok(());
    }
    write_registry(&registry)?;
    Ok(())
}

/// 登记过的目录，按登记顺序返回；不存在的目录也保留。
pub(crate) fn workspace_entries() -> Vec<ExtensionWorkspaceEntry> {
    let mut entries: Vec<ExtensionWorkspaceEntry> = Vec::new();
    for root in read_registry().roots {
        if entries.iter().any(|entry| same_root(&entry.root, &root)) {
            continue;
        }
        entries.push(workspace_entry(&root));
    }
    entries
}

fn workspace_entry(root: &str) -> ExtensionWorkspaceEntry {
    let path = Path::new(root);
    let available = path.is_dir();
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| root.to_string());
    let catalog_path = path.join(CATALOG_FILE);
    let has_catalog = available && catalog_path.is_file();
    let mut entry = ExtensionWorkspaceEntry {
        root: root.to_string(),
        name,
        available,
        has_catalog,
        valid: false,
        catalog_path: if has_catalog {
            display_path(&catalog_path)
        } else {
            String::new()
        },
        repository: String::new(),
        default_branch: String::new(),
        extension_count: 0,
        error: String::new(),
    };
    if !available {
        entry.error = "目录当前不可用".to_string();
        return entry;
    }
    if !has_catalog {
        return entry;
    }
    match read_catalog(path) {
        Ok(catalog) => {
            entry.valid = true;
            entry.repository = catalog.repository;
            entry.default_branch = catalog.default_branch;
            entry.extension_count = catalog.extensions.len();
        }
        Err(error) => entry.error = error.to_string(),
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl Into<OsString>) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value.into());
            Self { key, previous }
        }

        fn clear(key: &'static str) -> Self {
            let previous = env::var_os(key);
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn unique_root(prefix: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn reads_and_discovers_a_shared_workspace() {
        let root = env::temp_dir().join(format!(
            "himind-extension-workspace-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let plugin = root.join("plugins/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("plugin.json"), r#"{"id":"com.test.demo"}"#).unwrap();
        fs::write(
            root.join(CATALOG_FILE),
            r#"{"repository":"https://github.com/example/extensions.git","default_branch":"main","extensions":[{"type":"plugin","id":"com.test.demo","path":"plugins/demo"}]}"#,
        )
        .unwrap();
        let catalog = read_catalog(&root).unwrap();
        assert_eq!(catalog.extensions.len(), 1);
        assert_eq!(
            safe_child_path(&root, "plugins/demo").unwrap(),
            plugin.canonicalize().unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_workflow_projects_from_an_aggregate_workspace() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-workflow-discovery");
        let workflow = root.join("workflows/demo");
        fs::create_dir_all(&workflow).unwrap();
        fs::write(
            workflow.join("workflow.json"),
            r#"{"id":"com.test.workflow-discovery"}"#,
        )
        .unwrap();
        fs::write(
            root.join(CATALOG_FILE),
            r#"{"repository":"https://github.com/example/extensions.git","default_branch":"main","extensions":[{"type":"workflow","id":"com.test.workflow-discovery","path":"workflows/demo"}]}"#,
        )
        .unwrap();
        let workspace_file = root.join("profile/extension-workspace.json");
        let _workspace_file = EnvGuard::set("HIMIND_EXTENSIONS_WORKSPACE_FILE", &workspace_file);
        let _binding_file = EnvGuard::set(
            "HIMIND_EXTENSIONS_BINDING_FILE",
            root.join("profile/extension-workspace-binding.json"),
        );
        let _root = EnvGuard::set("HIMIND_EXTENSIONS_ROOT", &root);

        let discovered = discover();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].kind, "workflow");
        assert_eq!(discovered[0].id, "com.test.workflow-discovery");
        assert_eq!(classify_path(&workflow), "workflow");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_paths_outside_workspace() {
        let root = env::temp_dir().join(format!(
            "himind-extension-workspace-path-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(safe_child_path(&root, "../outside").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn display_path_preserves_unicode_windows_paths() {
        let path = Path::new(r"F:\WebProjects\项目看板\himind-extensions");
        assert_eq!(
            display_path(path),
            r"F:\WebProjects\项目看板\himind-extensions"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\F:\WebProjects\项目看板\himind-extensions")),
            r"F:\WebProjects\项目看板\himind-extensions"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\扩展")),
            r"\\server\share\扩展"
        );
    }

    #[test]
    fn bind_persists_and_clear_removes_the_external_workspace() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-binding");
        let binding_file = root.join("profile/extension-workspace-binding.json");
        let workspace = root.join("aggregate");
        fs::create_dir_all(&workspace).unwrap();
        let _binding = EnvGuard::set("HIMIND_EXTENSIONS_BINDING_FILE", &binding_file);
        let _session = EnvGuard::clear("HIMIND_AI_WORKSPACE");

        assert_eq!(bind(&workspace).unwrap(), workspace.canonicalize().unwrap());
        let persisted: Value =
            serde_json::from_str(&fs::read_to_string(&binding_file).unwrap()).unwrap();
        assert_eq!(persisted["version"], BINDING_SCHEMA_VERSION);
        assert_eq!(
            persisted["roots"],
            serde_json::json!([display_path(&workspace.canonicalize().unwrap())])
        );
        assert_eq!(bound_root(), Some(workspace.canonicalize().unwrap()));

        let (current, source, bound) = current_root().unwrap();
        assert_eq!(current, workspace.canonicalize().unwrap());
        assert_eq!(source, "mcp_binding");
        assert!(bound);

        clear_binding().unwrap();
        assert_eq!(bound_root(), None);
        assert!(!binding_file.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_sessions_keep_their_own_workspaces() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-multi-binding");
        let binding_file = root.join("profile/extension-workspace-binding.json");
        let first = root.join("repo-one");
        let second = root.join("repo-two");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let _binding = EnvGuard::set("HIMIND_EXTENSIONS_BINDING_FILE", &binding_file);
        let _session = EnvGuard::clear("HIMIND_AI_WORKSPACE");

        // 两个 DSH 工作区会话先后绑定：后一个不能覆盖前一个。
        bind(&first).unwrap();
        bind(&second).unwrap();
        let mut roots = bound_roots();
        roots.sort();
        let mut expected = vec![
            first.canonicalize().unwrap(),
            second.canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(roots, expected);
        // 没有显式 workspace_root 时，兜底用最近一次绑定。
        assert_eq!(bound_root(), Some(second.canonicalize().unwrap()));

        // 显式传入的工作区永远优先，且不受绑定顺序影响。
        let (resolved, source, _) = resolve_root(Some(first.to_str().unwrap())).unwrap();
        assert_eq!(resolved, first.canonicalize().unwrap());
        assert_eq!(source, "request");

        // 重复绑定同一个目录只保留一份。
        bind(&first).unwrap();
        assert_eq!(bound_roots().len(), 2);

        // 只解除自己那一个，另一个会话的工作区仍然保留。
        let removed = unbind(Some(&first)).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(
            removed[0].canonicalize().unwrap(),
            first.canonicalize().unwrap()
        );
        assert_eq!(bound_roots(), vec![second.canonicalize().unwrap()]);

        clear_binding().unwrap();
        assert!(bound_roots().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_binds_do_not_lose_updates() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-binding-race");
        let binding_file = root.join("profile/extension-workspace-binding.json");
        let _binding = EnvGuard::set("HIMIND_EXTENSIONS_BINDING_FILE", &binding_file);
        let _session = EnvGuard::clear("HIMIND_AI_WORKSPACE");
        let workspaces: Vec<PathBuf> = (0..8)
            .map(|index| {
                let path = root.join(format!("repo-{index}"));
                fs::create_dir_all(&path).unwrap();
                path
            })
            .collect();

        std::thread::scope(|scope| {
            for workspace in &workspaces {
                scope.spawn(|| bind(workspace).unwrap());
            }
        });

        let roots = bound_roots();
        assert_eq!(roots.len(), workspaces.len(), "并发绑定不得互相覆盖");
        for workspace in workspaces {
            assert!(roots.contains(&workspace.canonicalize().unwrap()));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_agent_override_yields_to_a_persisted_aggregate_binding() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-binding-fallback");
        let binding_file = root.join("profile/extension-workspace-binding.json");
        let workspace = root.join("aggregate");
        let plugin = workspace.join("plugins/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("plugin.json"), r#"{"id":"com.test.binding"}"#).unwrap();
        fs::write(
            workspace.join(CATALOG_FILE),
            r#"{"repository":"https://github.com/example/extensions.git","default_branch":"main","extensions":[{"type":"plugin","id":"com.test.binding","path":"plugins/demo"}]}"#,
        )
        .unwrap();

        let _binding = EnvGuard::set("HIMIND_EXTENSIONS_BINDING_FILE", &binding_file);
        let stale_root = root.join("stale-agent-root");
        let _root = EnvGuard::set("HIMIND_EXTENSIONS_ROOT", &stale_root);
        let _workspace_file = EnvGuard::set(
            "HIMIND_EXTENSIONS_WORKSPACE_FILE",
            root.join("missing-workspace.json"),
        );
        let _session = EnvGuard::clear("HIMIND_AI_WORKSPACE");

        bind(&workspace).unwrap();
        let discovered = discover();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, "com.test.binding");
        assert_eq!(discovered[0].path, plugin.canonicalize().unwrap());
        assert_eq!(classify_path(&workspace), "aggregate");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_agent_source_and_profile_data_as_extension_workspaces() {
        let _lock = env_lock();
        let root = unique_root("himind-extension-managed");
        let binding_file = root.join("binding.json");
        let _binding = EnvGuard::set("HIMIND_EXTENSIONS_BINDING_FILE", &binding_file);

        assert!(bind(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
        let agent_home = crate::store::paths::agent_home();
        assert!(bind(&agent_home).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
