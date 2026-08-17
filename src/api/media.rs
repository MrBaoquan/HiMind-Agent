use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use serde_json::{json, Value};
use std::error::Error;
use std::time::Duration;
use url::Url;

use crate::api::client::load_agent_state;
use crate::api::oauth::{
    platform_access_token, MEDIA_CANCEL_SCOPE, MEDIA_READ_SCOPE, MEDIA_SUBMIT_SCOPE,
};
use crate::Options;

const MEDIA_CLIENT_ID: &str = "himind-agent-media";

pub(crate) fn submit(
    options: &Options,
    kind: &str,
    operation: &str,
    mut input: Value,
) -> Result<Value, Box<dyn Error>> {
    let object = input
        .as_object_mut()
        .ok_or("media input must be an object")?;
    object.insert("kind".into(), Value::String(kind.into()));
    object.insert("operation".into(), Value::String(operation.into()));
    let key = media_idempotency_key();
    request(
        options,
        MEDIA_SUBMIT_SCOPE,
        Method::POST,
        &["api", "integrations", "ai", "media", "jobs"],
        Some(input),
        Some(&key),
    )
}

pub(crate) fn get(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let job_id = required_string(&input, "job_id")?;
    request(
        options,
        MEDIA_READ_SCOPE,
        Method::GET,
        &["api", "integrations", "ai", "media", "jobs", &job_id],
        None,
        None,
    )
}

pub(crate) fn cancel(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let job_id = required_string(&input, "job_id")?;
    request(
        options,
        MEDIA_CANCEL_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "media",
            "jobs",
            &job_id,
            "cancel",
        ],
        Some(json!({})),
        None,
    )
}

fn request(
    options: &Options,
    scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let delegated = platform_access_token(options, scope)?;
    let state = load_agent_state(&options.state_path)?;
    if delegated.agent_id.trim() != state.agent_id.trim() {
        return Err("Dashboard authorization does not match this Agent".into());
    }
    let url = api_url(&options.api_base, path)?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut builder = client
        .request(method, url)
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", MEDIA_CLIENT_ID);
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
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
    let value = response.json::<Value>().unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("media request failed");
        return Err(format!("{message} (HTTP {})", status.as_u16()).into());
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

fn media_idempotency_key() -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let suffix = random
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    format!("agent-media-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{api_url, media_idempotency_key};

    #[test]
    fn media_url_escapes_job_id() {
        let url = api_url(
            "https://example.test/root/",
            &["api", "integrations", "ai", "media", "jobs", "job/1"],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.test/root/api/integrations/ai/media/jobs/job%2F1"
        );
    }

    #[test]
    fn media_idempotency_keys_are_random_and_well_formed() {
        let first = media_idempotency_key();
        let second = media_idempotency_key();
        assert_ne!(first, second);
        assert_eq!(first.len(), "agent-media-".len() + 32);
        assert!(first["agent-media-".len()..]
            .chars()
            .all(|value| value.is_ascii_hexdigit()));
    }
}
