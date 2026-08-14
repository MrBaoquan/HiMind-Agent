use crate::api::distribution::{
    plugin_catalog, plugin_versions, report_plugin_status, PluginCatalogItem, PluginStatusReport,
    SkillPluginDependency,
};
use crate::app::system::{validate_signature_metadata, verify_rsa_pss_sha256};
use crate::capability::plugin::{is_builtin_plugin, plugin_registry_dir, PluginManifest};
use crate::skill::resolver::compare_versions;
use crate::store::plugin_outbox::{
    list as list_statuses, remove as remove_status, store as store_status, PluginStatusRecord,
};
use crate::Options;
use reqwest::{blocking::Client, Url};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const MAX_PLUGIN_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct LocalPluginStatus {
    pub current_version: String,
    pub previous_version: String,
    pub enabled: bool,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PluginDependencyAction {
    pub plugin_id: String,
    pub plugin_name: String,
    pub plugin_description: String,
    pub required: bool,
    pub current_version: String,
    pub target_version: String,
    pub action: String,
    pub reason: String,
    pub requested_by: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PluginInstallPlan {
    pub plugin: PluginCatalogItem,
    pub dependency_actions: Vec<PluginDependencyAction>,
    pub blocked_reasons: Vec<String>,
    pub ready: bool,
}

pub(crate) fn local_status(plugin_id: &str) -> LocalPluginStatus {
    let Ok(root) = plugin_root(plugin_id) else {
        return LocalPluginStatus::default();
    };
    let current_version = manifest_version(&root.join("current/plugin.json"));
    let previous_version = manifest_version(&root.join("previous/plugin.json"));
    LocalPluginStatus {
        enabled: !root.join("disabled").exists() && !current_version.is_empty(),
        status: if current_version.is_empty() {
            "uninstalled"
        } else if root.join("disabled").exists() {
            "disabled"
        } else {
            "installed"
        }
        .to_string(),
        current_version,
        previous_version,
    }
}

pub(crate) fn report_status(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    action: &str,
    from_version: &str,
    error: &str,
) -> Result<(), Box<dyn Error>> {
    flush_status_outbox(options, agent_id);
    let local = local_status(plugin_id);
    let status = if error.is_empty() {
        local.status.as_str()
    } else {
        "failed"
    };
    let record = PluginStatusRecord {
        agent_id: agent_id.to_string(),
        plugin_id: plugin_id.to_string(),
        action: action.to_string(),
        from_version: from_version.to_string(),
        current_version: local.current_version,
        previous_version: local.previous_version,
        enabled: local.enabled,
        status: status.to_string(),
        error: error.chars().take(2048).collect(),
    };
    if let Err(send_error) = send_status(options, &record) {
        store_status(&options.state_path, &record)?;
        return Err(send_error);
    }
    Ok(())
}

fn send_status(options: &Options, record: &PluginStatusRecord) -> Result<(), Box<dyn Error>> {
    let credential = options.agent_credential();
    if record.agent_id.is_empty() || credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let mut client_builder = Client::builder().timeout(std::time::Duration::from_secs(10));
    if Url::parse(&options.api_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .map(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .map(|address| address.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false)
    {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;
    report_plugin_status(
        &client,
        &options.api_base,
        &record.agent_id,
        &credential,
        &PluginStatusReport {
            plugin_id: &record.plugin_id,
            action: &record.action,
            from_version: &record.from_version,
            current_version: &record.current_version,
            previous_version: &record.previous_version,
            enabled: record.enabled,
            status: &record.status,
            error: &record.error,
        },
    )
}

pub(crate) fn flush_status_outbox(options: &Options, agent_id: &str) {
    let records = match list_statuses(&options.state_path) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("plugin status outbox read failed: {error}");
            return;
        }
    };
    for (path, mut record) in records {
        if record.agent_id.is_empty() {
            record.agent_id = agent_id.to_string();
        }
        match send_status(options, &record) {
            Ok(()) => {
                if let Err(error) = remove_status(&path) {
                    eprintln!("plugin status outbox cleanup failed: {error}");
                }
            }
            Err(error) => {
                eprintln!("plugin status outbox replay failed: {error}");
                break;
            }
        }
    }
}

fn manifest_version(path: &Path) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| {
            serde_json::from_str::<PluginManifest>(content.trim_start_matches('\u{feff}')).ok()
        })
        .map(|manifest| manifest.version)
        .unwrap_or_default()
}

pub(crate) fn local_display_name(plugin_id: &str) -> Option<String> {
    let root = plugin_root(plugin_id).ok()?;
    let content = fs::read_to_string(root.join("current/plugin.json")).ok()?;
    let manifest =
        serde_json::from_str::<PluginManifest>(content.trim_start_matches('\u{feff}')).ok()?;
    (!manifest.name.trim().is_empty()).then_some(manifest.name)
}

pub(crate) fn install(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    version: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let credential = options.agent_credential();
    let catalog = plugin_catalog(&client, &options.api_base, agent_id, &credential)?;
    let plugin = requested_catalog_item(&client, options, agent_id, plugin_id, version, &catalog)?;
    let plan = build_install_plan_for_item(&catalog, plugin)?;
    if !plan.ready {
        return Err(format!("插件安装计划被阻止：{}", plan.blocked_reasons.join("；")).into());
    }
    let root_before = local_status(plugin_id);
    let mut changes = Vec::new();
    for action in &plan.dependency_actions {
        if !action.required || !matches!(action.action.as_str(), "install" | "update") {
            continue;
        }
        let item = catalog
            .iter()
            .find(|item| item.plugin_id == action.plugin_id)
            .ok_or_else(|| format!("依赖插件未上架：{}", action.plugin_name))?;
        let before = local_status(&action.plugin_id);
        if let Err(error) = install_catalog_item(&client, options, agent_id, item) {
            compensate_plugin_changes(&changes);
            return Err(error);
        }
        changes.push((action.plugin_id.clone(), before));
    }
    if let Err(error) = install_catalog_item(&client, options, agent_id, &plan.plugin) {
        compensate_plugin_changes(&changes);
        return Err(error);
    }
    let dependency_owner = format!("plugin:{plugin_id}");
    let dependency_ids = plan
        .dependency_actions
        .iter()
        .filter(|action| action.required)
        .map(|action| action.plugin_id.clone())
        .collect::<Vec<_>>();
    if let Err(error) = set_owner_references(&dependency_owner, &dependency_ids) {
        compensate_plugin_changes(&[(plugin_id.to_string(), root_before)]);
        compensate_plugin_changes(&changes);
        return Err(format!("记录插件依赖失败：{error}").into());
    }
    Ok(())
}

pub(crate) fn plan_install(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    version: Option<&str>,
) -> Result<PluginInstallPlan, Box<dyn Error>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let catalog = plugin_catalog(
        &client,
        &options.api_base,
        agent_id,
        &options.agent_credential(),
    )?;
    let plugin = requested_catalog_item(&client, options, agent_id, plugin_id, version, &catalog)?;
    build_install_plan_for_item(&catalog, plugin)
}

