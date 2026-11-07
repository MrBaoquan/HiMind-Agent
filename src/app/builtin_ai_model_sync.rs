use crate::api::ai::{fetch_client_credential, AIClientCredential};
use crate::Options;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const MODEL_DISPLAY_POLICY_VERSION: &str = "model-id-v1";

#[derive(Clone)]
pub(crate) struct ModelSyncSnapshot {
    pub user_id: String,
    pub models: Vec<String>,
    pub credential_fingerprint: String,
    pub catalog_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAiModelSyncResult {
    pub status: String,
    pub model_count: usize,
    pub restarted: bool,
    pub session_url: String,
}

pub(crate) struct BuiltinAiModelSync {
    snapshot: Mutex<ModelSyncSnapshot>,
}

impl BuiltinAiModelSync {
    pub(crate) fn start(initial: ModelSyncSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(initial),
        }
    }

    /// 这次会话启动时拿到的模型目录。overlay 已经把目录固定下来，进程内改不了，
    /// 调用方用它判断目录是否已经和当前配置不一致、需要换进程。
    pub(crate) fn launched_models(&self) -> Vec<String> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.models.clone())
            .unwrap_or_default()
    }

    pub(crate) fn sync_now(&self, options: &Options) -> Result<BuiltinAiModelSyncResult, String> {
        let previous = self
            .snapshot
            .lock()
            .map_err(|_| "HiMind AI 模型同步状态不可用".to_string())?
            .clone();
        if previous.user_id == "local" {
            return Ok(BuiltinAiModelSyncResult {
                status: "unchanged".to_string(),
                model_count: previous.models.len(),
                restarted: false,
                session_url: String::new(),
            });
        }
        require_dashboard_model_sync(options)?;
        let credential = fetch_client_credential(options, &previous.user_id, "himind-agent")
            .map_err(|error| error.to_string())?;
        let next = snapshot(&previous.user_id, &credential);
        // 凭据与模型目录都写进了每次会话的 overlay：API key / base URL 是进程级
        // 的，目录变化又会被 DSH 判成「被 home patch 或命令行 overlay 覆盖」而拒绝
        // 就地写入。两者都只能按新的 overlay 重建会话，所以这里都不尝试写配置。
        if next.credential_fingerprint != previous.credential_fingerprint
            || next.catalog_fingerprint != previous.catalog_fingerprint
        {
            return Ok(BuiltinAiModelSyncResult {
                status: "restart_required".to_string(),
                model_count: next.models.len(),
                restarted: false,
                session_url: String::new(),
            });
        }
        Ok(BuiltinAiModelSyncResult {
            status: "unchanged".to_string(),
            model_count: next.models.len(),
            restarted: false,
            session_url: String::new(),
        })
    }
}

fn require_dashboard_model_sync(options: &Options) -> Result<(), String> {
    if crate::app::control_plane_provider::available(options) {
        Ok(())
    } else {
        Err(crate::app::runtime_mode::control_plane_required_error())
    }
}

pub(crate) fn snapshot(user_id: &str, credential: &AIClientCredential) -> ModelSyncSnapshot {
    let default_model = credential.access.model.trim().to_string();
    let base_url = credential.access.base_url.trim().to_string();
    let models = managed_models(&default_model, &credential.access.models);
    let catalog_fingerprint = fingerprint(&[
        MODEL_DISPLAY_POLICY_VERSION,
        &base_url,
        &default_model,
        &models.join("\n"),
        &credential.access.active_entitlement_id,
        &credential.access.active_personal_connection_id,
    ]);
    let credential_fingerprint = fingerprint(&[
        &base_url,
        &credential.api_key,
        &credential.access.active_entitlement_id,
        &credential.access.active_personal_connection_id,
    ]);
    ModelSyncSnapshot {
        user_id: user_id.to_string(),
        models,
        credential_fingerprint,
        catalog_fingerprint,
    }
}

fn managed_models(default_model: &str, models: &[String]) -> Vec<String> {
    let mut values = Vec::with_capacity(models.len() + 1);
    if !default_model.is_empty() {
        values.push(default_model.to_string());
    }
    values.extend(
        models
            .iter()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .map(str::to_string),
    );
    let mut seen = std::collections::HashSet::new();
    values.retain(|model| seen.insert(model.clone()));
    values
}

fn fingerprint(values: &[&str]) -> String {
    let mut digest = Sha256::new();
    for value in values {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::{managed_models, require_dashboard_model_sync};
    use crate::app::runtime_mode::AgentMode;
    use crate::Options;

    #[test]
    fn managed_models_keeps_default_first_and_removes_duplicates() {
        assert_eq!(
            managed_models(
                "primary",
                &["primary".into(), "fast".into(), " ".into(), "fast".into()]
            ),
            vec!["primary", "fast"]
        );
    }

    #[test]
    fn model_sync_rejects_independent_mode_before_remote_access() {
        let mut options = Options::from_env();
        options.set_mode(AgentMode::Independent);
        let error = require_dashboard_model_sync(&options).unwrap_err();
        assert!(error.contains("control_plane_required"));
    }
}
