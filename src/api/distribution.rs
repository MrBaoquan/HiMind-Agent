use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, Response};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionState {
    pub client_id: String,
    #[serde(default, skip_serializing)]
    pub token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_protected: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoftwareReleasePublishRequest {
    pub workspace_root: String,
    pub artifact_path: String,
    pub product_id: String,
    pub product_name: String,
    #[serde(default = "default_desktop_product_type")]
    pub product_type: String,
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
    pub inspection_receipt: String,
    pub expected_size: u64,
    pub expected_sha256: String,
    pub confirmed: bool,
}

fn default_stable_channel() -> String {
    "stable".to_string()
}

fn default_desktop_product_type() -> String {
    "desktop_app".to_string()
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
    pub release_id: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub package_type: String,
    #[serde(default)]
    pub size_bytes: i64,
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
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub min_supported_version: String,
    #[serde(default)]
    pub release_notes: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeComponentUpdate {
    #[serde(rename = "productId")]
    pub product_id: String,
    pub version: String,
    #[serde(rename = "releaseName", default)]
    pub release_name: String,
    #[serde(rename = "releaseNotes", default)]
    pub release_notes: String,
    pub channel: String,
    #[serde(rename = "artifactUrl")]
    pub artifact_url: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "packageType")]
    pub package_type: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(rename = "publishedAt", default)]
    pub published_at: String,
    #[serde(default)]
    pub signature: String,
    #[serde(rename = "signatureKeyId", default)]
    pub signature_key_id: String,
    #[serde(rename = "signatureAlgorithm", default)]
    pub signature_algorithm: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeComponentResolveResponse {
    pub update: Option<RuntimeComponentUpdate>,
}

