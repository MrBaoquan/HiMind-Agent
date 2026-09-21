use serde_json::Value;
use std::error::Error;

use super::{WorkflowPackage, WorkflowStep, WorkflowStepExecution, WorkflowStepExecutor};
use crate::agent_core_contracts::{LocalRunStatus, LocalRunUsage};
use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::store::local_runs::LocalRunLedger;

pub(crate) struct WorkflowGatewayExecutor {
    gateway: CapabilityGateway,
    context: InvocationContext,
    ledger: LocalRunLedger,
    run_id: String,
}

impl WorkflowGatewayExecutor {
    pub(crate) fn new(
        gateway: CapabilityGateway,
        context: InvocationContext,
        ledger: LocalRunLedger,
        run_id: String,
    ) -> Self {
        Self {
            gateway,
            context: context.without_agent_core_run(),
            ledger,
            run_id,
        }
    }
}

impl WorkflowStepExecutor for WorkflowGatewayExecutor {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        if step.runtime.is_some() {
            let is_canceled = || {
                Ok(self
                    .ledger
                    .get_run(&self.run_id)?
                    .is_some_and(|run| run.status == LocalRunStatus::Canceled))
            };
            return super::execute_runtime_step(
                package,
                step,
                input,
                Some(self.gateway.options()),
                &is_canceled,
            );
        }
        let capability_id = step.capability_id.trim();
        if capability_id.is_empty() {
            return Err(format!(
                "workflow step {} has no executable Capability; a Runtime Provider must execute or map this step",
                step.id
            )
            .into());
        }
        let mut context = self.context.clone();
        context.request_id = format!("{}:{}", self.context.request_id, step.id);
        let capability = self
            .gateway
            .list_capabilities(&context)?
            .into_iter()
            .find(|capability| capability.id == capability_id)
            .ok_or_else(|| format!("capability not found: {capability_id}"))?;
        let resolved =
            super::connector::resolve_connector_credentials_for_capability_with_redactions(
                package,
                &step.capability_id,
                input,
            )?;
        let input = capability_input(&capability.input_schema, &resolved.input);
        let output = self.gateway.invoke(&context, capability_id, input)?;
        let output = redact_connector_secret_values(output, &resolved.redactions);
        let (artifacts, usage) = workflow_result_metadata(&output)?;
        Ok(WorkflowStepExecution {
            output,
            artifacts,
            usage,
        })
    }
}

fn redact_connector_secret_values(value: Value, secrets: &[String]) -> Value {
    match value {
        Value::String(text) => {
            let mut redacted = text;
            for secret in secrets {
                if !secret.is_empty() {
                    redacted = redacted.replace(secret, "[redacted]");
                }
            }
            Value::String(redacted)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_connector_secret_values(value, secrets))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, redact_connector_secret_values(value, secrets)))
                .collect(),
        ),
        value => value,
    }
}

