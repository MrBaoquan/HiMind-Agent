use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub(crate) const INTERACTION_ENVELOPE_SCHEMA_VERSION: &str = "interaction_envelope.v1";
pub(crate) const LOCAL_RUN_SCHEMA_VERSION: &str = "local_run.v1";
pub(crate) const RUNTIME_EVENT_SCHEMA_VERSION: &str = "runtime_event.v1";
pub(crate) const RUN_PROJECTION_SCHEMA_VERSION: &str = "run_projection.v1";
const MAX_AI_CLIENT_ID_BYTES: usize = 160;

/// Normalize protocol handshake metadata into a stable, non-secret audit ID.
pub(crate) fn external_ai_client_id(protocol: &str, name: &str, version: &str) -> String {
    let protocol = normalize_attribution_component(protocol, 24);
    let name = normalize_attribution_component(name, 96);
    if protocol.is_empty() || name.is_empty() {
        return String::new();
    }
    let version = normalize_attribution_component(version, 32);
    let value = if version.is_empty() {
        format!("{protocol}:{name}")
    } else {
        format!("{protocol}:{name}@{version}")
    };
    value.chars().take(MAX_AI_CLIENT_ID_BYTES).collect()
}

fn normalize_attribution_component(value: &str, max_bytes: usize) -> String {
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in value.trim().chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            normalized.push(character);
            previous_separator = false;
        } else if !normalized.is_empty() && !previous_separator {
            normalized.push('-');
            previous_separator = true;
        }
        if normalized.len() >= max_bytes {
            break;
        }
    }
    normalized.trim_matches('-').to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InteractionSource {
    Dashboard,
    Dingtalk,
    Mcp,
    Acp,
    Cli,
    Cron,
    Tauri,
    Local,
    Workflow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InteractionTransport {
    Http,
    Websocket,
    Stdio,
    Local,
    Queue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InteractionPrincipal {
    pub local_principal_id: String,
    #[serde(default)]
    pub delegated_user_id: String,
    #[serde(default)]
    pub ai_client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InteractionEnvelope {
    pub schema_version: String,
    pub interaction_id: String,
    pub correlation_id: String,
    pub idempotency_key: String,
    pub source: InteractionSource,
    pub transport: InteractionTransport,
    pub principal: InteractionPrincipal,
    pub agent_id: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub workspace_ref: String,
    #[serde(default = "default_object")]
    pub business_context: Value,
    #[serde(default = "default_object")]
    pub reply_target: Value,
    #[serde(default)]
    pub attachments: Vec<Value>,
    #[serde(default = "default_object")]
    pub policy_context: Value,
    #[serde(default)]
    pub runtime_hint: String,
    pub created_at: String,
}

impl InteractionEnvelope {
    pub(crate) fn validate(&self) -> Result<(), String> {
        require_schema_version(
            "interaction envelope",
            &self.schema_version,
            INTERACTION_ENVELOPE_SCHEMA_VERSION,
        )?;
        require_text("interaction_id", &self.interaction_id)?;
        require_text("correlation_id", &self.correlation_id)?;
        require_text("idempotency_key", &self.idempotency_key)?;
        require_text(
            "principal.local_principal_id",
            &self.principal.local_principal_id,
        )?;
        validate_optional_attribution(
            "principal.delegated_user_id",
            &self.principal.delegated_user_id,
            240,
        )?;
        validate_optional_attribution(
            "principal.ai_client_id",
            &self.principal.ai_client_id,
            MAX_AI_CLIENT_ID_BYTES,
        )?;
        validate_optional_attribution("device_id", &self.device_id, 240)?;
        require_text("agent_id", &self.agent_id)?;
        require_text("created_at", &self.created_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalRunStatus {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Canceled,
}

impl LocalRunStatus {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalStepStatus {
    Pending,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Canceled,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRunStep {
    pub step_id: String,
    pub title: String,
    pub status: LocalStepStatus,
    #[serde(default)]
    pub capability_id: String,
    #[serde(default)]
    pub runtime_provider: String,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRunApproval {
    pub approval_id: String,
    pub capability_id: String,
    pub risk_level: String,
    pub status: LocalApprovalStatus,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRunArtifact {
    pub artifact_id: String,
    pub artifact_type: String,
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRunUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub estimated_cost: f64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub billing_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRunExecutionPlan {
    pub workflow_id: String,
    pub execution_policy: String,
    pub entrypoint: String,
    pub exitpoint: String,
    pub entry_step_id: String,
    pub exit_step_id: String,
    #[serde(default)]
    pub active_step_ids: Vec<String>,
    #[serde(default)]
    pub seed_artifacts: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    pub plan_digest: String,
}

impl LocalRunExecutionPlan {
    pub(crate) fn completion_mode(&self) -> String {
        if self.entrypoint == "full" && self.exitpoint == "full" {
            "full".to_string()
        } else {
            "partial".to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalRun {
    pub schema_version: String,
    pub run_id: String,
    pub interaction_id: String,
    #[serde(default)]
    pub parent_run_id: String,
    pub source: InteractionSource,
    pub transport: InteractionTransport,
    pub status: LocalRunStatus,
    #[serde(default)]
    pub runtime_provider: String,
    #[serde(default)]
    pub workspace_ref: String,
    #[serde(default)]
    pub current_step_id: String,
    #[serde(default = "default_full_completion_mode")]
    pub completion_mode: String,
    #[serde(default)]
    pub execution_plan: Option<LocalRunExecutionPlan>,
    #[serde(default)]
    pub steps: Vec<LocalRunStep>,
    #[serde(default)]
    pub approvals: Vec<LocalRunApproval>,
    #[serde(default)]
    pub artifacts: Vec<LocalRunArtifact>,
    #[serde(default)]
    pub usage: Option<LocalRunUsage>,
    #[serde(default)]
    pub error: String,
    pub created_at: String,
    pub updated_at: String,
}

impl LocalRun {
    pub(crate) fn validate(&self) -> Result<(), String> {
        require_schema_version("local run", &self.schema_version, LOCAL_RUN_SCHEMA_VERSION)?;
        require_text("run_id", &self.run_id)?;
        require_text("interaction_id", &self.interaction_id)?;
        require_text("created_at", &self.created_at)?;
        require_text("updated_at", &self.updated_at)?;
        if !matches!(self.completion_mode.as_str(), "full" | "partial") {
            return Err(format!(
                "local run completion_mode is invalid: {}",
                self.completion_mode
            ));
        }
        if let Some(plan) = self.execution_plan.as_ref() {
            for (name, value) in [
                ("execution_plan.workflow_id", plan.workflow_id.as_str()),
                (
                    "execution_plan.execution_policy",
                    plan.execution_policy.as_str(),
                ),
                ("execution_plan.entrypoint", plan.entrypoint.as_str()),
                ("execution_plan.exitpoint", plan.exitpoint.as_str()),
                ("execution_plan.entry_step_id", plan.entry_step_id.as_str()),
                ("execution_plan.exit_step_id", plan.exit_step_id.as_str()),
                ("execution_plan.plan_digest", plan.plan_digest.as_str()),
            ] {
                require_text(name, value)?;
            }
            let mut active = HashSet::new();
            for step_id in &plan.active_step_ids {
                require_text("execution_plan.active_step_id", step_id)?;
                if !active.insert(step_id.as_str()) {
                    return Err(format!(
                        "duplicate execution plan active step id: {step_id}"
                    ));
                }
            }
            if !active.contains(plan.entry_step_id.as_str())
                || !active.contains(plan.exit_step_id.as_str())
            {
                return Err("execution plan entry and exit steps must be active steps".to_string());
            }
        }

        let mut step_ids = HashSet::new();
        for step in &self.steps {
            require_text("step.step_id", &step.step_id)?;
            require_text("step.title", &step.title)?;
            if !step_ids.insert(step.step_id.as_str()) {
                return Err(format!("duplicate local run step id: {}", step.step_id));
            }
        }
        if !self.current_step_id.is_empty() && !step_ids.contains(self.current_step_id.as_str()) {
            return Err(format!(
                "current_step_id does not reference a run step: {}",
                self.current_step_id
            ));
        }

        let mut approval_ids = HashSet::new();
        for approval in &self.approvals {
            require_text("approval.approval_id", &approval.approval_id)?;
            require_text("approval.capability_id", &approval.capability_id)?;
            require_text("approval.risk_level", &approval.risk_level)?;
            if !approval_ids.insert(approval.approval_id.as_str()) {
                return Err(format!(
                    "duplicate local run approval id: {}",
                    approval.approval_id
                ));
            }
        }

        let mut artifact_ids = HashSet::new();
        for artifact in &self.artifacts {
            require_text("artifact.artifact_id", &artifact.artifact_id)?;
            require_text("artifact.artifact_type", &artifact.artifact_type)?;
            require_text("artifact.name", &artifact.name)?;
            if !artifact_ids.insert(artifact.artifact_id.as_str()) {
                return Err(format!(
                    "duplicate local run artifact id: {}",
                    artifact.artifact_id
                ));
            }
        }
        Ok(())
    }
}

fn default_full_completion_mode() -> String {
    "full".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeEventType {
    TurnStarted,
    TurnCompleted,
    MessageDelta,
    Message,
    Plan,
    Progress,
    ToolStarted,
    ToolCompleted,
    ApprovalRequested,
    ApprovalResolved,
    QuestionRequested,
    QuestionResolved,
    ArtifactCreated,
    UsageRecorded,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeEvent {
    pub schema_version: String,
    pub event_id: String,
    pub run_id: String,
    #[serde(default)]
    pub step_id: String,
    #[serde(default)]
    pub capability_id: String,
    pub sequence: u64,
    pub occurred_at: String,
    pub provider: String,
    pub event_type: RuntimeEventType,
    #[serde(default = "default_object")]
    pub payload: Value,
}

impl RuntimeEvent {
    pub(crate) fn validate(&self) -> Result<(), String> {
        require_schema_version(
            "runtime event",
            &self.schema_version,
            RUNTIME_EVENT_SCHEMA_VERSION,
        )?;
        require_text("event_id", &self.event_id)?;
        require_text("run_id", &self.run_id)?;
        require_text("occurred_at", &self.occurred_at)?;
        require_text("provider", &self.provider)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunProjection {
    pub schema_version: String,
    pub projection_id: String,
    pub idempotency_key: String,
    pub sent_at: String,
    pub interaction: InteractionEnvelope,
    pub run: LocalRun,
}

impl RunProjection {
    pub(crate) fn validate(&self) -> Result<(), String> {
        require_schema_version(
            "run projection",
            &self.schema_version,
            RUN_PROJECTION_SCHEMA_VERSION,
        )?;
        require_text("projection_id", &self.projection_id)?;
        require_text("idempotency_key", &self.idempotency_key)?;
        require_text("sent_at", &self.sent_at)?;
        self.interaction.validate()?;
        self.run.validate()?;
        if self.run.interaction_id != self.interaction.interaction_id {
            return Err("run projection interaction identity does not match".to_string());
        }
        Ok(())
    }
}

fn require_schema_version(name: &str, actual: &str, expected: &str) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "{name} schema_version must be {expected}, received {actual}"
        ));
    }
    Ok(())
}

fn default_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn require_text(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} is required"));
    }
    Ok(())
}

fn validate_optional_attribution(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!("{name} must not exceed {max_bytes} bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{name} must not contain control characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn interaction() -> InteractionEnvelope {
        serde_json::from_value(json!({
            "schema_version": INTERACTION_ENVELOPE_SCHEMA_VERSION,
            "interaction_id": "int-1",
            "correlation_id": "corr-1",
            "idempotency_key": "idem-1",
            "source": "mcp",
            "transport": "stdio",
            "principal": {
                "local_principal_id": "ai-client:codex",
                "delegated_user_id": "",
                "ai_client_id": "codex"
            },
            "agent_id": "agent-1",
            "created_at": "2026-09-16T00:00:00Z"
        }))
        .unwrap()
    }

    fn run() -> LocalRun {
        serde_json::from_value(json!({
            "schema_version": LOCAL_RUN_SCHEMA_VERSION,
            "run_id": "run-1",
            "interaction_id": "int-1",
            "source": "mcp",
            "transport": "stdio",
            "status": "running",
            "runtime_provider": "himind.builtin",
            "current_step_id": "step-1",
            "steps": [{
                "step_id": "step-1",
                "title": "Inspect workspace",
                "status": "running",
                "capability_id": "workspace.inspect"
            }],
            "created_at": "2026-09-16T00:00:00Z",
            "updated_at": "2026-09-16T00:00:01Z"
        }))
        .unwrap()
    }

    #[test]
    fn interaction_envelope_roundtrip_is_valid() {
        interaction().validate().unwrap();
    }

    #[test]
    fn local_run_roundtrip_is_valid() {
        run().validate().unwrap();
    }

    #[test]
    fn local_run_rejects_duplicate_step_ids() {
        let mut value = run();
        let mut duplicate = value.steps[0].clone();
        duplicate.title = "Second".to_string();
        value.steps.push(duplicate);
        assert!(value.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn local_run_rejects_current_step_outside_steps() {
        let mut value = run();
        value.current_step_id = "missing".to_string();
        assert!(value.validate().unwrap_err().contains("does not reference"));
    }

    #[test]
    fn runtime_event_roundtrip_is_valid() {
        let event: RuntimeEvent = serde_json::from_value(json!({
            "schema_version": RUNTIME_EVENT_SCHEMA_VERSION,
            "event_id": "event-1",
            "run_id": "run-1",
            "step_id": "step-1",
            "capability_id": "workspace.inspect",
            "sequence": 1,
            "occurred_at": "2026-09-16T00:00:01Z",
            "provider": "himind.builtin",
            "event_type": "tool_completed",
            "payload": {"ok": true}
        }))
        .unwrap();
        event.validate().unwrap();
    }

    #[test]
    fn agent_core_schema_files_are_valid_json() {
        for source in [
            include_str!("../contracts/agent-core/v1/interaction-envelope.schema.json"),
            include_str!("../contracts/agent-core/v1/local-run.schema.json"),
            include_str!("../contracts/agent-core/v1/runtime-event.schema.json"),
            include_str!("../contracts/agent-core/v1/run-projection.schema.json"),
            include_str!("../contracts/agent-core/v1/connector-manifest.schema.json"),
            include_str!("../contracts/agent-core/v1/connector-policy-bundle.schema.json"),
            include_str!("../contracts/agent-core/v1/extension-candidate.schema.json"),
            include_str!("../contracts/agent-core/v1/extension-lock.schema.json"),
            include_str!("../contracts/agent-core/v1/workflow-package.schema.json"),
            include_str!("../contracts/agent-core/v1/extension-release-manifest.schema.json"),
            include_str!("../contracts/agent-core/v1/client-capability-matrix.schema.json"),
        ] {
            let value: Value = serde_json::from_str(source).unwrap();
            assert!(value.get("$id").and_then(Value::as_str).is_some());
        }
    }

    #[test]
    fn shared_run_projection_fixture_is_valid() {
        let projection: RunProjection = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/examples/run-projection.example.json"
        ))
        .unwrap();
        projection.validate().unwrap();
    }

    #[test]
    fn workflow_package_schema_accepts_v1_compatibility_fixtures() {
        let schema: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/workflow-package.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for source in [
            include_str!("../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"),
            include_str!("../workflows/wechat-experience-upload/workflow.json"),
            include_str!("../workflows/engine-build/workflow.json"),
        ] {
            let fixture: Value = serde_json::from_str(source).unwrap();
            assert!(
                validator.is_valid(&fixture),
                "workflow package fixture failed schema validation: {fixture:#?}"
            );
        }
    }

    #[test]
    fn workflow_package_schema_rejects_future_versions_and_unknown_fields() {
        let schema: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/workflow-package.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let mut future: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"
        ))
        .unwrap();
        future["schema_version"] = Value::String("workflow_package.v2".to_string());
        assert!(!validator.is_valid(&future));

        let mut unknown: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"
        ))
        .unwrap();
        unknown["future_extension"] = Value::Bool(true);
        assert!(!validator.is_valid(&unknown));
    }

    #[test]
    fn external_ai_client_ids_are_stable_and_audit_safe() {
        assert_eq!(
            external_ai_client_id("acp", "Claude Code", "1.2.3"),
            "acp:claude-code@1.2.3"
        );
        assert_eq!(
            external_ai_client_id("mcp", " Visual Studio Code ", ""),
            "mcp:visual-studio-code"
        );
        assert!(external_ai_client_id("acp", " ", "1.0.0").is_empty());
        assert!(external_ai_client_id("acp", &"x".repeat(500), "").len() <= MAX_AI_CLIENT_ID_BYTES);
    }
}
