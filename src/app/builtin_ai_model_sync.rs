use crate::api::ai::{fetch_client_credential, AIClientCredential};
use crate::app::builtin_ai_proxy::BuiltinAiProxyControl;
use crate::Options;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const MODEL_DISPLAY_POLICY_VERSION: &str = "model-id-v1";

#[derive(Clone)]
pub(crate) struct ModelSyncSnapshot {
    pub user_id: String,
    pub default_model: String,
    pub base_url: String,
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

    pub(crate) fn sync_now(
        &self,
        options: &Options,
        proxy: &BuiltinAiProxyControl,
    ) -> Result<BuiltinAiModelSyncResult, String> {
        require_dashboard_model_sync(options)?;
        let previous = self
            .snapshot
            .lock()
            .map_err(|_| "HiMind AI 模型同步状态不可用".to_string())?
            .clone();
        let credential = fetch_client_credential(options, &previous.user_id, "himind-agent")
            .map_err(|error| error.to_string())?;
        let next = snapshot(&previous.user_id, &credential);
        if next.credential_fingerprint != previous.credential_fingerprint {
            // The API key and base URL are process-scoped. A fresh DSH
            // process is required when the managed route or credential changes.
            return Ok(BuiltinAiModelSyncResult {
                status: "restart_required".to_string(),
                model_count: next.models.len(),
                restarted: false,
                session_url: String::new(),
            });
        }
        let status = if next.catalog_fingerprint != previous.catalog_fingerprint {
            proxy.sync_model_catalog(&next.default_model, &next.base_url, &next.models)?;
            "updated"
        } else {
            "unchanged"
        };
        *self
            .snapshot
            .lock()
            .map_err(|_| "HiMind AI 模型同步状态不可用".to_string())? = next.clone();
        Ok(BuiltinAiModelSyncResult {
            status: status.to_string(),
            model_count: next.models.len(),
            restarted: false,
            session_url: String::new(),
        })
    }
}

fn require_dashboard_model_sync(options: &Options) -> Result<(), String> {
    if options.mode().dashboard_enabled() {
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
        default_model,
        base_url,
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
        options.effective_mode = AgentMode::Independent;
        let error = require_dashboard_model_sync(&options).unwrap_err();
        assert!(error.contains("control_plane_required"));
    }
}
