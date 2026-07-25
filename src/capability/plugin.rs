use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static ACTIVE_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_PLUGIN_INVOCATIONS: usize = 4;
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PLUGIN_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_PLUGIN_STDERR_BYTES: usize = 64 * 1024;
const PLUGIN_FAILURE_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct PluginHealth {
    #[serde(default)]
    failure_count: u32,
    #[serde(default)]
    last_failure_at: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_plugin_author")]
    pub author: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub min_agent_version: String,
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
    pub version: String,
    pub runtime: String,
    pub min_agent_version: String,
    pub governance: String,
    pub status: String,
    pub enabled: bool,
    pub path: String,
    pub development: bool,
    pub entry: String,
    pub entry_modified_at: Option<u64>,
    pub entry_size: Option<u64>,
    pub previous_version: Option<String>,
    pub rollback_available: bool,
    pub capabilities: Vec<PluginCapabilityManifest>,
    pub permissions: Vec<String>,
    pub plugin_dependencies: Vec<PluginDependencyManifest>,
    pub views: Vec<PluginViewContribution>,
    pub commands: Vec<PluginCommandContribution>,
    pub error: Option<String>,
    pub failure_count: u32,
    pub circuit_open: bool,
}

pub(crate) fn plugin_registry_dir() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("HiMindAgent")
        .join("plugins")
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
    let mut entries = development_plugins_at(registry_path);
    entries.retain(|entry| entry.id != manifest.id);
    entries.push(DevelopmentPlugin {
        id: manifest.id.clone(),
        path: root.to_string_lossy().to_string(),
    });
    write_development_plugins_at(registry_path, &entries)?;
    Ok(manifest.id)
}

pub(crate) fn unregister_development_plugin(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    unregister_development_plugin_at(plugin_id, &development_registry_path())
}

