//! Generic filesystem-backed Agent Skills adapters.
//!
//! Most AI coding clients intentionally use the same portable `SKILL.md`
//! package shape. Their MCP configuration formats differ, but their skill
//! distribution contract is a directory containing one folder per skill.
//! This module keeps that contract in one place and makes each client a data
//! definition instead of a new copy of the renderer.

use super::clients::{directory_client, SkillClientDefinition, DIRECTORY_CLIENTS};
use super::copilot::{self, DirectSkillTarget};
use crate::skill::resolver::CapabilityFact;
use crate::skill::store::SkillStore;
use crate::skill::types::SkillRecord;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

pub(crate) fn status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let store = SkillStore::new();
    let mut result = BTreeMap::new();
    for definition in DIRECTORY_CLIENTS {
        let target = resolve_target(&store, definition);
        let detected = target_detected(&target, definition);
        let mut status =
            copilot::status_for_target(definition.id, target, agent_version, capability_facts)?;
        if let Some(object) = status.as_object_mut() {
            object.insert(
                "client_name".to_string(),
                Value::String(definition.name.to_string()),
            );
            object.insert("client_detected".to_string(), Value::Bool(detected));
            object.insert(
                "skill_standard".to_string(),
                Value::String("agentskills.io".to_string()),
            );
            object.insert(
                "support_level".to_string(),
                Value::String(definition.support_level.to_string()),
            );
            object.insert(
                "support_note".to_string(),
                Value::String(definition.support_note.to_string()),
            );
        }
        result.insert(definition.id.to_string(), status);
    }
    Ok(result)
}

pub(crate) fn sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let store = SkillStore::new();
    let mut result = BTreeMap::new();
    for definition in DIRECTORY_CLIENTS {
        let target = resolve_target(&store, definition);
        if !target_detected(&target, definition) {
            continue;
        }
        result.insert(
            definition.id.to_string(),
            copilot::sync_for_target(
                definition.id,
                definition.name,
                target,
                agent_version,
                capability_facts,
            )?,
        );
    }
    Ok(result)
}

pub(crate) fn sync_record_json(
    client_id: &str,
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let definition = definition(client_id)?;
    copilot::sync_record_for_target(
        definition.id,
        definition.name,
        resolve_target(&SkillStore::new(), definition),
        record,
        agent_version,
        capability_facts,
    )
}

pub(crate) fn repair_json(
    client_id: &str,
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let definition = definition(client_id)?;
    copilot::repair_for_target(
        definition.id,
        definition.name,
        resolve_target(&SkillStore::new(), definition),
        skill_id,
        preserve_modified,
        agent_version,
        capability_facts,
    )
}

pub(crate) fn uninstall_for_client(
    client_id: &str,
    skill_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let definition = definition(client_id)?;
    copilot::uninstall_for_target(
        definition.id,
        definition.name,
        resolve_target(&SkillStore::new(), definition),
        skill_id,
    )
}

pub(crate) fn uninstall_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    let mut clients = BTreeMap::new();
    for definition in DIRECTORY_CLIENTS {
        clients.insert(
            definition.id.to_string(),
            uninstall_for_client(definition.id, skill_id)?,
        );
    }
    Ok(serde_json::json!({ "skill_id": skill_id, "clients": clients }))
}

fn definition(client_id: &str) -> Result<&'static SkillClientDefinition, Box<dyn Error>> {
    directory_client(client_id)
        .ok_or_else(|| format!("Agent 尚未实现 Skill 客户端适配器: {client_id}").into())
}

pub(crate) fn active_client_ids() -> Vec<&'static str> {
    let store = SkillStore::new();
    DIRECTORY_CLIENTS
        .iter()
        .filter(|definition| {
            let target = resolve_target(&store, definition);
            target_detected(&target, definition)
        })
        .map(|definition| definition.id)
        .collect()
}

fn resolve_target(store: &SkillStore, definition: &SkillClientDefinition) -> DirectSkillTarget {
    if let Some(path) = env::var_os(definition.env_key) {
        return DirectSkillTarget {
            root: PathBuf::from(path),
            source: format!("env:{}", definition.env_key),
            configured: true,
        };
    }
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from);
    let workspace = env::var_os("HIMIND_SKILL_WORKSPACE").map(PathBuf::from);
    if let Some(workspace) = workspace {
        let project = workspace.join(Path::new(definition.project_dir));
        if project.exists() {
            return DirectSkillTarget {
                root: project,
                source: "workspace".to_string(),
                configured: true,
            };
        }
    }
    if let Some(home) = home {
        return DirectSkillTarget {
            root: home.join(Path::new(definition.user_dir)),
            source: format!("userprofile:{}", definition.user_dir),
            configured: false,
        };
    }
    DirectSkillTarget {
        root: store.rendered_skill_root(definition.id, ".preview"),
        source: "preview".to_string(),
        configured: false,
    }
}

fn target_detected(target: &DirectSkillTarget, definition: &SkillClientDefinition) -> bool {
    if target.configured || target.root.exists() {
        return true;
    }
    let Some(home) = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
    else {
        return false;
    };
    home.join(Path::new(definition.user_dir))
        .parent()
        .is_some_and(Path::exists)
}