fn requested_catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    version: Option<&str>,
    catalog: &[PluginCatalogItem],
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let latest = catalog
        .iter()
        .find(|item| item.plugin_id == plugin_id)
        .cloned()
        .ok_or_else(|| "插件未上架或当前不可用".to_string())?;
    let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(latest);
    };
    if latest.version == version {
        return Ok(latest);
    }
    if latest.management != "user_managed" || latest.managed {
        return Err("该插件由组织管理，不能切换版本".into());
    }
    plugin_versions(
        client,
        &options.api_base,
        agent_id,
        &options.agent_credential(),
        plugin_id,
    )?
    .into_iter()
    .find(|item| item.version == version)
    .ok_or_else(|| format!("插件版本 v{version} 不可用").into())
}

pub(crate) fn plan_dependency_set(
    options: &Options,
    agent_id: &str,
    dependencies: &[SkillPluginDependency],
    root_name: &str,
) -> Result<(Vec<PluginDependencyAction>, Vec<String>), Box<dyn Error>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let catalog_items = plugin_catalog(
        &client,
        &options.api_base,
        agent_id,
        &options.agent_credential(),
    )?;
    let catalog = catalog_items
        .iter()
        .map(|item| (item.plugin_id.clone(), item))
        .collect::<HashMap<_, _>>();
    let mut actions = Vec::new();
    let mut action_indexes = HashMap::new();
    let mut visiting = HashSet::new();
    let mut blocked_reasons = Vec::new();
    for dependency in dependencies {
        resolve_dependency(
            dependency,
            root_name,
            true,
            &catalog,
            &mut visiting,
            &mut action_indexes,
            &mut actions,
            &mut blocked_reasons,
        );
    }
    blocked_reasons.sort();
    blocked_reasons.dedup();
    Ok((actions, blocked_reasons))
}

fn install_catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &PluginCatalogItem,
) -> Result<(), Box<dyn Error>> {
    if item.governance == "blocked" {
        return Err(format!("插件 {} 已被组织策略禁止安装", item.name).into());
    }
    ensure_agent_version_supported(&item.min_agent_version)?;
    let archive = download(client, options, agent_id, item)?;
    let result = install_archive(&archive, item);
    let _ = fs::remove_file(archive);
    result
}

fn build_install_plan(
    catalog: &[PluginCatalogItem],
    plugin_id: &str,
) -> Result<PluginInstallPlan, Box<dyn Error>> {
    let plugin = catalog
        .iter()
        .find(|item| item.plugin_id == plugin_id)
        .cloned()
        .ok_or_else(|| "插件未上架或当前不可用".to_string())?;
    build_install_plan_for_item(catalog, plugin)
}