fn unregister_development_plugin_at(
    plugin_id: &str,
    registry_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let mut entries = development_plugins_at(registry_path);
    entries.retain(|entry| entry.id != plugin_id);
    write_development_plugins_at(registry_path, &entries)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct DevelopmentPlugin {
    id: String,
    path: String,
}

fn development_plugins() -> Vec<DevelopmentPlugin> {
    development_plugins_at(&development_registry_path())
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(entries)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
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
    for entry in development_plugins() {
        items.retain(|item| item.id != entry.id);
        items.push(read_plugin_item(PathBuf::from(entry.path), true));
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

fn is_plugin_install_directory(path: &std::path::Path) -> bool {
    path.is_dir()
        && (path.join("plugin.json").is_file()
            || path.join("current").join("plugin.json").is_file())
}

pub(crate) fn is_builtin_plugin(plugin_id: &str) -> bool {
    matches!(
        plugin_id,
        "com.himind.builtin.svn" | "com.himind.builtin.smb" | "com.himind.builtin.inner-admin"
    )
}

fn builtin_plugin_items() -> Vec<PluginRegistryItem> {
    vec![
        builtin_plugin(
            "com.himind.builtin.svn",
            "SVN 工作区与仓库",
            "受控 SVN 账号、工作区、建仓、模板和权限能力。",
            &[
                "svn.connection.list",
                "svn.connection.test",
                "exhibit.workspace.checkout",
                "exhibit.workspace.status",
                "exhibit.workspace.update",
                "exhibit.workspace.open",
                "exhibit.repository_path.create",
                "exhibit.repository.initialize_template",
                "project.repository.create",
                "project.repository.exhibits_access.ensure",
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
    ]
}

fn builtin_plugin(
    id: &str,
    name: &str,
    description: &str,
    capability_ids: &[&str],
    permissions: &[&str],
) -> PluginRegistryItem {
    PluginRegistryItem {
        id: id.to_string(),
        name: name.to_string(),
        author_name: "马宝全".to_string(),
        description: description.to_string(),
        version: crate::VERSION.to_string(),
        runtime: "builtin".to_string(),
        min_agent_version: crate::VERSION.to_string(),
        governance: "required".to_string(),
        status: "installed".to_string(),
        enabled: true,
        path: String::new(),
        development: false,
        entry: String::new(),
        entry_modified_at: None,
        entry_size: None,
        previous_version: None,
        rollback_available: false,
        capabilities: capability_ids
            .iter()
            .map(|id| PluginCapabilityManifest {
                id: (*id).to_string(),
                description: description.to_string(),
                input_schema: Value::Null,
                risk_level: "builtin_policy".to_string(),
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
        || (url.scheme() == "http" && url.host_str() == Some("plugin-ui.localhost"))
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
            let circuit_open = health.failure_count >= PLUGIN_FAILURE_THRESHOLD;
            PluginRegistryItem {
                id: manifest.id,
                name: manifest.name,
                author_name: manifest.author,
                description: manifest.description,
                version: manifest.version,
                runtime: manifest.runtime,
                min_agent_version: manifest.min_agent_version,
                governance,
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
                previous_version: previous_plugin_version(&path),
                rollback_available: path.join("current").join("plugin.json").exists()
                    && path.join("previous").join("plugin.json").exists(),
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
            }
        }
        Err(error) => PluginRegistryItem {
            id: default_id.clone(),
            name: default_id,
            author_name: "未知作者".to_string(),
            description: String::new(),
            version: String::new(),
            runtime: String::new(),
            min_agent_version: String::new(),
            governance: "optional".to_string(),
            status: "failed".to_string(),
            enabled: false,
            path: path.to_string_lossy().to_string(),
            development,
            entry: String::new(),
            entry_modified_at: None,
            entry_size: None,
            previous_version: None,
            rollback_available: false,
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            views: Vec::new(),
            commands: Vec::new(),
            error: Some(error.to_string()),
            failure_count: 0,
            circuit_open: false,
        },
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
    let items = scan_plugins()?;
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
    health.failure_count = health
        .failure_count
        .saturating_add(1)
        .min(PLUGIN_FAILURE_THRESHOLD);
    health.last_failure_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_secs());
    health.last_error = Some(error.chars().take(2048).collect());
    let _ = write_plugin_health(root, &health);
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
    Ok(())
}

pub(crate) fn invoke_plugin_capability(
    capability_id: &str,
    input: Value,
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

    invoke_plugin_capability_for_item(&plugin, capability_id, input)
}

pub(crate) fn invoke_plugin_capability_for_plugin(
    plugin_id: &str,
    capability_id: &str,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let plugin = scan_plugins()?
        .into_iter()
        .find(|item| item.id == plugin_id && item.enabled)
        .ok_or_else(|| format!("plugin not found or unavailable: {plugin_id}"))?;
    invoke_plugin_capability_for_item(&plugin, capability_id, input)
}

fn invoke_plugin_capability_for_item(
    plugin: &PluginRegistryItem,
    capability_id: &str,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let capability = plugin
        .capabilities
        .iter()
        .find(|capability| capability.id == capability_id)
        .ok_or_else(|| format!("plugin capability not found: {capability_id}"))?;
    validate_input_schema(&capability.input_schema, &input)?;
    let active = ACTIVE_INVOCATIONS.fetch_add(1, Ordering::AcqRel);
    if active >= MAX_PLUGIN_INVOCATIONS {
        ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::Release);
        return Err("plugin invocation limit reached".into());
    }

    let result = invoke_plugin_process(&plugin, capability_id, input);
    let health_root = plugin_health_root(
        std::path::Path::new(&plugin.path),
        plugin.development,
        &plugin.id,
    );
    if result.is_ok() {
        clear_plugin_health(&health_root);
    } else if let Err(error) = &result {
        record_plugin_failure(&health_root, &error.to_string());
    }
    ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::Release);
    result
}

fn invoke_plugin_process(
    plugin: &PluginRegistryItem,
    capability_id: &str,
    input: Value,
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

    let mut child = Command::new(entry)
        .current_dir(plugin_execution_dir(&plugin))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start plugin {}: {error}", plugin.id))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| format!("plugin stdin unavailable: {}", plugin.id))?;
        writeln!(stdin, "{}", request)?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("plugin stdout unavailable: {}", plugin.id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("plugin stderr unavailable: {}", plugin.id))?;
    let (response_tx, response_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        let result = reader
            .by_ref()
            .take((MAX_PLUGIN_RESPONSE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map(|_| bytes);
        let _ = response_tx.send(result);
    });
    thread::spawn(move || {
        let mut reader = stderr.take((MAX_PLUGIN_STDERR_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
    });

    let response_bytes = match response_rx.recv_timeout(PLUGIN_TIMEOUT) {
        Ok(result) => result?,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "plugin timed out after {} seconds: {}",
                PLUGIN_TIMEOUT.as_secs(),
                plugin.id
            )
            .into());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("plugin output channel closed: {}", plugin.id).into());
        }
    };

    let status = child.wait()?;
    if !status.success() {
        return Err(format!("plugin exited with status: {status}").into());
    }
    if response_bytes.len() > MAX_PLUGIN_RESPONSE_BYTES {
        return Err(format!(
            "plugin response exceeds {} bytes: {}",
            MAX_PLUGIN_RESPONSE_BYTES, plugin.id
        )
        .into());
    }
    let response_line = String::from_utf8(response_bytes)?;
    if response_line.trim().is_empty() {
        return Err(format!("plugin returned empty response: {}", plugin.id).into());
    }

    let response: Value = serde_json::from_str(response_line.trim())?;
    if let Some(error) = response.get("error") {
        return Err(format!("plugin error: {error}").into());
    }
    Ok(response.get("result").cloned().unwrap_or(response))
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
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
                    location: "plugin_navigation".to_string(),
                    entry: "ui/index.html".to_string(),
                }],
                commands: Vec::new(),
            },
        };

        assert!(validate_manifest_contributions(&root, &manifest).is_ok());
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
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
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
            version: "1.0.0".to_string(),
            entry: "plugin.exe".to_string(),
            runtime: "process-jsonrpc-stdio".to_string(),
            min_agent_version: String::new(),
            governance: "optional".to_string(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            plugin_dependencies: Vec::new(),
            contributes: PluginContributions {
                views: vec![PluginViewContribution {
                    id: "demo.view.main".to_string(),
                    title: "Demo".to_string(),
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
        let url = url::Url::parse(
            "http://plugin-ui.localhost/demo.multi-cap/demo.multi-cap.overview/ui/index.html",
        )
        .unwrap();
        assert!(is_plugin_ui_origin(&url));
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
}
