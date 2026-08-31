use reqwest::blocking::Client;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::oauth::{
    authorization_snapshot, platform_access_token, BUSINESS_CONTEXT_READ_SCOPE,
};
use crate::approval::remote::ApprovalProof;
use crate::Options;

const CATALOG_SCHEMA_VERSION: &str = "1";
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const CATALOG_STALE_AFTER: Duration = Duration::from_secs(60);
const CATALOG_CLIENT_ID: &str = "himind-agent-capability-catalog";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DashboardCapabilityContract {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub risk_level: String,
    pub http_method: String,
    pub scope: String,
    pub dashboard_route: String,
    pub input_schema: Value,
    pub execution_mode: String,
    #[serde(default)]
    pub supports_progress: bool,
    #[serde(default)]
    pub supports_cancel: bool,
    pub idempotency: String,
    #[serde(default = "default_retry_policy")]
    pub retry_policy: String,
    #[serde(default = "default_concurrency_policy")]
    pub concurrency: String,
    #[serde(default)]
    pub approval_required: bool,
}

fn default_retry_policy() -> String {
    "never".to_string()
}

fn default_concurrency_policy() -> String {
    "keyed".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct DashboardCatalogResponse {
    schema_version: String,
    generation: String,
    items: Vec<DashboardCapabilityContract>,
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardCatalogSnapshot {
    pub generation: String,
    pub items: Vec<DashboardCapabilityContract>,
}

/// Execute a catalog operation using the same Dashboard OAuth boundary as
/// the built-in business handlers. The catalog can add ordinary CRUD routes
/// without adding another Agent enum variant; special long-running handlers
/// remain explicitly implemented by Agent code.
pub(crate) fn invoke_catalog_capability(
    options: &Options,
    contract: &DashboardCapabilityContract,
    input: Value,
    request_id: &str,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let access = platform_access_token(options, &contract.scope)?;
    let (url, consumed) = catalog_operation_url(options, &contract.dashboard_route, &input)?;
    let method = Method::from_bytes(contract.http_method.as_bytes())?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut remaining = input;
    let (body, query) = catalog_request_payload(&method, &mut remaining, &consumed)?;
    let idempotency_key = if method != Method::GET && method != Method::HEAD {
        Some(format!("himind-agent:{}", request_id.trim()))
    } else {
        None
    };
    let mut attempt = 0;
    loop {
        let mut request = client
            .request(method.clone(), url.clone())
            .bearer_auth(&access.token)
            .header("X-HiMind-Agent-ID", &access.agent_id)
            .header("X-HiMind-AI-Client", CATALOG_CLIENT_ID)
            .header("X-HiMind-Retry-Policy", &contract.retry_policy)
            .header("X-HiMind-Concurrency", &contract.concurrency);
        if let Some(proof) = proof {
            match proof {
                ApprovalProof::Approval(id) => {
                    request = request.header("X-HiMind-Approval-ID", id);
                }
                ApprovalProof::Grant(id) => {
                    request = request.header("X-HiMind-Grant-ID", id);
                }
            }
        }
        if let Some(key) = idempotency_key.as_deref() {
            request = request.header("Idempotency-Key", key);
        }
        if method == Method::GET {
            request = request.query(&query);
        } else {
            request = request.json(&body);
        }
        let response = request.send()?;
        let status = response.status();
        let raw = response.bytes()?;
        let value = serde_json::from_slice::<Value>(&raw).unwrap_or_else(|_| serde_json::json!({}));
        if status.is_success() {
            return Ok(value);
        }
        if attempt == 0
            && retryable_status(status)
            && retry_allowed(contract, idempotency_key.is_some())
        {
            attempt += 1;
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Dashboard 业务能力调用失败");
        return Err(format!("{message}（HTTP {}）", status.as_u16()).into());
    }
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn retry_allowed(contract: &DashboardCapabilityContract, has_idempotency_key: bool) -> bool {
    match contract.retry_policy.as_str() {
        "safe" => contract.idempotency == "safe",
        "idempotency_key" => contract.idempotency == "conditional" && has_idempotency_key,
        _ => false,
    }
}

fn catalog_request_payload(
    method: &Method,
    input: &mut Value,
    consumed: &[String],
) -> Result<(Value, Vec<(String, String)>), Box<dyn Error>> {
    if let Some(object) = input.as_object_mut() {
        for key in consumed {
            object.remove(key);
        }
    }
    if *method != Method::GET {
        return Ok((input.clone(), Vec::new()));
    }
    let mut query = Vec::new();
    if let Some(object) = input.as_object() {
        for (key, value) in object {
            if value.is_null() {
                continue;
            }
            let encoded = if let Some(text) = value.as_str() {
                text.to_string()
            } else if let Some(boolean) = value.as_bool() {
                boolean.to_string()
            } else if let Some(number) = value.as_number() {
                number.to_string()
            } else {
                serde_json::to_string(value)?
            };
            query.push((key.clone(), encoded));
        }
    }
    query.sort_by(|left, right| left.0.cmp(&right.0));
    Ok((Value::Null, query))
}

fn catalog_operation_url(
    options: &Options,
    route: &str,
    input: &Value,
) -> Result<(Url, Vec<String>), Box<dyn Error>> {
    let mut url = Url::parse(&options.api_base)?;
    let mut consumed = Vec::new();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Dashboard API URL cannot be a base")?;
        segments.pop_if_empty();
        for segment in route.trim_start_matches('/').split('/') {
            if segment.starts_with('{') && segment.ends_with('}') {
                let key = segment.trim_start_matches('{').trim_end_matches('}');
                if key.is_empty()
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err("Dashboard capability route placeholder is invalid".into());
                }
                let value = input
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| format!("{key} is required"))?;
                segments.push(value);
                consumed.push(key.to_string());
            } else if !segment.is_empty() {
                segments.push(segment);
            }
        }
    }
    Ok((url, consumed))
}

#[derive(Default)]
struct CatalogState {
    last_attempt: Option<Instant>,
    updated_at: Option<Instant>,
    etag: String,
    snapshot: Option<DashboardCatalogSnapshot>,
}

#[derive(Clone)]
pub(crate) struct DashboardCatalogProvider {
    options: Options,
    state: Arc<Mutex<CatalogState>>,
}

impl DashboardCatalogProvider {
    pub(crate) fn new(options: &Options) -> Self {
        Self {
            options: options.clone(),
            state: Arc::new(Mutex::new(CatalogState::default())),
        }
    }

    /// Returns the last validated Connected-mode catalog. Independent mode is
    /// a first-class topology and never reads or refreshes Dashboard state.
    pub(crate) fn snapshot(&self) -> Option<DashboardCatalogSnapshot> {
        if !self.options.mode().control_plane_enabled() {
            return None;
        }
        let refresh = self
            .state
            .lock()
            .ok()
            .map(|state| {
                state
                    .last_attempt
                    .map(|attempt| attempt.elapsed() >= CATALOG_REFRESH_INTERVAL)
                    .unwrap_or(true)
            })
            .unwrap_or(false);
        if refresh {
            // Avoid a network/refresh-token round trip for a fresh Connected
            // Agent that has never been authorized. Static capabilities stay
            // immediately available in that state.
            match authorization_snapshot(&self.options.state_path) {
                Ok(Some(_)) => self.refresh(),
                Ok(None) => {
                    if let Ok(mut state) = self.state.lock() {
                        state.last_attempt = Some(Instant::now());
                        state.etag.clear();
                        state.snapshot = None;
                        state.updated_at = None;
                    }
                }
                Err(_) => {
                    if let Ok(mut state) = self.state.lock() {
                        state.last_attempt = Some(Instant::now());
                    }
                }
            }
        }
        self.state.lock().ok().and_then(|state| {
            let fresh = state
                .updated_at
                .map(|updated| updated.elapsed() <= CATALOG_STALE_AFTER)
                .unwrap_or(false);
            fresh.then(|| state.snapshot.clone()).flatten()
        })
    }

    fn refresh(&self) {
        let etag = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state
                .last_attempt
                .map(|attempt| attempt.elapsed() < CATALOG_REFRESH_INTERVAL)
                .unwrap_or(false)
            {
                return;
            }
            state.last_attempt = Some(Instant::now());
            state.etag.clone()
        };
        let Ok(result) = fetch_catalog(&self.options, &etag) else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        match result {
            FetchResult::NotModified => {
                state.updated_at = Some(Instant::now());
            }
            FetchResult::Modified { snapshot, etag } => {
                state.etag = etag;
                state.snapshot = Some(snapshot);
                state.updated_at = Some(Instant::now());
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn from_snapshot(options: &Options, snapshot: DashboardCatalogSnapshot) -> Self {
        Self {
            options: options.clone(),
            state: Arc::new(Mutex::new(CatalogState {
                last_attempt: Some(Instant::now()),
                updated_at: Some(Instant::now()),
                etag: snapshot.generation.clone(),
                snapshot: Some(snapshot),
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_snapshot(&self, snapshot: DashboardCatalogSnapshot) {
        let mut state = self.state.lock().expect("catalog test state lock");
        state.last_attempt = Some(Instant::now());
        state.updated_at = Some(Instant::now());
        state.etag = snapshot.generation.clone();
        state.snapshot = Some(snapshot);
    }
}

enum FetchResult {
    NotModified,
    Modified {
        snapshot: DashboardCatalogSnapshot,
        etag: String,
    },
}

fn fetch_catalog(options: &Options, etag: &str) -> Result<FetchResult, Box<dyn Error>> {
    let access = platform_access_token(options, BUSINESS_CONTEXT_READ_SCOPE)?;
    let url = catalog_url(&options.api_base)?;
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let mut request = client
        .get(url)
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", CATALOG_CLIENT_ID);
    if !etag.trim().is_empty() {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let response = request.send()?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(FetchResult::NotModified);
    }
    if !response.status().is_success() {
        return Err(format!(
            "Dashboard capability catalog request failed: HTTP {}",
            response.status().as_u16()
        )
        .into());
    }
    let response_etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload: DashboardCatalogResponse = response.json()?;
    validate_catalog(&payload)?;
    Ok(FetchResult::Modified {
        etag: if response_etag.is_empty() {
            payload.generation.clone()
        } else {
            response_etag
        },
        snapshot: DashboardCatalogSnapshot {
            generation: payload.generation,
            items: payload.items,
        },
    })
}

fn catalog_url(base: &str) -> Result<Url, Box<dyn Error>> {
    let mut url = Url::parse(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Dashboard API URL cannot be a base")?;
        segments.pop_if_empty();
        for segment in ["api", "integrations", "ai", "business", "capabilities"] {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn validate_catalog(payload: &DashboardCatalogResponse) -> Result<(), Box<dyn Error>> {
    if payload.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Dashboard capability catalog schema: {}",
            payload.schema_version
        )
        .into());
    }
    if payload.generation.trim().is_empty() {
        return Err("Dashboard capability catalog generation is required".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut route_contracts = std::collections::BTreeMap::new();
    for item in &payload.items {
        validate_contract(item)?;
        if !ids.insert(item.id.as_str()) {
            return Err(format!("duplicate Dashboard capability id: {}", item.id).into());
        }
        let route_key = format!("{} {}", item.http_method, item.dashboard_route);
        if let Some(previous) = route_contracts.insert(route_key.clone(), item) {
            if previous.scope != item.scope
                || previous.risk_level != item.risk_level
                || previous.execution_mode != item.execution_mode
                || previous.supports_progress != item.supports_progress
                || previous.supports_cancel != item.supports_cancel
                || previous.idempotency != item.idempotency
                || previous.approval_required != item.approval_required
                || previous.input_schema != item.input_schema
            {
                return Err(format!(
                    "conflicting Dashboard capability contracts for route {route_key}"
                )
                .into());
            }
        }
    }
    Ok(())
}

fn validate_contract(item: &DashboardCapabilityContract) -> Result<(), Box<dyn Error>> {
    let version = semver::Version::parse(&item.version)
        .map_err(|_| format!("Dashboard capability version is invalid: {}", item.id))?;
    if item.id.trim().is_empty()
        || !version.pre.is_empty()
        || !version.build.is_empty()
        || item.name.trim().is_empty()
        || item.description.trim().is_empty()
        || !item
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("Dashboard capability identity is incomplete".into());
    }
    if !matches!(item.risk_level.as_str(), "read_only" | "network_write") {
        return Err(format!("invalid Dashboard capability risk level: {}", item.id).into());
    }
    if !matches!(
        item.http_method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
    ) {
        return Err(format!("unsupported Dashboard capability method: {}", item.id).into());
    }
    if item.scope.trim().is_empty()
        || !item
            .scope
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
        || !item.dashboard_route.starts_with("/api/integrations/ai/")
        || item.dashboard_route.contains("..")
        || item.dashboard_route.contains('?')
        || item.dashboard_route.contains('#')
    {
        return Err(format!("unsafe Dashboard capability route: {}", item.id).into());
    }
    if !matches!(item.execution_mode.as_str(), "sync" | "long_running") {
        return Err(format!("invalid Dashboard execution mode: {}", item.id).into());
    }
    if item.execution_mode == "sync" && (item.supports_progress || item.supports_cancel) {
        return Err(format!(
            "synchronous Dashboard capability cannot advertise progress or cancellation: {}",
            item.id
        )
        .into());
    }
    if !matches!(
        item.idempotency.as_str(),
        "safe" | "conditional" | "not_guaranteed"
    ) {
        return Err(format!("invalid Dashboard idempotency contract: {}", item.id).into());
    }
    if item.input_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(format!(
            "Dashboard capability input schema must be an object: {}",
            item.id
        )
        .into());
    }
    if !matches!(
        item.retry_policy.as_str(),
        "safe" | "idempotency_key" | "never"
    ) {
        return Err(format!("invalid Dashboard retry policy: {}", item.id).into());
    }
    if !matches!(
        item.concurrency.as_str(),
        "parallel" | "keyed" | "exclusive"
    ) {
        return Err(format!("invalid Dashboard concurrency policy: {}", item.id).into());
    }
    if item.idempotency == "safe" && item.retry_policy != "safe" {
        return Err(format!(
            "safe Dashboard capability must advertise safe retries: {}",
            item.id
        )
        .into());
    }
    if item.idempotency == "conditional" && item.retry_policy != "idempotency_key" {
        return Err(format!(
            "conditional Dashboard capability must advertise idempotency-key retries: {}",
            item.id
        )
        .into());
    }
    if item.idempotency == "not_guaranteed" && item.retry_policy != "never" {
        return Err(format!(
            "non-guaranteed Dashboard capability cannot be retried: {}",
            item.id
        )
        .into());
    }
    if item
        .input_schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(format!(
            "Dashboard capability input schema must be closed: {}",
            item.id
        )
        .into());
    }
    let properties = item
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("Dashboard capability properties are required: {}", item.id))?;
    let required_values = item
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let required = required_values
        .iter()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if item.input_schema.get("required").is_some_and(|value| {
        value
            .as_array()
            .is_none_or(|values| values.iter().any(|entry| !entry.is_string()))
    }) {
        return Err(format!(
            "Dashboard capability required must be string array: {}",
            item.id
        )
        .into());
    }
    if required.len() != required_values.len() {
        return Err(format!(
            "Dashboard capability required fields must be unique: {}",
            item.id
        )
        .into());
    }
    for required_field in &required {
        if !properties.contains_key(*required_field) {
            return Err(format!(
                "Dashboard capability required field is undeclared: {}",
                item.id
            )
            .into());
        }
    }
    for (name, schema) in properties {
        validate_catalog_schema_node(schema)
            .map_err(|error| format!("Dashboard capability field {name} is invalid: {error}"))?;
    }
    let mut placeholders = std::collections::BTreeSet::new();
    for segment in item.dashboard_route.trim_start_matches('/').split('/') {
        let has_brace = segment.contains('{') || segment.contains('}');
        if !has_brace {
            continue;
        }
        if !(segment.starts_with('{') && segment.ends_with('}')) {
            return Err(format!(
                "Dashboard capability route placeholder is invalid: {}",
                item.id
            )
            .into());
        }
        let key = segment.trim_start_matches('{').trim_end_matches('}');
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !placeholders.insert(key.to_string())
            || !properties.contains_key(key)
            || !required.contains(key)
        {
            return Err(format!(
                "Dashboard capability route placeholder is undeclared: {}",
                item.id
            )
            .into());
        }
    }
    Ok(())
}

fn validate_catalog_schema_node(schema: &Value) -> Result<(), Box<dyn Error>> {
    let allowed = |kind: &str| {
        matches!(
            kind,
            "string" | "integer" | "number" | "boolean" | "array" | "object" | "null"
        )
    };
    let types = match schema.get("type") {
        Some(Value::String(kind)) if allowed(kind) => vec![kind.as_str()],
        Some(Value::Array(values))
            if !values.is_empty()
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(&allowed)) =>
        {
            values.iter().filter_map(Value::as_str).collect::<Vec<_>>()
        }
        _ => return Err("schema type is missing or unsupported".into()),
    };
    if types.contains(&"array") {
        let items = schema
            .get("items")
            .ok_or("array items schema is required")?;
        validate_catalog_schema_node(items)?;
    }
    if types.contains(&"object") {
        if let Some(properties) = schema.get("properties") {
            let properties = properties
                .as_object()
                .ok_or("object properties must be an object")?;
            for property in properties.values() {
                validate_catalog_schema_node(property)?;
            }
        }
        if let Some(additional) = schema.get("additionalProperties") {
            if !additional.is_boolean() && !additional.is_object() {
                return Err("additionalProperties must be boolean or schema".into());
            }
            if additional.is_object() {
                validate_catalog_schema_node(additional)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::oauth::AgentAccessToken;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn contract() -> DashboardCapabilityContract {
        DashboardCapabilityContract {
            id: "business.example.list".into(),
            version: "1.0.0".into(),
            name: "示例列表".into(),
            description: "读取示例。".into(),
            risk_level: "read_only".into(),
            http_method: "GET".into(),
            scope: "business.example.read".into(),
            dashboard_route: "/api/integrations/ai/business/examples".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
            execution_mode: "sync".into(),
            supports_progress: false,
            supports_cancel: false,
            idempotency: "safe".into(),
            retry_policy: "safe".into(),
            concurrency: "parallel".into(),
            approval_required: false,
        }
    }

    #[test]
    fn rejects_routes_outside_the_ai_integration_boundary() {
        let mut item = contract();
        item.dashboard_route = "https://example.test/admin".into();
        assert!(validate_contract(&item).is_err());
    }

    #[test]
    fn accepts_a_versioned_sync_contract() {
        validate_contract(&contract()).unwrap();
    }

    #[test]
    fn rejects_invalid_catalog_contract_shapes() {
        let cases = [
            ("version", "1"),
            ("scope", "business example read"),
            ("risk", "unknown"),
            ("execution", "streaming"),
        ];
        for (field, value) in cases {
            let mut item = contract();
            match field {
                "version" => item.version = value.into(),
                "scope" => item.scope = value.into(),
                "risk" => item.risk_level = value.into(),
                "execution" => item.execution_mode = value.into(),
                _ => unreachable!(),
            }
            assert!(
                validate_contract(&item).is_err(),
                "accepted invalid {field}"
            );
        }

        let mut invalid_type = contract();
        invalid_type.input_schema = json!({
            "type":"object",
            "properties":{"value":{"type":"file"}},
            "additionalProperties":false
        });
        assert!(validate_contract(&invalid_type).is_err());

        let mut duplicate_required = contract();
        duplicate_required.dashboard_route =
            "/api/integrations/ai/business/examples/{example_id}".into();
        duplicate_required.input_schema = json!({
            "type":"object",
            "properties":{"example_id":{"type":"string"}},
            "required":["example_id", "example_id"],
            "additionalProperties":false
        });
        assert!(validate_contract(&duplicate_required).is_err());
    }

    #[test]
    fn rejects_conflicting_catalog_route_aliases() {
        let first = contract();
        let mut second = first.clone();
        second.id = "business.example.alias".into();
        second.scope = "business.example.write".into();
        let payload = DashboardCatalogResponse {
            schema_version: CATALOG_SCHEMA_VERSION.into(),
            generation: "generation-test".into(),
            items: vec![first, second],
        };
        assert!(validate_catalog(&payload).is_err());
    }

    #[test]
    fn independent_provider_does_not_expose_a_cached_dashboard_snapshot() {
        let mut options = Options::from_env();
        options.effective_mode = crate::app::runtime_mode::AgentMode::Independent;
        let provider = DashboardCatalogProvider::from_snapshot(
            &options,
            DashboardCatalogSnapshot {
                generation: "cached".into(),
                items: vec![contract()],
            },
        );
        assert!(provider.snapshot().is_none());
    }

    #[test]
    fn catalog_routes_encode_path_segments_and_remove_consumed_fields() {
        let mut options = Options::from_env();
        options.api_base = "https://example.test/root".into();
        let input = json!({"example_id":"展项 1", "q":"hello"});
        let (url, consumed) = catalog_operation_url(
            &options,
            "/api/integrations/ai/business/examples/{example_id}",
            &input,
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://example.test/root/api/integrations/ai/business/examples/%E5%B1%95%E9%A1%B9%201");
        assert_eq!(consumed, vec!["example_id"]);
    }

    #[test]
    fn catalog_get_inputs_become_query_without_path_fields() {
        let mut input =
            json!({"example_id":"one", "q":"hello", "page":2, "enabled":true, "ignored":null});
        let (body, query) =
            catalog_request_payload(&Method::GET, &mut input, &["example_id".to_string()]).unwrap();
        assert_eq!(body, Value::Null);
        assert_eq!(
            input,
            json!({"q":"hello", "page":2, "enabled":true, "ignored":null})
        );
        assert_eq!(
            query,
            vec![
                ("enabled".to_string(), "true".to_string()),
                ("page".to_string(), "2".to_string()),
                ("q".to_string(), "hello".to_string())
            ]
        );
    }

    #[test]
    fn catalog_write_inputs_become_body_without_path_fields() {
        let mut input = json!({"example_id":"one", "name":"demo"});
        let (body, query) =
            catalog_request_payload(&Method::POST, &mut input, &["example_id".to_string()])
                .unwrap();
        assert!(query.is_empty());
        assert_eq!(body, json!({"name":"demo"}));
    }

    fn options_for_test_server(listener: &TcpListener) -> Options {
        let mut options = Options::from_env();
        options.api_base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add(3600);
        *options.platform_access.write().unwrap() = Some(AgentAccessToken {
            token: "test-access-token".into(),
            expires_at,
            scope: "business.context.read business.example.read business.example.write".into(),
            user_id: "user-test".into(),
            agent_id: "agent-test".into(),
        });
        options
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end;
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "HTTP request ended before headers");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = index + 4;
                break;
            }
        }
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "HTTP request ended before body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn respond_json(mut stream: std::net::TcpStream) {
        let body = br#"{"ok":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    #[test]
    fn catalog_fetch_uses_etag_and_accepts_not_modified() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let options = options_for_test_server(&listener);
        let server = thread::spawn(move || {
            let (mut first_stream, _) = listener.accept().unwrap();
            let first_request = read_http_request(&mut first_stream);
            assert!(first_request
                .starts_with("GET /api/integrations/ai/business/capabilities HTTP/1.1"));
            assert!(!first_request
                .to_ascii_lowercase()
                .contains("if-none-match:"));
            let body = serde_json::to_vec(&json!({
                "schema_version": "1",
                "generation": "generation-one",
                "items": [{
                    "id": "business.example.list",
                    "version": "1.0.0",
                    "name": "示例列表",
                    "description": "读取示例。",
                    "risk_level": "read_only",
                    "http_method": "GET",
                    "scope": "business.example.read",
                    "dashboard_route": "/api/integrations/ai/business/examples",
                    "input_schema": {"type":"object","properties":{},"additionalProperties":false},
                    "execution_mode": "sync",
                    "supports_progress": false,
                    "supports_cancel": false,
                    "idempotency": "safe",
                    "retry_policy": "safe",
                    "concurrency": "parallel",
                    "approval_required": false
                }]
            }))
            .unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"catalog-one\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            first_stream.write_all(response.as_bytes()).unwrap();
            first_stream.write_all(&body).unwrap();

            let (mut second_stream, _) = listener.accept().unwrap();
            let second_request = read_http_request(&mut second_stream);
            assert!(second_request
                .to_ascii_lowercase()
                .contains("if-none-match: \"catalog-one\""));
            second_stream
                .write_all(
                    b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let etag = match fetch_catalog(&options, "").unwrap() {
            FetchResult::Modified { snapshot, etag } => {
                assert_eq!(snapshot.generation, "generation-one");
                assert_eq!(snapshot.items.len(), 1);
                etag
            }
            FetchResult::NotModified => panic!("initial catalog fetch must return content"),
        };
        assert_eq!(etag, "\"catalog-one\"");
        assert!(matches!(
            fetch_catalog(&options, &etag).unwrap(),
            FetchResult::NotModified
        ));
        server.join().unwrap();
    }

    #[test]
    fn dynamic_catalog_capability_executes_get_path_and_query_contract() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let options = options_for_test_server(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /api/integrations/ai/business/examples/%E5%B1%95%E9%A1%B9%201?q=hello HTTP/1.1"), "unexpected request: {request}");
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-access-token"));
            respond_json(stream);
        });
        let result = invoke_catalog_capability(
            &options,
            &DashboardCapabilityContract {
                id: "business.example.get".into(),
                version: "1.0.0".into(),
                name: "读取示例".into(),
                description: "读取示例".into(),
                risk_level: "read_only".into(),
                http_method: "GET".into(),
                scope: "business.example.read".into(),
                dashboard_route: "/api/integrations/ai/business/examples/{example_id}".into(),
                input_schema: json!({"type":"object","properties":{"example_id":{"type":"string"},"q":{"type":"string"}},"required":["example_id"]}),
                execution_mode: "sync".into(),
                supports_progress: false,
                supports_cancel: false,
                idempotency: "safe".into(),
                retry_policy: "safe".into(),
                concurrency: "parallel".into(),
                approval_required: false,
            },
            json!({"example_id":"展项 1", "q":"hello"}),
            "test-request-get",
            None,
        )
        .unwrap();
        assert_eq!(result, json!({"ok":true}));
        server.join().unwrap();
    }

    #[test]
    fn dynamic_catalog_capability_executes_write_body_contract() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let options = options_for_test_server(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(
                request.starts_with("POST /api/integrations/ai/business/examples/one HTTP/1.1"),
                "unexpected request: {request}"
            );
            assert!(request
                .to_ascii_lowercase()
                .contains("idempotency-key: himind-agent:test-request-write"));
            assert!(request
                .to_ascii_lowercase()
                .contains("x-himind-grant-id: grant-1"));
            let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
            assert_eq!(
                serde_json::from_str::<Value>(body).unwrap(),
                json!({"name":"demo"})
            );
            respond_json(stream);
        });
        let proof = ApprovalProof::Grant("grant-1".into());
        let result = invoke_catalog_capability(
            &options,
            &DashboardCapabilityContract {
                id: "business.example.create".into(),
                version: "1.0.0".into(),
                name: "创建示例".into(),
                description: "创建示例".into(),
                risk_level: "network_write".into(),
                http_method: "POST".into(),
                scope: "business.example.write".into(),
                dashboard_route: "/api/integrations/ai/business/examples/{example_id}".into(),
                input_schema: json!({"type":"object","properties":{"example_id":{"type":"string"},"name":{"type":"string"}},"required":["example_id","name"]}),
                execution_mode: "sync".into(),
                supports_progress: false,
                supports_cancel: false,
                idempotency: "conditional".into(),
                retry_policy: "idempotency_key".into(),
                concurrency: "keyed".into(),
                approval_required: false,
            },
            json!({"example_id":"one", "name":"demo"}),
            "test-request-write",
            Some(&proof),
        )
        .unwrap();
        assert_eq!(result, json!({"ok":true}));
        server.join().unwrap();
    }

    #[test]
    fn conditional_write_retries_once_on_transient_failure() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let options = options_for_test_server(&listener);
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_http_request(&mut first);
            assert!(request
                .to_ascii_lowercase()
                .contains("idempotency-key: himind-agent:test-request-retry"));
            first
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            let (mut second, _) = listener.accept().unwrap();
            let request = read_http_request(&mut second);
            assert!(request
                .to_ascii_lowercase()
                .contains("idempotency-key: himind-agent:test-request-retry"));
            respond_json(second);
        });
        let result = invoke_catalog_capability(
            &options,
            &DashboardCapabilityContract {
                id: "business.example.create".into(),
                version: "1.0.0".into(),
                name: "创建示例".into(),
                description: "创建示例".into(),
                risk_level: "network_write".into(),
                http_method: "POST".into(),
                scope: "business.example.write".into(),
                dashboard_route: "/api/integrations/ai/business/examples".into(),
                input_schema: json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
                execution_mode: "sync".into(),
                supports_progress: false,
                supports_cancel: false,
                idempotency: "conditional".into(),
                retry_policy: "idempotency_key".into(),
                concurrency: "keyed".into(),
                approval_required: false,
            },
            json!({"name":"demo"}),
            "test-request-retry",
            None,
        )
        .unwrap();
        assert_eq!(result, json!({"ok":true}));
        server.join().unwrap();
    }
}