fn build_install_plan_for_item(
    catalog: &[PluginCatalogItem],
    plugin: PluginCatalogItem,
) -> Result<PluginInstallPlan, Box<dyn Error>> {
    let mut blocked_reasons = Vec::new();
    if plugin.governance == "blocked" {
        blocked_reasons.push(format!("{} 已被组织禁止使用", plugin.name));
    }
    if let Err(error) = ensure_agent_version_supported(&plugin.min_agent_version) {
        blocked_reasons.push(error.to_string());
    }
    let catalog_by_id = catalog
        .iter()
        .map(|item| (item.plugin_id.clone(), item))
        .collect::<HashMap<_, _>>();
    let mut actions = Vec::new();
    let mut action_indexes = HashMap::new();
    let mut visiting = HashSet::from([plugin.plugin_id.clone()]);
    for dependency in &plugin.plugin_dependencies {
        resolve_dependency(
            dependency,
            &plugin.name,
            true,
            &catalog_by_id,
            &mut visiting,
            &mut action_indexes,
            &mut actions,
            &mut blocked_reasons,
        );
    }
    blocked_reasons.sort();
    blocked_reasons.dedup();
    Ok(PluginInstallPlan {
        plugin,
        dependency_actions: actions,
        ready: blocked_reasons.is_empty(),
        blocked_reasons,
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_dependency(
    dependency: &SkillPluginDependency,
    requested_by: &str,
    parent_required: bool,
    catalog: &HashMap<String, &PluginCatalogItem>,
    visiting: &mut HashSet<String>,
    action_indexes: &mut HashMap<String, usize>,
    actions: &mut Vec<PluginDependencyAction>,
    blocked_reasons: &mut Vec<String>,
) {
    let required = parent_required && dependency.required;
    if visiting.contains(&dependency.plugin_id) {
        if required {
            blocked_reasons.push(format!("检测到插件循环依赖：{}", dependency.plugin_id));
        }
        return;
    }
    if let Some(index) = action_indexes.get(&dependency.plugin_id).copied() {
        if required {
            actions[index].required = true;
        }
        let Some(item) = catalog.get(&dependency.plugin_id).copied() else {
            if required {
                blocked_reasons.push(format!("缺少必需插件 {}", dependency.plugin_id));
            }
            return;
        };
        if required && item.governance == "blocked" {
            blocked_reasons.push(format!("{} 已被组织禁止使用", item.name));
        }
        if required {
            if let Err(error) = ensure_agent_version_supported(&item.min_agent_version) {
                blocked_reasons.push(format!("{}：{error}", item.name));
            }
        }
        let minimum = dependency.min_version.trim();
        if !minimum.is_empty() && compare_versions(&item.version, minimum) == Ordering::Less {
            if required {
                blocked_reasons.push(format!("{} 的可用版本低于要求 v{}", item.name, minimum));
            }
            return;
        }
        let local = local_status(&dependency.plugin_id);
        if !minimum.is_empty()
            && !local.current_version.is_empty()
            && compare_versions(&local.current_version, minimum) == Ordering::Less
            && item.governance != "blocked"
        {
            actions[index].action = "update".to_string();
            actions[index].reason = "升级到所需版本".to_string();
        }
        return;
    }
    let Some(item) = catalog.get(&dependency.plugin_id).copied() else {
        if required {
            blocked_reasons.push(format!("缺少必需插件 {}", dependency.plugin_id));
        }
        action_indexes.insert(dependency.plugin_id.clone(), actions.len());
        actions.push(PluginDependencyAction {
            plugin_id: dependency.plugin_id.clone(),
            plugin_name: dependency.plugin_id.clone(),
            plugin_description: String::new(),
            required,
            current_version: String::new(),
            target_version: String::new(),
            action: "unavailable".to_string(),
            reason: "插件未上架".to_string(),
            requested_by: requested_by.to_string(),
        });
        return;
    };
    visiting.insert(dependency.plugin_id.clone());
    for child in &item.plugin_dependencies {
        resolve_dependency(
            child,
            &item.name,
            required,
            catalog,
            visiting,
            action_indexes,
            actions,
            blocked_reasons,
        );
    }
    visiting.remove(&dependency.plugin_id);
    let local = local_status(&dependency.plugin_id);
    let minimum = dependency.min_version.trim();
    let local_satisfied = !local.current_version.is_empty()
        && (minimum.is_empty()
            || compare_versions(&local.current_version, minimum) != Ordering::Less);
    let (action, reason) = if item.governance == "blocked" {
        if required {
            blocked_reasons.push(format!("{} 已被组织禁止使用", item.name));
        }
        ("blocked", "组织策略禁止安装")
    } else if let Err(error) = ensure_agent_version_supported(&item.min_agent_version) {
        if required {
            blocked_reasons.push(format!("{}：{error}", item.name));
        }
        ("blocked", "当前 Agent 版本不满足要求")
    } else if !minimum.is_empty() && compare_versions(&item.version, minimum) == Ordering::Less {
        if required {
            blocked_reasons.push(format!("{} 的可用版本低于要求 v{}", item.name, minimum));
        }
        ("blocked", "可用版本不满足最低要求")
    } else if local_satisfied {
        ("satisfied", "本机版本已满足")
    } else if local.current_version.is_empty() {
        ("install", "安装所需插件")
    } else {
        ("update", "升级到所需版本")
    };
    action_indexes.insert(dependency.plugin_id.clone(), actions.len());
    actions.push(PluginDependencyAction {
        plugin_id: dependency.plugin_id.clone(),
        plugin_name: item.name.clone(),
        plugin_description: item.description.clone(),
        required,
        current_version: local.current_version,
        target_version: item.version.clone(),
        action: action.to_string(),
        reason: reason.to_string(),
        requested_by: requested_by.to_string(),
    });
}

fn compensate_plugin_changes(changes: &[(String, LocalPluginStatus)]) {
    for (plugin_id, before) in changes.iter().rev() {
        let current = local_status(plugin_id);
        if before.current_version.is_empty() {
            let _ = remove_for_policy(plugin_id);
        } else if current.current_version != before.current_version {
            let _ = rollback(plugin_id);
        }
        if !before.enabled {
            let _ = set_enabled(plugin_id, false);
        }
    }
}

fn dependency_references_at(root: &Path) -> Result<HashSet<String>, Box<dyn Error>> {
    let path = root.join("dependency-references.json");
    if !path.exists() {
        return Ok(HashSet::new());
    }
    Ok(serde_json::from_slice::<Vec<String>>(&fs::read(path)?)?
        .into_iter()
        .collect())
}

fn add_dependency_reference_at(root: &Path, owner: &str) -> Result<(), Box<dyn Error>> {
    if !root.exists() {
        return Err("依赖插件尚未安装".into());
    }
    let mut references = dependency_references_at(root)?;
    references.insert(owner.to_string());
    let mut values = references.into_iter().collect::<Vec<_>>();
    values.sort();
    fs::write(
        root.join("dependency-references.json"),
        serde_json::to_vec_pretty(&values)?,
    )?;
    Ok(())
}

fn owner_dependency_ids_in(registry: &Path, owner: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(registry) else {
        return Vec::new();
    };
    let mut result = entries
        .flatten()
        .filter_map(|entry| {
            dependency_references_at(&entry.path())
                .ok()
                .filter(|references| references.contains(owner))
                .map(|_| entry.file_name().to_string_lossy().to_string())
        })
        .collect::<Vec<_>>();
    result.sort();
    result
}

pub(crate) fn owner_dependency_ids(owner: &str) -> Vec<String> {
    owner_dependency_ids_in(&plugin_registry_dir(), owner)
}

fn remove_owner_references_from(registry: &Path, owner: &str) {
    let Ok(entries) = fs::read_dir(registry) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(mut references) = dependency_references_at(&entry.path()) else {
            continue;
        };
        if !references.remove(owner) {
            continue;
        }
        let path = entry.path().join("dependency-references.json");
        if references.is_empty() {
            let _ = fs::remove_file(path);
        } else {
            let mut values = references.into_iter().collect::<Vec<_>>();
            values.sort();
            let _ = fs::write(path, serde_json::to_vec_pretty(&values).unwrap_or_default());
        }
    }
}

pub(crate) fn remove_owner_references(owner: &str) {
    remove_owner_references_from(&plugin_registry_dir(), owner);
}

fn set_owner_references_in(
    registry: &Path,
    owner: &str,
    plugin_ids: &[String],
) -> Result<(), Box<dyn Error>> {
    let previous = owner_dependency_ids_in(registry, owner);
    remove_owner_references_from(registry, owner);
    let mut unique_ids = plugin_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    unique_ids.sort();
    for plugin_id in unique_ids {
        if let Err(error) = add_dependency_reference_at(&registry.join(&plugin_id), owner) {
            remove_owner_references_from(registry, owner);
            for previous_id in &previous {
                let _ = add_dependency_reference_at(&registry.join(previous_id), owner);
            }
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn set_owner_references(
    owner: &str,
    plugin_ids: &[String],
) -> Result<(), Box<dyn Error>> {
    for plugin_id in plugin_ids {
        let _ = plugin_root(plugin_id)?;
    }
    set_owner_references_in(&plugin_registry_dir(), owner, plugin_ids)
}

fn ensure_plugin_not_referenced(root: &Path) -> Result<(), Box<dyn Error>> {
    let references = dependency_references_at(root)?;
    if references.is_empty() {
        Ok(())
    } else {
        Err(format!("该插件仍被 {} 个 Skill 或插件使用", references.len()).into())
    }
}

pub(crate) fn rollback(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    if is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不支持回滚".into());
    }
    rollback_root(&plugin_root(plugin_id)?, plugin_id)
}

fn rollback_root(root: &Path, plugin_id: &str) -> Result<(), Box<dyn Error>> {
    let current = root.join("current");
    let previous = root.join("previous");
    if !current.exists() || !previous.exists() {
        return Err("插件没有可用的上一版本".into());
    }
    let current_manifest: PluginManifest = serde_json::from_str(
        fs::read_to_string(current.join("plugin.json"))?.trim_start_matches('\u{feff}'),
    )?;
    let previous_manifest: PluginManifest = serde_json::from_str(
        fs::read_to_string(previous.join("plugin.json"))?.trim_start_matches('\u{feff}'),
    )?;
    if current_manifest.id != previous_manifest.id || current_manifest.id != plugin_id {
        return Err("插件 current/previous 身份不一致".into());
    }
    ensure_agent_version_supported(&previous_manifest.min_agent_version)?;
    swap_current_previous(&root)
}

pub(crate) fn uninstall(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    if is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不允许卸载".into());
    }
    let root = plugin_root(plugin_id)?;
    let governance = installed_governance(&root)?;
    if matches!(governance.as_str(), "required" | "managed") {
        return Err("核心或组织管理插件不允许卸载".into());
    }
    ensure_plugin_not_referenced(&root)?;
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    remove_owner_references(&format!("plugin:{plugin_id}"));
    Ok(())
}

pub(crate) fn remove_for_policy(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    if is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不能由分发策略移除".into());
    }
    let root = plugin_root(plugin_id)?;
    ensure_plugin_not_referenced(&root)?;
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    remove_owner_references(&format!("plugin:{plugin_id}"));
    Ok(())
}

pub(crate) fn apply_effective_policy(
    plugin_id: &str,
    governance: &str,
    source: &str,
    assignment_id: &str,
    reason: &str,
    allow_disable: bool,
    allow_uninstall: bool,
) -> Result<(), Box<dyn Error>> {
    let root = plugin_root(plugin_id)?;
    let current = root.join("current");
    if !current.exists() {
        return Ok(());
    }
    fs::write(
        current.join("policy.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "governance": governance,
            "source": source,
            "assignment_id": assignment_id,
            "reason": reason,
            "allow_disable": allow_disable,
            "allow_uninstall": allow_uninstall,
        }))?,
    )?;
    Ok(())
}

