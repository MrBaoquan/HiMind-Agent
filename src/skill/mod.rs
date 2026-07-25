pub(crate) mod authoring;
pub(crate) mod cli;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod manifest;
pub(crate) mod resolver;
pub(crate) mod store;
pub(crate) mod types;
pub(crate) mod workbuddy;

use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::retired_skill_ids;
use crate::skill::store::SkillStore;
use crate::skill::types::SkillRecord;
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

pub(crate) fn codex_repair_json(
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    codex::repair_json(skill_id, preserve_modified, agent_version, capability_facts)
}

pub(crate) fn client_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    retire_removed_client_skills();
    Ok(json!({
        "codex": codex::status_json(agent_version, capability_facts)?,
        "github-copilot": copilot::status_json(agent_version, capability_facts)?,
        "workbuddy": workbuddy::status_json(agent_version, capability_facts)?,
    }))
}

pub(crate) fn client_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    retire_removed_client_skills();
    Ok(json!({
        "codex": codex::sync_json(agent_version, capability_facts)?,
        "github-copilot": copilot::sync_json(agent_version, capability_facts)?,
        "workbuddy": workbuddy::sync_json(agent_version, capability_facts)?,
    }))
}

fn retire_removed_client_skills() {
    for skill_id in retired_skill_ids() {
        let _ = codex::uninstall_json(skill_id);
        let _ = copilot::uninstall_json(skill_id);
        let _ = workbuddy::uninstall_json(skill_id);
    }
}

pub(crate) fn sync_record_to_supported_clients(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let mut clients = BTreeMap::new();
    for client in &record.manifest.supported_clients {
        let normalized = client.trim().to_ascii_lowercase();
        if clients.contains_key(&normalized) {
            continue;
        }
        let rendered = match normalized.as_str() {
            "codex" => codex::sync_record_json(record, agent_version, capability_facts)?,
            "github-copilot" => copilot::sync_record_json(record, agent_version, capability_facts)?,
            "workbuddy" => workbuddy::sync_record_json(record, agent_version, capability_facts)?,
            _ => return Err(format!("Agent 尚未实现 Skill 客户端适配器: {client}").into()),
        };
        clients.insert(normalized, rendered);
    }
    Ok(clients)
}

pub(crate) fn uninstall_supported_clients_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    uninstall_supported_clients_impl(skill_id, false)
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
    for client in &record.manifest.supported_clients {
        let normalized = client.trim().to_ascii_lowercase();
        if clients.contains_key(&normalized) {
            continue;
        }
        let removed = match normalized.as_str() {
            "codex" => codex::uninstall_json(skill_id)?,
            "github-copilot" => copilot::uninstall_json(skill_id)?,
            "workbuddy" => workbuddy::uninstall_json(skill_id)?,
            _ => return Err(format!("Agent 尚未实现 Skill 客户端适配器: {client}").into()),
        };
        clients.insert(normalized, removed);
    }
    Ok(json!({"skill_id": skill_id, "clients": clients}))
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
