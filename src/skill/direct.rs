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
use crate::skill::target::{self, SkillTarget};
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
        let target = resolve_target(&store, definition)?;
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
        let target = resolve_target(&store, definition)?;
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
        resolve_target(&SkillStore::new(), definition)?,
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
        resolve_target(&SkillStore::new(), definition)?,
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
        resolve_target(&SkillStore::new(), definition)?,
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
            resolve_target(&store, definition)
                .map(|target| target_detected(&target, definition))
                .unwrap_or(false)
        })
        .map(|definition| definition.id)
        .collect()
}

fn resolve_target(
    store: &SkillStore,
    definition: &SkillClientDefinition,
) -> Result<DirectSkillTarget, Box<dyn Error>> {
    // A selected project target is explicit and must not be shadowed by a
    // client-specific global directory environment variable.
    if let Some(workspace) = target::resolve_workspace_root(None)? {
        return SkillTarget::workspace(&workspace, definition.project_dir, "workspace");
    }
    if let Some(path) = env::var_os(definition.env_key) {
        return Ok(SkillTarget::global(
            PathBuf::from(path),
            format!("env:{}", definition.env_key),
            true,
        ));
    }
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(home) = home {
        return Ok(SkillTarget::global(
            home.join(Path::new(definition.user_dir)),
            format!("userprofile:{}", definition.user_dir),
            false,
        ));
    }
    Ok(SkillTarget::global(
        store.rendered_skill_root(definition.id, ".preview"),
        "preview",
        false,
    ))
}

fn target_detected(target: &DirectSkillTarget, definition: &SkillClientDefinition) -> bool {
    if target.is_workspace() {
        // The standard `.agents/skills` projection covers portable Skills
        // (that path is owned by the Codex adapter).  Other client-specific
        // project directories are only created when the project already uses
        // that client, the machine has it installed, or the user pointed at it
        // explicitly.  Rendering into every registered client would scatter a
        // dozen dot-folders through a repository that has nothing to do with
        // those tools.
        if target.root.exists() || env::var_os(definition.env_key).is_some() {
            return true;
        }
        return workspace_client_installed(definition);
    }
    if target.root.exists() || target.configured || env::var_os(definition.env_key).is_some() {
        return true;
    }
    workspace_client_installed(definition)
}

/// Whether this machine has the client installed, inferred from the parent of
/// its user-level Skill directory (`~/.cursor`, `~/.claude`, ...).
fn workspace_client_installed(definition: &SkillClientDefinition) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unused_client() -> SkillClientDefinition {
        SkillClientDefinition {
            id: "himind-test-unused-client",
            name: "Unused Test Client",
            env_key: "HIMIND_TEST_UNUSED_CLIENT_SKILL_DIR",
            project_dir: ".himind-test-unused/skills",
            user_dir: ".himind-test-unused-client-not-installed/skills",
            support_level: "compatible",
            support_note: "",
            mcp_target_ids: &[],
        }
    }

    fn workspace_root() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "himind-direct-client-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn project_targets_skip_clients_the_machine_does_not_use() {
        let root = workspace_root();
        std::fs::create_dir_all(&root).unwrap();
        let definition = unused_client();
        let target = SkillTarget::workspace(&root, definition.project_dir, "workspace").unwrap();

        assert!(!target_detected(&target, &definition));
        std::fs::create_dir_all(&target.root).unwrap();
        assert!(target_detected(&target, &definition));
        let _ = std::fs::remove_dir_all(root);
    }
}