pub(crate) fn set_enabled(plugin_id: &str, enabled: bool) -> Result<(), Box<dyn Error>> {
    if !enabled && is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不允许停用".into());
    }
    let root = plugin_root(plugin_id)?;
    if !root.exists() {
        return Err("插件未安装".into());
    }
    let governance = installed_governance(&root)?;
    if !enabled && matches!(governance.as_str(), "required" | "managed") {
        return Err("核心或组织管理插件不允许停用".into());
    }
    let marker = root.join("disabled");
    if enabled {
        crate::capability::plugin::reset_plugin_health(plugin_id)?;
        if marker.exists() {
            fs::remove_file(marker)?;
        }
    } else {
        fs::write(marker, b"disabled")?;
    }
    Ok(())
}

fn catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let credential = options.agent_credential();
    if credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    plugin_catalog(client, &options.api_base, agent_id, &credential)?
        .into_iter()
        .find(|item| item.plugin_id == plugin_id)
        .ok_or_else(|| "插件未上架或当前不可用".into())
}

fn download(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &PluginCatalogItem,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_PLUGIN_ARCHIVE_BYTES {
        return Err("插件制品大小无效或超过 512 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base)?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("插件制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    if response
        .content_length()
        .map(|size| size > item.file_size || size > MAX_PLUGIN_ARCHIVE_BYTES)
        .unwrap_or(false)
    {
        return Err("插件制品响应大小超过发布记录".into());
    }
    let path = env::temp_dir().join(format!("himind-plugin-{}.zip", unique_suffix()));
    let mut file = File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_PLUGIN_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("插件制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "插件制品大小校验失败，期望 {}，实际 {total}",
            item.file_size
        )
        .into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "插件制品 SHA-256 校验失败，期望 {}，实际 {actual}",
            item.sha256
        )
        .into());
    }
    verify_signature(&path, item)?;
    Ok(path)
}

