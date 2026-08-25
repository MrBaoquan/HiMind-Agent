use crate::api::distribution::{
    extension_desired_state, report_extension_reconcile, ExtensionDesiredItem,
    ExtensionReconcileItem, ExtensionReconcileReport,
};
use crate::app::{plugin_manager, skill_manager};
use crate::skill::store::{SkillManagementPolicy, SkillStore};
use crate::{Options, VERSION};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct PersistedDesiredSnapshot {
    generation: String,
    #[serde(default)]
    items: Vec<ExtensionDesiredItem>,
}

pub(crate) fn reconcile(
    options: &Options,
    agent_id: &str,
    previous_generation: &mut String,
) -> Result<(), Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let desired = extension_desired_state(&client, &options.api_base, agent_id, &credential)?;
    let persisted = load_snapshot(&snapshot_path())?;
    if previous_generation.is_empty() {
        *previous_generation = persisted.generation.clone();
    }
    let policy_changed = desired.generation != *previous_generation;
    let mut items = Vec::new();
    if policy_changed {
        for removed in removed_items(&persisted.items, &desired.items) {
            items.push(reconcile_scope_exit(removed));
        }
    }
    for item in &desired.items {
        if !policy_changed && extension_is_in_desired_state(item)? {
            continue;
        }
        if item.desired_state == "optional" && item.asset_kind == "plugin" {
            let local = plugin_manager::local_status(&item.asset_key);
            if local.current_version.is_empty() {
                continue;
            }
        }
        if item.desired_state == "optional" && item.asset_kind == "skill" {
            if SkillStore::new().get_record(&item.asset_key)?.is_none() {
                continue;
            }
        }
        let result = reconcile_item(options, agent_id, item);
        if result.status != "not_applicable" {
            items.push(result);
        }
    }
    if items.is_empty() {
        save_snapshot(
            &snapshot_path(),
            &PersistedDesiredSnapshot {
                generation: desired.generation.clone(),
                items: desired.items,
            },
        )?;
        *previous_generation = desired.generation;
        return Ok(());
    }
    let report = ExtensionReconcileReport {
        generation: desired.generation.clone(),
        items,
    };
    report_extension_reconcile(&client, &options.api_base, agent_id, &credential, &report)?;
    save_snapshot(
        &snapshot_path(),
        &PersistedDesiredSnapshot {
            generation: desired.generation.clone(),
            items: desired.items,
        },
    )?;
    *previous_generation = desired.generation;
    Ok(())
}

/// Releases policies from the last complete Dashboard snapshot after the
/// user explicitly starts the Agent in Independent mode.
pub(crate) fn release_control_plane_policies() -> Result<(), Box<dyn Error>> {
    let path = snapshot_path();
    let snapshot = load_snapshot(&path)?;
    if snapshot.items.is_empty() {
        return Ok(());
    }
    for item in &snapshot.items {
        let _ = reconcile_scope_exit(item);
    }
    save_snapshot(&path, &PersistedDesiredSnapshot::default())?;
    Ok(())
}

fn removed_items<'a>(
    previous: &'a [ExtensionDesiredItem],
    current: &[ExtensionDesiredItem],
) -> Vec<&'a ExtensionDesiredItem> {
    let current_keys = current
        .iter()
        .map(|item| (item.asset_kind.as_str(), item.asset_key.as_str()))
        .collect::<HashSet<_>>();
    previous
        .iter()
        .filter(|item| !current_keys.contains(&(item.asset_kind.as_str(), item.asset_key.as_str())))
        .collect()
}

fn reconcile_scope_exit(item: &ExtensionDesiredItem) -> ExtensionReconcileItem {
    match item.asset_kind.as_str() {
        "plugin" => reconcile_plugin_scope_exit(item),
        "skill" => reconcile_skill_scope_exit(item),
        _ => result(item, "not_applicable", "scope_exit", "", json!({}), ""),
    }
}

