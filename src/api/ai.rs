use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use std::error::Error;
use std::time::Duration;

use crate::api::oauth::{platform_access_token, AI_CONVERSATION_SCOPE};
use crate::Options;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AIUserCredential {
    #[serde(default)]
    pub active_entitlement_id: String,
    #[serde(default)]
    pub active_personal_connection_id: String,
    #[serde(default)]
    pub status: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// OpenAI 兼容协议：`openai-chat` 或 `openai-responses`。
    /// Dashboard 旧版本未返回该字段时保持 Responses 兼容行为。
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "openai-responses".to_string()
}

#[derive(Debug, Deserialize)]
struct AIUserAccess {
    #[serde(default)]
    active_source: String,
    credential: Option<AIUserCredential>,
}

#[derive(Debug, Deserialize)]
struct RevealedCredential {
    api_key: String,
}

pub(crate) struct AIClientCredential {
    pub access: AIUserCredential,
    pub api_key: String,
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
        common_headers(client.get(format!("{}/api/integrations/ai/access", options.api_base)))
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
        options.api_base
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
        .get(format!("{}/api/integrations/ai/access", options.api_base))
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
    let access = match access_response.json::<AIUserAccess>() {
        Ok(value) => value,
        Err(_) => return unavailable("parse_error"),
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
