use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use serde_json::Value;
use std::error::Error;
use std::time::Duration;
use url::Url;

use crate::api::client::load_agent_state;
use crate::api::oauth::{
    platform_access_token, BUSINESS_CONTEXT_READ_SCOPE, KNOWLEDGE_SEARCH_SCOPE,
};
use crate::Options;

const BUSINESS_CLIENT_ID: &str = "himind-agent-business";

pub(crate) fn resolve_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request(
        options,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "context",
            "resolve",
        ],
        Some(input),
    )
}

pub(crate) fn project_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    request(
        options,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
        ],
        None,
    )
}

pub(crate) fn exhibit_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request(
        options,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
        ],
        None,
    )
}

pub(crate) fn my_work_summary(options: &Options) -> Result<Value, Box<dyn Error>> {
    request(
        options,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "my-work",
            "summary",
        ],
        None,
    )
}

pub(crate) fn search_knowledge(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request_with_scope(
        options,
        KNOWLEDGE_SEARCH_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "knowledge",
            "search",
        ],
        Some(input),
    )
}

fn request(
    options: &Options,
    method: Method,
    path: &[&str],
    body: Option<Value>,
) -> Result<Value, Box<dyn Error>> {
    request_with_scope(options, BUSINESS_CONTEXT_READ_SCOPE, method, path, body)
}

fn request_with_scope(
    options: &Options,
    required_scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
) -> Result<Value, Box<dyn Error>> {
    let delegated = platform_access_token(options, required_scope)?;
    let state = load_agent_state(&options.state_path)?;
    if delegated.agent_id.trim() != state.agent_id.trim() {
        return Err("Dashboard 授权与当前 Agent 实例不匹配，请重新授权".into());
    }
    let url = api_url(&options.api_base, path)?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut builder = client
        .request(method, url)
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", BUSINESS_CLIENT_ID);
    if let Some(value) = body {
        builder = builder.json(&value);
    }
    read_json(builder)
}

fn api_url(base: &str, path: &[&str]) -> Result<Url, Box<dyn Error>> {
    let mut url = Url::parse(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Dashboard API URL cannot be a base")?;
        segments.pop_if_empty();
        for segment in path {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn read_json(builder: RequestBuilder) -> Result<Value, Box<dyn Error>> {
    let response = builder.send()?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Dashboard 业务能力调用失败");
        return Err(format!("{message}（HTTP {}）", status.as_u16()).into());
    }
    Ok(value)
}

fn required_string(input: &Value, name: &str) -> Result<String, Box<dyn Error>> {
    let value = input
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        return Err(format!("{name} is required").into());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::api_url;

    #[test]
    fn api_url_preserves_base_path_and_escapes_identifiers() {
        let url = api_url(
            "https://example.test/root/",
            &[
                "api",
                "integrations",
                "ai",
                "business",
                "exhibits",
                "展项/1",
            ],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.test/root/api/integrations/ai/business/exhibits/%E5%B1%95%E9%A1%B9%2F1"
        );
    }
}
