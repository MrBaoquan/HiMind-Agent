use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::atomic_file;

const PREFERENCES_FILE: &str = "agent-preferences.json";

/// Controls whether the Agent participates in the optional Dashboard control
/// plane. This is deliberately separate from Dashboard enrollment state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentMode {
    Connected,
    Independent,
}

/// The Agent core talks to a control plane through an adapter. Dashboard is
/// the first adapter, but the core deliberately models the binding without
/// making every capability depend on the Dashboard product name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlPlaneKind {
    None,
    Dashboard,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Connected
    }
}

impl AgentMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "connected" => Some(Self::Connected),
            "independent" => Some(Self::Independent),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Independent => "independent",
        }
    }

    pub(crate) fn dashboard_enabled(self) -> bool {
        matches!(self, Self::Connected)
    }

    pub(crate) fn control_plane(self) -> ControlPlaneKind {
        if self.dashboard_enabled() {
            ControlPlaneKind::Dashboard
        } else {
            ControlPlaneKind::None
        }
    }

    pub(crate) fn control_plane_enabled(self) -> bool {
        !matches!(self.control_plane(), ControlPlaneKind::None)
    }
}

/// Stable machine-readable error returned when a control-plane-owned
/// operation is requested while the Agent is running individually.
pub(crate) fn control_plane_required_error() -> String {
    serde_json::json!({
        "code": "control_plane_required",
        "message": "当前运行模式不支持此功能；如需使用，请在设置中切换 Connected 模式并重启 Agent"
    })
    .to_string()
}

#[allow(dead_code)]
pub(crate) fn dashboard_required_error() -> String {
    control_plane_required_error()
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct AgentPreferences {
    #[serde(default)]
    mode: AgentMode,
}

pub(crate) fn path_for(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(PREFERENCES_FILE)
}

pub(crate) fn load(state_path: &Path) -> AgentMode {
    let path = path_for(state_path);
    let Ok(content) = fs::read_to_string(path) else {
        return AgentMode::default();
    };
    serde_json::from_str::<AgentPreferences>(&content)
        .map(|preferences| preferences.mode)
        .unwrap_or_default()
}

pub(crate) fn save(state_path: &Path, mode: AgentMode) -> Result<(), Box<dyn std::error::Error>> {
    let path = path_for(state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(&AgentPreferences { mode })?;
    atomic_file::atomic_write(&path, &content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, path_for, save, AgentMode};
    use std::fs;

    #[test]
    fn defaults_to_connected_and_round_trips_independent_mode() {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state_path = root.join("data/agent-state.json");
        assert_eq!(load(&state_path), AgentMode::Connected);
        save(&state_path, AgentMode::Independent).unwrap();
        assert_eq!(load(&state_path), AgentMode::Independent);
        assert!(path_for(&state_path).is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_mode_is_rejected() {
        assert_eq!(AgentMode::parse("connected"), Some(AgentMode::Connected));
        assert_eq!(
            AgentMode::parse("INDEPENDENT"),
            Some(AgentMode::Independent)
        );
        assert_eq!(AgentMode::parse("offline"), None);
    }
}
