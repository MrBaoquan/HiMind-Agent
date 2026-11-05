use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use crate::api::oauth::{platform_access_token, AI_CONVERSATION_SCOPE};
use crate::Options;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AIUserCredential {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub active_entitlement_id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub active_personal_connection_id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub status: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub base_url: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub model: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub models: Vec<String>,
    /// OpenAI 兼容协议：`openai-chat` 或 `openai-responses`。
    /// Dashboard 旧版本未返回该字段时保持 Responses 兼容行为。
    #[serde(
        default = "default_protocol",
        deserialize_with = "deserialize_nullable_string_or_protocol"
    )]
    pub protocol: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub created_at: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub updated_at: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub rotated_at: String,
    /// Per-model capability facts returned by Dashboard/AI Control. Older
    /// servers omit this field; the agent then falls back to live discovery and
    /// versioned known-model facts.
    #[serde(default)]
    pub model_metadata: BTreeMap<String, AIModelMetadata>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_nullable_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_nullable_string_or_protocol<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(default_protocol))
}

fn default_protocol() -> String {
    "openai-responses".to_string()
}

#[cfg(test)]
mod tests {
    use super::{fetch_ai_service_templates_with_token, known_model_metadata, AIUserAccess};
    use std::io::{Read, Write};

    #[test]
    fn agent_plan_model_metadata_matches_provider_catalog() {
        let cases = [
            ("doubao-seed-evolving", true),
            ("glm-5.3", false),
            ("glm-5.3-flash", true),
            ("kimi-k2.8-preview", true),
            ("kimi-k3", true),
            ("kimi-k3.1-pro", true),
            ("deepseek-v4.1-flash", true),
            ("deepseek-v4.1-flash-economy", true),
            ("deepseek-v4-flash", false),
            ("deepseek-v4-pro", false),
        ];
        for (model, supports_image) in cases {
            let metadata = known_model_metadata(model);
            assert_eq!(metadata.context_window, Some(1_000_000), "{model} context");
            assert_eq!(metadata.max_output_tokens, Some(64_000), "{model} output");
            assert_eq!(metadata.supports_image(), supports_image, "{model} image");
            assert!(!metadata.reasoning_efforts.is_empty(), "{model} reasoning");
        }
    }

