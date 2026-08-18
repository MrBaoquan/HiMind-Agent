use reqwest::blocking::Client;
use serde::Deserialize;
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
