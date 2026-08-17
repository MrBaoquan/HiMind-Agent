use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::api::client::update_agent_run_status;
use crate::api::types::{AgentRunClaim, Task};
use crate::capability::service::CapabilityGateway;
use crate::capability::types::{InvocationContext, InvocationSource};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope, PROVIDER_NATIVE};
use crate::store::types::LocalWorkerStatus;
use crate::Options;

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    execute_managed(
        client,
        options,
        agent_id,
        task,
        envelope,
        PROVIDER_NATIVE,
        |claim| execute_claimed(client, options, agent_id, claim),
    )
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
) -> Result<Value, Box<dyn Error>> {
    let plan = claim
        .run
        .input
        .as_object()
        .ok_or("native Agent Run input must be an object")?;
    let capability_id = plan
        .get("capability_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if capability_id.is_empty() {
        return Err("native Agent Run is missing capability_id".into());
    }
    let mut capability_input = plan
        .get("capability_input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if capability_input.is_null() {
        capability_input = json!({});
    }
    validate_capability_scope(capability_id, &mut capability_input, claim)?;

    update_agent_run_status(
        client,
        &options.api_base,
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = process::start_run_lease_renewal(client, options, agent_id, claim);

    let worker_status = Arc::new(Mutex::new(LocalWorkerStatus::default()));
    let gateway = CapabilityGateway::new(options.clone(), worker_status);
    let context = InvocationContext::new(
        InvocationSource::DashboardWorker,
        format!("dashboard-user:{}", claim.run.created_by_user_id),
    );
    let result = gateway.invoke(&context, capability_id, capability_input.clone())?;
    let detail = process::summarize_output(&result.to_string(), 2_048);
    let final_message = json!({
        "summary": format!("内置能力 {capability_id} 已完成"),
        "changes": [],
        "verification": [{"name": capability_id, "status": "passed", "detail": detail}],
        "remaining_risks": []
    });
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": PROVIDER_NATIVE,
        "completed": true,
        "final_message": serde_json::to_string(&final_message)?,
        "billing_owner": "user",
        "capability_id": capability_id,
        "capability_result": result
    }))
}

fn validate_capability_scope(
    capability_id: &str,
    input: &mut Value,
    claim: &AgentRunClaim,
) -> Result<(), Box<dyn Error>> {
    let path_key = match capability_id {
        "system.open_folder" => Some("path"),
        "exhibit.workspace.build" | "exhibit.workspace.open" => Some("target_path"),
        _ => None,
    };
    let Some(path_key) = path_key else {
        return Ok(());
    };
    let object = input
        .as_object_mut()
        .ok_or("native capability input must be an object")?;
    if object.get(path_key).and_then(Value::as_str).is_none() {
        if !claim.workspace_path.trim().is_empty()
            && matches!(
                capability_id,
                "exhibit.workspace.build" | "exhibit.workspace.open"
            )
        {
            object.insert(
                path_key.to_string(),
                Value::String(claim.workspace_path.clone()),
            );
        } else {
            return Err(format!("native capability requires {path_key}").into());
        }
    }
    let requested = object
        .get(path_key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if requested.is_empty() {
        return Err(format!("native capability requires {path_key}").into());
    }
    let access_mode = if claim.run.access_mode.trim().is_empty() {
        claim.access_mode.trim()
    } else {
        claim.run.access_mode.trim()
    };
    if access_mode != crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED {
        return Ok(());
    }
    let workspace = Path::new(claim.workspace_path.trim())
        .canonicalize()
        .map_err(|error| format!("Agent workspace is unavailable: {error}"))?;
    let target = Path::new(requested)
        .canonicalize()
        .map_err(|error| format!("requested local path is unavailable: {error}"))?;
    if !target.starts_with(&workspace) {
        return Err("requested local path is outside the Agent exhibit-linked workspace".into());
    }
    Ok(())
}
