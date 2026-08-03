use crate::api::distribution::{
    extension_desired_state, report_extension_reconcile, ExtensionDesiredItem,
    ExtensionReconcileItem, ExtensionReconcileReport,
};
use crate::app::{plugin_manager, skill_manager};
use crate::skill::store::{SkillManagementPolicy, SkillStore};
use crate::{Options, VERSION};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::time::Duration;

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
    let policy_changed = desired.generation != *previous_generation;
    let mut items = Vec::new();
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
        *previous_generation = desired.generation;
        return Ok(());
    }
    let report = ExtensionReconcileReport {
        generation: desired.generation.clone(),
        items,
    };
    report_extension_reconcile(&client, &options.api_base, agent_id, &credential, &report)?;
    *previous_generation = desired.generation;
    Ok(())
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
