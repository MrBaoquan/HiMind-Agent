use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "extension-workspace.json";
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
    Ok(settings())
}

pub(crate) fn clear() -> Result<ExtensionWorkspaceSettings, Box<dyn std::error::Error>> {
    let path = config_path();
    if path.is_file() {
        fs::remove_file(path)?;
    }
    env::remove_var("HIMIND_EXTENSIONS_ROOT");
    Ok(settings())
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
    let from_environment = env::var_os("HIMIND_EXTENSIONS_ROOT").map(PathBuf::from);
    let configured = from_environment.or_else(|| {
        fs::read_to_string(config_path())
            .ok()
            .and_then(|content| serde_json::from_str::<WorkspaceConfig>(&content).ok())
            .map(|value| PathBuf::from(value.root))
    })?;
    configured.canonicalize().ok().filter(|path| path.is_dir())
}

fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSIONS_WORKSPACE_FILE") {
        return PathBuf::from(path);
    }
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    base.join("HiMindAgent").join(CONFIG_FILE)
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
    use std::time::{SystemTime, UNIX_EPOCH};

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
}
