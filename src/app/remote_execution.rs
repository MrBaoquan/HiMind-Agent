use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const ACCESS_MODE_EXHIBIT_LINKED: &str = "exhibit_linked";
pub(crate) const ACCESS_MODE_FULL_ACCESS: &str = "full_access";
pub(crate) const PROVIDER_AUTO: &str = "auto";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RemoteExecutionSettings {
    pub enabled: bool,
    pub access_mode: String,
    pub default_provider: String,
}

impl Default for RemoteExecutionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            access_mode: ACCESS_MODE_EXHIBIT_LINKED.to_string(),
            default_provider: PROVIDER_AUTO.to_string(),
        }
    }
}

impl RemoteExecutionSettings {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if !matches!(
            self.access_mode.as_str(),
            ACCESS_MODE_EXHIBIT_LINKED | ACCESS_MODE_FULL_ACCESS
        ) {
            return Err("访问模式必须是仅展项关联目录或完全访问此电脑".into());
        }
        if !matches!(
            self.default_provider.as_str(),
            PROVIDER_AUTO
                | crate::runtime::PROVIDER_CODEX
                | crate::runtime::PROVIDER_GITHUB_COPILOT
                | crate::runtime::PROVIDER_OPENHANDS
        ) {
            return Err("默认执行器必须是自动、Codex、GitHub Copilot 或 OpenHands".into());
        }
        Ok(())
    }
}

pub(crate) fn settings_path(agent_state_path: &Path) -> PathBuf {
    agent_state_path.with_file_name("agent-remote-execution.json")
}

pub(crate) fn load(agent_state_path: &Path) -> Result<RemoteExecutionSettings, Box<dyn Error>> {
    let path = settings_path(agent_state_path);
    if !path.exists() {
        return Ok(RemoteExecutionSettings::default());
    }
    let settings = serde_json::from_str::<RemoteExecutionSettings>(&fs::read_to_string(path)?)?;
    settings.validate()?;
    Ok(settings)
}

pub(crate) fn save(
    agent_state_path: &Path,
    settings: &RemoteExecutionSettings,
) -> Result<(), Box<dyn Error>> {
    settings.validate()?;
    let path = settings_path(agent_state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_string_pretty(settings)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, save, settings_path, RemoteExecutionSettings, ACCESS_MODE_FULL_ACCESS};
    use std::fs;

    #[test]
    fn defaults_are_closed_and_exhibit_scoped() {
        let root = std::env::temp_dir().join(format!(
            "himind-remote-execution-defaults-{}",
            std::process::id()
        ));
        let state_path = root.join("agent-state.json");
        let settings = load(&state_path).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.access_mode, "exhibit_linked");
        assert_eq!(settings.default_provider, "auto");
    }

    #[test]
    fn settings_round_trip_next_to_agent_state() {
        let root = std::env::temp_dir().join(format!(
            "himind-remote-execution-roundtrip-{}",
            std::process::id()
        ));
        let state_path = root.join("agent-state.json");
        let settings = RemoteExecutionSettings {
            enabled: true,
            access_mode: ACCESS_MODE_FULL_ACCESS.to_string(),
            default_provider: crate::runtime::PROVIDER_CODEX.to_string(),
        };
        save(&state_path, &settings).unwrap();
        assert_eq!(load(&state_path).unwrap(), settings);
        assert!(settings_path(&state_path).is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
