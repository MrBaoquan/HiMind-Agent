//! 扩展开发的用户偏好。
//!
//! 这类偏好属于「这个 Agent 怎么工作」，跟着 agent 数据走（按 profile 隔离），
//! 而不是塞进 WebView 的 localStorage：换窗口、清缓存都不该丢。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;

const STORE_FILE: &str = "extension-dev-settings.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct ExtensionDevSettings {
    /// 构建成功后是否直接把候选包安装到本机（插件同时启用）。
    pub auto_install_after_build: bool,
}

impl Default for ExtensionDevSettings {
    fn default() -> Self {
        Self {
            auto_install_after_build: true,
        }
    }
}

fn store_path() -> std::path::PathBuf {
    crate::store::paths::agent_home().join(STORE_FILE)
}

pub(crate) fn load() -> ExtensionDevSettings {
    fs::read(store_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save(settings: &ExtensionDevSettings) -> Result<(), Box<dyn Error>> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)?;
    crate::store::atomic_file::atomic_write(&path, &bytes)?;
    Ok(())
}
