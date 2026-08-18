use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
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
    #[serde(default)]
    entitlements: Vec<AIEntitlementSummary>,
    #[serde(default)]
    personal_channels: Vec<AIPersonalChannelSummary>,
}

#[derive(Debug, Deserialize)]
struct AIEntitlementSummary {
    #[serde(default)]
    id: String,
    #[serde(default)]
    channel_name: String,
    #[serde(default)]
    provider_name: String,
}

#[derive(Debug, Deserialize)]
struct AIPersonalChannelSummary {
    #[serde(default)]
    connection_id: String,
    #[serde(default)]
    connection_name: String,
    #[serde(default)]
    provider_name: String,
}

#[derive(Debug, Deserialize)]
struct RevealedCredential {
    api_key: String,
}

pub(crate) struct AIClientCredential {
    pub access: AIUserCredential,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AIModelOptions {
    pub selected_model: String,
    pub models: Vec<String>,
    pub source_type: String,
    pub source_name: String,
    pub source_provider: String,
}

/// Reads the active AI service model catalog without revealing the API credential.
pub(crate) fn fetch_client_model_options(
    options: &Options,
    client_id: &str,
) -> Result<AIModelOptions, Box<dyn Error>> {
    let delegated = platform_access_token(options, AI_CONVERSATION_SCOPE)?;
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client
        .get(format!(
            "{}/api/integrations/ai/access",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", client_id)
        .send()?;
    if !response.status().is_success() {
        return Err(format!(
            "读取当前 AI 接入失败（HTTP {}）",
            response.status().as_u16()
        )
        .into());
    }
    let access = response.json::<AIUserAccess>()?;
    model_options_from_access(access)
}

fn model_options_from_access(access: AIUserAccess) -> Result<AIModelOptions, Box<dyn Error>> {
    let credential = access
        .credential
        .ok_or("当前账号尚未生成 AI 凭证，请先在“我的接入”中选择渠道")?;
    if credential.status != "active" {
        return Err("当前 AI 凭证未处于可用状态，请先选择有效渠道".into());
    }
    let mut models = credential
        .models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    if !credential.model.trim().is_empty()
        && !models.iter().any(|model| model == credential.model.trim())
    {
        models.insert(0, credential.model.trim().to_string());
    }
    models.sort();
    models.dedup();
    let source_type = if access.active_source == "personal" {
        "personal"
    } else {
        "organization"
    };
    let (source_name, source_provider) = if source_type == "personal" {
        access
            .personal_channels
            .iter()
            .find(|item| item.connection_id == credential.active_personal_connection_id)
            .map(|item| {
                (
                    first_non_empty(&item.connection_name, &item.provider_name, "个人服务"),
                    item.provider_name.trim().to_string(),
                )
            })
            .unwrap_or_else(|| ("个人服务".to_string(), String::new()))
    } else {
        access
            .entitlements
            .iter()
            .find(|item| item.id == credential.active_entitlement_id)
            .map(|item| {
                (
                    first_non_empty(&item.channel_name, &item.provider_name, "组织服务"),
                    item.provider_name.trim().to_string(),
                )
            })
            .unwrap_or_else(|| ("组织服务".to_string(), String::new()))
    };
    Ok(AIModelOptions {
        selected_model: credential.model.trim().to_string(),
        models,
        source_type: source_type.to_string(),
        source_name,
        source_provider,
    })
}

fn first_non_empty(primary: &str, secondary: &str, fallback: &str) -> String {
    [primary, secondary, fallback]
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> AIUserCredential {
        AIUserCredential {
            active_entitlement_id: "entitlement-1".to_string(),
            active_personal_connection_id: "connection-1".to_string(),
            status: "active".to_string(),
            base_url: "http://127.0.0.1".to_string(),
            model: "model-default".to_string(),
            models: vec!["model-default".to_string(), "model-fast".to_string()],
        }
    }

    #[test]
    fn model_options_identify_personal_source() {
        let options = model_options_from_access(AIUserAccess {
            active_source: "personal".to_string(),
            credential: Some(credential()),
            entitlements: Vec::new(),
            personal_channels: vec![AIPersonalChannelSummary {
                connection_id: "connection-1".to_string(),
                connection_name: "我的 DeepSeek".to_string(),
                provider_name: "DeepSeek".to_string(),
            }],
        })
        .expect("personal model options");

        assert_eq!(options.source_type, "personal");
        assert_eq!(options.source_name, "我的 DeepSeek");
        assert_eq!(options.source_provider, "DeepSeek");
        assert_eq!(options.models, vec!["model-default", "model-fast"]);
    }

    #[test]
    fn model_options_identify_organization_channel() {
        let options = model_options_from_access(AIUserAccess {
            active_source: "organization".to_string(),
            credential: Some(credential()),
            entitlements: vec![AIEntitlementSummary {
                id: "entitlement-1".to_string(),
                channel_name: "研发服务".to_string(),
                provider_name: "统一 AI 服务".to_string(),
            }],
            personal_channels: Vec::new(),
        })
        .expect("organization model options");

        assert_eq!(options.source_type, "organization");
        assert_eq!(options.source_name, "研发服务");
        assert_eq!(options.source_provider, "统一 AI 服务");
    }
}
