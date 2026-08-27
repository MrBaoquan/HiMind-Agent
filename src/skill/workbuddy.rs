use crate::skill::copilot::{self, DirectSkillTarget};
use crate::skill::resolver::CapabilityFact;
use crate::skill::store::SkillStore;
use crate::skill::types::SkillRecord;
use std::env;
use std::error::Error;
use std::path::PathBuf;

const CLIENT_ID: &str = "workbuddy";
const CLIENT_NAME: &str = "WorkBuddy";

pub(crate) fn status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    copilot::status_for_target(
        CLIENT_ID,
        resolve_target(&SkillStore::new()),
        agent_version,
        capability_facts,
    )
}

pub(crate) fn sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    copilot::sync_for_target(
        CLIENT_ID,
        CLIENT_NAME,
        resolve_target(&SkillStore::new()),
        agent_version,
        capability_facts,
    )
}

pub(crate) fn sync_record_json(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    copilot::sync_record_for_target(
        CLIENT_ID,
        CLIENT_NAME,
        resolve_target(&SkillStore::new()),
        record,
        agent_version,
        capability_facts,
    )
}

pub(crate) fn repair_json(
    skill_id: &str,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<serde_json::Value, Box<dyn Error>> {
    copilot::repair_for_target(
        CLIENT_ID,
        CLIENT_NAME,
        resolve_target(&SkillStore::new()),
        skill_id,
        preserve_modified,
        agent_version,
        capability_facts,
    )
}

pub(crate) fn uninstall_json(skill_id: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    copilot::uninstall_for_target(
        CLIENT_ID,
        CLIENT_NAME,
        resolve_target(&SkillStore::new()),
        skill_id,
    )
}

fn resolve_target(store: &SkillStore) -> DirectSkillTarget {
    if let Some(path) = env::var_os("HIMIND_WORKBUDDY_SKILL_DIR") {
        return DirectSkillTarget {
            root: PathBuf::from(path),
            source: "env:HIMIND_WORKBUDDY_SKILL_DIR".to_string(),
            configured: true,
        };
    }
    if let Some(path) = env::var_os("WORKBUDDY_HOME") {
        return DirectSkillTarget {
            root: PathBuf::from(path).join("skills"),
            source: "env:WORKBUDDY_HOME".to_string(),
            configured: true,
        };
    }
    if let Some(userprofile) = env::var_os("USERPROFILE") {
        return DirectSkillTarget {
            root: PathBuf::from(userprofile).join(".workbuddy").join("skills"),
            source: "userprofile:dot-workbuddy".to_string(),
            configured: false,
        };
    }
    if let Some(home) = env::var_os("HOME") {
        return DirectSkillTarget {
            root: PathBuf::from(home).join(".workbuddy").join("skills"),
            source: "home:dot-workbuddy".to_string(),
            configured: false,
        };
    }
    DirectSkillTarget {
        root: store.rendered_skill_root(CLIENT_ID, ".preview"),
        source: "preview".to_string(),
        configured: false,
    }
}
