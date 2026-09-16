use serde::{Deserialize, Deserializer, Serialize};
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
    #[serde(default)]
    pub author: String,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub categories: Vec<String>,
    pub version: String,
    pub scope: SkillScope,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default)]
    pub min_agent_version: String,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub supported_clients: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub capabilities: Vec<SkillCapabilityDependency>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
    #[serde(default)]
    pub risk_summary: String,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub contents: Vec<String>,
}

fn deserialize_null_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
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
    #[serde(default = "default_agent_profile")]
    pub agent_profile: String,
    pub source_root: String,
    pub rendered_root: String,
    pub rendered_at: String,
    #[serde(default = "default_render_mode")]
    pub render_mode: String,
    /// Where this client projection is installed.  Old receipts predate
    /// project targets and are treated as global installations.
    #[serde(default = "default_target_kind")]
    pub target_kind: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub files: Vec<String>,
    pub checksums: BTreeMap<String, String>,
}

fn default_render_mode() -> String {
    "copy".to_string()
}

fn default_target_kind() -> String {
    crate::skill::target::TARGET_KIND_GLOBAL.to_string()
}

fn default_agent_profile() -> String {
    "production".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillRecord {
    pub manifest: SkillManifest,
    pub root: PathBuf,
    pub version_root: PathBuf,
    pub current: bool,
    pub previous_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::SkillReceipt;

    #[test]
    fn legacy_receipts_belong_to_the_production_profile() {
        let receipt: SkillReceipt = serde_json::from_str(
            r#"{"skill_id":"com.example.skill","version":"1.0.0","client":"codex","source_root":"source","rendered_root":"target","rendered_at":"now","files":[],"checksums":{}}"#,
        )
        .unwrap();

        assert_eq!(receipt.agent_profile, "production");
        assert_eq!(receipt.render_mode, "copy");
        assert_eq!(receipt.target_kind, "global");
        assert_eq!(receipt.workspace_root, None);
        assert_eq!(receipt.workspace_id, None);
    }
}
