pub(crate) mod authoring;
pub(crate) mod cli;
pub(crate) mod clients;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod direct;
pub(crate) mod manifest;
pub(crate) mod resolver;
pub(crate) mod store;
pub(crate) mod types;

use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::skill::clients::{
    declares_portable_skill, manifest_supports_client, PORTABLE_PROFILE_ID,
};
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::retired_skill_ids;
use crate::skill::store::SkillStore;
use crate::skill::types::{SkillManifest, SkillRecord};
use crate::store::types::LocalWorkerStatus;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

pub(crate) fn catalog_json(
    agent_version: &str,
    client_id: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    retire_removed_client_skills();
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let records = store.list_records()?;
    let items = records
        .into_iter()
        .map(|record| {
            let readiness = SkillReadiness::resolve(
                &record.manifest,
                capability_facts,
                agent_version,
                client_id,
            );
            json!({
                "record": record,
                "readiness": readiness,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "client_id": client_id,
        "agent_version": agent_version,
        "store_root": store.root().to_string_lossy().to_string(),
        "items": items,
    }))
}

pub(crate) fn client_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    retire_removed_client_skills();
    let mut clients = BTreeMap::new();
    clients.insert(
        "himind-ai".to_string(),
        himind_ai_status_json(agent_version, capability_facts)?,
    );
    clients.insert(
        "codex".to_string(),
        codex::status_json(agent_version, capability_facts)?,
    );
    for (client_id, status) in direct::status_json(agent_version, capability_facts)? {
        clients.insert(client_id, status);
    }
    Ok(json!(clients))
}

pub(crate) fn client_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    retire_removed_client_skills();
    let mut clients = BTreeMap::new();
    clients.insert(
        "himind-ai".to_string(),
        himind_ai_sync_json(agent_version, capability_facts)?,
    );
    clients.insert(
        "codex".to_string(),
        codex::sync_json(agent_version, capability_facts)?,
    );
    for (client_id, sync) in direct::sync_json(agent_version, capability_facts)? {
        clients.insert(client_id, sync);
    }
    Ok(json!(clients))
}

fn retire_removed_client_skills() {
    for skill_id in retired_skill_ids() {
        let _ = codex::uninstall_json(skill_id);
        let _ = direct::uninstall_json(skill_id);
    }
}

pub(crate) fn sync_record_to_supported_clients(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let mut clients = BTreeMap::new();
    for normalized in sync_client_ids(record) {
        if clients.contains_key(&normalized) {
            continue;
        }
        let rendered = match normalized.as_str() {
            "codex" => codex::sync_record_json(record, agent_version, capability_facts)?,
            "himind-ai" => himind_ai_sync_record_json(record, agent_version, capability_facts)?,
            _ => direct::sync_record_json(&normalized, record, agent_version, capability_facts)?,
        };
        clients.insert(normalized, rendered);
    }
    Ok(clients)
}

/// Synchronize one Skill to one explicitly selected AI client.
///
/// The regular sync path intentionally targets every active, supported client.
/// This narrower operation is used by the UI and MCP management surfaces when
/// a user wants to repair one client without changing the others.
pub(crate) fn sync_skill_client_json(
    skill_id: &str,
    client_id: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let normalized = client_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("client_id 不能为空".into());
    }
    if normalized != "himind-ai"
        && normalized != "codex"
        && clients::directory_client(&normalized).is_none()
    {
        return Err(format!("Agent 尚未实现 Skill 客户端适配器: {normalized}").into());
    }
    if !manifest_supports_client(&record.manifest, &normalized) {
        return Ok(json!({
            "skill_id": skill_id,
            "client_id": normalized,
            "client_name": client_name(&normalized),
            "target_configured": false,
            "rendered": {
                "skill_id": skill_id,
                "version": record.manifest.version,
                "state": "unsupported",
                "reason": "该 Skill 未声明此客户端",
            },
        }));
    }
    match normalized.as_str() {
        "codex" => codex::sync_record_json(&record, agent_version, capability_facts),
        "himind-ai" => himind_ai_sync_record_json(&record, agent_version, capability_facts),
        _ => direct::sync_record_json(&normalized, &record, agent_version, capability_facts),
    }
}

