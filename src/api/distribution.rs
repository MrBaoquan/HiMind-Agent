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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillCatalogItem {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    pub author_name: String,
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

pub fn submit_skill(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
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
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
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
    credential: &str,
) -> Result<Vec<SkillSubmissionStatus>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/skills/submissions"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<SkillSubmissionResponse>()?
        .items)
}

pub fn submit_plugin(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
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
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
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
    credential: &str,
) -> Result<Vec<PluginSubmissionStatus>, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/plugins/submissions"))
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<PluginSubmissionResponse>()?
        .items)
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
    use super::distribution_state_path;
    use std::path::Path;

    #[test]
    fn distribution_state_uses_a_separate_file() {
        assert_eq!(
            distribution_state_path(Path::new("agent-state.json")),
            Path::new("agent-state.distribution.json")
        );
    }
}
