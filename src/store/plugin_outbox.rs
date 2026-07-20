use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PLUGIN_STATUS_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PluginStatusRecord {
    pub agent_id: String,
    pub plugin_id: String,
    pub action: String,
    pub from_version: String,
    pub current_version: String,
    pub previous_version: String,
    pub enabled: bool,
    pub status: String,
    pub error: String,
}

fn outbox_dir(state_path: &Path) -> PathBuf {
    state_path.with_file_name("plugin-status-outbox")
}

pub(crate) fn store(state_path: &Path, record: &PluginStatusRecord) -> io::Result<PathBuf> {
    let dir = outbox_dir(state_path);
    fs::create_dir_all(&dir)?;
    let name = format!(
        "{}-{}-{}.json",
        safe_name(&record.plugin_id),
        safe_name(&record.action),
        timestamp()
    );
    let path = dir.join(name);
    let temp = dir.join(format!(".{}.tmp", timestamp()));
    let data = serde_json::to_vec(record).map_err(io::Error::other)?;
    if data.len() > MAX_PLUGIN_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "plugin status is too large",
        ));
    }
    fs::write(&temp, data)?;
    fs::rename(&temp, &path)?;
    Ok(path)
}

pub(crate) fn list(state_path: &Path) -> io::Result<Vec<(PathBuf, PluginStatusRecord)>> {
    let dir = outbox_dir(state_path);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let data = match fs::read(&path) {
            Ok(value) if value.len() <= MAX_PLUGIN_STATUS_BYTES => value,
            _ => continue,
        };
        if let Ok(record) = serde_json::from_slice::<PluginStatusRecord>(&data) {
            records.push((path, record));
        }
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(records)
}

pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_removes_plugin_status_atomically() {
        let root = std::env::temp_dir().join(format!("himind-plugin-outbox-{}", timestamp()));
        let state_path = root.join("agent-state.json");
        let record = PluginStatusRecord {
            agent_id: "agent-1".into(),
            plugin_id: "com.himind.demo".into(),
            action: "upgrade".into(),
            from_version: "1.0.0".into(),
            current_version: "2.0.0".into(),
            previous_version: "1.0.0".into(),
            enabled: true,
            status: "installed".into(),
            error: String::new(),
        };
        let path = store(&state_path, &record).unwrap();
        let listed = list(&state_path).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.current_version, "2.0.0");
        remove(&path).unwrap();
        assert!(list(&state_path).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
