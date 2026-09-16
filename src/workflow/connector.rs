use semver::Version;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::Path;
use std::time::Duration;

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowHttpHealthCheck {
    pub url: String,
    pub method: String,
    pub expected_status: Vec<u16>,
    pub timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowHttpHealthCheckManifest {
    #[serde(rename = "type")]
    check_type: String,
    url: String,
    #[serde(default = "default_http_health_method")]
    method: String,
    #[serde(default = "default_http_health_status")]
    expected_status: Vec<u16>,
    #[serde(default = "default_http_health_timeout")]
    timeout_seconds: u64,
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
                    "http" => {
                        parse_http_health_check(&self.health_check)?;
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

impl WorkflowHttpHealthCheck {
    pub(crate) fn from_manifest(value: &serde_json::Value) -> Result<Self, String> {
        parse_http_health_check(value)
    }
}

pub(crate) fn execute_http_health_check(
    check: &WorkflowHttpHealthCheck,
) -> Result<u16, Box<dyn Error>> {
    validate_http_health_url(&check.url)?;
    let method = match check.method.as_str() {
        "GET" => reqwest::Method::GET,
        "HEAD" => reqwest::Method::HEAD,
        _ => return Err("HTTP health check method is invalid".into()),
    };
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(check.timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("HiMind-Agent-Health");
    if url::Url::parse(&check.url)
        .ok()
        .and_then(|url| url.host_str().map(is_loopback_host))
        .unwrap_or(false)
    {
        builder = builder.no_proxy();
    }
    let client = builder.build()?;
    let status = client.request(method, &check.url).send()?.status().as_u16();
    if !check.expected_status.contains(&status) {
        return Err(format!("HTTP health check returned unexpected status: {status}").into());
    }
    Ok(status)
}

fn parse_http_health_check(value: &serde_json::Value) -> Result<WorkflowHttpHealthCheck, String> {
    let manifest: WorkflowHttpHealthCheckManifest =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if manifest.check_type != "http" {
        return Err("HTTP health check type is invalid".to_string());
    }
    let method = manifest.method.trim().to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "HEAD") {
        return Err("HTTP health check method must be GET or HEAD".to_string());
    }
    if manifest.expected_status.is_empty()
        || manifest
            .expected_status
            .iter()
            .any(|status| !(100..=599).contains(status))
    {
        return Err("HTTP health check expected_status is invalid".to_string());
    }
    let mut statuses = std::collections::HashSet::new();
    if manifest
        .expected_status
        .iter()
        .any(|status| !statuses.insert(*status))
    {
        return Err("HTTP health check expected_status contains duplicates".to_string());
    }
    if !(1..=30).contains(&manifest.timeout_seconds) {
        return Err("HTTP health check timeout_seconds must be between 1 and 30".to_string());
    }
    validate_http_health_url(&manifest.url)?;
    Ok(WorkflowHttpHealthCheck {
        url: manifest.url,
        method,
        expected_status: manifest.expected_status,
        timeout_seconds: manifest.timeout_seconds,
    })
}

fn validate_http_health_url(value: &str) -> Result<(), String> {
    let url = url::Url::parse(value).map_err(|error| error.to_string())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("HTTP health check URL must not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "HTTP health check URL host is required".to_string())?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        "http" => Err("HTTP health check URL must use HTTPS unless it is loopback".to_string()),
        _ => Err("HTTP health check URL scheme is invalid".to_string()),
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

fn default_http_health_method() -> String {
    "GET".to_string()
}

fn default_http_health_status() -> Vec<u16> {
    vec![200]
}

fn default_http_health_timeout() -> u64 {
    5
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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

    #[test]
    fn validates_and_executes_restricted_http_health_checks() {
        let manifest = WorkflowConnectorManifest {
            schema_version: CONNECTOR_MANIFEST_SCHEMA_VERSION.to_string(),
            id: "network-connector".to_string(),
            version: "1.0.0".to_string(),
            name: "Network Connector".to_string(),
            description: String::new(),
            availability: "network_service".to_string(),
            credential_ownership: "agent".to_string(),
            auth: vec!["none".to_string()],
            capabilities: Vec::new(),
            scopes: Vec::new(),
            supported_platforms: Vec::new(),
            health_check: serde_json::json!({
                "type": "http",
                "url": "https://example.com/health",
                "method": "HEAD",
                "expected_status": [200, 204],
                "timeout_seconds": 3
            }),
            credentials: Vec::new(),
        };
        manifest.validate().unwrap();

        let mut insecure = manifest.clone();
        insecure.health_check["url"] = serde_json::json!("http://example.com/health");
        assert!(insecure.validate().unwrap_err().contains("must use HTTPS"));

        let mut invalid_method = manifest;
        invalid_method.health_check["method"] = serde_json::json!("POST");
        assert!(invalid_method
            .validate()
            .unwrap_err()
            .contains("must be GET or HEAD"));
    }

    #[test]
    fn executes_http_health_check_against_real_local_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let check = WorkflowHttpHealthCheck {
            url: format!("http://{address}/health"),
            method: "GET".to_string(),
            expected_status: vec![204],
            timeout_seconds: 3,
        };
        assert_eq!(execute_http_health_check(&check).unwrap(), 204);
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let check = WorkflowHttpHealthCheck {
            url: format!("http://{address}/health"),
            method: "GET".to_string(),
            expected_status: vec![200],
            timeout_seconds: 3,
        };
        assert!(execute_http_health_check(&check).is_err());
        server.join().unwrap();
    }
}