/// Remove this Skill from every external client independently.
///
/// A stale or user-owned copy in one client must not prevent cleanup in other
/// clients, so failures are returned per client instead of aborting the batch.
pub(crate) fn unregister_skill_clients_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    ensure_skill_client_unregister_allowed(&store, skill_id)?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let mut results = BTreeMap::new();
    let mut failures = BTreeMap::new();
    let mut removed_count = 0usize;
    for client_id in uninstall_client_ids(&record)
        .into_iter()
        .filter(|client_id| client_id != "himind-ai")
    {
        match unregister_skill_client_json(skill_id, &client_id) {
            Ok(result) => {
                if result
                    .get("removed")
                    .and_then(|removed| removed.get("removed"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    removed_count += 1;
                }
                results.insert(client_id, result);
            }
            Err(error) => {
                failures.insert(client_id, error.to_string());
            }
        }
    }
    Ok(json!({
        "skill_id": skill_id,
        "removed_count": removed_count,
        "results": results,
        "failures": failures,
    }))
}

pub(crate) fn repair_record_for_supported_clients(
    record: &SkillRecord,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let mut clients = BTreeMap::new();
    for normalized in sync_client_ids(record) {
        if clients.contains_key(&normalized) {
            continue;
        }
        let repaired = match normalized.as_str() {
            "codex" => codex::repair_json(
                &record.manifest.id,
                preserve_modified,
                agent_version,
                capability_facts,
            )?,
            "himind-ai" => himind_ai_sync_record_json(record, agent_version, capability_facts)?,
            _ => direct::repair_json(
                &normalized,
                &record.manifest.id,
                preserve_modified,
                agent_version,
                capability_facts,
            )?,
        };
        clients.insert(normalized, repaired);
    }
    Ok(clients)
}

fn himind_ai_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let items = store
        .list_records()?
        .into_iter()
        .map(|record| {
            let readiness = SkillReadiness::resolve(
                &record.manifest,
                capability_facts,
                agent_version,
                "himind-ai",
            );
            let supported = manifest_supports_client(&record.manifest, "himind-ai");
            let client_state = if !supported {
                "unsupported"
            } else if readiness.state == "blocked" {
                "blocked"
            } else {
                "installed"
            };
            json!({
                "record": record,
                "readiness": readiness,
                "rendered_root": record.version_root,
                "rendered": supported,
                "rendered_valid": supported && client_state == "installed",
                "client_state": client_state,
                "installed_version": if supported { Some(record.manifest.version.clone()) } else { None },
                "available_version": record.manifest.version,
                "last_synced_at": Value::Null,
                "managed_files": record.manifest.contents,
                "modified_files": Vec::<String>::new(),
                "available_actions": Vec::<String>::new(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "client_id": "himind-ai",
        "client_name": "HiMind AI",
        "client_detected": true,
        "skill_standard": "agentskills.io",
        "support_level": "official",
        "support_note": "由 HiMind Agent 会话直接加载",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "target_exists": store.root().exists(),
        "target_mode": "builtin",
        "sync_mode": store.sync_mode()?,
        "items": items,
    }))
}

fn himind_ai_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let mut rendered = Vec::new();
    let mut blocked = Vec::new();
    for record in store.list_records()? {
        if !manifest_supports_client(&record.manifest, "himind-ai") {
            continue;
        }
        let readiness = SkillReadiness::resolve(
            &record.manifest,
            capability_facts,
            agent_version,
            "himind-ai",
        );
        if readiness.state == "blocked" {
            blocked.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "reasons": readiness.reasons,
            }));
        } else {
            rendered.push(himind_ai_rendered_result(&record));
        }
    }
    Ok(json!({
        "client_id": "himind-ai",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "rendered": rendered,
        "skipped": [],
        "blocked": blocked,
    }))
}

fn himind_ai_sync_record_json(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let readiness = SkillReadiness::resolve(
        &record.manifest,
        capability_facts,
        agent_version,
        "himind-ai",
    );
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    Ok(json!({
        "client_id": "himind-ai",
        "target_root": SkillStore::new().root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "rendered": himind_ai_rendered_result(record),
        "activation": "next_session",
    }))
}

fn himind_ai_rendered_result(record: &SkillRecord) -> Value {
    json!({
        "skill_id": record.manifest.id,
        "version": record.manifest.version,
        "state": "available",
        "reason": Value::Null,
        "rendered_root": record.version_root,
        "files": record.manifest.contents,
    })
}

