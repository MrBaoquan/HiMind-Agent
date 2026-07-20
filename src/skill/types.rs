use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SkillScope {
    Builtin,
    Organization,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SkillCapabilityDependency {
    pub id: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min_version: Option<String>,
    #[serde(default)]
    pub max_version: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SkillPluginDependency {
    pub plugin_id: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub scope: SkillScope,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub min_agent_version: String,
    #[serde(default)]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<SkillCapabilityDependency>,
    #[serde(default)]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
    #[serde(default)]
    pub risk_summary: String,
    #[serde(default)]
    pub contents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillPointer {
    pub version: String,
    pub path: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillReceipt {
    pub skill_id: String,
    pub version: String,
    pub client: String,
    pub source_root: String,
    pub rendered_root: String,
    pub rendered_at: String,
    pub files: Vec<String>,
    pub checksums: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillRecord {
    pub manifest: SkillManifest,
    pub root: PathBuf,
    pub version_root: PathBuf,
    pub current: bool,
    pub previous_version: Option<String>,
}