fn reconcile_plugin_scope_exit(item: &ExtensionDesiredItem) -> ExtensionReconcileItem {
    let before = plugin_manager::local_status(&item.asset_key);
    if before.current_version.is_empty() {
        return result(item, "uninstalled", "scope_exit", "", json!({}), "");
    }
    let action = normalized_scope_exit(item);
    let outcome = match action {
        "disable" => plugin_manager::apply_effective_policy(
            &item.asset_key,
            "optional",
            "local",
            "",
            "",
            true,
            true,
        )
        .and_then(|_| plugin_manager::set_enabled(&item.asset_key, false)),
        "remove_if_unused" => plugin_manager::remove_for_policy(&item.asset_key),
        _ => plugin_manager::apply_effective_policy(
            &item.asset_key,
            "optional",
            "local",
            "",
            "",
            true,
            true,
        ),
    };
    match outcome {
        Ok(()) if action == "remove_if_unused" => {
            result(item, "uninstalled", "scope_exit", "", json!({}), "")
        }
        Ok(()) if action == "disable" => result(
            item,
            "disabled",
            "scope_exit",
            &before.current_version,
            json!({}),
            "",
        ),
        Ok(()) => result(
            item,
            "installed",
            "scope_exit",
            &before.current_version,
            json!({}),
            "",
        ),
        Err(error) if action == "remove_if_unused" => {
            let _ = plugin_manager::apply_effective_policy(
                &item.asset_key,
                "optional",
                "local",
                "",
                "",
                true,
                true,
            );
            result(
                item,
                "installed",
                "scope_exit",
                &before.current_version,
                json!({}),
                &format!("扩展仍被依赖，已保留为本机扩展: {error}"),
            )
        }
        Err(error) => result(
            item,
            "failed",
            "scope_exit",
            &before.current_version,
            json!({}),
            &error.to_string(),
        ),
    }
}

fn reconcile_skill_scope_exit(item: &ExtensionDesiredItem) -> ExtensionReconcileItem {
    let store = SkillStore::new();
    let existing = match store.get_record(&item.asset_key) {
        Ok(value) => value,
        Err(error) => {
            return result(
                item,
                "failed",
                "scope_exit",
                "",
                json!({}),
                &error.to_string(),
            )
        }
    };
    let Some(existing) = existing else {
        return result(item, "uninstalled", "scope_exit", "", json!({}), "");
    };
    let version = existing.manifest.version.clone();
    let action = normalized_scope_exit(item);
    if action == "remove_if_unused" {
        if let Err(error) =
            crate::skill::uninstall_supported_clients_for_policy_json(&item.asset_key)
        {
            return result(
                item,
                "failed",
                "scope_exit",
                &version,
                json!({}),
                &error.to_string(),
            );
        }
        crate::app::plugin_manager::remove_owner_references(&format!("skill:{}", item.asset_key));
        return match store.remove_organization_skill(&item.asset_key) {
            Ok(_) => result(item, "uninstalled", "scope_exit", "", json!({}), ""),
            Err(error) => result(
                item,
                "failed",
                "scope_exit",
                &version,
                json!({}),
                &error.to_string(),
            ),
        };
    }
    if action == "disable" {
        let policy = SkillManagementPolicy {
            management: "user_managed".to_string(),
            source: "local".to_string(),
            assignment_id: String::new(),
            reason: String::new(),
            allow_uninstall: true,
        };
        if let Err(error) = store.apply_management_policy(&item.asset_key, &policy) {
            return result(
                item,
                "failed",
                "scope_exit",
                &version,
                json!({}),
                &error.to_string(),
            );
        }
        return match crate::skill::uninstall_supported_clients_for_policy_json(&item.asset_key) {
            Ok(clients) => result(item, "disabled", "scope_exit", &version, clients, ""),
            Err(error) => result(
                item,
                "failed",
                "scope_exit",
                &version,
                json!({}),
                &error.to_string(),
            ),
        };
    }
    let policy = SkillManagementPolicy {
        management: "user_managed".to_string(),
        source: "local".to_string(),
        assignment_id: String::new(),
        reason: String::new(),
        allow_uninstall: true,
    };
    match store.apply_management_policy(&item.asset_key, &policy) {
        Ok(()) => result(item, "installed", "scope_exit", &version, json!({}), ""),
        Err(error) => result(
            item,
            "failed",
            "scope_exit",
            &version,
            json!({}),
            &error.to_string(),
        ),
    }
}

fn normalized_scope_exit(item: &ExtensionDesiredItem) -> &str {
    match item.on_scope_exit.as_str() {
        "disable" => "disable",
        "remove_if_unused" => "remove_if_unused",
        _ => "retain",
    }
}

fn snapshot_path() -> PathBuf {
    crate::store::paths::agent_home().join("data/extension-desired-dashboard.json")
}

