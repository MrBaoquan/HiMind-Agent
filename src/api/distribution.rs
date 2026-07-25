use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionState {
    pub client_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoftwareReleasePublishRequest {
    pub workspace_root: String,
    pub artifact_path: String,
    pub product_id: String,
    pub product_name: String,
    pub version: String,
    #[serde(default = "default_stable_channel")]
    pub channel: String,
    pub platform: String,
    pub architecture: String,
    pub package_type: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default = "default_full_rollout")]
    pub rollout_percent: i64,
    pub confirmed: bool,
}

fn default_stable_channel() -> String {
    "stable".to_string()
}

fn default_full_rollout() -> i64 {
    100
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    id: String,
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCheckResponse {
    pub has_update: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub download_url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_key_id: String,
    #[serde(default)]
    pub signature_algorithm: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginCatalogItem {
    pub plugin_id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub review_status: String,
    pub governance: String,
    pub version: String,
    pub release_notes: String,
    pub min_agent_version: String,
    pub channel: String,
    pub artifact_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub sha256: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_key_id: String,
    #[serde(default)]
    pub signature_algorithm: String,
    pub download_url: String,
    #[serde(default = "default_marketplace_source")]
    pub source: String,
    #[serde(default = "default_optional_assignment")]
    pub assignment: String,
    #[serde(default = "default_user_management")]
    pub management: String,
    #[serde(default = "default_prompt_mode")]
    pub install_mode: String,
    #[serde(default)]
    pub organization_reason: String,
    #[serde(default)]
    pub managed: bool,
    #[serde(default = "default_true")]
    pub allow_disable: bool,
    #[serde(default = "default_true")]
    pub allow_uninstall: bool,
    #[serde(default)]
    pub capability_ids: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub view_count: usize,
    #[serde(default)]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillCatalogItem {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    pub author_name: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    pub release_notes: String,
    pub min_agent_version: String,
    pub supported_clients: Vec<String>,
    pub capability_ids: Vec<String>,
    #[serde(default)]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
    pub risk_summary: String,
    pub channel: String,
    pub artifact_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub sha256: String,
    pub signature: String,
    pub signature_key_id: String,
    pub signature_algorithm: String,
    pub download_url: String,
    #[serde(default = "default_marketplace_source")]
    pub source: String,
    #[serde(default = "default_optional_assignment")]
    pub assignment: String,
    #[serde(default = "default_user_management")]
    pub management: String,
    #[serde(default = "default_prompt_mode")]
    pub install_mode: String,
    #[serde(default)]
    pub organization_reason: String,
    #[serde(default)]
    pub managed: bool,
    #[serde(default = "default_true")]
    pub allow_disable: bool,
    #[serde(default = "default_true")]
    pub allow_uninstall: bool,
}

fn default_marketplace_source() -> String {
    "marketplace".to_string()
}
fn default_optional_assignment() -> String {
    "optional".to_string()
}
fn default_user_management() -> String {
    "user_managed".to_string()
}
fn default_prompt_mode() -> String {
    "prompt".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionDesiredState {
    pub generation: String,
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_seconds: u64,
    #[serde(default)]
    pub items: Vec<ExtensionDesiredItem>,
}

fn default_reconcile_interval() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionDesiredItem {
    pub product_id: String,
    pub asset_key: String,
    pub asset_kind: String,
    pub name: String,
    pub desired_state: String,
    #[serde(default)]
    pub desired_version: String,
    #[serde(default = "default_true")]
    pub desired_enabled: bool,
    pub intent: String,
    pub management: String,
    pub install_mode: String,
    #[serde(default)]
    pub assignment_id: String,
    pub source: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default = "default_true")]
    pub allow_disable: bool,
    #[serde(default = "default_true")]
    pub allow_uninstall: bool,
    #[serde(default)]
    pub on_scope_exit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReconcileReport {
    pub generation: String,
    pub items: Vec<ExtensionReconcileItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReconcileItem {
    pub asset_key: String,
    pub asset_kind: String,
    pub desired_version: String,
    pub installed_version: String,
    pub enabled: bool,
    pub status: String,
    pub phase: String,
    pub install_source: String,
    pub assignment_id: String,
    pub target_clients: serde_json::Value,
    pub error: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillPluginDependency {
    pub plugin_id: String,
    pub required: bool,
    #[serde(default)]
    pub min_version: String,
}

#[derive(Debug, Serialize)]
pub struct PluginStatusReport<'a> {
    pub plugin_id: &'a str,
    pub action: &'a str,
    pub from_version: &'a str,
    pub current_version: &'a str,
    pub previous_version: &'a str,
    pub enabled: bool,
    pub status: &'a str,
    pub error: &'a str,
}

#[derive(Debug, Deserialize)]
struct PluginCatalogResponse {
    items: Vec<PluginCatalogItem>,
}

#[derive(Debug, Deserialize)]
struct SkillCatalogResponse {
    items: Vec<SkillCatalogItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillSubmissionStatus {
    pub id: String,
    pub product_key: String,
    pub version: String,
    pub status: String,
    #[serde(default)]
    pub review_note: String,
    #[serde(default)]
    pub artifact_id: String,
    #[serde(default)]
    pub release_id: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginSubmissionStatus {
    pub id: String,
    pub product_key: String,
    pub name: String,
    pub version: String,
    pub status: String,
    pub review_status: String,
    #[serde(default)]
    pub review_note: String,
    pub artifact_id: String,
    pub release_id: String,
    pub sha256: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
struct SkillSubmissionResponse {
    items: Vec<SkillSubmissionStatus>,
}

#[derive(Debug, Deserialize)]
struct PluginSubmissionResponse {
    items: Vec<PluginSubmissionStatus>,
}

pub fn plugin_catalog(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
) -> Result<Vec<PluginCatalogItem>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/plugins/catalog"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<PluginCatalogResponse>()?
        .items)
}

pub fn skill_catalog(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
) -> Result<Vec<SkillCatalogItem>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/skills/catalog"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<SkillCatalogResponse>()?
        .items)
}

pub fn extension_desired_state(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
) -> Result<ExtensionDesiredState, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/extensions/desired-state"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<ExtensionDesiredState>()?)
}

pub fn report_extension_reconcile(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    report: &ExtensionReconcileReport,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!("{api_base}/api/agent/extensions/reconcile-result"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .json(report)
        .send()?
        .error_for_status()?;
    Ok(())
}

pub fn submit_skill(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    package_path: &Path,
    test_report: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let file_name = package_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("skill.hmskill")
        .to_string();
    let package = Part::bytes(fs::read(package_path)?)
        .file_name(file_name)
        .mime_str("application/vnd.himind.skill+zip")?;
    Ok(client
        .post(format!("{api_base}/api/agent/skills/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .multipart(
            Form::new()
                .part("file", package)
                .text("test_report", serde_json::to_string(test_report)?),
        )
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?)
}

pub fn skill_submissions(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
) -> Result<Vec<SkillSubmissionStatus>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/skills/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .send()?
        .error_for_status()?
        .json::<SkillSubmissionResponse>()?
        .items)
}

pub fn submit_plugin(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    package_path: &Path,
    test_report: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let file_name = package_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("plugin.hmpkg")
        .to_string();
    let package = Part::bytes(fs::read(package_path)?)
        .file_name(file_name)
        .mime_str("application/vnd.himind.plugin+zip")?;
    Ok(client
        .post(format!("{api_base}/api/agent/plugins/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .multipart(
            Form::new()
                .part("file", package)
                .text("test_report", serde_json::to_string(test_report)?),
        )
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?)
}

pub fn plugin_submissions(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
) -> Result<Vec<PluginSubmissionStatus>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/plugins/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .send()?
        .error_for_status()?
        .json::<PluginSubmissionResponse>()?
        .items)
}

