use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::error::Error;

use crate::agent_core_service::AgentCoreRunRecorder;
use crate::api::client::{claim_agent_run, is_task_canceled_error, update_agent_run_status};
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::Options;

pub(crate) mod acp;
pub(crate) mod artifacts;
pub(crate) mod builtin;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod deepseek_harness;
pub(crate) mod distribution;
pub(crate) mod native;
pub(crate) mod process;

pub(crate) const PROVIDER_NATIVE: &str = "himind.native";
pub(crate) const PROVIDER_BUILTIN: &str = "himind.builtin";
pub(crate) const PROVIDER_CODEX: &str = "personal.codex";
pub(crate) const PROVIDER_GITHUB_COPILOT: &str = "personal.github-copilot";

const EXECUTION_SUMMARY_LIMIT: usize = 32 * 1024;
const EXECUTION_LIST_ITEM_LIMIT: usize = 2 * 1024;
const EXECUTION_LIST_LIMIT: usize = 100;

#[derive(Clone, Debug, Deserialize, Default)]
struct ProviderExecutionReport {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    changes: Vec<String>,
    #[serde(default)]
    verification: Vec<ProviderVerification>,
    #[serde(default)]
    remaining_risks: Vec<String>,
    #[serde(default)]
    output_artifacts: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct ProviderVerification {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    detail: String,
}

pub(crate) fn probe_installations() -> Vec<RuntimeInstallationReport> {
    let mut reports = vec![builtin::probe(), codex::probe(), copilot::probe()];
    reports.extend(acp::probe_installations());
    reports
}

pub(crate) fn probe_acp_executables() -> Value {
    acp::probe_executables()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentRunEnvelope {
    pub(crate) run_id: String,
    pub(crate) runtime_provider: String,
    #[serde(default)]
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) attempt_no: u32,
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
) -> Result<Value, Box<dyn Error>> {
    let envelope = parse_envelope(task)?;
    match envelope.runtime_provider.as_str() {
        PROVIDER_NATIVE => native::execute(client, options, agent_id, task, &envelope),
        PROVIDER_BUILTIN => builtin::execute(client, options, agent_id, task, &envelope),
        PROVIDER_CODEX => codex::execute(client, options, agent_id, task, &envelope),
        PROVIDER_GITHUB_COPILOT => copilot::execute(client, options, agent_id, task, &envelope),
        provider if acp::is_provider(provider) => {
            acp::execute(client, options, agent_id, task, &envelope)
        }
        provider => Err(format!("unsupported Agent Run provider: {provider}").into()),
    }
}

fn parse_envelope(task: &Task) -> Result<AgentRunEnvelope, Box<dyn Error>> {
    let envelope = serde_json::from_value::<AgentRunEnvelope>(
        task.payload
            .clone()
            .ok_or("Agent Run task is missing its execution envelope")?,
    )?;
    if envelope.run_id.trim().is_empty() || envelope.runtime_provider.trim().is_empty() {
        return Err("Agent Run task envelope is incomplete".into());
    }
    Ok(envelope)
}

pub(super) fn execute_managed<F>(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
    expected_provider: &str,
    executor: F,
) -> Result<Value, Box<dyn Error>>
where
    F: FnOnce(&AgentRunClaim) -> Result<Value, Box<dyn Error>>,
{
    if envelope.runtime_provider != expected_provider {
        return Err(format!(
            "Agent Run provider mismatch: expected {expected_provider}, received {}",
            envelope.runtime_provider
        )
        .into());
    }
    let credential = options.agent_credential();
    let claim = claim_agent_run(
        client,
        &options.api_base(),
        agent_id,
        &task.id,
        &envelope.run_id,
        &credential,
    )?;
    if claim.run.id != envelope.run_id || claim.run.runtime_provider != expected_provider {
        return Err("Dashboard returned an invalid Agent Run identity or provider".into());
    }
    if claim.run.status != "claimed" || claim.run.created_by_user_id.trim().is_empty() {
        return Err("Dashboard returned an invalid Agent Run status or user identity".into());
    }
    let local_context =
        agent_run_invocation_context(options, agent_id, task, envelope, &claim, expected_provider);
    let local_recording = match AgentCoreRunRecorder::open_default() {
        Ok(recorder) => match recorder.begin(agent_id, &local_context, expected_provider) {
            Ok(run) => Some((recorder, run)),
            Err(error) => {
                eprintln!(
                    "Agent Run local run begin failed for {}: {error}",
                    claim.run.id
                );
                None
            }
        },
        Err(error) => {
            eprintln!(
                "Agent Run local run ledger unavailable for {}: {error}",
                claim.run.id
            );
            None
        }
    };

    let settings = crate::app::remote_execution::load(&options.state_path)?;
    if !settings.enabled {
        return Err("remote execution is disabled on this Agent".into());
    }
    let access_mode = if claim.run.access_mode.trim().is_empty() {
        claim.access_mode.trim()
    } else {
        claim.run.access_mode.trim()
    };
    match access_mode {
        crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED => {
            if claim.workspace_path.trim().is_empty() {
                return Err("exhibit-linked Agent Run is missing its associated directory".into());
            }
        }
        crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED => {
            if settings.access_mode
                != crate::app::remote_execution::ACCESS_MODE_MACHINE_UNRESTRICTED
            {
                return Err("full computer access is not enabled on this Agent".into());
            }
        }
        _ => return Err("Dashboard returned an unsupported Agent Run access mode".into()),
    }

    let execution = executor(&claim).and_then(|value| {
        let _artifact_lease = process::start_run_lease_renewal(client, options, agent_id, &claim);
        let value = artifacts::prepare_execution_result(client, options, agent_id, &claim, value)?;
        let value = normalize_execution_result(value, expected_provider);
        update_agent_run_status(
            client,
            &options.api_base(),
            agent_id,
            &claim.run.id,
            &claim.claim_token,
            "succeeded",
            Some(&value),
            "",
            &credential,
        )?;
        Ok(value)
    });
    match execution {
        Ok(value) => {
            if let Some((recorder, run)) = local_recording {
                if let Err(error) = recorder.complete(run, &value) {
                    eprintln!(
                        "Agent Run local run completion failed for {}: {error}",
                        claim.run.id
                    );
                }
            }
            Ok(value)
        }
        Err(error) => {
            let message = process::redact_error(&error.to_string(), &claim, &credential);
            let status = if is_task_canceled_error(&message) {
                "canceled"
            } else {
                "failed"
            };
            if let Err(report_error) = update_agent_run_status(
                client,
                &options.api_base(),
                agent_id,
                &claim.run.id,
                &claim.claim_token,
                status,
                None,
                &message,
                &credential,
            ) {
                eprintln!(
                    "Agent Run {} failure report failed: {report_error}",
                    claim.run.id
                );
            }
            if let Some((recorder, run)) = local_recording {
                let recording_result = if status == "canceled" {
                    recorder.cancel(run, &message)
                } else {
                    recorder.fail(run, &message)
                };
                if let Err(recording_error) = recording_result {
                    eprintln!(
                        "Agent Run local run terminal update failed for {}: {recording_error}",
                        claim.run.id
                    );
                }
            }
            Err(message.into())
        }
    }
}

fn agent_run_invocation_context(
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
    claim: &AgentRunClaim,
    runtime_provider: &str,
) -> InvocationContext {
    let work_item_id = if envelope.work_item_id.trim().is_empty() {
        claim.run.work_item_id.trim()
    } else {
        envelope.work_item_id.trim()
    };
    let attempt_no = if envelope.attempt_no > 0 {
        envelope.attempt_no
    } else {
        claim.run.attempt_no
    };
    InvocationContext::new(
        InvocationSource::DashboardWorker,
        format!("dashboard-user:{}", claim.run.created_by_user_id.trim()),
    )
    .with_workspace_ref(claim.workspace_path.clone())
    .with_device_id(crate::agent_core_service::current_device_id(
        &options.state_path,
    ))
    .with_business_context(json!({
        "agent_run": {
            "run_id": claim.run.id,
            "work_item_id": work_item_id,
            "parent_run_id": claim.run.parent_run_id,
            "root_run_id": claim.run.root_run_id,
            "parent_work_item_id": claim.run.parent_work_item_id,
            "root_work_item_id": claim.run.root_work_item_id,
            "task_id": task.id,
            "attempt_no": attempt_no,
            "runtime_provider": runtime_provider,
            "requested_agent_id": if claim.run.requested_agent_id.trim().is_empty() {
                agent_id
            } else {
                claim.run.requested_agent_id.trim()
            },
            "project_id": claim.run.project_id,
            "exhibit_pid": claim.run.exhibit_pid,
            "workspace_ref": claim.workspace_path,
        }
    }))
}

pub(super) fn normalize_execution_result(value: Value, runtime_provider: &str) -> Value {
    let final_message = value
        .get("final_message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let report =
        parse_provider_execution_report(final_message).unwrap_or_else(|| ProviderExecutionReport {
            summary: process::summarize_output(final_message.trim(), EXECUTION_SUMMARY_LIMIT),
            ..ProviderExecutionReport::default()
        });
    let summary = process::summarize_output(report.summary.trim(), EXECUTION_SUMMARY_LIMIT);
    let changes = normalize_execution_list(report.changes);
    let remaining_risks = normalize_execution_list(report.remaining_risks);
    let verification = report
        .verification
        .into_iter()
        .take(EXECUTION_LIST_LIMIT)
        .filter_map(|item| {
            let name = process::summarize_output(item.name.trim(), EXECUTION_LIST_ITEM_LIMIT);
            if name.is_empty() {
                return None;
            }
            let normalized_status = item.status.trim().to_ascii_lowercase();
            let status = if matches!(
                normalized_status.as_str(),
                "passed" | "pass" | "success" | "ok"
            ) {
                "passed"
            } else if matches!(normalized_status.as_str(), "failed" | "fail" | "error") {
                "failed"
            } else {
                "not_run"
            };
            Some(json!({
                "name": name,
                "status": status,
                "detail": process::summarize_output(item.detail.trim(), EXECUTION_LIST_ITEM_LIMIT),
            }))
        })
        .collect::<Vec<_>>();
    let output_artifacts = report
        .output_artifacts
        .into_iter()
        .take(10)
        .filter_map(|item| {
            let object = item.as_object()?;
            let file_object_id = object.get("file_object_id").and_then(Value::as_str)?.trim();
            let title = object.get("title").and_then(Value::as_str)?.trim();
            let name = object.get("name").and_then(Value::as_str)?.trim();
            let content_type = object.get("content_type").and_then(Value::as_str)?.trim();
            let artifact_type = object.get("artifact_type").and_then(Value::as_str)?.trim();
            if file_object_id.is_empty() || title.is_empty() || name.is_empty() || content_type.is_empty() || artifact_type.is_empty() {
                return None;
            }
            Some(json!({
                "file_object_id": process::summarize_output(file_object_id, 300),
                "artifact_type": process::summarize_output(artifact_type, 80),
                "title": process::summarize_output(title, 240),
                "content_type": process::summarize_output(content_type, 160),
                "name": process::summarize_output(name, 500),
                "size_bytes": object.get("size_bytes").and_then(Value::as_i64).unwrap_or(0).max(0),
                "sha256": process::summarize_output(object.get("sha256").and_then(Value::as_str).unwrap_or_default(), 128),
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "agent_execution_result.v1",
        "run_id": value.get("run_id").and_then(Value::as_str).unwrap_or_default(),
        "runtime_provider": runtime_provider,
        "completed": value.get("completed").and_then(Value::as_bool).unwrap_or(false),
        "billing_owner": value.get("billing_owner").and_then(Value::as_str).unwrap_or_default(),
        "summary": summary,
        "changes": changes,
        "verification": verification,
        "remaining_risks": remaining_risks,
        "provider_session_id": value.get("session_id").and_then(Value::as_str).unwrap_or_default(),
        "provider_version": value.get("version").and_then(Value::as_str).unwrap_or_default(),
        "output_artifacts": output_artifacts,
    })
}

fn normalize_execution_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .take(EXECUTION_LIST_LIMIT)
        .map(|value| process::summarize_output(value.trim(), EXECUTION_LIST_ITEM_LIMIT))
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_provider_execution_report(value: &str) -> Option<ProviderExecutionReport> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut candidates = vec![value];
    if value.starts_with("```") && value.ends_with("```") {
        let without_opening = value
            .strip_prefix("```json")
            .or_else(|| value.strip_prefix("```JSON"))
            .or_else(|| value.strip_prefix("```"))?;
        candidates.push(without_opening.strip_suffix("```")?.trim());
    }
    candidates.extend(
        value
            .lines()
            .rev()
            .map(str::trim)
            .filter(|line| line.starts_with('{')),
    );
    if let Some(report) = candidates.into_iter().find_map(|candidate| {
        let report = serde_json::from_str::<ProviderExecutionReport>(candidate).ok()?;
        (!report.summary.trim().is_empty()).then_some(report)
    }) {
        return Some(report);
    }
    value.lines().rev().find_map(|line| {
        let event = serde_json::from_str::<Value>(line.trim()).ok()?;
        find_provider_execution_report(&event, 0)
    })
}

fn find_provider_execution_report(value: &Value, depth: usize) -> Option<ProviderExecutionReport> {
    if depth > 8 {
        return None;
    }
    match value {
        Value::String(text) => {
            let text = text.trim();
            let candidate = text
                .strip_prefix("```json")
                .or_else(|| text.strip_prefix("```JSON"))
                .or_else(|| text.strip_prefix("```"))
                .and_then(|value| value.strip_suffix("```"))
                .map(str::trim)
                .unwrap_or(text);
            let report = serde_json::from_str::<ProviderExecutionReport>(candidate).ok()?;
            (!report.summary.trim().is_empty()).then_some(report)
        }
        Value::Array(items) => items
            .iter()
            .rev()
            .find_map(|item| find_provider_execution_report(item, depth + 1)),
        Value::Object(fields) => {
            if let Ok(report) = serde_json::from_value::<ProviderExecutionReport>(value.clone()) {
                if !report.summary.trim().is_empty() {
                    return Some(report);
                }
            }
            fields
                .values()
                .find_map(|item| find_provider_execution_report(item, depth + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        agent_run_invocation_context, normalize_execution_result, parse_envelope, PROVIDER_BUILTIN,
        PROVIDER_CODEX,
    };
    use crate::api::types::Task;
    use crate::api::types::{AgentRun, AgentRunClaim};
    use serde_json::json;

    #[test]
    fn parses_provider_agnostic_agent_run_envelope() {
        let task = Task {
            id: "task-1".to_string(),
            task_type: "agent_run".to_string(),
            detail: None,
            payload: Some(json!({
                "run_id":"run-1",
                "runtime_provider":PROVIDER_CODEX,
                "work_item_id":"ai_work_1",
                "attempt_no":2
            })),
            execution_id: String::new(),
            lease_id: String::new(),
            lease_expires_at: None,
        };
        let envelope = parse_envelope(&task).unwrap();
        assert_eq!(envelope.run_id, "run-1");
        assert_eq!(envelope.runtime_provider, PROVIDER_CODEX);
        assert_eq!(envelope.work_item_id, "ai_work_1");
        assert_eq!(envelope.attempt_no, 2);
    }

    #[test]
    fn agent_run_context_preserves_work_item_correlation() {
        let options = crate::Options::from_env();
        let task = Task {
            id: "tsk_1".to_string(),
            task_type: "agent_run".to_string(),
            detail: None,
            payload: Some(json!({})),
            execution_id: String::new(),
            lease_id: String::new(),
            lease_expires_at: None,
        };
        let envelope = super::AgentRunEnvelope {
            run_id: "agent_run_1".to_string(),
            runtime_provider: PROVIDER_CODEX.to_string(),
            work_item_id: "ai_work_1".to_string(),
            attempt_no: 2,
        };
        let claim = AgentRunClaim {
            run: AgentRun {
                id: "agent_run_1".to_string(),
                work_item_id: "ai_work_1".to_string(),
                parent_run_id: "agent_run_parent".to_string(),
                root_run_id: "agent_run_root".to_string(),
                parent_work_item_id: "ai_work_parent".to_string(),
                root_work_item_id: "ai_work_root".to_string(),
                attempt_no: 2,
                instruction: "Fix the issue".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "usr_123".to_string(),
                requested_agent_id: "agent-1".to_string(),
                project_id: "project-1".to_string(),
                exhibit_pid: "pid-1".to_string(),
                runtime_provider: PROVIDER_CODEX.to_string(),
                access_mode: "exhibit_linked".to_string(),
                input: json!({}),
            },
            claim_token: "claim-1".to_string(),
            workspace_path: "/workspace/project".to_string(),
            ai_model: String::new(),
            resume_provider_session_id: String::new(),
            access_mode: "exhibit_linked".to_string(),
        };
        let context = agent_run_invocation_context(
            &options,
            "agent-1",
            &task,
            &envelope,
            &claim,
            PROVIDER_CODEX,
        );
        assert_eq!(context.principal, "dashboard-user:usr_123");
        assert_eq!(context.workspace_ref, "/workspace/project");
        assert_eq!(
            context.business_context["agent_run"]["work_item_id"],
            "ai_work_1"
        );
        assert_eq!(context.business_context["agent_run"]["task_id"], "tsk_1");
        assert_eq!(
            context.business_context["agent_run"]["parent_run_id"],
            "agent_run_parent"
        );
        assert_eq!(
            context.business_context["agent_run"]["root_work_item_id"],
            "ai_work_root"
        );
    }

    #[test]
    fn normalizes_structured_provider_completion() {
        let value = normalize_execution_result(
            json!({
                "run_id":"run-1",
                "completed":true,
                "billing_owner":"user",
                "session_id":"session-1",
                "final_message":"{\"summary\":\"Fixed the issue\",\"changes\":[\"Updated validation\"],\"verification\":[{\"name\":\"cargo test\",\"status\":\"pass\",\"detail\":\"ok\"}],\"remaining_risks\":[]}"
            }),
            PROVIDER_CODEX,
        );
        assert_eq!(value["schema_version"], "agent_execution_result.v1");
        assert_eq!(value["summary"], "Fixed the issue");
        assert_eq!(value["verification"][0]["status"], "passed");
        assert_eq!(value["provider_session_id"], "session-1");
    }

    #[test]
    fn normalizes_structured_completion_nested_in_provider_event() {
        let value = normalize_execution_result(
            json!({
                "run_id":"run-1",
                "completed":true,
                "billing_owner":"himind",
                "final_message":"{\"type\":\"message\",\"payload\":{\"content\":\"{\\\"summary\\\":\\\"Implemented\\\",\\\"changes\\\":[],\\\"verification\\\":[{\\\"name\\\":\\\"tests\\\",\\\"status\\\":\\\"passed\\\",\\\"detail\\\":\\\"ok\\\"}],\\\"remaining_risks\\\":[]}\"}}"
            }),
            PROVIDER_BUILTIN,
        );
        assert_eq!(value["summary"], "Implemented");
        assert_eq!(value["verification"][0]["status"], "passed");
    }
}
