use chrono::DateTime;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::api::client::load_agent_state;
use crate::api::oauth::platform_access_token;
use crate::approval::policy;
use crate::store::approval_outbox::{self, ApprovalDecisionRecord};
use crate::Options;

#[derive(Debug, Clone)]
pub(crate) enum ApprovalProof {
    Approval(String),
    Grant(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionSync {
    /// Dashboard accepted the decision during this call.
    Synced,
    /// The decision is durably queued and will be replayed by the Worker.
    Queued,
}

#[derive(Debug, Deserialize)]
struct GrantList {
    #[serde(default)]
    items: Vec<Grant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Grant {
    #[serde(default)]
    id: String,
    #[serde(default)]
    agent_id: String,
    capability_id: String,
    #[serde(default)]
    capability_version: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    workspace_ref: String,
    #[serde(default)]
    resource_scope: Value,
    max_risk_level: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    policy_version: String,
    #[serde(default)]
    args_digest: String,
    #[serde(default)]
    generation: i64,
    #[serde(default)]
    expires_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedGrants {
    fetched_at: u64,
    agent_id: String,
    user_id: String,
    grants: Vec<Grant>,
}

#[derive(Debug)]
struct ApprovalHttpError {
    status: u16,
    message: String,
}

#[derive(Debug)]
struct ApprovalIdentityError(String);

impl fmt::Display for ApprovalIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ApprovalIdentityError {}

impl fmt::Display for ApprovalHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}（HTTP {}）", self.message, self.status)
    }
}

impl Error for ApprovalHttpError {}

const GRANT_CACHE_TTL_SECONDS: u64 = 15;

/// Return the active grant that covers one invocation, retaining its ID so the
/// downstream Dashboard mutation can enforce the same proof server-side.
pub(crate) fn active_grant(
    options: &Options,
    capability_id: &str,
    capability_version: &str,
    provider: &str,
    risk_level: &str,
    generation: i64,
    args_digest: &str,
    input: &Value,
) -> Result<Option<String>, Box<dyn Error>> {
    if !options.mode().dashboard_enabled() {
        return Ok(None);
    }
    let access = platform_access_token(options, "")?;
    let state = load_agent_state(&options.state_path)?;
    if access.agent_id.trim() != state.agent_id.trim() {
        return Err("Dashboard 授权与当前 Agent 实例不匹配".into());
    }
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let response = client
        .get(format!(
            "{}/api/grants?status=active",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent-approval")
        .send();
    let list = match response {
        Ok(response) if response.status().is_success() => {
            let list = response.json::<GrantList>()?;
            let _ = write_grant_cache(
                &options.state_path,
                &state.agent_id,
                &access.user_id,
                &list.items,
            );
            list.items
        }
        Ok(response) => {
            return cached_grant(
                &options.state_path,
                &state.agent_id,
                &access.user_id,
                capability_id,
                capability_version,
                provider,
                risk_level,
                generation,
                args_digest,
                input,
                format!(
                    "Dashboard Grant 查询失败（HTTP {}）",
                    response.status().as_u16()
                ),
            );
        }
        Err(error) => {
            return cached_grant(
                &options.state_path,
                &state.agent_id,
                &access.user_id,
                capability_id,
                capability_version,
                provider,
                risk_level,
                generation,
                args_digest,
                input,
                format!("Dashboard Grant 查询失败：{error}"),
            );
        }
    };
    Ok(matching_grant_id(
        list,
        &state.agent_id,
        capability_id,
        capability_version,
        provider,
        risk_level,
        generation,
        args_digest,
        input,
    ))
}

fn cached_grant(
    state_path: &Path,
    agent_id: &str,
    user_id: &str,
    capability_id: &str,
    capability_version: &str,
    provider: &str,
    risk_level: &str,
    generation: i64,
    args_digest: &str,
    input: &Value,
    network_error: String,
) -> Result<Option<String>, Box<dyn Error>> {
    // High-risk destructive calls never use an offline cache: revocation and
    // expiry must take effect immediately for R3/R4 operations.
    if policy::risk_rank(risk_level) >= policy::risk_rank("R3") {
        return Err(network_error.into());
    }
    let path = state_path.with_file_name("approval-grants.cache");
    let Ok(encoded) = fs::read(&path) else {
        return Err(network_error.into());
    };
    let Ok(protected) = String::from_utf8(encoded) else {
        return Err(network_error.into());
    };
    let Ok(payload) = crate::store::credentials::unprotect_secret_for_current_user(&protected)
    else {
        return Err(network_error.into());
    };
    let Ok(snapshot) = serde_json::from_str::<CachedGrants>(&payload) else {
        return Err(network_error.into());
    };
    if snapshot.agent_id != agent_id
        || snapshot.user_id != user_id
        || unix_now().saturating_sub(snapshot.fetched_at) > GRANT_CACHE_TTL_SECONDS
    {
        return Err(network_error.into());
    }
    Ok(matching_grant_id(
        snapshot.grants,
        agent_id,
        capability_id,
        capability_version,
        provider,
        risk_level,
        generation,
        args_digest,
        input,
    ))
}

#[allow(clippy::too_many_arguments)]
fn matching_grant_id(
    grants: Vec<Grant>,
    agent_id: &str,
    capability_id: &str,
    capability_version: &str,
    provider: &str,
    risk_level: &str,
    generation: i64,
    args_digest: &str,
    input: &Value,
) -> Option<String> {
    let required_risk = policy::risk_rank(risk_level);
    grants.into_iter().find_map(|grant| {
        if grant.status == "active"
            && grant.policy_version == policy::APPROVAL_POLICY_VERSION
            && grant_not_expired(&grant)
            && (grant.agent_id.trim().is_empty() || grant.agent_id.trim() == agent_id.trim())
            && (grant.capability_id == "*" || grant.capability_id == capability_id)
            && (grant.capability_version.trim().is_empty()
                || grant.capability_version.trim() == capability_version.trim())
            && (grant.provider.trim().is_empty() || grant.provider.trim() == provider.trim())
            && policy::risk_rank(&grant.max_risk_level) >= required_risk
            && (grant.generation == generation
                || (grant.mode == "allow_all" && grant.generation == 0))
            && grant_mode_matches(&grant, input)
            && grant_args_match(&grant, args_digest)
            && resource_scope_matches(&grant.resource_scope, input)
            && !grant.id.trim().is_empty()
        {
            Some(grant.id)
        } else {
            None
        }
    })
}

fn grant_not_expired(grant: &Grant) -> bool {
    DateTime::parse_from_rfc3339(grant.expires_at.trim())
        .map(|expires_at| expires_at.timestamp() > unix_now() as i64)
        .unwrap_or(false)
}

fn grant_args_match(grant: &Grant, args_digest: &str) -> bool {
    match grant.mode.as_str() {
        "allow_for_capability_scope" | "allow_until_expiry" => {
            !grant.args_digest.trim().is_empty() && grant.args_digest.trim() == args_digest.trim()
        }
        _ => true,
    }
}

fn write_grant_cache(
    state_path: &Path,
    agent_id: &str,
    user_id: &str,
    grants: &[Grant],
) -> Result<(), Box<dyn Error>> {
    let payload = serde_json::to_string(&CachedGrants {
        fetched_at: unix_now(),
        agent_id: agent_id.to_string(),
        user_id: user_id.to_string(),
        grants: grants.to_vec(),
    })?;
    let protected = crate::store::credentials::protect_secret_for_current_user(&payload)?;
    let path = state_path.with_file_name("approval-grants.cache");
    let _lock = crate::store::atomic_file::lock(&path)?;
    crate::store::atomic_file::atomic_write(&path, protected.as_bytes())?;
    Ok(())
}

/// Create a Dashboard approval request for a local interactive decision.
/// The delegated Agent token is authenticated as the bound Dashboard user;
/// the Dashboard remains the durable approval fact source.
pub(crate) fn create_approval(
    options: &Options,
    agent_id: &str,
    source_id: &str,
    capability_id: &str,
    capability_version: &str,
    provider: &str,
    risk_level: &str,
    target_scope: &Value,
    impact_summary: &str,
    args_digest: &str,
    generation: i64,
    ttl_seconds: i32,
) -> Result<String, Box<dyn Error>> {
    let access = platform_access_token(options, "")?;
    let state = load_agent_state(&options.state_path)?;
    if access.agent_id.trim() != state.agent_id.trim() || access.agent_id.trim() != agent_id.trim()
    {
        return Err("Dashboard 授权与当前 Agent 实例不匹配".into());
    }
    let body = serde_json::json!({
        "agent_id": agent_id,
        "source_type": "agent_local",
        "source_id": source_id,
        "capability_id": capability_id,
        "capability_version": capability_version,
        "provider": provider,
        "risk_level": risk_level,
        "target_scope": target_scope,
        "impact_summary": impact_summary,
        "args_digest": args_digest,
        "policy_snapshot": {
            "version": policy::APPROVAL_POLICY_VERSION,
            "risk_level": risk_level,
        },
        "generation": generation,
        "ttl_seconds": ttl_seconds,
    });
    let value = approval_request(
        options,
        reqwest::Method::POST,
        &["api", "approvals"],
        Some(body),
        None,
    )?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Dashboard 未返回 approval_id".into())
}

/// Persist the local UI decision in the Dashboard approval fact model.
///
/// The decision is written to a DPAPI-protected outbox *before* the network
/// call. This makes a local approval durable across process crashes and lets
/// the Worker replay it after Dashboard connectivity returns. Dashboard's
/// idempotency key makes retries safe.
pub(crate) fn decide_approval(
    options: &Options,
    approval_id: &str,
    approved: bool,
    idempotency_key: &str,
) -> Result<DecisionSync, Box<dyn Error>> {
    let approval_id = approval_id.trim();
    if approval_id.is_empty() {
        return Err("approval_id 不能为空".into());
    }
    let idempotency_key = if idempotency_key.trim().is_empty() {
        format!("approval:{approval_id}")
    } else {
        idempotency_key.trim().to_string()
    };
    let (agent_id, user_id) = match platform_access_token(options, "") {
        Ok(access) => (access.agent_id, access.user_id),
        Err(error) => {
            crate::api::oauth::persisted_authorization_identity(&options.state_path).ok_or(error)?
        }
    };
    let mut record = ApprovalDecisionRecord {
        approval_id: approval_id.to_string(),
        approved,
        idempotency_key,
        agent_id,
        user_id,
        created_at: unix_now(),
        status: "pending".to_string(),
        attempt_count: 0,
        next_attempt_at: 0,
        last_error: String::new(),
    };
    let _path = approval_outbox::store(&options.state_path, &record)?;
    match send_decision(options, &record) {
        Ok(()) => {
            // Re-read to remove the deterministic record created above. A
            // concurrent heartbeat flush may already have removed it.
            if let Ok(items) = approval_outbox::list(&options.state_path) {
                if let Some((path, _)) = items.into_iter().find(|(_, item)| {
                    item.approval_id == record.approval_id
                        && item.idempotency_key == record.idempotency_key
                }) {
                    let _ = approval_outbox::remove(&path);
                }
            }
            Ok(DecisionSync::Synced)
        }
        Err(error) if is_retryable_error(error.as_ref()) => {
            let safe_error = sanitized_error(error.as_ref());
            approval_outbox::schedule_retry(&mut record, unix_now(), &safe_error);
            // A failed rewrite must not hide the original durable record. The
            // initial store succeeded; retaining the old schedule is safer.
            let _ = approval_outbox::store(&options.state_path, &record);
            Ok(DecisionSync::Queued)
        }
        Err(error) => {
            let safe_error = sanitized_error(error.as_ref());
            approval_outbox::mark_dead_letter(&mut record, &safe_error);
            let _ = approval_outbox::store(&options.state_path, &record);
            Err(error)
        }
    }
}

fn send_decision(options: &Options, record: &ApprovalDecisionRecord) -> Result<(), Box<dyn Error>> {
    let access = platform_access_token(options, "")?;
    if record.agent_id.trim().is_empty()
        || record.user_id.trim().is_empty()
        || access.agent_id.trim() != record.agent_id.trim()
        || access.user_id.trim() != record.user_id.trim()
    {
        return Err(Box::new(ApprovalIdentityError(
            "本地审批 outbox 与当前 Dashboard 用户或 Agent 实例不匹配".to_string(),
        )));
    }
    let body = serde_json::json!({
        "decision": if record.approved { "approved" } else { "rejected" },
        "channel": "agent_desktop",
        "grant_mode": "allow_once",
        "idempotency_key": record.idempotency_key,
    });
    let _ = approval_request(
        options,
        reqwest::Method::POST,
        &["api", "approvals", record.approval_id.as_str(), "decisions"],
        Some(body),
        Some(&record.idempotency_key),
    )?;
    Ok(())
}

/// Replay due local approval decisions. Returns the number acknowledged by
/// Dashboard; individual failures are retained with exponential backoff.
pub(crate) fn flush_approval_decision_outbox(options: &Options) -> Result<usize, Box<dyn Error>> {
    let records = approval_outbox::list(&options.state_path)?;
    let now = unix_now();
    let mut sent = 0;
    for (path, mut record) in records {
        if !approval_outbox::is_due(&record, now) {
            continue;
        }
        match send_decision(options, &record) {
            Ok(()) => {
                approval_outbox::remove(&path)?;
                sent += 1;
            }
            Err(error) => {
                let safe_error = sanitized_error(error.as_ref());
                if is_retryable_error(error.as_ref()) {
                    approval_outbox::schedule_retry(&mut record, now, &safe_error);
                } else {
                    approval_outbox::mark_dead_letter(&mut record, &safe_error);
                }
                let _ = approval_outbox::store(&options.state_path, &record);
            }
        }
    }
    Ok(sent)
}

fn is_retryable_error(error: &(dyn Error + 'static)) -> bool {
    if error.downcast_ref::<ApprovalIdentityError>().is_some() {
        return false;
    }
    let Some(http_error) = error.downcast_ref::<ApprovalHttpError>() else {
        // Transport, token-refresh and local I/O errors are transient from the
        // approval writer's perspective and should remain durable for replay.
        return true;
    };
    matches!(http_error.status, 401 | 408 | 425 | 429) || http_error.status >= 500
}

fn sanitized_error(error: &(dyn Error + 'static)) -> String {
    crate::approval::manager::redact_message(&error.to_string())
}

fn approval_request(
    options: &Options,
    method: reqwest::Method,
    path: &[&str],
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let access = platform_access_token(options, "")?;
    let state = load_agent_state(&options.state_path)?;
    if access.agent_id.trim() != state.agent_id.trim() {
        return Err("Dashboard 授权与当前 Agent 实例不匹配".into());
    }
    let mut url = url::Url::parse(&options.api_base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Dashboard API URL cannot be a base")?;
        segments.pop_if_empty();
        for segment in path {
            segments.push(segment);
        }
    }
    let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
    let mut request = client
        .request(method, url)
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent-approval");
    if let Some(key) = idempotency_key {
        request = request.header("Idempotency-Key", key);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send()?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Dashboard 审批调用失败");
        return Err(Box::new(ApprovalHttpError {
            status: status.as_u16(),
            message: message.to_string(),
        }));
    }
    Ok(value)
}

fn resource_scope_matches(scope: &Value, input: &Value) -> bool {
    let Some(scope) = scope.as_object() else {
        return true;
    };
    if scope.is_empty() {
        return true;
    }
    scope.iter().all(|(key, expected)| {
        let Some(actual) = input.get(key) else {
            return false;
        };
        if key == "path" {
            path_scope_matches(
                expected.as_str().unwrap_or_default(),
                actual.as_str().unwrap_or_default(),
            )
        } else {
            actual == expected
        }
    })
}

fn grant_mode_matches(grant: &Grant, input: &Value) -> bool {
    match grant.mode.as_str() {
        "allow_all" | "allow_until_expiry" | "allow_for_capability_scope" => {
            grant.workspace_ref.trim().is_empty()
                || invocation_scope_ref(input)
                    .is_some_and(|actual| scope_ref_matches(&grant.workspace_ref, actual))
        }
        "allow_for_workspace" => {
            !grant.workspace_ref.trim().is_empty()
                && invocation_scope_ref(input)
                    .is_some_and(|actual| scope_ref_matches(&grant.workspace_ref, actual))
        }
        "allow_for_run" => {
            !grant.workspace_ref.trim().is_empty()
                && ["run_id", "agent_run_id", "work_item_id"]
                    .into_iter()
                    .filter_map(|key| input.get(key).and_then(Value::as_str))
                    .any(|actual| actual.trim() == grant.workspace_ref.trim())
        }
        _ => false,
    }
}

fn invocation_scope_ref(input: &Value) -> Option<&str> {
    ["workspace_ref", "workspace_root", "path"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
}

fn scope_ref_matches(expected: &str, actual: &str) -> bool {
    if Path::new(expected).is_absolute() || Path::new(actual).is_absolute() {
        path_scope_matches(expected, actual)
    } else {
        expected.trim() == actual.trim()
    }
}

fn path_scope_matches(expected: &str, actual: &str) -> bool {
    let Some(expected) = normalize_scope_path(expected) else {
        return false;
    };
    let Some(actual) = normalize_scope_path(actual) else {
        return false;
    };
    actual == expected || actual.starts_with(&format!("{expected}\\"))
}

fn normalize_scope_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    let normalized = fs::canonicalize(path).ok().or_else(|| lexical_path(path))?;
    let value = normalized
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string();
    if value.is_empty() {
        return None;
    }
    Some(if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    })
}

fn lexical_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    return None;
                }
                normalized.pop();
            }
        }
    }
    Some(normalized)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        grant_mode_matches, is_retryable_error, matching_grant_id, resource_scope_matches,
        ApprovalHttpError, Grant,
    };
    use serde_json::json;

    #[test]
    fn resource_scope_is_exact_for_ids_and_prefix_safe_for_paths() {
        assert!(resource_scope_matches(
            &json!({}),
            &json!({"path":"C:/work/a.txt"})
        ));
        assert!(resource_scope_matches(
            &json!({"project_id":"p1"}),
            &json!({"project_id":"p1"})
        ));
        assert!(!resource_scope_matches(
            &json!({"project_id":"p1"}),
            &json!({"project_id":"p2"})
        ));
        assert!(resource_scope_matches(
            &json!({"path":"C:/work"}),
            &json!({"path":"C:/work/file.txt"})
        ));
        assert!(!resource_scope_matches(
            &json!({"path":"C:/work"}),
            &json!({"path":"C:/workspace/file.txt"})
        ));
        assert!(!resource_scope_matches(
            &json!({"path":"C:/work"}),
            &json!({"path":"C:/work/../outside/file.txt"})
        ));
        assert!(!resource_scope_matches(
            &json!({"path":"C:/work"}),
            &json!({"path":"relative/file.txt"})
        ));
    }

    fn grant(mode: &str, workspace_ref: &str) -> Grant {
        Grant {
            id: "grant-1".into(),
            agent_id: String::new(),
            capability_id: "example.write".into(),
            capability_version: String::new(),
            provider: String::new(),
            workspace_ref: workspace_ref.into(),
            resource_scope: json!({}),
            max_risk_level: "R2".into(),
            mode: mode.into(),
            status: "active".into(),
            policy_version: crate::approval::policy::APPROVAL_POLICY_VERSION.into(),
            args_digest: String::new(),
            generation: 0,
            expires_at: "2999-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn grant_modes_enforce_run_and_workspace_binding() {
        assert!(grant_mode_matches(
            &grant("allow_for_workspace", "C:/work"),
            &json!({"workspace_root":"C:/work/project"})
        ));
        assert!(!grant_mode_matches(
            &grant("allow_for_workspace", "C:/work"),
            &json!({"workspace_root":"C:/other"})
        ));
        assert!(grant_mode_matches(
            &grant("allow_for_run", "run-1"),
            &json!({"run_id":"run-1"})
        ));
        assert!(!grant_mode_matches(
            &grant("allow_for_run", "run-1"),
            &json!({"run_id":"run-2"})
        ));
    }

    #[test]
    fn grant_metadata_rejects_stale_policy_generation_args_and_expiry() {
        let input = json!({"project_id":"p1"});
        let mut current = grant("allow_until_expiry", "");
        current.args_digest = "digest-1".into();
        current.generation = 7;
        assert_eq!(
            matching_grant_id(
                vec![current.clone()],
                "agent-1",
                "example.write",
                "1.0.0",
                "provider-1",
                "R2",
                7,
                "digest-1",
                &input,
            )
            .as_deref(),
            Some("grant-1")
        );

        for stale in [
            {
                let mut grant = current.clone();
                grant.policy_version = "old-policy".into();
                grant
            },
            {
                let mut grant = current.clone();
                grant.generation = 0;
                grant
            },
            {
                let mut grant = current.clone();
                grant.generation = 8;
                grant
            },
            {
                let mut grant = current.clone();
                grant.args_digest = "digest-2".into();
                grant
            },
            {
                let mut grant = current.clone();
                grant.expires_at = "2020-01-01T00:00:00Z".into();
                grant
            },
        ] {
            assert!(matching_grant_id(
                vec![stale],
                "agent-1",
                "example.write",
                "1.0.0",
                "provider-1",
                "R2",
                7,
                "digest-1",
                &input,
            )
            .is_none());
        }

        let mut allow_all = grant("allow_all", "");
        allow_all.capability_id = "*".into();
        assert_eq!(
            matching_grant_id(
                vec![allow_all],
                "agent-1",
                "example.write",
                "1.0.0",
                "provider-1",
                "R2",
                7,
                "digest-1",
                &input,
            )
            .as_deref(),
            Some("grant-1")
        );
    }

    #[test]
    fn only_transient_http_failures_are_replayed() {
        assert!(is_retryable_error(&ApprovalHttpError {
            status: 503,
            message: "unavailable".into(),
        }));
        assert!(is_retryable_error(&ApprovalHttpError {
            status: 401,
            message: "expired token".into(),
        }));
        assert!(!is_retryable_error(&ApprovalHttpError {
            status: 409,
            message: "approval expired".into(),
        }));
    }
}