fn verify_signature(path: &Path, item: &PluginCatalogItem) -> Result<(), Box<dyn Error>> {
    let require_signed = env::var("HIMIND_REQUIRE_SIGNED_UPDATES")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    validate_signature_metadata(
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        require_signed,
    )?;
    if item.signature.is_empty() {
        return Ok(());
    }
    let trusted = env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR").ok_or("未配置插件受信公钥目录")?;
    let pem =
        fs::read_to_string(PathBuf::from(trusted).join(format!("{}.pem", item.signature_key_id)))?;
    verify_rsa_pss_sha256(path, &pem, &item.signature)
}

fn install_archive(archive_path: &Path, item: &PluginCatalogItem) -> Result<(), Box<dyn Error>> {
    let root = plugin_root(&item.plugin_id)?;
    fs::create_dir_all(root.join("versions"))?;
    let staging = root.join(format!("staging-{}", unique_suffix()));
    fs::create_dir_all(&staging)?;
    let result = (|| {
        let mut archive = ZipArchive::new(File::open(archive_path)?)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let relative = entry
                .enclosed_name()
                .ok_or("插件 ZIP 包含非法路径")?
                .to_path_buf();
            let output = staging.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            std::io::copy(&mut entry, &mut File::create(output)?)?;
        }
        verify_plugin_checksums(&staging)?;
        let manifest_path = staging.join("plugin.json");
        let manifest: PluginManifest = serde_json::from_str(
            fs::read_to_string(manifest_path)?.trim_start_matches('\u{feff}'),
        )?;
        if manifest.id != item.plugin_id || manifest.version != item.version {
            return Err("插件 Manifest ID 或版本与发布记录不一致".into());
        }
        let version_dir = root.join("versions").join(&item.version);
        if version_dir.exists() {
            let existing = fs::read(version_dir.join("checksums.sha256"))?;
            let incoming = fs::read(staging.join("checksums.sha256"))?;
            if existing != incoming {
                return Err("同一插件版本已存在且内容不同，请提升版本号".into());
            }
            fs::remove_dir_all(&staging)?;
        } else {
            fs::rename(&staging, &version_dir)?;
        }
        let next = root.join(format!("current-{}", unique_suffix()));
        copy_dir(&version_dir, &next)?;
        fs::write(
            next.join("policy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "governance": item.governance,
                "source": item.source,
                "assignment": item.assignment,
                "management": item.management,
                "install_mode": item.install_mode,
                "assignment_id": "",
                "reason": item.organization_reason,
                "allow_disable": item.allow_disable,
                "allow_uninstall": item.allow_uninstall,
            }))?,
        )?;
        let current = root.join("current");
        let previous = root.join("previous");
        if previous.exists() {
            fs::remove_dir_all(&previous)?;
        }
        if current.exists() {
            fs::rename(&current, &previous)?;
        }
        if let Err(error) = fs::rename(&next, &current) {
            if previous.exists() && !current.exists() {
                let _ = fs::rename(&previous, &current);
            }
            return Err(error.into());
        }
        let marker = root.join("disabled");
        if marker.exists() {
            fs::remove_file(marker)?;
        }
        Ok(())
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}