pub(crate) fn uninstall_supported_clients_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    uninstall_supported_clients_impl(skill_id, false)
}

/// Remove a Skill's managed copy from one external AI client while keeping the
/// Skill in the Agent store. HiMind AI is backed by the Agent store directly,
/// so it has no registration to remove.
pub(crate) fn unregister_skill_client_json(
    skill_id: &str,
    client_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    ensure_skill_client_unregister_allowed(&store, skill_id)?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let normalized = client_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("client_id 不能为空".into());
    }
    if normalized != "himind-ai"
        && normalized != "codex"
        && clients::directory_client(&normalized).is_none()
    {
        return Err(format!("Agent 尚未实现 Skill 客户端适配器: {normalized}").into());
    }
    if !manifest_supports_client(&record.manifest, &normalized) {
        return Ok(json!({
            "skill_id": skill_id,
            "client_id": normalized,
            "client_name": client_name(&normalized),
            "target_root": if normalized == "himind-ai" { store.root().to_string_lossy().to_string() } else { String::new() },
            "target_source": if normalized == "himind-ai" { "agent-skill-store" } else { "" },
            "target_configured": normalized == "himind-ai",
            "removed": { "skill_id": skill_id, "removed": false },
            "state": "unsupported",
            "reason": "该 Skill 未声明此客户端",
        }));
    }
    let raw = match normalized.as_str() {
        "himind-ai" => json!({
            "skill_id": skill_id,
            "removed": false,
            "state": "builtin",
            "reason": "HiMind AI 直接从 Agent Skill Store 加载，无需注册",
        }),
        "codex" => codex::uninstall_json(skill_id)?,
        _ => direct::uninstall_for_client(&normalized, skill_id)?,
    };
    let removal = raw.get("removed").unwrap_or(&raw);
    let removed = removal
        .as_bool()
        .or_else(|| removal.get("removed").and_then(Value::as_bool))
        .unwrap_or(false);
    let mut response = json!({
        "skill_id": skill_id,
        "client_id": normalized,
        "client_name": client_name(&normalized),
        "target_root": raw.get("target_root").cloned().unwrap_or(Value::Null),
        "target_source": raw.get("target_source").cloned().unwrap_or(Value::Null),
        "target_configured": raw.get("target_configured").cloned().unwrap_or(Value::Bool(false)),
        "removed": { "skill_id": skill_id, "removed": removed },
    });
    if let Some(object) = response.as_object_mut() {
        if let Some(state) = raw.get("state").or_else(|| removal.get("state")) {
            object.insert("state".to_string(), state.clone());
        }
        if let Some(profile) = raw
            .get("managing_profile")
            .or_else(|| removal.get("managing_profile"))
        {
            object.insert("managing_profile".to_string(), profile.clone());
        }
        if let Some(reason) = raw.get("reason").or_else(|| removal.get("reason")) {
            object.insert("reason".to_string(), reason.clone());
        }
    }
    Ok(response)
}

fn ensure_skill_client_unregister_allowed(
    store: &SkillStore,
    skill_id: &str,
) -> Result<(), Box<dyn Error>> {
    if let Some(policy) = store.management_policy(skill_id)? {
        if policy.management != "user_managed" && !policy.allow_uninstall {
            return Err("由组织管理的 AI 技能不能取消客户端同步".into());
        }
    }
    Ok(())
}

fn client_name(client_id: &str) -> &'static str {
    if client_id == "himind-ai" {
        return "HiMind AI";
    }
    if client_id == "codex" {
        return "Codex";
    }
    clients::directory_client(client_id)
        .map(|definition| definition.name)
        .unwrap_or("AI 工具")
}

pub(crate) fn uninstall_supported_clients_for_policy_json(
    skill_id: &str,
) -> Result<Value, Box<dyn Error>> {
    uninstall_supported_clients_impl(skill_id, true)
}

