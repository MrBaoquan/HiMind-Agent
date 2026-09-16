use serde_json::Value;
use std::error::Error;

use super::{WorkflowPackage, WorkflowStep, WorkflowStepExecution, WorkflowStepExecutor};
use crate::agent_core_contracts::LocalRunUsage;
use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;

pub(crate) struct WorkflowGatewayExecutor {
    gateway: CapabilityGateway,
    context: InvocationContext,
}

impl WorkflowGatewayExecutor {
    pub(crate) fn new(gateway: CapabilityGateway, context: InvocationContext) -> Self {
        Self {
            gateway,
            context: context.without_agent_core_run(),
        }
    }
}

impl WorkflowStepExecutor for WorkflowGatewayExecutor {
    fn execute(
        &self,
        _package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        if step.runtime.is_some() {
            return super::execute_runtime_step(_package, step, input);
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
        let input = capability_input(&capability.input_schema, input);
        let output = self.gateway.invoke(&context, capability_id, input)?;
        let (artifacts, usage) = workflow_result_metadata(&output)?;
        Ok(WorkflowStepExecution {
            output,
            artifacts,
            usage,
        })
    }
}

fn capability_input(schema: &Value, input: &Value) -> Value {
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
}
