use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
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
    use super::{fetch_ai_service_templates_with_token, AIUserAccess};
    use std::io::{Read, Write};

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
            let body = r#"{"items":[{"id":"anthropic","name":"Anthropic Claude","vendor_name":"Anthropic","category":"国际厂商","description":"Anthropic 官方 API","service_type":"token","protocol":"anthropic","base_url":"https://api.anthropic.com","models":[{"display_name":"Claude Sonnet 4","model_alias":"claude-sonnet-4","upstream_model":"claude-sonnet-4-20250514","recommended":true}]}]}"#;
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
        assert_eq!(templates[0].base_url, "https://api.anthropic.com");
        assert_eq!(
            templates[0].models[0].upstream_model,
            "claude-sonnet-4-20250514"
        );
        assert!(templates[0].models[0].recommended);
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

#[derive(Clone)]
pub(crate) struct AIClientCredential {
    pub access: AIUserCredential,
    pub api_key: String,
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
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
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
    Ok(AIClientCredential {
        access: credential,
        api_key: revealed.api_key,
    })
}

/// ADR 0118：读取当前 managed 接入的来源修订号（`updated_at|rotated_at`），
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
        Some(credential) => format!("{}|{}", credential.updated_at, credential.rotated_at),
        None => String::new(),
    })
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