pub fn resolve_runtime_component(
    client: &Client,
    api_base: &str,
    product_id: &str,
    current_version: &str,
    channel: &str,
    platform: &str,
    architecture: &str,
    client_instance_id: &str,
) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>> {
    let response = client
        .post(format!(
            "{api_base}/api/software-distribution/v1/runtime/resolve"
        ))
        .json(&json!({
            "productId": product_id,
            "currentVersion": current_version,
            "channel": channel,
            "platform": platform,
            "architecture": architecture,
            "clientInstanceId": client_instance_id,
        }))
        .send()?
        .error_for_status()?
        .json::<RuntimeComponentResolveResponse>()?;
    Ok(response.update)
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
    #[serde(default)]
    pub published_at: String,
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
    #[serde(default)]
    pub published_at: String,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogPage<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

pub type PluginCatalogPage = CatalogPage<PluginCatalogItem>;
pub type SkillCatalogPage = CatalogPage<SkillCatalogItem>;

#[derive(Debug, Deserialize)]
struct SkillCatalogResponse {
    items: Vec<SkillCatalogItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillSubmissionStatus {
    pub id: String,
    pub product_key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author_name: String,
    pub version: String,
    pub status: String,
    #[serde(default)]
    pub review_note: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default)]
    pub artifact_id: String,
    #[serde(default)]
    pub release_id: String,
    #[serde(default)]
    pub release_status: String,
    #[serde(default)]
    pub parent_release_id: String,
    #[serde(default)]
    pub revision_of_version: String,
    #[serde(default)]
    pub source_type: String,
    #[serde(default)]
    pub source_repository: String,
    #[serde(default)]
    pub source_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub role: String,
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
    #[serde(default)]
    pub release_notes: String,
    pub artifact_id: String,
    pub release_id: String,
    #[serde(default)]
    pub release_status: String,
    #[serde(default)]
    pub parent_release_id: String,
    #[serde(default)]
    pub revision_of_version: String,
    #[serde(default)]
    pub source_type: String,
    #[serde(default)]
    pub source_repository: String,
    #[serde(default)]
    pub source_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub role: String,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionCollaborationMember {
    pub id: String,
    pub product_id: String,
    pub product_key: String,
    pub user_id: String,
    pub user_name: String,
    pub role: String,
    pub status: String,
    #[serde(default)]
    pub granted_by: String,
    #[serde(default)]
    pub responded_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionCollaboration {
    pub registered: bool,
    pub product_key: String,
    #[serde(default)]
    pub product_name: String,
    #[serde(default)]
    pub product_type: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub can_manage: bool,
    #[serde(default)]
    pub can_submit: bool,
    #[serde(default)]
    pub source_repository: String,
    #[serde(default)]
    pub source_default_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    #[serde(default)]
    pub members: Vec<ExtensionCollaborationMember>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentExtensionProject {
    pub product_key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub product_type: String,
    pub role: String,
    #[serde(default)]
    pub can_manage: bool,
    #[serde(default)]
    pub can_submit: bool,
    #[serde(default)]
    pub source_repository: String,
    #[serde(default)]
    pub source_default_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionCollaboratorOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub department_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionCollaborationInvitation {
    pub id: String,
    pub product_id: String,
    pub product_key: String,
    pub product_name: String,
    pub product_type: String,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub invited_by: String,
    #[serde(default)]
    pub invited_by_name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
struct ExtensionCollaboratorOptionsResponse {
    items: Vec<ExtensionCollaboratorOption>,
}

#[derive(Debug, Deserialize)]
struct ExtensionCollaborationInvitationsResponse {
    items: Vec<ExtensionCollaborationInvitation>,
}

#[derive(Debug, Deserialize)]
struct AgentExtensionProjectsResponse {
    items: Vec<AgentExtensionProject>,
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

pub fn plugin_catalog_page(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    q: &str,
    category: &str,
    page: usize,
    page_size: usize,
) -> Result<PluginCatalogPage, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/plugins/catalog"))
        .query(&[
            ("q", q.to_string()),
            ("category", category.to_string()),
            ("page", page.to_string()),
            ("page_size", page_size.to_string()),
        ])
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<PluginCatalogPage>()?)
}

pub fn plugin_versions(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    plugin_id: &str,
) -> Result<Vec<PluginCatalogItem>, Box<dyn Error>> {
    let plugin_id = url::form_urlencoded::byte_serialize(plugin_id.as_bytes()).collect::<String>();
    Ok(client
        .get(format!(
            "{api_base}/api/agent/plugins/catalog/{plugin_id}/versions"
        ))
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

pub fn skill_catalog_page(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    q: &str,
    category: &str,
    page: usize,
    page_size: usize,
) -> Result<SkillCatalogPage, Box<dyn Error>> {
    Ok(client
        .get(format!("{api_base}/api/agent/skills/catalog"))
        .query(&[
            ("q", q.to_string()),
            ("category", category.to_string()),
            ("page", page.to_string()),
            ("page_size", page_size.to_string()),
        ])
        .header("Authorization", format!("Agent {agent_id}:{credential}"))
        .send()?
        .error_for_status()?
        .json::<SkillCatalogPage>()?)
}

pub fn skill_versions(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    credential: &str,
    skill_id: &str,
) -> Result<Vec<SkillCatalogItem>, Box<dyn Error>> {
    let skill_id = url::form_urlencoded::byte_serialize(skill_id.as_bytes()).collect::<String>();
    Ok(client
        .get(format!(
            "{api_base}/api/agent/skills/catalog/{skill_id}/versions"
        ))
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
    revision_of_version: Option<&str>,
    source: &crate::extension_projects::ExtensionSubmissionSource,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let file_name = package_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("skill.hmskill")
        .to_string();
    let package = Part::bytes(fs::read(package_path)?)
        .file_name(file_name)
        .mime_str("application/vnd.himind.skill+zip")?;
    let source_type = if source.source_repository.trim().is_empty() {
        "local"
    } else {
        "repository"
    };
    let mut form = Form::new()
        .part("file", package)
        .text("test_report", serde_json::to_string(test_report)?)
        .text("source_type", source_type)
        .text("source_repository", source.source_repository.clone())
        .text("source_branch", source.source_default_branch.clone())
        .text("source_subdirectory", source.source_subdirectory.clone())
        .text("source_commit", source.source_commit.clone());
    if let Some(version) = revision_of_version.filter(|value| !value.trim().is_empty()) {
        form = form.text("revision_of_version", version.to_string());
    }
    Ok(client
        .post(format!("{api_base}/api/agent/skills/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .multipart(form)
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
    revision_of_version: Option<&str>,
    source: &crate::extension_projects::ExtensionSubmissionSource,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let file_name = package_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("plugin.hmpkg")
        .to_string();
    let package = Part::bytes(fs::read(package_path)?)
        .file_name(file_name)
        .mime_str("application/vnd.himind.plugin+zip")?;
    let source_type = if source.source_repository.trim().is_empty() {
        "local"
    } else {
        "repository"
    };
    let mut form = Form::new()
        .part("file", package)
        .text("test_report", serde_json::to_string(test_report)?)
        .text("source_type", source_type)
        .text("source_repository", source.source_repository.clone())
        .text("source_branch", source.source_default_branch.clone())
        .text("source_subdirectory", source.source_subdirectory.clone())
        .text("source_commit", source.source_commit.clone());
    if let Some(version) = revision_of_version.filter(|value| !value.trim().is_empty()) {
        form = form.text("revision_of_version", version.to_string());
    }
    Ok(client
        .post(format!("{api_base}/api/agent/plugins/submissions"))
        .bearer_auth(access_token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", ai_client_id())
        .multipart(form)
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

/// Read the Dashboard administrator review queue through the Agent's delegated
/// OAuth identity. The response is kept as JSON so new review metadata can be
/// added server-side without requiring an Agent release.
pub fn extension_review_queue(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    input: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(kind) = input.get("kind").and_then(|value| value.as_str()) {
        if !kind.trim().is_empty() && kind != "all" {
            query.push(("kind".to_string(), kind.trim().to_string()));
        }
    }
    if let Some(value) = input.get("query").and_then(|value| value.as_str()) {
        if !value.trim().is_empty() {
            query.push(("q".to_string(), value.trim().to_string()));
        }
    }
    for key in ["page", "page_size"] {
        if let Some(value) = input.get(key).and_then(|value| value.as_i64()) {
            if value > 0 {
                query.push((key.to_string(), value.to_string()));
            }
        }
    }
    review_json(
        client
            .get(format!("{api_base}/api/distribution/review-queue"))
            .query(&query)
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )
}

pub fn extension_review_get(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    kind: &str,
    review_id: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    review_json(
        client
            .get(format!(
                "{api_base}/api/distribution/reviews/{}/{}",
                encode_path_segment(kind),
                encode_path_segment(review_id)
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )
}

pub fn extension_review_decide(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    kind: &str,
    review_id: &str,
    artifact_id: &str,
    action: &str,
    note: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    review_json(
        client
            .post(format!(
                "{api_base}/api/distribution/reviews/{}/{}/decision",
                encode_path_segment(kind),
                encode_path_segment(review_id)
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({
                "action": action,
                "note": note,
                "artifact_id": artifact_id,
            }))
            .send()?,
    )
}

pub fn extension_collaboration(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    product_key: &str,
) -> Result<ExtensionCollaboration, Box<dyn Error>> {
    let product_key = encode_path_segment(product_key);
    collaboration_json(
        client
            .get(format!(
                "{api_base}/api/agent/extensions/{product_key}/collaboration"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )
}

pub fn extension_projects(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
) -> Result<Vec<AgentExtensionProject>, Box<dyn Error>> {
    Ok(collaboration_json::<AgentExtensionProjectsResponse>(
        client
            .get(format!("{api_base}/api/agent/extensions/projects"))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )?
    .items)
}

pub fn upsert_extension_source(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    project: &crate::extension_projects::ExtensionProject,
    source: &crate::extension_projects::ExtensionProjectSourceInput,
) -> Result<AgentExtensionProject, Box<dyn Error>> {
    let product_key = encode_path_segment(&project.extension_id);
    collaboration_json(
        client
            .put(format!(
                "{api_base}/api/agent/extensions/{product_key}/source"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({
                "kind": project.kind,
                "name": project.name,
                "description": project.description,
                "source_repository": source.source_repository,
                "source_default_branch": source.source_default_branch,
                "source_subdirectory": source.source_subdirectory,
            }))
            .send()?,
    )
}

pub fn extension_collaborator_options(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    product_key: &str,
    query: &str,
) -> Result<Vec<ExtensionCollaboratorOption>, Box<dyn Error>> {
    let product_key = encode_path_segment(product_key);
    Ok(collaboration_json::<ExtensionCollaboratorOptionsResponse>(
        client
            .get(format!(
                "{api_base}/api/agent/extensions/{product_key}/collaborator-options"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .query(&[("q", query)])
            .send()?,
    )?
    .items)
}

pub fn invite_extension_collaborator(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    product_key: &str,
    user_id: &str,
    role: &str,
) -> Result<ExtensionCollaborationMember, Box<dyn Error>> {
    let product_key = encode_path_segment(product_key);
    collaboration_json(
        client
            .post(format!(
                "{api_base}/api/agent/extensions/{product_key}/invitations"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({"user_id": user_id, "role": role}))
            .send()?,
    )
}

pub fn update_extension_collaborator(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    product_key: &str,
    user_id: &str,
    role: &str,
) -> Result<(), Box<dyn Error>> {
    let product_key = encode_path_segment(product_key);
    let user_id = encode_path_segment(user_id);
    collaboration_json::<serde_json::Value>(
        client
            .put(format!(
                "{api_base}/api/agent/extensions/{product_key}/members/{user_id}"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({"role": role}))
            .send()?,
    )?;
    Ok(())
}

pub fn delete_extension_collaborator(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    product_key: &str,
    user_id: &str,
) -> Result<(), Box<dyn Error>> {
    let product_key = encode_path_segment(product_key);
    let user_id = encode_path_segment(user_id);
    collaboration_empty(
        client
            .delete(format!(
                "{api_base}/api/agent/extensions/{product_key}/members/{user_id}"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )
}

pub fn extension_collaboration_invitations(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
) -> Result<Vec<ExtensionCollaborationInvitation>, Box<dyn Error>> {
    Ok(
        collaboration_json::<ExtensionCollaborationInvitationsResponse>(
            client
                .get(format!("{api_base}/api/agent/extensions/invitations"))
                .bearer_auth(access_token)
                .header("X-HiMind-Agent-ID", agent_id)
                .header("X-HiMind-AI-Client", ai_client_id())
                .send()?,
        )?
        .items,
    )
}

pub fn respond_extension_collaboration_invitation(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    invitation_id: &str,
    action: &str,
) -> Result<(), Box<dyn Error>> {
    let invitation_id = encode_path_segment(invitation_id);
    collaboration_json::<serde_json::Value>(
        client
            .post(format!(
                "{api_base}/api/agent/extensions/invitations/{invitation_id}/respond"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .json(&json!({"action": action}))
            .send()?,
    )?;
    Ok(())
}

fn encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.trim().as_bytes()).collect()
}

fn collaboration_json<T: DeserializeOwned>(response: Response) -> Result<T, Box<dyn Error>> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json()?);
    }
    let message = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("Dashboard request failed: {status}"));
    Err(message.into())
}

fn review_json(response: Response) -> Result<serde_json::Value, Box<dyn Error>> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json()?);
    }
    let message = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("Dashboard review request failed: {status}"));
    Err(message.into())
}

fn collaboration_empty(response: Response) -> Result<(), Box<dyn Error>> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let message = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("Dashboard request failed: {status}"));
    Err(message.into())
}

pub fn publish_software_release(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    request: &SoftwareReleasePublishRequest,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let artifact_file = File::open(&request.artifact_path)?;
    let artifact_size = artifact_file.metadata()?.len();
    let file_name = Path::new(&request.artifact_path)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("artifact file name is invalid")?
        .to_string();
    publish_software_release_with_artifact(
        client,
        api_base,
        agent_id,
        access_token,
        request,
        artifact_file,
        artifact_size,
        file_name,
    )
}

pub fn publish_software_release_with_artifact(
    client: &Client,
    api_base: &str,
    agent_id: &str,
    access_token: &str,
    request: &SoftwareReleasePublishRequest,
    artifact_file: File,
    artifact_size: u64,
    file_name: String,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let products = collaboration_json::<serde_json::Value>(
        client
            .get(format!("{api_base}/api/distribution/products"))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .query(&[
                ("product_key", request.product_id.as_str()),
                ("page_size", "200"),
            ])
            .send()?,
    )?;
    let existing_product = products["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| item["product_key"] == request.product_id)
    });
    if let Some(product) = existing_product {
        let existing_type = product["product_type"].as_str().unwrap_or_default();
        if existing_type != request.product_type {
            return Err(format!(
                "Dashboard product {} has type {}, expected {}",
                request.product_id,
                if existing_type.is_empty() {
                    "<missing>"
                } else {
                    existing_type
                },
                request.product_type
            )
            .into());
        }
    } else {
        collaboration_json::<serde_json::Value>(
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
                    "product_type": request.product_type,
                    "active": true
                }))
                .send()?,
        )?;
    }

    let release_workspace = collaboration_json::<serde_json::Value>(
        client
            .get(format!("{api_base}/api/distribution/releases"))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .query(&[
                ("product_key", request.product_id.as_str()),
                ("page_size", "200"),
            ])
            .send()?,
    )?;
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

    let artifact_part = Part::reader_with_length(artifact_file, artifact_size).file_name(file_name);
    let artifact = collaboration_json::<serde_json::Value>(
        client
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
            .send()?,
    )?;
    let artifact_id = artifact["id"]
        .as_str()
        .ok_or("Dashboard artifact response is missing id")?;

    let release = collaboration_json::<serde_json::Value>(
        client
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
            .send()?,
    )?;
    let release_id = release["id"]
        .as_str()
        .ok_or("Dashboard release response is missing id")?;

    let published = collaboration_json::<serde_json::Value>(
        client
            .post(format!(
                "{api_base}/api/distribution/releases/{release_id}/publish"
            ))
            .bearer_auth(access_token)
            .header("X-HiMind-Agent-ID", agent_id)
            .header("X-HiMind-AI-Client", ai_client_id())
            .send()?,
    )?;
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

pub fn load_state(state_path: &Path) -> Result<Option<DistributionState>, Box<dyn Error>> {
    if !state_path.is_file() {
        return Ok(None);
    }
    let mut state = serde_json::from_slice::<DistributionState>(&fs::read(state_path)?)?;
    if state.token.trim().is_empty() && !state.token_protected.trim().is_empty() {
        state.token =
            crate::store::credentials::unprotect_secret_for_current_user(&state.token_protected)?;
    }
    if state.client_id.trim().is_empty() || state.token.trim().is_empty() {
        return Err("stored Distribution client identity is incomplete".into());
    }
    if state.token_protected.trim().is_empty() {
        save_state(state_path, &state)?;
    }
    Ok(Some(state))
}

fn save_state(state_path: &Path, state: &DistributionState) -> Result<(), Box<dyn Error>> {
    let mut stored = state.clone();
    stored.token_protected =
        crate::store::credentials::protect_secret_for_current_user(&state.token)?;
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = state_path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&stored)?)?;
    if state_path.exists() {
        fs::remove_file(state_path)?;
    }
    fs::rename(temporary, state_path)?;
    Ok(())
}

pub fn load_or_register(
    client: &Client,
    api_base: &str,
    state_path: &Path,
    product_key: &str,
    client_key: &str,
    version: &str,
    channel: &str,
    host_agent_id: &str,
    host_agent_credential: &str,
) -> Result<Option<DistributionState>, Box<dyn Error>> {
    let mut enrollment_token = env::var("HIMIND_DISTRIBUTION_ENROLLMENT_TOKEN").unwrap_or_default();
    if let Some(state) = load_state(state_path)? {
        if !state.client_id.trim().is_empty()
            && !state.token.trim().is_empty()
            && heartbeat(client, api_base, &state, version, channel).is_ok()
        {
            return Ok(Some(state));
        }
    }
    if enrollment_token.trim().is_empty() {
        enrollment_token = request_client_enrollment(
            client,
            api_base,
            product_key,
            host_agent_id,
            host_agent_credential,
        )?;
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

fn request_client_enrollment(
    client: &Client,
    api_base: &str,
    product_key: &str,
    host_agent_id: &str,
    host_agent_credential: &str,
) -> Result<String, Box<dyn Error>> {
    if host_agent_id.trim().is_empty() || host_agent_credential.trim().is_empty() {
        return Err("a trusted host identity is required for Distribution enrollment".into());
    }
    #[derive(Deserialize)]
    struct EnrollmentResponse {
        token: String,
    }
    let response = client
        .post(format!("{api_base}/api/distribution/client/enrollment"))
        .header(
            "Authorization",
            format!("Agent {host_agent_id}:{host_agent_credential}"),
        )
        .json(&json!({"product_key": product_key}))
        .send()?
        .error_for_status()?
        .json::<EnrollmentResponse>()?;
    if response.token.trim().is_empty() {
        return Err("Distribution enrollment response did not include a token".into());
    }
    Ok(response.token)
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
        token_protected: String::new(),
    };
    save_state(state_path, &state)?;
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
    use super::{
        distribution_state_path, publish_software_release, DistributionState,
        SoftwareReleasePublishRequest,
    };
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
    fn distribution_state_never_serializes_plaintext_token() {
        let state = DistributionState {
            client_id: "client-1".to_string(),
            token: "plaintext-secret".to_string(),
            token_protected: "protected-value".to_string(),
        };
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains("plaintext-secret"));
        assert!(serialized.contains("protected-value"));
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
            product_type: "runtime_component".to_string(),
            version: "1.0.0".to_string(),
            channel: "stable".to_string(),
            platform: "windows".to_string(),
            architecture: "x64".to_string(),
            package_type: "directory-zip".to_string(),
            release_notes: "Broker test".to_string(),
            mandatory: false,
            rollout_percent: 100,
            inspection_receipt: "inspection_test_receipt_00000000000000000000000000000000"
                .to_string(),
            expected_size: fs::metadata(&artifact).unwrap().len(),
            expected_sha256: String::new(),
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
        assert!(captured[1].contains("\"product_type\":\"runtime_component\""));
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

    #[test]
    fn software_release_publish_rejects_existing_product_type_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            let body = r#"{"items":[{"product_key":"himind-agent","product_type":"desktop_app"}]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let root = std::env::temp_dir().join(format!(
            "himind-release-type-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let artifact = root.join("himind-agent-update.zip");
        fs::write(&artifact, b"agent-update").unwrap();
        let request = SoftwareReleasePublishRequest {
            workspace_root: root.to_string_lossy().to_string(),
            artifact_path: artifact.to_string_lossy().to_string(),
            product_id: "himind-agent".to_string(),
            product_name: "HiMind Agent".to_string(),
            product_type: "desktop_agent".to_string(),
            version: "0.3.22".to_string(),
            channel: "stable".to_string(),
            platform: "windows".to_string(),
            architecture: "x64".to_string(),
            package_type: "directory-zip".to_string(),
            release_notes: String::new(),
            mandatory: false,
            rollout_percent: 100,
            inspection_receipt: "inspection_test_receipt_00000000000000000000000000000000"
                .to_string(),
            expected_size: fs::metadata(&artifact).unwrap().len(),
            expected_sha256: String::new(),
            confirmed: true,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let error = publish_software_release(
            &client,
            &format!("http://{address}"),
            "agent-test",
            "secret-access-token",
            &request,
        )
        .unwrap_err();
        server.join().unwrap();
        let _ = fs::remove_file(artifact);
        let _ = fs::remove_dir(root);
        assert!(error.to_string().contains(
            "Dashboard product himind-agent has type desktop_app, expected desktop_agent"
        ));
    }
}