    #[test]
    fn catalog_request_carries_delegated_headers_and_parses_templates() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let received = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured = received.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            *captured.lock().unwrap() = String::from_utf8_lossy(&buffer[..read]).to_string();
            let body = r#"{"items":[{"id":"anthropic","name":"Anthropic Claude","vendor_name":"Anthropic","category":"按量付费","description":"Anthropic 官方 API","service_type":"token","protocol":"anthropic","api_mode":"anthropic","base_url":"https://api.anthropic.com","models":[{"display_name":"Claude Sonnet 4","model_alias":"claude-sonnet-4","upstream_model":"claude-sonnet-4-20250514","recommended":true,"reasoning_efforts":["low","medium","high"],"default_reasoning_effort":"medium"}]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let templates = fetch_ai_service_templates_with_token(
            &format!("http://{addr}/"),
            "delegated-token",
            "agent-1",
            "himind-agent",
        )
        .expect("fetch templates");

        let request = received.lock().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /api/integrations/ai/personal-connections/catalog "));
        assert!(request.contains("authorization: bearer delegated-token"));
        assert!(request.contains("x-himind-agent-id: agent-1"));
        assert!(request.contains("x-himind-ai-client: himind-agent"));

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].protocol, "anthropic");
        assert_eq!(templates[0].api_mode, "anthropic");
        assert_eq!(templates[0].base_url, "https://api.anthropic.com");
        assert_eq!(
            templates[0].models[0].upstream_model,
            "claude-sonnet-4-20250514"
        );
        assert!(templates[0].models[0].recommended);
        assert_eq!(
            templates[0].models[0].reasoning_efforts,
            vec!["low", "medium", "high"]
        );
        assert_eq!(templates[0].models[0].default_reasoning_effort, "medium");
    }

    #[test]
    fn model_metadata_revision_tracks_reasoning_contract() {
        use crate::api::ai::{model_metadata_revision, AIModelMetadata};
        use std::collections::BTreeMap;

        let mut first = BTreeMap::new();
        first.insert(
            "model-a".to_string(),
            AIModelMetadata {
                reasoning_efforts: vec!["low".to_string(), "high".to_string()],
                default_reasoning_effort: "high".to_string(),
                ..Default::default()
            },
        );
        let mut second = first.clone();
        second.get_mut("model-a").unwrap().reasoning_efforts =
            vec!["low".to_string(), "medium".to_string(), "high".to_string()];
        second.get_mut("model-a").unwrap().default_reasoning_effort = "medium".to_string();

        let first_revision = model_metadata_revision(&first);
        assert_eq!(first_revision, model_metadata_revision(&first));
        assert_ne!(first_revision, model_metadata_revision(&second));
    }

    #[test]
    fn catalog_tolerates_missing_optional_fields() {
        let catalog: super::AiProviderTemplateCatalog = serde_json::from_str(
            r#"{"items":[{"id":"custom_openai_compatible","name":"自定义","models":null},{"id":"kimi"}]}"#,
        )
        .expect("catalog payload");
        assert_eq!(catalog.items.len(), 2);
        assert_eq!(catalog.items[0].name, "自定义");
        assert!(catalog.items[0].models.is_empty());
        assert_eq!(catalog.items[1].protocol, "");
        assert_eq!(catalog.items[1].api_mode, "");
        assert!(catalog.items[1].base_url.is_empty());
    }

    #[test]
    fn accepts_dashboard_optional_null_fields() {
        let access: AIUserAccess = serde_json::from_value(serde_json::json!({
            "active_source": "organization",
            "credential": {
                "active_entitlement_id": "entitlement-1",
                "active_personal_connection_id": null,
                "status": "active",
                "base_url": "https://gateway.example/v1",
                "model": "model-1",
                "models": ["model-1"],
                "model_metadata": {
                    "model-1": {
                        "reasoning_efforts": ["low", "medium", "high", "xhigh"],
                        "default_reasoning_effort": "medium"
                    }
                },
                "protocol": null,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "rotated_at": null
            }
        }))
        .expect("Dashboard access payload should accept nullable optional fields");
        let credential = access.credential.expect("credential");
        assert_eq!(credential.active_personal_connection_id, "");
        assert_eq!(credential.rotated_at, "");
        assert_eq!(credential.protocol, "openai-responses");
        assert_eq!(
            credential.model_metadata["model-1"].reasoning_efforts,
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            credential.model_metadata["model-1"].default_reasoning_effort,
            "medium"
        );
    }
}

#[derive(Debug, Deserialize)]
struct AIUserAccess {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    active_source: String,
    credential: Option<AIUserCredential>,
}

#[derive(Debug, Deserialize)]
struct RevealedCredential {
    api_key: String,
}

/// Dashboard 用户级 AI 服务模板（GCMP 供应商目录的用户可见投影）。
///
/// `protocol` / `base_url` 是模板自带的接入事实，Agent 据此预填本机服务表单，
/// 不再维护第二份供应商清单；不含任何凭据。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AiProviderTemplate {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub vendor_name: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub category: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub description: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub service_type: String,
    /// GCMP 协议名：`openai_compatible` / `anthropic`（`async_http` 不可用于本机推理）。
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub protocol: String,
    /// 精确线协议：`openai_chat` / `openai_responses` / `anthropic`。
    /// 旧目录没有该字段时由 Agent 按 `protocol` 兼容推断。
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub api_mode: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub base_url: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec_models")]
    pub models: Vec<AiProviderTemplateModel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AiProviderTemplateModel {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub display_name: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub model_alias: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub upstream_model: String,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_reasoning_effort: String,
}