pub fn publish_software_release(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    request: &SoftwareReleasePublishRequest,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let products = client
        .get(format!("{api_base}/api/distribution/products"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .query(&[
            ("product_key", request.product_id.as_str()),
            ("page_size", "200"),
        ])
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?;
    let product_exists = products["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .any(|item| item["product_key"] == request.product_id)
        })
        .unwrap_or(false);
    if !product_exists {
        client
            .post(format!("{api_base}/api/distribution/products"))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({
                "product_key": request.product_id,
                "name": request.product_name,
                "description": "由 HiMind 软件分发能力创建",
                "default_channel": request.channel,
                "update_mode": "self_update",
                "product_type": "desktop_app",
                "active": true
            }))
            .send()?
            .error_for_status()?;
    }

    let release_workspace = client
        .get(format!("{api_base}/api/distribution/releases"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .query(&[
            ("product_key", request.product_id.as_str()),
            ("page_size", "200"),
        ])
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?;
    let channel_id = release_workspace["channels"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["channel_key"] == request.channel)
                .and_then(|item| item["id"].as_str())
        })
        .ok_or_else(|| format!("Dashboard channel not found: {}", request.channel))?
        .to_string();

    let file_name = Path::new(&request.artifact_path)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("artifact file name is invalid")?
        .to_string();
    let artifact_part = Part::file(&request.artifact_path)?.file_name(file_name);
    let artifact = client
        .post(format!("{api_base}/api/distribution/artifacts"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .query(&[("product_key", request.product_id.as_str())])
        .multipart(
            Form::new()
                .text("version", request.version.clone())
                .text("platform", request.platform.clone())
                .text("architecture", request.architecture.clone())
                .text("package_type", request.package_type.clone())
                .part("file", artifact_part),
        )
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?;
    let artifact_id = artifact["id"]
        .as_str()
        .ok_or("Dashboard artifact response is missing id")?;

    let release = client
        .post(format!("{api_base}/api/distribution/releases"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .query(&[("product_key", request.product_id.as_str())])
        .json(&json!({
            "channel_id": channel_id,
            "artifact_id": artifact_id,
            "version": request.version,
            "release_notes": request.release_notes,
            "mandatory": request.mandatory,
            "rollout_percent": request.rollout_percent
        }))
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?;
    let release_id = release["id"]
        .as_str()
        .ok_or("Dashboard release response is missing id")?;

    let published = client
        .post(format!(
            "{api_base}/api/distribution/releases/{release_id}/publish"
        ))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .send()?
        .error_for_status()?
        .json::<serde_json::Value>()?;
    Ok(json!({
        "product_id": request.product_id,
        "artifact": artifact,
        "release": published,
        "status": "published"
    }))
}

fn ai_client_id() -> String {
    std::env::var("HIMIND_AI_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "himind-agent".to_string())
}

pub fn report_plugin_status(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    report: &PluginStatusReport<'_>,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!("{api_base}/api/agent/plugins/status"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .json(report)
        .send()?
        .error_for_status()?;
    Ok(())
}

pub fn distribution_state_path(agent_state_path: &Path) -> PathBuf {
    agent_state_path.with_file_name("agent-state.distribution.json")
}

pub fn load_or_register(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    product_key: &str,
    client_key: &str,
    version: &str,
    channel: &str,
) -> Result<Option<DistributionState>, Box<dyn Error>> {
    let enrollment_token = env::var("HIMIND_DISTRIBUTION_ENROLLMENT_TOKEN").unwrap_or_default();
    if state_path.exists() {
        let state = serde_json::from_str::<DistributionState>(&fs::read_to_string(state_path)?)?;
        if !state.client_id.trim().is_empty()
            && !state.token.trim().is_empty()
            && heartbeat(client, api_base, &state, version, channel).is_ok()
        {
            return Ok(Some(state));
        }
    }
    if enrollment_token.trim().is_empty() {
        return Ok(None);
    }
    let state = register(
        client,
        api_base,
        state_path,
        product_key,
        client_key,
        version,
        channel,
        &enrollment_token,
    )?;
    Ok(Some(state))
}

fn register(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    product_key: &str,
    client_key: &str,
    version: &str,
    channel: &str,
    enrollment_token: &str,
) -> Result<DistributionState, Box<dyn Error>> {
    let name = env::var("COMPUTERNAME").unwrap_or_else(|_| "windows-agent".to_string());
    let response = client
        .post(format!("{api_base}/api/distribution/client/register"))
        .json(&json!({
            "product_key": product_key,
            "client_key": client_key,
            "machine_name": name,
            "current_version": version,
            "channel": channel,
            "enrollment_token": enrollment_token,
        }))
        .send()?
        .error_for_status()?
        .json::<RegistrationResponse>()?;
    let state = DistributionState {
        client_id: response.id,
        token: response.token,
    };
    fs::write(state_path, serde_json::to_string_pretty(&state)?)?;
    Ok(state)
}

pub fn heartbeat(
    client: &Client,
    api_base: &str,
    state: &DistributionState,
    version: &str,
    channel: &str,
) -> Result<(), Box<dyn Error>> {
    let response = client
        .post(format!("{api_base}/api/distribution/client/heartbeat"))
        .bearer_auth(&state.token)
        .json(&json!({
            "current_version": version,
            "channel": channel,
            "machine_name": env::var("COMPUTERNAME").unwrap_or_default(),
        }))
        .send()?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err("Distribution client token is no longer valid".into());
    }
    response.error_for_status()?.json::<serde_json::Value>()?;
    Ok(())
}

pub fn check_update(
    client: &Client,
    api_base: &str,
    state: &DistributionState,
) -> Result<UpdateCheckResponse, Box<dyn Error>> {
    Ok(client
        .post(format!("{api_base}/api/distribution/client/check-update"))
        .bearer_auth(&state.token)
        .json(&json!({}))
        .send()?
        .error_for_status()?
        .json::<UpdateCheckResponse>()?)
}

pub fn report_update_result(
    client: &Client,
    api_base: &str,
    state: &DistributionState,
    report_type: &str,
    from_version: &str,
    to_version: &str,
    detail: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .post(format!("{api_base}/api/distribution/client/update-result"))
        .bearer_auth(&state.token)
        .json(&json!({
            "report_type": report_type,
            "from_version": from_version,
            "to_version": to_version,
            "detail": detail,
        }))
        .send()?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{distribution_state_path, publish_software_release, SoftwareReleasePublishRequest};
    use reqwest::blocking::Client;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn distribution_state_uses_a_separate_file() {
        assert_eq!(
            distribution_state_path(Path::new("agent-state.json")),
            Path::new("agent-state.distribution.json")
        );
    }

    #[test]
    fn software_release_publish_uses_brokered_dashboard_sequence() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = Arc::clone(&requests);
        let responses = [
            r#"{"items":[]}"#,
            r#"{"id":"product-mediaresolver"}"#,
            r#"{"channels":[{"id":"channel-stable","channel_key":"stable"}]}"#,
            r#"{"id":"artifact-mediaresolver"}"#,
            r#"{"id":"release-mediaresolver"}"#,
            r#"{"id":"release-mediaresolver","status":"published"}"#,
        ];
        let server = thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..count]);
                    if let Some(header_end) = buffer.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let header_text = String::from_utf8_lossy(&buffer[..header_end + 4]);
                        let content_length = header_text
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if buffer.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buffer).to_string());
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "himind-release-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let artifact = root.join("MediaResolver.zip");
        fs::write(&artifact, b"media-resolver-package").unwrap();
        let request = SoftwareReleasePublishRequest {
            workspace_root: root.to_string_lossy().to_string(),
            artifact_path: artifact.to_string_lossy().to_string(),
            product_id: "com.himind.media-resolver".to_string(),
            product_name: "MediaResolver".to_string(),
            version: "1.0.0".to_string(),
            channel: "stable".to_string(),
            platform: "windows".to_string(),
            architecture: "x64".to_string(),
            package_type: "directory-zip".to_string(),
            release_notes: "Broker test".to_string(),
            mandatory: false,
            rollout_percent: 100,
            confirmed: true,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let result = publish_software_release(
            &client,
            &format!("http://{address}"),
            "agent-test",
            "secret-access-token",
            &request,
        )
        .unwrap();
        server.join().unwrap();
        let _ = fs::remove_file(&artifact);
        let _ = fs::remove_dir(&root);
        assert_eq!(result["status"], "published");
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 6);
        let request_lines = captured
            .iter()
            .map(|request| request.lines().next().unwrap_or_default())
            .collect::<Vec<_>>();
        assert!(request_lines[0].starts_with("GET /api/distribution/products"));
        assert!(request_lines[1].starts_with("POST /api/distribution/products"));
        assert!(request_lines[2].starts_with("GET /api/distribution/releases"));
        assert!(request_lines[3].starts_with("POST /api/distribution/artifacts"));
        assert!(request_lines[4].starts_with("POST /api/distribution/releases"));
        assert!(
            request_lines[5].contains("/api/distribution/releases/release-mediaresolver/publish")
        );
        assert!(captured.iter().all(|request| request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-access-token")));
    }
}
