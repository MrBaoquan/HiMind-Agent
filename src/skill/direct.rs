//! Generic filesystem-backed Agent Skills adapters.
//!
//! Most AI coding clients intentionally use the same portable `SKILL.md`
//! package shape. Their MCP configuration formats differ, but their skill
//! distribution contract is a directory containing one folder per skill.
//! This module keeps that contract in one place and makes each client a data
//! definition instead of a new copy of the renderer.

use super::clients::{directory_client, ClientDefinition, DIRECTORY_CLIENTS};
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
    store: &SkillStore,
    records: &[SkillRecord],
    configured_sync_mode: &str,
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    // 二十来个目录客户端的探测、渲染校验彼此独立，而且这一段只读目标目录：
    // 串行等于几千次文件读取排队，并行后整段耗时接近最慢的那个客户端。
    // 技能记录与同步模式由调用方读一次共享，避免每个客户端各自重跑全量读盘。
    let statuses = std::thread::scope(|scope| {
        let handles = DIRECTORY_CLIENTS
            .iter()
            .map(|definition| {
                scope.spawn(move || -> Result<(String, Value), String> {
                    let __t_client = std::time::Instant::now();
                    let target =
                        resolve_target(store, definition).map_err(|error| error.to_string())?;
                    let detected = target_detected(&target, definition);
                    let mut status = copilot::status_for_target_with(
                        definition.id,
                        target,
                        agent_version,
                        capability_facts,
                        records,
                        configured_sync_mode,
                    )
                    .map_err(|error| error.to_string())?;
                    if let Some(object) = status.as_object_mut() {
                        object.insert(
                            "client_name".to_string(),
                            Value::String(definition.name.to_string()),
                        );
                        object.insert("client_detected".to_string(), Value::Bool(detected));
                        object.insert(
                            "skill_standard".to_string(),
                            Value::String(definition.skill_standard().to_string()),
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
                    super::trace_lap(&format!("client:{}", definition.id), __t_client);
                    Ok((definition.id.to_string(), status))
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                Err(_) => Err("客户端技能状态计算线程异常退出".into()),
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    Ok(statuses.into_iter().collect())
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

fn definition(client_id: &str) -> Result<&'static ClientDefinition, Box<dyn Error>> {
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

/// 安装计划需要"这次会写到哪个目录、那个目标现在存不存在"。
///
/// 计划和执行必须是同一个答案，所以这里直接复用同一套解析与探测逻辑，
/// 而不是在计划侧再推断一遍客户端目录约定。
pub(crate) fn client_target(client_id: &str) -> Option<(SkillTarget, bool)> {
    let definition = directory_client(client_id)?;
    let store = SkillStore::new();
    let target = resolve_target(&store, definition).ok()?;
    let detected = target_detected(&target, definition);
    Some((target, detected))
}

fn resolve_target(
    store: &SkillStore,
    definition: &ClientDefinition,
) -> Result<DirectSkillTarget, Box<dyn Error>> {
    // A selected project target is explicit and must not be shadowed by a
    // client-specific global directory environment variable.
    if let Some(project_dir) = definition.skill_project_dir() {
        if let Some(workspace) = target::resolve_workspace_root(None)? {
            return SkillTarget::workspace(&workspace, project_dir, "workspace");
        }
    }
    if let Some(env_key) = definition.skill_env_key() {
        if let Some(path) = env::var_os(env_key) {
            return Ok(SkillTarget::global(
                PathBuf::from(path),
                format!("env:{env_key}"),
                true,
            ));
        }
    }
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from);
    if let (Some(home), Some(user_dir)) = (home, definition.skill_user_dir()) {
        return Ok(SkillTarget::global(
            home.join(Path::new(user_dir)),
            format!("userprofile:{user_dir}"),
            false,
        ));
    }
    Ok(SkillTarget::global(
        store.rendered_skill_root(definition.id, ".preview"),
        "preview",
        false,
    ))
}

fn target_detected(target: &DirectSkillTarget, definition: &ClientDefinition) -> bool {
    if target.is_workspace() {
        // The standard `.agents/skills` projection covers portable Skills
        // (that path is owned by the Codex adapter).  Other client-specific
        // project directories are only created when the project already uses
        // that client, the machine has it installed, or the user pointed at it
        // explicitly.  Rendering into every registered client would scatter a
        // dozen dot-folders through a repository that has nothing to do with
        // those tools.
        if target.root.exists() || client_env_configured(definition) {
            return true;
        }
        return workspace_client_installed(definition);
    }
    if target.root.exists() || target.configured || client_env_configured(definition) {
        return true;
    }
    workspace_client_installed(definition)
}

/// Whether the user pointed this client's Skill directory somewhere explicit.
fn client_env_configured(definition: &ClientDefinition) -> bool {
    definition
        .skill_env_key()
        .is_some_and(|key| env::var_os(key).is_some())
}

/// Whether this machine has the client installed, inferred from the parent of
/// its user-level Skill directory (`~/.cursor`, `~/.claude`, ...).
fn workspace_client_installed(definition: &ClientDefinition) -> bool {
    let Some(user_dir) = definition.skill_user_dir() else {
        return false;
    };
    let Some(home) = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
    else {
        return false;
    };
    home.join(Path::new(user_dir))
        .parent()
        .is_some_and(Path::exists)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::clients;

    fn unused_client() -> ClientDefinition {
        clients::skill_directory_client(
            "himind-test-unused-client",
            "Unused Test Client",
            "HIMIND_TEST_UNUSED_CLIENT_SKILL_DIR",
            ".himind-test-unused/skills",
            ".himind-test-unused-client-not-installed/skills",
            "compatible",
            "",
            &[],
        )
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
        let target =
            SkillTarget::workspace(&root, definition.skill_project_dir().unwrap(), "workspace")
                .unwrap();

        assert!(!target_detected(&target, &definition));
        std::fs::create_dir_all(&target.root).unwrap();
        assert!(target_detected(&target, &definition));
        let _ = std::fs::remove_dir_all(root);
    }
}