#[derive(Debug, Deserialize)]
struct AiProviderTemplateCatalog {
    #[serde(default)]
    items: Vec<AiProviderTemplate>,
}

fn deserialize_nullable_vec_models<'de, D>(
    deserializer: D,
) -> Result<Vec<AiProviderTemplateModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<AiProviderTemplateModel>>::deserialize(deserializer)?.unwrap_or_default())
}

/// 客户端注册时使用的模型能力事实。
///
/// 它来自供应商的 `/v1/models` 或内置的已知模型兜底，不能由注册器按供应商
/// 写死。`context_window` 与 `input_modalities` 都会直接影响客户端的上下文
/// 预算和附件入口；缺失时保持为空，由各客户端自己的缺省策略处理。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AIModelMetadata {
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub input_modalities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: String,
}

impl AIModelMetadata {
    pub fn supports_image(&self) -> bool {
        self.input_modalities
            .iter()
            .any(|value| value.eq_ignore_ascii_case("image"))
    }

    pub(crate) fn supports_reasoning(&self) -> bool {
        !self.reasoning_efforts.is_empty()
    }
}

fn reasoning_effort_rank(value: &str) -> usize {
    match value {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        "max" => 6,
        "ultra" => 7,
        _ => 100,
    }
}

pub(crate) fn normalize_reasoning_efforts(values: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() || result.iter().any(|existing: &String| existing == &value) {
            continue;
        }
        result.push(value);
    }
    result.sort_by_key(|value| reasoning_effort_rank(value));
    result
}

pub(crate) fn choose_default_reasoning_effort(efforts: &[String], requested: &str) -> String {
    let requested = requested.trim().to_ascii_lowercase();
    if !requested.is_empty() && efforts.iter().any(|value| value == &requested) {
        return requested;
    }
    for preferred in [
        "medium", "high", "low", "minimal", "none", "xhigh", "max", "ultra",
    ] {
        if efforts.iter().any(|value| value == preferred) {
            return preferred.to_string();
        }
    }
    efforts.first().cloned().unwrap_or_default()
}

pub(crate) fn normalize_model_reasoning_metadata(mut metadata: AIModelMetadata) -> AIModelMetadata {
    metadata.reasoning_efforts = normalize_reasoning_efforts(&metadata.reasoning_efforts);
    metadata.default_reasoning_effort = choose_default_reasoning_effort(
        &metadata.reasoning_efforts,
        &metadata.default_reasoning_effort,
    );
    metadata
}

fn normalized_model_key(model: &str) -> String {
    let mut value = model.trim().to_ascii_lowercase();
    if let Some(stripped) = value.strip_prefix("models/") {
        value = stripped.to_string();
    }
    value.rsplit('/').next().unwrap_or(&value).to_string()
}