fn uninstall_supported_clients_impl(
    skill_id: &str,
    policy_override: bool,
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    if !policy_override {
        if let Some(policy) = store.management_policy(skill_id)? {
            if policy.management != "user_managed" && !policy.allow_uninstall {
                return Err("由组织管理的 AI 技能不能自行卸载".into());
            }
        }
    }
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let mut clients = BTreeMap::new();
    for normalized in uninstall_client_ids(&record) {
        if clients.contains_key(&normalized) {
            continue;
        }
        let removed = match normalized.as_str() {
            "codex" => codex::uninstall_json(skill_id)?,
            "himind-ai" => json!({
                "client_id": "himind-ai",
                "target_root": store.root().to_string_lossy().to_string(),
                "target_source": "agent-skill-store",
                "target_configured": true,
                "removed": {
                    "skill_id": skill_id,
                    "removed": false,
                },
            }),
            _ => direct::uninstall_for_client(&normalized, skill_id)?,
        };
        clients.insert(normalized, removed);
    }
    Ok(json!({"skill_id": skill_id, "clients": clients}))
}

fn declared_client_ids(record: &SkillRecord) -> Vec<String> {
    record
        .manifest
        .supported_clients
        .iter()
        .map(|client| client.trim().to_ascii_lowercase())
        .filter(|client| !client.is_empty() && client != PORTABLE_PROFILE_ID)
        .collect()
}

fn sync_client_ids(record: &SkillRecord) -> Vec<String> {
    active_client_ids_for_manifest(&record.manifest)
}

pub(crate) fn active_client_ids_for_manifest(manifest: &SkillManifest) -> Vec<String> {
    let active_directory_clients = direct::active_client_ids();
    let mut clients = manifest
        .supported_clients
        .iter()
        .map(|client| client.trim().to_ascii_lowercase())
        .filter(|client| !client.is_empty() && client != PORTABLE_PROFILE_ID)
        .into_iter()
        .filter(|client_id| {
            clients::directory_client(client_id).is_none()
                || active_directory_clients.contains(&client_id.as_str())
        })
        .collect::<Vec<_>>();
    if declares_portable_skill(manifest) {
        clients.push("himind-ai".to_string());
        if codex::is_detected() {
            clients.push("codex".to_string());
        }
        clients.extend(active_directory_clients.into_iter().map(str::to_string));
    }
    clients.sort();
    clients.dedup();
    clients
}

fn uninstall_client_ids(record: &SkillRecord) -> Vec<String> {
    let mut clients = declared_client_ids(record);
    if declares_portable_skill(&record.manifest) {
        clients.push("himind-ai".to_string());
        clients.push("codex".to_string());
        clients.extend(
            crate::skill::clients::DIRECTORY_CLIENTS
                .iter()
                .map(|definition| definition.id.to_string()),
        );
    }
    clients.sort();
    clients.dedup();
    clients
}

pub(crate) fn capability_facts_from_gateway(
    options: &crate::Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    context: &InvocationContext,
) -> Result<Vec<CapabilityFact>, Box<dyn Error>> {
    let gateway = CapabilityGateway::new(options.clone(), worker_status);
    let descriptors = gateway.list_capabilities(context)?;
    Ok(descriptors
        .into_iter()
        .map(|descriptor| CapabilityFact {
            id: descriptor.id,
            version: descriptor.version,
            source: descriptor.source,
        })
        .collect())
}

pub(crate) fn records_json(records: &[SkillRecord]) -> Value {
    json!({
        "items": records,
        "total": records.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::store::SkillManagementPolicy;
    use std::fs;

    #[test]
    fn organization_policy_can_block_client_unregister() {
        let root = std::env::temp_dir().join(format!(
            "himind-skill-unregister-policy-{}",
            std::process::id()
        ));
        let store = SkillStore::with_root(root.clone());
        let skill_id = "com.himind.skill.managed";
        let skill_root = root.join("managed").join(skill_id);
        fs::create_dir_all(&skill_root).unwrap();
        store
            .apply_management_policy(
                skill_id,
                &SkillManagementPolicy {
                    management: "organization_managed".to_string(),
                    source: "organization".to_string(),
                    assignment_id: "assignment-1".to_string(),
                    reason: "required".to_string(),
                    allow_uninstall: false,
                },
            )
            .unwrap();

        let error = ensure_skill_client_unregister_allowed(&store, skill_id).unwrap_err();
        assert!(error.to_string().contains("不能取消客户端同步"));
        let _ = fs::remove_dir_all(root);
    }
}