fn load_snapshot(path: &Path) -> Result<PersistedDesiredSnapshot, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(PersistedDesiredSnapshot::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn save_snapshot(path: &Path, snapshot: &PersistedDesiredSnapshot) -> Result<(), Box<dyn Error>> {
    crate::store::atomic_file::atomic_write(path, &serde_json::to_vec_pretty(snapshot)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::removed_items;
    use crate::api::distribution::ExtensionDesiredItem;

    fn item(kind: &str, key: &str) -> ExtensionDesiredItem {
        ExtensionDesiredItem {
            product_id: format!("product-{key}"),
            asset_key: key.to_string(),
            asset_kind: kind.to_string(),
            name: key.to_string(),
            desired_state: "present".to_string(),
            desired_version: "1.0.0".to_string(),
            desired_enabled: true,
            intent: "required".to_string(),
            management: "organization_managed".to_string(),
            install_mode: "silent".to_string(),
            assignment_id: format!("assignment-{key}"),
            source: "organization".to_string(),
            reason: String::new(),
            allow_disable: false,
            allow_uninstall: false,
            on_scope_exit: "retain".to_string(),
        }
    }

    #[test]
    fn complete_snapshot_diff_detects_removed_assets_only() {
        let previous = vec![item("plugin", "a"), item("skill", "b")];
        let current = vec![item("plugin", "a"), item("plugin", "c")];
        let removed = removed_items(&previous, &current);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].asset_key, "b");
    }
}

fn extension_is_in_desired_state(item: &ExtensionDesiredItem) -> Result<bool, Box<dyn Error>> {
    if item.asset_kind == "plugin" {
        let local = plugin_manager::local_status(&item.asset_key);
        return Ok(match item.desired_state.as_str() {
            "absent" => local.current_version.is_empty(),
            "present" => {
                !local.current_version.is_empty()
                    && (item.desired_version.is_empty()
                        || local.current_version == item.desired_version)
                    && (!item.desired_enabled || local.enabled)
            }
            _ => true,
        });
    }
    if item.asset_kind == "skill" {
        let record = SkillStore::new().get_record(&item.asset_key)?;
        return Ok(match item.desired_state.as_str() {
            "absent" => record.is_none(),
            "present" => record
                .map(|value| {
                    item.desired_version.is_empty()
                        || value.manifest.version == item.desired_version
                })
                .unwrap_or(false),
            _ => true,
        });
    }
    Ok(true)
}

fn reconcile_item(
    options: &Options,
    agent_id: &str,
    item: &ExtensionDesiredItem,
) -> ExtensionReconcileItem {
    match item.asset_kind.as_str() {
        "plugin" => reconcile_plugin(options, agent_id, item),
        "skill" => reconcile_skill(options, agent_id, item),
        _ => result(
            item,
            "not_applicable",
            "",
            "",
            json!({}),
            "unsupported asset kind",
        ),
    }
}

fn reconcile_plugin(
    options: &Options,
    agent_id: &str,
    item: &ExtensionDesiredItem,
) -> ExtensionReconcileItem {
    let before = plugin_manager::local_status(&item.asset_key);
    if item.desired_state == "absent" {
        if before.current_version.is_empty() {
            return result(item, "uninstalled", "health_check", "", json!({}), "");
        }
        let outcome = plugin_manager::remove_for_policy(&item.asset_key);
        return match outcome {
            Ok(()) => result(item, "uninstalled", "installing", "", json!({}), ""),
            Err(error) => result(
                item,
                "failed",
                "rollback",
                &before.current_version,
                json!({}),
                &error.to_string(),
            ),
        };
    }
    let needs_install = before.current_version.is_empty()
        || (!item.desired_version.is_empty() && before.current_version != item.desired_version);
    if needs_install {
        if let Err(error) = plugin_manager::install(options, agent_id, &item.asset_key, None) {
            let local = plugin_manager::local_status(&item.asset_key);
            return result(
                item,
                "failed",
                "installing",
                &local.current_version,
                json!({}),
                &error.to_string(),
            );
        }
    }
    let governance = if item.management == "builtin" {
        "required"
    } else if item.management == "organization_managed" || item.intent == "required" {
        "managed"
    } else {
        "optional"
    };
    if let Err(error) = plugin_manager::apply_effective_policy(
        &item.asset_key,
        governance,
        &item.source,
        &item.assignment_id,
        &item.reason,
        item.allow_disable,
        item.allow_uninstall,
    ) {
        return result(
            item,
            "failed",
            "activating",
            &before.current_version,
            json!({}),
            &error.to_string(),
        );
    }
    let local = plugin_manager::local_status(&item.asset_key);
    if item.desired_enabled && !local.enabled {
        if let Err(error) = plugin_manager::set_enabled(&item.asset_key, true) {
            return result(
                item,
                "failed",
                "activating",
                &local.current_version,
                json!({}),
                &error.to_string(),
            );
        }
    }
    result(
        item,
        "installed",
        "health_check",
        &local.current_version,
        json!({}),
        "",
    )
}

