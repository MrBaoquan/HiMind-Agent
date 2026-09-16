use semver::Version;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::Path;

use super::validate_relative_asset_path;
use crate::capability::types::CapabilityAvailability;

pub(crate) const CONNECTOR_MANIFEST_SCHEMA_VERSION: &str = "connector_manifest.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowConnectorManifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub availability: String,
    pub credential_ownership: String,
    pub auth: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub supported_platforms: Vec<String>,
    #[serde(default)]
    pub health_check: serde_json::Value,
    #[serde(default)]
    pub credentials: Vec<WorkflowConnectorCredential>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowConnectorCredential {
    pub handle: String,
    pub target: String,
    pub kind: String,
    #[serde(default)]
    pub required: bool,
}

impl WorkflowConnectorManifest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONNECTOR_MANIFEST_SCHEMA_VERSION {
            return Err("connector manifest schema_version is invalid".to_string());
        }
        validate_identifier("connector id", &self.id)?;
        Version::parse(&self.version)
            .map_err(|error| format!("invalid connector version: {error}"))?;
        if self.name.trim().is_empty() {
            return Err("connector name is required".to_string());
        }
        if !matches!(
            self.availability.as_str(),
            "local" | "network_service" | "control_plane"
        ) {
            return Err(format!("connector {} availability is invalid", self.id));
        }
        if !matches!(
            self.credential_ownership.as_str(),
            "agent" | "dashboard" | "none"
        ) {
            return Err(format!(
                "connector {} credential_ownership is invalid",
                self.id
            ));
        }
        if self.auth.is_empty()
            || self.auth.iter().any(|auth| {
                !matches!(
                    auth.as_str(),
                    "none" | "oauth2" | "api_key" | "mcp" | "managed"
                )
            })
        {
            return Err(format!("connector {} auth is invalid", self.id));
        }
        let mut capability_ids = std::collections::HashSet::new();
        for capability in &self.capabilities {
            if capability.trim().is_empty() || !capability_ids.insert(capability.as_str()) {
                return Err(format!("connector {} capabilities are invalid", self.id));
            }
        }
        if !self.health_check.is_null() {
            let health = self
                .health_check
                .as_object()
                .ok_or_else(|| format!("connector {} health_check must be an object", self.id))?;
            if !health.is_empty() {
                let check_type = health
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                match check_type {
                    "capability" => {
                        let target = health
                            .get("target")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| {
                                format!("connector {} health_check target is required", self.id)
                            })?;
                        if !capability_ids.contains(target) {
                            return Err(format!(
                                "connector {} health_check target is not declared: {target}",
                                self.id
                            ));
                        }
                        if let Some(input) = health.get("input") {
                            if !input.is_object() {
                                return Err(format!(
                                    "connector {} health_check input must be an object",
                                    self.id
                                ));
                            }
                        }
                    }
                    "none" => {}
                    _ => {
                        return Err(format!(
                            "connector {} health_check type is invalid: {check_type}",
                            self.id
                        ))
                    }
                }
            }
        }
        let mut credential_handles = std::collections::HashSet::new();
        for credential in &self.credentials {
            validate_identifier("connector credential handle", &credential.handle)?;
            validate_identifier("connector credential target", &credential.target)?;
            if !matches!(credential.kind.as_str(), "file_path" | "secret") {
                return Err(format!(
                    "connector {} credential kind is invalid: {}",
                    self.id, credential.kind
                ));
            }
            if !credential_handles.insert(credential.handle.as_str()) {
                return Err(format!(
                    "connector {} contains duplicate credential handle: {}",
                    self.id, credential.handle
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn availability(&self) -> CapabilityAvailability {
        match self.availability.as_str() {
            "control_plane" => CapabilityAvailability::ControlPlane,
            "network_service" => CapabilityAvailability::NetworkService,
            _ => CapabilityAvailability::Local,
        }
    }
}

pub(crate) fn load_connector_manifests(
    root: &Path,
    connector_ids: &[String],
) -> Result<Vec<WorkflowConnectorManifest>, Box<dyn Error>> {
    let mut manifests = Vec::new();
    for connector_id in connector_ids {
        validate_identifier("connector id", connector_id).map_err(std::io::Error::other)?;
        let relative = format!("connectors/{connector_id}.json");
        validate_relative_asset_path(&relative).map_err(std::io::Error::other)?;
        let path = root.join(&relative);
        let manifest: WorkflowConnectorManifest =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                format!(
                    "workflow connector manifest is unavailable: {}: {error}",
                    path.display()
                )
            })?)?;
        manifest.validate().map_err(std::io::Error::other)?;
        if manifest.id != *connector_id {
            return Err(format!(
                "workflow connector manifest id mismatch: expected {connector_id}, got {}",
                manifest.id
            )
            .into());
        }
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn validate_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid {name}: {value}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_connector_credentials() {
        let manifest = WorkflowConnectorManifest {
            schema_version: CONNECTOR_MANIFEST_SCHEMA_VERSION.to_string(),
            id: "wechat-miniprogram".to_string(),
            version: "1.0.0".to_string(),
            name: "WeChat".to_string(),
            description: String::new(),
            availability: "local".to_string(),
            credential_ownership: "agent".to_string(),
            auth: vec!["api_key".to_string()],
            capabilities: vec!["wechat.miniprogram.preview".to_string()],
            scopes: Vec::new(),
            supported_platforms: vec!["windows".to_string()],
            health_check: serde_json::json!({}),
            credentials: vec![WorkflowConnectorCredential {
                handle: "wechat-upload-key".to_string(),
                target: "private_key_path".to_string(),
                kind: "file_path".to_string(),
                required: true,
            }],
        };
        manifest.validate().unwrap();

        let mut invalid = manifest.clone();
        invalid.health_check = serde_json::json!({
            "type": "capability",
            "target": "wechat.miniprogram.upload"
        });
        assert!(invalid
            .validate()
            .unwrap_err()
            .contains("health_check target is not declared"));
    }
}
