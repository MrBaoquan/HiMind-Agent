use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const WORKFLOW_HANDOFF_SCHEMA_VERSION: &str = "workflow_handoff.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowHandoff {
    pub schema_version: String,
    pub handoff_id: String,
    pub project_id: String,
    #[serde(default)]
    pub target_id: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub from_run_id: String,
    #[serde(default)]
    pub from_checkpoint_id: String,
    pub workflow_id: String,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub exitpoint: String,
    #[serde(default)]
    pub development_checkpoint: Option<Value>,
    #[serde(default)]
    pub candidate: Option<Value>,
    #[serde(default)]
    pub seed_artifacts: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub notes: String,
    pub created_at: String,
}

pub(crate) fn create(input: &Value) -> Result<Value, Box<dyn Error>> {
    let project_id = required_text(input, "project_id")?;
    let workflow_id = required_text(input, "workflow_id")?;
    let development_checkpoint = input.get("development_checkpoint").cloned();
    let from_checkpoint_id = development_checkpoint
        .as_ref()
        .and_then(|checkpoint| checkpoint.get("checkpoint_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let handoff_id = format!(
        "handoff_{:x}",
        Sha256::digest(
            format!(
                "{}:{}:{}:{}",
                project_id,
                workflow_id,
                from_checkpoint_id,
                unix_timestamp_string()
            )
            .as_bytes()
        )
    );
    let handoff = WorkflowHandoff {
        schema_version: WORKFLOW_HANDOFF_SCHEMA_VERSION.to_string(),
        handoff_id,
        project_id: project_id.to_string(),
        target_id: optional_text(input, "target_id"),
        environment: optional_text(input, "environment"),
        workspace_root: optional_text(input, "workspace_root"),
        from_run_id: optional_text(input, "from_run_id"),
        from_checkpoint_id,
        workflow_id: workflow_id.to_string(),
        entrypoint: optional_text(input, "entrypoint"),
        exitpoint: optional_text(input, "exitpoint"),
        development_checkpoint,
        candidate: input.get("candidate").cloned(),
        seed_artifacts: string_array(input, "seed_artifacts"),
        next_actions: string_array(input, "next_actions"),
        notes: optional_text(input, "notes"),
        created_at: unix_timestamp_string(),
    };
    let path = handoff_path(&handoff);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&handoff)?)?;
    let bytes = std::fs::read(&path)?;
    Ok(json!({
        "ok": true,
        "handoff": handoff,
        "artifacts": [{
            "artifact_id": "workflow-handoff",
            "artifact_type": "workflow_handoff",
            "name": "Workflow Handoff",
            "uri": path.to_string_lossy(),
            "sha256": format!("{:x}", Sha256::digest(&bytes)),
            "size_bytes": bytes.len(),
        }]
    }))
}

fn handoff_path(handoff: &WorkflowHandoff) -> PathBuf {
    let root = if handoff.workspace_root.trim().is_empty() {
        crate::store::paths::agent_home()
    } else {
        PathBuf::from(handoff.workspace_root.trim())
    };
    root.join(".himind")
        .join("handoffs")
        .join(format!("{}.json", handoff.handoff_id))
}

fn required_text<'a>(input: &'a Value, key: &str) -> Result<&'a str, Box<dyn Error>> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("workflow handoff requires {key}").into())
}

fn optional_text(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_handoff_from_checkpoint() {
        let root = std::env::temp_dir().join(format!(
            "himind-workflow-handoff-{}",
            unix_timestamp_string()
        ));
        let result = create(&json!({
            "project_id": "kerun-user",
            "target_id": "szkjg",
            "environment": "development",
            "workspace_root": root,
            "workflow_id": "com.himind.workflow.wechat-miniprogram-experience-upload",
            "entrypoint": "build",
            "exitpoint": "experience_version",
            "development_checkpoint": {
                "checkpoint_id": "devcp_1",
                "commit_sha": "abc",
                "tree_digest": "tree"
            },
            "seed_artifacts": ["development-checkpoint"]
        }))
        .unwrap();
        assert_eq!(result["handoff"]["supported_workflow"], Value::Null);
        assert_eq!(result["handoff"]["from_checkpoint_id"], "devcp_1");
        let _ = std::fs::remove_dir_all(root);
    }
}