fn swap_current_previous(root: &Path) -> Result<(), Box<dyn Error>> {
    let current = root.join("current");
    let previous = root.join("previous");
    let temporary = root.join(format!("swap-{}", unique_suffix()));
    fs::rename(&current, &temporary)?;
    if let Err(error) = fs::rename(&previous, &current) {
        let _ = fs::rename(&temporary, &current);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporary, &previous) {
        let restore_previous = root.join(format!("restore-{}", unique_suffix()));
        let _ = fs::rename(&current, &restore_previous);
        let _ = fs::rename(&temporary, &current);
        let _ = fs::rename(&restore_previous, &previous);
        return Err(error.into());
    }
    Ok(())
}

fn ensure_agent_version_supported(minimum: &str) -> Result<(), Box<dyn Error>> {
    let minimum = minimum.trim();
    if minimum.is_empty() {
        return Ok(());
    }
    if compare_versions(crate::VERSION, minimum) == Ordering::Less {
        return Err(format!(
            "当前 Agent {} 不满足插件最低版本 {}",
            crate::VERSION,
            minimum
        )
        .into());
    }
    Ok(())
}

pub(crate) fn verify_plugin_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
    let checksum_path = root.join("checksums.sha256");
    let content = fs::read_to_string(&checksum_path).map_err(|_| "插件包缺少 checksums.sha256")?;
    let mut expected = HashMap::new();
    for (index, line) in content.lines().enumerate() {
        let Some((checksum, relative)) = line.split_once("  ") else {
            return Err(format!("checksums.sha256 第 {} 行格式无效", index + 1).into());
        };
        if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("checksums.sha256 第 {} 行摘要无效", index + 1).into());
        }
        let relative_path = PathBuf::from(relative);
        if relative == "checksums.sha256"
            || relative.is_empty()
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!("checksums.sha256 第 {} 行路径无效", index + 1).into());
        }
        if expected
            .insert(relative.replace('\\', "/"), checksum.to_ascii_lowercase())
            .is_some()
        {
            return Err(format!("checksums.sha256 包含重复路径: {relative}").into());
        }
    }

    let mut actual_files = HashSet::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path() == checksum_path {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        actual_files.insert(relative.clone());
        let expected_checksum = expected
            .get(&relative)
            .ok_or_else(|| format!("插件文件未包含在 checksums.sha256: {relative}"))?;
        let mut file = File::open(entry.path())?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        let actual = format!("{:x}", hasher.finalize());
        if &actual != expected_checksum {
            return Err(format!("插件文件摘要不匹配: {relative}").into());
        }
    }
    if let Some(missing) = expected.keys().find(|name| !actual_files.contains(*name)) {
        return Err(format!("checksums.sha256 引用了缺失文件: {missing}").into());
    }
    Ok(())
}

fn installed_governance(root: &Path) -> Result<String, Box<dyn Error>> {
    let policy = root.join("current/policy.json");
    if policy.exists() {
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(policy)?)?;
        if let Some(value) = value.get("governance").and_then(|value| value.as_str()) {
            return Ok(value.to_string());
        }
    }
    let manifest: PluginManifest =
        serde_json::from_str(&fs::read_to_string(root.join("current/plugin.json"))?)?;
    Ok(manifest.governance)
}