// 启动前检查复用同一套过滤规则，避免「预检查的输入」和「真正发给能力的输入」
// 不是同一个对象，导致预检查报的错和运行时报的错对不上。
pub(crate) fn capability_input(schema: &Value, input: &Value) -> Value {
    let Some(input) = input.as_object() else {
        return input.clone();
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Value::Object(input.clone());
    };
    if schema.get("additionalProperties").and_then(Value::as_bool) != Some(false) {
        return Value::Object(input.clone());
    }
    Value::Object(
        input
            .iter()
            .filter(|(name, _)| properties.contains_key(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    )
}

fn workflow_result_metadata(
    output: &Value,
) -> Result<(Vec<super::WorkflowArtifactOutput>, Option<LocalRunUsage>), Box<dyn Error>> {
    let artifacts = output
        .get("artifacts")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let usage = output
        .get("usage")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok((artifacts, usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workflow_artifacts_and_usage() {
        let output = serde_json::json!({
            "ok": true,
            "artifacts": [{
                "artifact_id": "preview",
                "artifact_type": "wechat_preview",
                "name": "preview",
                "uri": "file:///tmp/preview.png",
                "sha256": "abc",
                "size_bytes": 12
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "estimated_cost": 0.01,
                "currency": "CNY",
                "billing_owner": "local-user"
            }
        });
        let (artifacts, usage) = workflow_result_metadata(&output).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_id, "preview");
        assert_eq!(usage.unwrap().input_tokens, 10);
    }

    #[test]
    fn strips_context_for_strict_builtin_schemas() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "additionalProperties": false
        });
        let input = serde_json::json!({
            "path": "C:\\work",
            "workflow_context": {"run_id": "run-1"}
        });
        assert_eq!(
            capability_input(&schema, &input),
            serde_json::json!({"path": "C:\\work"})
        );
    }

    #[test]
    fn keeps_only_declared_properties_for_step_scoped_input() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "project_root": {"type": "string"},
                "workflow_context": {"type": "object"}
            },
            "additionalProperties": false
        });
        let input = serde_json::json!({
            "project_root": "C:\\work",
            "app_id": "wx-test",
            "workflow_context": {"run_id": "run-1"}
        });
        assert_eq!(
            capability_input(&schema, &input),
            serde_json::json!({
                "project_root": "C:\\work",
                "workflow_context": {"run_id": "run-1"}
            })
        );
    }

    #[test]
    fn removes_credential_handles_from_capability_input() {
        let package = WorkflowPackage {
            schema_version: super::super::WORKFLOW_PACKAGE_SCHEMA_VERSION.to_string(),
            id: "com.himind.workflow.test".to_string(),
            version: "1.0.0".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            min_agent_version: "0.3.47".to_string(),
            local_requirements: serde_json::json!({}),
            optional_providers: Vec::new(),
            capabilities: vec!["test.capability".to_string()],
            dependencies: Default::default(),
            candidate: None,
            execution_policy: "strict".to_string(),
            entrypoints: Vec::new(),
            default_entrypoint: String::new(),
            default_exitpoint: String::new(),
            exits: Vec::new(),
            steps: Vec::new(),
            artifacts: Vec::new(),
            ui: super::super::WorkflowUi {
                mode: "standard".to_string(),
                entry: String::new(),
                surfaces: Vec::new(),
            },
            supported_runtimes: Vec::new(),
            created_at: String::new(),
            source_root: std::path::PathBuf::new(),
            connectors: Vec::new(),
        };
        let step = WorkflowStep {
            id: "TEST".to_string(),
            title: "Test".to_string(),
            kind: "capability".to_string(),
            capability_id: "test.capability".to_string(),
            runtime: None,
            loop_config: None,
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "sync".to_string(),
            risk_level: "read_only".to_string(),
            approval_required: false,
            on_failure: String::new(),
            depends_on: Vec::new(),
        };
        let input = serde_json::json!({
            "project_root": "C:\\work",
            "credential_handles": {"private_key_path": "wechat-key"}
        });
        let resolved = crate::workflow::connector::resolve_connector_credentials_for_capability(
            &package,
            &step.capability_id,
            &input,
        )
        .unwrap();
        assert!(resolved.get("credential_handles").is_none());
    }

    #[test]
    fn redacts_resolved_connector_secret_values_from_output() {
        let output = serde_json::json!({
            "ok": true,
            "path": "C:\\secrets\\upload.key",
            "nested": {
                "message": "using C:\\secrets\\upload.key",
                "token": "token-123"
            }
        });
        let redacted = redact_connector_secret_values(
            output,
            &[
                "C:\\secrets\\upload.key".to_string(),
                "token-123".to_string(),
            ],
        );
        assert_eq!(redacted["path"], "[redacted]");
        assert_eq!(redacted["nested"]["message"], "using [redacted]");
        assert_eq!(redacted["nested"]["token"], "[redacted]");
    }
}
