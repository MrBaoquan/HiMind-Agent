pub(crate) mod authoring;
pub(crate) mod cli;
pub(crate) mod codex;
pub(crate) mod manifest;
pub(crate) mod resolver;
pub(crate) mod store;
pub(crate) mod types;

use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::SkillStore;
use crate::skill::types::SkillRecord;
use crate::store::types::LocalWorkerStatus;
use serde_json::{json, Value};
use std::error::Error;
use std::sync::{Arc, Mutex};

pub(crate) fn catalog_json(
    agent_version: &str,
    client_id: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
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

pub(crate) fn codex_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    codex::status_json(agent_version, capability_facts)
}

pub(crate) fn codex_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    codex::sync_json(agent_version, capability_facts)
}

pub(crate) fn codex_sync_one_json(
    skill_id: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    codex::sync_one_json(skill_id, agent_version, capability_facts)
}

pub(crate) fn codex_repair_json(
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    codex::repair_json(skill_id, preserve_modified, agent_version, capability_facts)
}

pub(crate) fn codex_uninstall_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    codex::uninstall_json(skill_id)
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