fn known_model_reasoning(model: &str) -> Option<(&'static [&'static str], &'static str)> {
    let full = model.trim().to_ascii_lowercase();
    let suffix = normalized_model_key(model);
    match suffix.as_str() {
        "gpt-6-astra" | "gpt-6.1-sol" => {
            Some((&["low", "medium", "high", "xhigh", "max"], "medium"))
        }
        "gpt-6-sol" | "gpt-6.1-terra" => {
            Some((&["none", "low", "medium", "high", "xhigh", "max"], "medium"))
        }
        "gpt-6-luna" => Some((&["none", "low", "medium", "high", "xhigh", "max"], "medium")),
        "gpt-5.6-sol" | "gpt-5.6-terra" => Some((
            &["low", "medium", "high", "xhigh", "max", "ultra"],
            "medium",
        )),
        "gpt-5.6-luna" => Some((&["low", "medium", "high", "xhigh", "max"], "medium")),
        "gpt-5.5" => Some((&["low", "medium", "high", "xhigh"], "medium")),
        "deepseek-flash" => Some((&["none", "low", "high", "max"], "high")),
        // 火山方舟 Agent Plan 目录中的模型能力。这里按规范化模型 ID 精确匹配，
        // 避免把某个套餐的上下文/输出上限误套到其它供应商的同名模型。
        "doubao-seed-evolving" => Some((&["minimal", "low", "medium", "high"], "high")),
        "glm-5.3" => Some((&["high", "max", "low"], "high")),
        "glm-5.3-flash" => Some((&["max", "high", "low"], "max")),
        "kimi-k2.8-preview" | "kimi-k3" | "kimi-k3.1-pro" => Some((&["max", "high", "low"], "max")),
        "deepseek-v4.1-flash-economy" => Some((&["high", "max", "low", "none"], "high")),
        "deepseek-v4-flash" | "deepseek-v4-pro" => Some((&["high", "max", "none"], "high")),
        value if value.starts_with("deepseek-v4.1-flash") => {
            if full.contains("opencode-go/") || full.contains("opencode/") {
                Some((&["none", "low", "high", "max"], "high"))
            } else {
                Some((&["none", "low", "high", "max"], "high"))
            }
        }
        _ => None,
    }
}

/// Versioned fallback for model families whose capabilities are stable and
/// already represented in the GCMP/Codex catalogs. Live provider metadata and
/// Dashboard metadata always win; this only fills missing fields.
pub(crate) fn merge_known_model_metadata(model: &str, metadata: &mut AIModelMetadata) {
    let key = model.to_ascii_lowercase();
    let suffix = normalized_model_key(model);
    let (context_window, max_output_tokens, image) = match suffix.as_str() {
        "doubao-seed-evolving" => (Some(1_000_000), Some(64_000), true),
        "glm-5.3" => (Some(1_000_000), Some(64_000), false),
        "glm-5.3-flash" => (Some(1_000_000), Some(64_000), true),
        "kimi-k2.8-preview" | "kimi-k3" | "kimi-k3.1-pro" => (Some(1_000_000), Some(64_000), true),
        "deepseek-v4.1-flash" | "deepseek-v4.1-flash-economy" => {
            (Some(1_000_000), Some(64_000), true)
        }
        "deepseek-v4-flash" | "deepseek-v4-pro" => (Some(1_000_000), Some(64_000), false),
        _ if key.contains("deepseek-v4.1-flash") => (Some(1_000_000), Some(64_000), true),
        _ if key.contains("deepseek-v4-flash") || key.contains("deepseek-v4-pro") => {
            (Some(1_000_000), Some(64_000), false)
        }
        _ if key.contains("qwen3.8-max") => (Some(1_000_000), None, true),
        _ if key.contains("kimi-k3") => (Some(1_000_000), None, true),
        _ if key.contains("glm-5.3") => (Some(1_000_000), None, false),
        _ if key.contains("minimax-m3") => (None, None, true),
        _ => (None, None, false),
    };
    if metadata.context_window.is_none() {
        metadata.context_window = context_window;
    }
    if metadata.max_output_tokens.is_none() {
        metadata.max_output_tokens = max_output_tokens;
    }
    if metadata.input_modalities.is_empty() {
        metadata.input_modalities = if image {
            vec!["text".to_string(), "image".to_string()]
        } else {
            vec!["text".to_string()]
        };
    }
    if metadata.reasoning_efforts.is_empty() {
        if let Some((efforts, default_effort)) = known_model_reasoning(model) {
            metadata.reasoning_efforts = efforts.iter().map(|value| (*value).to_string()).collect();
            metadata.default_reasoning_effort = default_effort.to_string();
        }
    }
    *metadata = normalize_model_reasoning_metadata(metadata.clone());
}

pub(crate) fn known_model_metadata(model: &str) -> AIModelMetadata {
    let mut metadata = AIModelMetadata::default();
    merge_known_model_metadata(model, &mut metadata);
    metadata
}

