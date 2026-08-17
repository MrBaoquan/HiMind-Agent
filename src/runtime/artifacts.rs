use reqwest::blocking::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::api::client::{get_agent_run_artifact, upload_agent_run_artifact};
use crate::api::types::{AgentRunArtifactResponse, AgentRunClaim};
use crate::Options;

const MAX_ARTIFACT_BYTES: u64 = 50 * 1024 * 1024;

pub(super) fn prepare_execution_result(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    mut value: Value,
) -> Result<Value, Box<dyn Error>> {
    let Some(message) = value
        .get("final_message")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(value);
    };
    let Some(mut report) = parse_report(&message) else {
        return Ok(value);
    };
    let mut index = 0usize;
    if !rewrite_value(&mut report, client, options, agent_id, claim, &mut index)? {
        return Ok(value);
    }
    value["final_message"] = Value::String(serde_json::to_string(&report)?);
    Ok(value)
}

fn parse_report(message: &str) -> Option<Value> {
    let trimmed = message.trim();
    let fence = "\x60\x60\x60";
    let candidate = trimmed
        .strip_prefix(&(fence.to_string() + "json"))
        .or_else(|| trimmed.strip_prefix(&(fence.to_string() + "JSON")))
        .or_else(|| trimmed.strip_prefix(fence))
        .and_then(|value| value.strip_suffix(fence))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(value) = serde_json::from_str(candidate) {
        return Some(value);
    }
    candidate
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
        .find_map(|line| serde_json::from_str(line).ok())
}

fn rewrite_value(
    value: &mut Value,
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    index: &mut usize,
) -> Result<bool, Box<dyn Error>> {
    match value {
        Value::Object(fields) => {
            let mut changed = false;
            if let Some(items) = fields
                .get_mut("output_artifacts")
                .and_then(Value::as_array_mut)
            {
                changed |= rewrite_artifact_items(items, client, options, agent_id, claim, index)?;
            }
            let keys = fields.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if key == "output_artifacts" {
                    continue;
                }
                if let Some(child) = fields.get_mut(&key) {
                    changed |= rewrite_value(child, client, options, agent_id, claim, index)?;
                }
            }
            Ok(changed)
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= rewrite_value(item, client, options, agent_id, claim, index)?;
            }
            Ok(changed)
        }
        Value::String(text) => {
            let Some(mut nested) = parse_report(text) else {
                return Ok(false);
            };
            if !rewrite_value(&mut nested, client, options, agent_id, claim, index)? {
                return Ok(false);
            }
            *text = serde_json::to_string(&nested)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn rewrite_artifact_items(
    items: &mut [Value],
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    index: &mut usize,
) -> Result<bool, Box<dyn Error>> {
    let mut changed = false;
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let path = object
            .get("local_path")
            .or_else(|| object.get("artifact_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty());
        let Some(path) = path else { continue };
        let artifact_type = object
            .get("artifact_type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("result_link")
            .to_string();
        let artifact = upload_local_artifact(
            client,
            options,
            agent_id,
            claim,
            Path::new(path),
            &artifact_type,
            *index,
        )?;
        *index += 1;
        object.remove("local_path");
        object.remove("artifact_path");
        object.insert(
            "file_object_id".into(),
            Value::String(artifact.file_object_id),
        );
        object.insert("artifact_type".into(), Value::String(artifact_type));
        object.insert("name".into(), Value::String(artifact.name.clone()));
        object.insert("content_type".into(), Value::String(artifact.content_type));
        object.insert(
            "size_bytes".into(),
            Value::Number(artifact.file_size.into()),
        );
        object.insert("sha256".into(), Value::String(artifact.sha256));
        if object
            .get("title")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            object.insert("title".into(), Value::String(artifact.name));
        }
        changed = true;
    }
    Ok(changed)
}

fn upload_local_artifact(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    path: &Path,
    artifact_type: &str,
    index: usize,
) -> Result<AgentRunArtifactResponse, Box<dyn Error>> {
    let canonical = path
        .canonicalize()
        .map_err(|_| "Agent artifact path is unavailable")?;
    if !canonical.is_file() {
        return Err("Agent artifact path is not a regular file".into());
    }
    let access_mode = if claim.run.access_mode.trim().is_empty() {
        claim.access_mode.trim()
    } else {
        claim.run.access_mode.trim()
    };
    if access_mode == crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED {
        let workspace = Path::new(claim.workspace_path.trim())
            .canonicalize()
            .map_err(|_| "Agent workspace is unavailable")?;
        if !canonical.starts_with(&workspace) {
            return Err("Agent artifact path is outside the exhibit-linked workspace".into());
        }
    }
    let metadata =
        fs::metadata(&canonical).map_err(|_| "Agent artifact metadata is unavailable")?;
    if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
        return Err("Agent artifact must be between 1 byte and 50 MiB".into());
    }
    let digest = hash_file(&canonical)?;
    let request_key = format!("{}-{}-{}", claim.run.id, index, digest);
    let response = upload_agent_run_artifact(
        client,
        &options.api_base,
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        &request_key,
        artifact_type,
        &canonical,
        &options.agent_credential(),
    )?;
    if response.run_id != claim.run.id
        || response.file_object_id.trim().is_empty()
        || response.artifact_type != artifact_type
        || !response.sha256.eq_ignore_ascii_case(&digest)
        || response.file_size != metadata.len() as i64
    {
        return Err("Dashboard returned inconsistent Agent artifact metadata".into());
    }
    if response.scan_status == "passed" {
        return Ok(response);
    }
    wait_for_scan(client, options, agent_id, claim, &response.file_object_id)
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = fs::File::open(path).map_err(|_| "Agent artifact cannot be read")?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let size = file
            .read(&mut buffer)
            .map_err(|_| "Agent artifact cannot be read")?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn wait_for_scan(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
    file_object_id: &str,
) -> Result<AgentRunArtifactResponse, Box<dyn Error>> {
    let timeout = std::env::var("HIMIND_AGENT_ARTIFACT_SCAN_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 1)
        .unwrap_or(120);
    let started = Instant::now();
    loop {
        let artifact = get_agent_run_artifact(
            client,
            &options.api_base,
            agent_id,
            &claim.run.id,
            &claim.claim_token,
            file_object_id,
            &options.agent_credential(),
        )?;
        match artifact.scan_status.as_str() {
            "passed" => return Ok(artifact),
            "failed" | "quarantined" => {
                return Err(
                    format!("Agent artifact scan did not pass: {}", artifact.scan_status).into(),
                )
            }
            _ if started.elapsed() >= Duration::from_secs(timeout) => {
                return Err("Agent artifact scan timed out".into())
            }
            _ => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_execution_result;

    #[test]
    fn module_compiles_with_governed_result_boundary() {
        // The integration path requires a live Dashboard claim; this test
        // keeps the module linked in the normal Agent test target.
        let _ = prepare_execution_result;
    }
}