fn reconcile_skill(
    options: &Options,
    agent_id: &str,
    item: &ExtensionDesiredItem,
) -> ExtensionReconcileItem {
    let store = SkillStore::new();
    if item.desired_state == "absent" {
        let existing = match store.get_record(&item.asset_key) {
            Ok(value) => value,
            Err(error) => {
                return result(
                    item,
                    "failed",
                    "health_check",
                    "",
                    json!({}),
                    &error.to_string(),
                )
            }
        };
        if existing.is_none() {
            return result(item, "uninstalled", "health_check", "", json!({}), "");
        }
        let removed = crate::skill::uninstall_supported_clients_for_policy_json(&item.asset_key);
        if let Err(error) = removed {
            return result(
                item,
                "failed",
                "rollback",
                "",
                json!({}),
                &error.to_string(),
            );
        }
        return match store.remove_organization_skill(&item.asset_key) {
            Ok(_) => result(item, "uninstalled", "installing", "", json!({}), ""),
            Err(error) => result(
                item,
                "failed",
                "rollback",
                "",
                json!({}),
                &error.to_string(),
            ),
        };
    }
    let existing = match store.get_record(&item.asset_key) {
        Ok(value) => value,
        Err(error) => {
            return result(
                item,
                "failed",
                "health_check",
                "",
                json!({}),
                &error.to_string(),
            )
        }
    };
    let policy = SkillManagementPolicy {
        management: item.management.clone(),
        source: item.source.clone(),
        assignment_id: item.assignment_id.clone(),
        reason: item.reason.clone(),
        allow_uninstall: item.allow_uninstall,
    };
    let needs_install = existing
        .as_ref()
        .map(|record| {
            !item.desired_version.is_empty() && record.manifest.version != item.desired_version
        })
        .unwrap_or(true);
    if needs_install {
        let record = match skill_manager::install_with_dependencies(
            options,
            agent_id,
            &item.asset_key,
            None,
            &[],
        ) {
            Ok((_, record)) => record,
            Err(error) => {
                return result(
                    item,
                    "failed",
                    "installing_dependencies",
                    "",
                    json!({}),
                    &error.to_string(),
                )
            }
        };
        if let Err(error) = store.apply_management_policy(&item.asset_key, &policy) {
            return result(
                item,
                "failed",
                "activating",
                &record.manifest.version,
                json!({}),
                &error.to_string(),
            );
        }
        match crate::skill::sync_record_to_supported_clients(&record, VERSION, &[]) {
            Ok(clients) => {
                let version = record.manifest.version.clone();
                return result(
                    item,
                    "installed",
                    "health_check",
                    &version,
                    json!(clients),
                    "",
                );
            }
            Err(error) => {
                return result(
                    item,
                    "needs_setup",
                    "activating",
                    &record.manifest.version,
                    json!({}),
                    &error.to_string(),
                )
            }
        }
    }
    if let Err(error) = store.apply_management_policy(&item.asset_key, &policy) {
        return result(
            item,
            "failed",
            "activating",
            "",
            json!({}),
            &error.to_string(),
        );
    }
    let version = existing
        .map(|record| record.manifest.version)
        .unwrap_or_default();
    result(item, "installed", "health_check", &version, json!({}), "")
}

fn result(
    item: &ExtensionDesiredItem,
    status: &str,
    phase: &str,
    installed_version: &str,
    target_clients: Value,
    error: &str,
) -> ExtensionReconcileItem {
    ExtensionReconcileItem {
        asset_key: item.asset_key.clone(),
        asset_kind: item.asset_kind.clone(),
        desired_version: item.desired_version.clone(),
        installed_version: installed_version.to_string(),
        enabled: item.desired_state != "absent" && item.desired_enabled,
        status: status.to_string(),
        phase: phase.to_string(),
        install_source: item.source.clone(),
        assignment_id: item.assignment_id.clone(),
        target_clients,
        error: error.chars().take(2048).collect(),
    }
}
