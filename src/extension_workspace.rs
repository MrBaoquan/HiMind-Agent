use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "extension-workspace.json";
const BINDING_FILE: &str = "extension-workspace-binding.json";
const CATALOG_FILE: &str = "extensions.json";

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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WorkspaceBinding {
    root: String,
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
    let path = config_path();
    if path.is_file() {
        fs::remove_file(path)?;
    }
    clear_binding()?;
    env::remove_var("HIMIND_EXTENSIONS_ROOT");
    Ok(settings())
}

/// Bind the current AI authoring session to a local extension workspace.
/// The binding accepts an aggregate repository, a single extension project,
/// or an empty directory before a manifest exists. It is persisted per Agent
/// profile and never opens a folder or requires Dashboard.
pub(crate) fn bind(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("无法访问扩展工作区: {error}"))?;
    if !canonical.is_dir() {
        return Err("扩展工作区必须是目录".into());
    }
    if is_agent_managed_path(&canonical) {
        return Err("不能将 Agent 安装目录或数据目录绑定为扩展工作区".into());
    }
    let path = binding_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(&WorkspaceBinding {
        root: display_path(&canonical),
    })?;
    fs::write(path, content)?;
    Ok(canonical)
}

pub(crate) fn clear_binding() -> Result<(), Box<dyn std::error::Error>> {
    let path = binding_path();
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn bound_root() -> Option<PathBuf> {
    fs::read_to_string(binding_path())
        .ok()
        .and_then(|content| serde_json::from_str::<WorkspaceBinding>(&content).ok())
        .and_then(|binding| PathBuf::from(binding.root).canonicalize().ok())
        .filter(|path| path.is_dir() && !is_agent_managed_path(path))
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

pub(crate) fn classify_path(path: &Path) -> &'static str {
    if path.join(CATALOG_FILE).is_file() {
        "aggregate"
    } else if path.join("plugin.json").is_file() {
        "plugin"
    } else if path.join("skill.json").is_file() {
        "skill"
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
            if item.kind != "plugin" && item.kind != "skill" {
                return None;
            }
            let path = safe_child_path(&root, &item.path).ok()?;
            let manifest_name = if item.kind == "plugin" {
                "plugin.json"
            } else {
                "skill.json"
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
        if item.kind != "plugin" && item.kind != "skill" {
            return Err(format!("extensions.json 包含不支持的扩展类型: {}", item.kind).into());
        }
        if !ids.insert(format!("{}:{}", item.kind, item.id.trim())) {
            return Err(format!("extensions.json 包含重复扩展 ID: {}", item.id).into());
        }
        let path = safe_child_path(root, &item.path)?;
        let manifest_name = if item.kind == "plugin" {
            "plugin.json"
        } else {
            "skill.json"
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
        assert_eq!(
            persisted["root"],
            display_path(&workspace.canonicalize().unwrap())
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