fn plugin_root(plugin_id: &str) -> Result<PathBuf, Box<dyn Error>> {
    if plugin_id.is_empty()
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("插件 ID 无效".into());
    }
    Ok(plugin_registry_dir().join(plugin_id))
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        add_dependency_reference_at, build_install_plan, build_install_plan_for_item,
        compare_versions, dependency_references_at, ensure_plugin_not_referenced,
        flush_status_outbox, owner_dependency_ids_in, remove_owner_references_from, report_status,
        rollback_root, set_enabled, set_owner_references_in, uninstall, verify_plugin_checksums,
    };
    use crate::api::distribution::{PluginCatalogItem, SkillPluginDependency};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, RwLock};

    #[test]
    fn builtin_plugins_cannot_be_disabled_or_uninstalled() {
        assert!(set_enabled("com.himind.builtin.svn", false).is_err());
        assert!(uninstall("com.himind.builtin.smb").is_err());
    }

    fn dependency(plugin_id: &str, required: bool, min_version: &str) -> SkillPluginDependency {
        SkillPluginDependency {
            plugin_id: plugin_id.to_string(),
            required,
            min_version: min_version.to_string(),
        }
    }

    fn catalog_plugin(
        plugin_id: &str,
        name: &str,
        version: &str,
        dependencies: Vec<SkillPluginDependency>,
    ) -> PluginCatalogItem {
        PluginCatalogItem {
            plugin_id: plugin_id.to_string(),
            name: name.to_string(),
            description: format!("{name} description"),
            author_name: "测试团队".to_string(),
            categories: vec!["开发工具".to_string()],
            review_status: "approved".to_string(),
            governance: "optional".to_string(),
            version: version.to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: String::new(),
            channel: "stable".to_string(),
            artifact_id: format!("artifact-{plugin_id}"),
            file_name: format!("{plugin_id}.hmpkg"),
            file_size: 1,
            sha256: "0".repeat(64),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: "http://localhost/plugin".to_string(),
            source: "marketplace".to_string(),
            assignment: "optional".to_string(),
            management: "user_managed".to_string(),
            install_mode: "prompt".to_string(),
            organization_reason: String::new(),
            managed: false,
            allow_disable: true,
            allow_uninstall: true,
            capability_ids: Vec::new(),
            permissions: Vec::new(),
            view_count: 0,
            plugin_dependencies: dependencies,
        }
    }

    #[test]
    fn selected_version_uses_its_own_dependencies() {
        let root_id = "com.himind.test.versioned-root";
        let latest_dependency = "com.himind.test.latest-dependency";
        let previous_dependency = "com.himind.test.previous-dependency";
        let catalog = vec![
            catalog_plugin(latest_dependency, "最新依赖", "1.0.0", Vec::new()),
            catalog_plugin(previous_dependency, "旧版依赖", "1.0.0", Vec::new()),
            catalog_plugin(
                root_id,
                "版本化插件",
                "2.0.0",
                vec![dependency(latest_dependency, true, "1.0.0")],
            ),
        ];
        let selected = catalog_plugin(
            root_id,
            "版本化插件",
            "1.0.0",
            vec![dependency(previous_dependency, true, "1.0.0")],
        );

        let plan = build_install_plan_for_item(&catalog, selected).unwrap();

        assert_eq!(plan.plugin.version, "1.0.0");
        assert_eq!(plan.dependency_actions.len(), 1);
        assert_eq!(plan.dependency_actions[0].plugin_id, previous_dependency);
    }

    #[test]
    fn plans_transitive_dependencies_in_install_order() {
        let leaf_id = "com.himind.test.plan.leaf";
        let middle_id = "com.himind.test.plan.middle";
        let root_id = "com.himind.test.plan.root";
        let catalog = vec![
            catalog_plugin(leaf_id, "基础能力", "1.0.0", Vec::new()),
            catalog_plugin(
                middle_id,
                "组合能力",
                "1.0.0",
                vec![dependency(leaf_id, true, "1.0.0")],
            ),
            catalog_plugin(
                root_id,
                "桌面工具",
                "1.0.0",
                vec![dependency(middle_id, true, "1.0.0")],
            ),
        ];

        let plan = build_install_plan(&catalog, root_id).unwrap();

        assert!(plan.ready, "{:?}", plan.blocked_reasons);
        assert_eq!(
            plan.dependency_actions
                .iter()
                .map(|action| action.plugin_id.as_str())
                .collect::<Vec<_>>(),
            vec![leaf_id, middle_id]
        );
        assert!(plan
            .dependency_actions
            .iter()
            .all(|action| action.action == "install"));
    }

    #[test]
    fn blocks_required_cycles_and_unsatisfied_versions() {
        let root_id = "com.himind.test.plan.cycle-root";
        let child_id = "com.himind.test.plan.cycle-child";
        let catalog = vec![
            catalog_plugin(
                root_id,
                "循环入口",
                "1.0.0",
                vec![dependency(child_id, true, "2.0.0")],
            ),
            catalog_plugin(
                child_id,
                "循环依赖",
                "1.0.0",
                vec![dependency(root_id, true, "1.0.0")],
            ),
        ];

        let plan = build_install_plan(&catalog, root_id).unwrap();

        assert!(!plan.ready);
        assert!(plan
            .blocked_reasons
            .iter()
            .any(|reason| reason.contains("循环依赖")));
        assert!(plan
            .blocked_reasons
            .iter()
            .any(|reason| reason.contains("低于要求")));
    }

    #[test]
    fn optional_missing_dependency_does_not_block_install() {
        let root_id = "com.himind.test.plan.optional-root";
        let missing_id = "com.himind.test.plan.optional-missing";
        let catalog = vec![catalog_plugin(
            root_id,
            "可选能力工具",
            "1.0.0",
            vec![dependency(missing_id, false, "1.0.0")],
        )];

        let plan = build_install_plan(&catalog, root_id).unwrap();

        assert!(plan.ready);
        assert_eq!(plan.dependency_actions.len(), 1);
        assert!(!plan.dependency_actions[0].required);
        assert_eq!(plan.dependency_actions[0].action, "unavailable");
    }

    #[test]
    fn updates_dependency_references_atomically_and_blocks_removal() {
        let registry = std::env::temp_dir().join(format!(
            "himind-plugin-reference-test-{}",
            super::unique_suffix()
        ));
        let first = registry.join("com.himind.test.reference.first");
        let second = registry.join("com.himind.test.reference.second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        add_dependency_reference_at(&second, "plugin:other").unwrap();

        let owner = "skill:com.himind.skill.reference-test";
        set_owner_references_in(
            &registry,
            owner,
            &[
                "com.himind.test.reference.first".to_string(),
                "com.himind.test.reference.second".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            owner_dependency_ids_in(&registry, owner),
            vec![
                "com.himind.test.reference.first".to_string(),
                "com.himind.test.reference.second".to_string()
            ]
        );
        assert!(ensure_plugin_not_referenced(&first).is_err());

        set_owner_references_in(
            &registry,
            owner,
            &["com.himind.test.reference.second".to_string()],
        )
        .unwrap();
        assert!(dependency_references_at(&first).unwrap().is_empty());
        assert_eq!(dependency_references_at(&second).unwrap().len(), 2);

        let failed = set_owner_references_in(
            &registry,
            owner,
            &["com.himind.test.reference.missing".to_string()],
        );
        assert!(failed.is_err());
        assert_eq!(
            owner_dependency_ids_in(&registry, owner),
            vec!["com.himind.test.reference.second".to_string()]
        );

        remove_owner_references_from(&registry, owner);
        remove_owner_references_from(&registry, "plugin:other");
        assert!(ensure_plugin_not_referenced(&second).is_ok());
        let _ = fs::remove_dir_all(registry);
    }

    #[test]
    fn applies_strongest_version_constraint_across_dependency_paths() {
        let shared_id = "com.himind.test.plan.shared";
        let first_id = "com.himind.test.plan.first";
        let second_id = "com.himind.test.plan.second";
        let root_id = "com.himind.test.plan.constraints-root";
        let catalog = vec![
            catalog_plugin(shared_id, "共享能力", "1.5.0", Vec::new()),
            catalog_plugin(
                first_id,
                "低版本路径",
                "1.0.0",
                vec![dependency(shared_id, true, "1.0.0")],
            ),
            catalog_plugin(
                second_id,
                "高版本路径",
                "1.0.0",
                vec![dependency(shared_id, true, "2.0.0")],
            ),
            catalog_plugin(
                root_id,
                "约束测试工具",
                "1.0.0",
                vec![
                    dependency(first_id, true, "1.0.0"),
                    dependency(second_id, true, "1.0.0"),
                ],
            ),
        ];

        let plan = build_install_plan(&catalog, root_id).unwrap();

        assert!(!plan.ready);
        assert!(plan
            .blocked_reasons
            .iter()
            .any(|reason| reason.contains("共享能力") && reason.contains("v2.0.0")));
    }

    #[test]
    fn verifies_complete_plugin_checksums_and_rejects_tampering() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-checksum-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("plugin.json"), b"{}").unwrap();
        fs::write(root.join("bin/tool.exe"), b"binary").unwrap();
        let manifest_hash = format!("{:x}", Sha256::digest(b"{}"));
        let entry_hash = format!("{:x}", Sha256::digest(b"binary"));
        fs::write(
            root.join("checksums.sha256"),
            format!("{manifest_hash}  plugin.json\n{entry_hash}  bin/tool.exe\n"),
        )
        .unwrap();
        assert!(verify_plugin_checksums(&root).is_ok());
        fs::write(root.join("bin/tool.exe"), b"tampered").unwrap();
        assert!(verify_plugin_checksums(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compares_agent_versions_for_plugin_minimum_gate() {
        assert_eq!(
            compare_versions("0.2.0", "0.1.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.2.0", "0.2.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(compare_versions("0.2.0", "0.3.0"), std::cmp::Ordering::Less);
        assert_eq!(
            compare_versions("0.2.0-beta.1", "0.2.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn rolls_back_by_swapping_current_and_previous() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-rollback-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(root.join("current")).unwrap();
        fs::create_dir_all(root.join("previous")).unwrap();
        let manifest = |version: &str| {
            format!(
                r#"{{"id":"com.himind.rollback","name":"Rollback","version":"{version}","runtime":"process-jsonrpc-stdio","min_agent_version":"0.1.0"}}"#
            )
        };
        fs::write(root.join("current/plugin.json"), manifest("2.0.0")).unwrap();
        fs::write(root.join("previous/plugin.json"), manifest("1.0.0")).unwrap();

        rollback_root(&root, "com.himind.rollback").unwrap();

        let current = fs::read_to_string(root.join("current/plugin.json")).unwrap();
        let previous = fs::read_to_string(root.join("previous/plugin.json")).unwrap();
        assert!(current.contains(r#""version":"1.0.0""#));
        assert!(previous.contains(r#""version":"2.0.0""#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn queues_failed_status_and_replays_after_dashboard_recovers() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-status-replay-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let options = crate::Options {
            api_base: "http://127.0.0.1:1".to_string(),
            state_path: state_path.clone(),
            once: false,
            interval_seconds: 10,
            local_app: false,
            local_port: 18181,
            reenroll: false,
            enrollment_token: String::new(),
            agent_credential: Arc::new(RwLock::new("credential".to_string())),
            identity_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            platform_access: Arc::new(RwLock::new(None)),
            task_execution: Arc::new(RwLock::new(None)),
        };

        assert!(report_status(
            &options,
            "agent-1",
            "com.himind.replay",
            "enable",
            "1.0.0",
            ""
        )
        .is_err());
        assert_eq!(
            crate::store::plugin_outbox::list(&state_path)
                .unwrap()
                .len(),
            1
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let recovered = crate::Options {
            api_base: format!("http://{address}"),
            ..options
        };
        flush_status_outbox(&recovered, "agent-1");
        server.join().unwrap();

        assert!(crate::store::plugin_outbox::list(&state_path)
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