#[derive(Clone)]
pub(crate) struct AIClientCredential {
    pub access: AIUserCredential,
    pub api_key: String,
    /// 以模型 ID 为键；注册器只读取上面声明的模型，不会把端点返回的未授权
    /// 模型混进客户端选择器。
    pub model_metadata: BTreeMap<String, AIModelMetadata>,
}

/// 拉取 Dashboard 用户级 AI 服务模板目录（只读，不含凭据）。
pub(crate) fn fetch_ai_service_templates(
    options: &Options,
    client_id: &str,
) -> Result<Vec<AiProviderTemplate>, Box<dyn Error>> {
    let delegated = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    fetch_ai_service_templates_with_token(
        &options.api_base(),
        &delegated.token,
        &delegated.agent_id,
        client_id,
    )
}

pub(crate) fn fetch_ai_service_templates_with_token(
    api_base: &str,
    token: &str,
    agent_id: &str,
    client_id: &str,
) -> Result<Vec<AiProviderTemplate>, Box<dyn Error>> {
    let mut client_builder = Client::builder().timeout(Duration::from_secs(20));
    if url::Url::parse(api_base.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"))
    {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;
    let response = client
        .get(format!(
            "{}/api/integrations/ai/personal-connections/catalog",
            api_base.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header("X-HiMind-Agent-ID", agent_id)
        .header("X-HiMind-AI-Client", client_id)
        .send()?;
    if !response.status().is_success() {
        return Err(format!(
            "读取 AI 服务模板失败（HTTP {}）",
            response.status().as_u16()
        )
        .into());
    }
    let catalog = response.json::<AiProviderTemplateCatalog>()?;
    Ok(catalog.items)
}

pub(crate) fn fetch_client_credential(
    options: &Options,
    expected_user_id: &str,
    client_id: &str,
) -> Result<AIClientCredential, Box<dyn Error>> {
    let delegated = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    if delegated.user_id.trim() != expected_user_id.trim() {
        return Err("本机 Agent 授权账号与当前 Dashboard 用户不一致，请重新授权 Agent".into());
    }
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let common_headers = |request: reqwest::blocking::RequestBuilder| {
        request
            .bearer_auth(&delegated.token)
            .header("X-HiMind-Agent-ID", &delegated.agent_id)
            .header("X-HiMind-AI-Client", client_id)
    };

    let access_response =
        common_headers(client.get(format!("{}/api/integrations/ai/access", options.api_base())))
            .send()?;
    if !access_response.status().is_success() {
        return Err(format!(
            "读取当前 AI 接入失败（HTTP {}）",
            access_response.status().as_u16()
        )
        .into());
    }
    let access = access_response.json::<AIUserAccess>()?;
    let credential = access
        .credential
        .ok_or("当前账号尚未生成 AI 凭证，请先在“我的接入”中选择渠道")?;
    let active_reference = if access.active_source == "personal" {
        credential.active_personal_connection_id.trim()
    } else {
        credential.active_entitlement_id.trim()
    };
    if active_reference.is_empty() || credential.status != "active" {
        return Err("当前 AI 凭证未处于可用状态，请先选择有效渠道".into());
    }

    let reveal_response = common_headers(client.post(format!(
        "{}/api/integrations/ai/access/credential/reveal",
        options.api_base()
    )))
    .send()?;
    if !reveal_response.status().is_success() {
        return Err(format!(
            "领取 AI 凭证失败（HTTP {}）",
            reveal_response.status().as_u16()
        )
        .into());
    }
    let revealed = reveal_response.json::<RevealedCredential>()?;
    if revealed.api_key.trim().is_empty() {
        return Err("Dashboard 返回的 AI 凭证为空".into());
    }
    let model_metadata = credential.model_metadata.clone();
    Ok(AIClientCredential {
        access: credential,
        api_key: revealed.api_key,
        model_metadata,
    })
}

/// ADR 0118：读取当前 managed 接入的来源修订号（`updated_at|rotated_at|model_metadata_digest`），
/// 与工作台「我的接入」用于判断同步态的 `activeServiceRevision` 同源。
///
/// 只读、不领取密钥；未授权、用户不一致、未生成凭据或请求失败都返回空串，
/// 由调用方退回到「不参与对账」的旧行为，绝不把读取失败当作不同步。
pub(crate) fn fetch_managed_service_revision(
    options: &Options,
    expected_user_id: &str,
) -> Result<String, Box<dyn Error>> {
    let delegated = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    if delegated.user_id.trim() != expected_user_id.trim() {
        return Ok(String::new());
    }
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .get(format!("{}/api/integrations/ai/access", options.api_base()))
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", "ai-service-revision")
        .send()?;
    if !response.status().is_success() {
        return Ok(String::new());
    }
    let access = response.json::<AIUserAccess>()?;
    Ok(match access.credential {
        Some(credential) => format!(
            "{}|{}|{}",
            credential.updated_at,
            credential.rotated_at,
            model_metadata_revision(&credential.model_metadata)
        ),
        None => String::new(),
    })
}

fn model_metadata_revision(metadata: &BTreeMap<String, AIModelMetadata>) -> String {
    let digest = Sha256::digest(serde_json::to_vec(metadata).unwrap_or_default());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Dashboard 分发的个人 AI 服务摘要（只读，不领取 API Key）。
///
/// 用于 `ai.service.list` 的 managed 摘要与 Agent「AI 服务」页展示；
/// 未授权、用户不一致或未配置接入时返回 `available: false` 状态对象，
/// 不让只读列表能力因为登录态缺失而整体失败。
pub(crate) fn managed_ai_service_summary(
    options: &Options,
    expected_user_id: &str,
) -> serde_json::Value {
    let unavailable = |reason: &str| json!({ "available": false, "reason": reason });
    let delegated = match platform_access_token(options, AI_CONVERSATION_SCOPE) {
        Ok(value) => value,
        Err(_) => return unavailable("not_authorized"),
    };
    if !expected_user_id.trim().is_empty() && delegated.user_id.trim() != expected_user_id.trim() {
        return unavailable("user_mismatch");
    }
    let client = match Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(value) => value,
        Err(_) => return unavailable("client_error"),
    };
    let access_response = client
        .get(format!("{}/api/integrations/ai/access", options.api_base()))
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", "ai-service-list")
        .send();
    let access_response = match access_response {
        Ok(response) => response,
        Err(_) => return unavailable("network_error"),
    };
    if !access_response.status().is_success() {
        return unavailable("dashboard_error");
    }
    let access_body = match access_response.text() {
        Ok(value) => value,
        Err(_) => return unavailable("response_error"),
    };
    let access = match serde_json::from_str::<AIUserAccess>(&access_body) {
        Ok(value) => value,
        Err(error) => {
            // Never include the response body or serde details because the
            // payload may contain credential metadata. The UI maps this to a
            // user-facing retry message.
            let _ = error;
            return unavailable("parse_error");
        }
    };
    let Some(credential) = access.credential else {
        return unavailable("no_credential");
    };
    let active_reference = if access.active_source == "personal" {
        credential.active_personal_connection_id.trim()
    } else {
        credential.active_entitlement_id.trim()
    };
    if active_reference.is_empty() || credential.status != "active" {
        return json!({
            "available": false,
            "reason": "not_ready",
            "active_source": access.active_source,
            "status": credential.status,
            "base_url": credential.base_url,
            "model": credential.model,
            "models": credential.models,
        });
    }
    json!({
        "available": true,
        "active_source": access.active_source,
        "active_entitlement_id": credential.active_entitlement_id,
        "active_personal_connection_id": credential.active_personal_connection_id,
        "base_url": credential.base_url,
        "model": credential.model,
        "models": credential.models,
    })
}
