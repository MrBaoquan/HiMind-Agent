use serde_json::Value;
use std::error::Error;

use super::{
    SharedStepExecutor, WorkflowPackage, WorkflowStep, WorkflowStepExecution, WorkflowStepExecutor,
};
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

/// 候选冻结步骤由运行器注入的固定入参。
///
/// 运行期和启动前预检查共用这一份来源：否则预检查会报「缺少
/// candidate_artifact_id」，而真正运行时其实由运行器补上。
pub(crate) fn candidate_freeze_inputs(
    package: &WorkflowPackage,
    step: &WorkflowStep,
) -> Vec<(&'static str, Value)> {
    if step.candidate_action.trim() != "freeze" {
        return Vec::new();
    }
    let Some(policy) = package.candidate.as_ref() else {
        return Vec::new();
    };
    vec![
        (
            "candidate_artifact_id",
            Value::String(policy.artifact_id.clone()),
        ),
        ("allow_dirty", Value::Bool(policy.allow_dirty)),
    ]
}

impl WorkflowGatewayExecutor {
    pub(crate) fn new(
        gateway: CapabilityGateway,
        context: InvocationContext,
        ledger: LocalRunLedger,
        run_id: String,
    ) -> Self {
        let mut context = context;
        // Resume uses the immutable interaction's host context, never run input.
        if context.trusted_workspace_ref.is_empty() {
            if let Ok(Some(run)) = ledger.get_run(&run_id) {
                if let Ok(Some(interaction)) = ledger.get_interaction(&run.interaction_id) {
                    context.workspace_ref = interaction.workspace_ref.clone();
                    context.trusted_workspace_ref = interaction.workspace_ref;
                }
            }
        }
        Self {
            gateway,
            context: context.without_agent_core_run(),
            ledger,
            run_id,
        }
    }
}

impl WorkflowStepExecutor for WorkflowGatewayExecutor {
    fn approval_target(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<Value, Box<dyn Error>> {
        if step.capability_id.is_empty() || step.runtime.is_some() {
            return Ok(Value::Null);
        }
        let resolved = resolve_completed_step_inputs(input);
        let available = self.gateway.list_capabilities(&self.context)?;
        let resolved_id = resolve_capability_id(package, &step.capability_id, &available);
        let capability = available
            .into_iter()
            .find(|c| c.id == resolved_id)
            .ok_or("approval capability unavailable")?;
        let mut args = capability_input(&capability.input_schema, &resolved);
        if crate::app::extension_binding::prepare(&self.context, &resolved_id, &mut args)?.is_none()
        {
            return Ok(Value::Null);
        }
        Ok(
            serde_json::json!({"capability_id":resolved_id,"arguments":args,"args_digest":crate::approval::policy::args_digest(&args)?}),
        )
    }
    /// 网关/台账/上下文都是可克隆的句柄，允许多个委派分支并发调用。
    fn as_shared(&self) -> Option<&dyn SharedStepExecutor> {
        Some(self)
    }

    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        let resolved_input = resolve_completed_step_inputs(input);
        let input = &resolved_input;
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
        let capabilities = self.gateway.list_capabilities(&context)?;
        // 主能力不可用时按清单里的候选 provider 执行。必须与预检用同一份声明顺序，
        // 否则会出现「预检说能用候选、运行期却仍按主能力调」的错位。
        let resolved_id = resolve_capability_id(package, step.capability_id.trim(), &capabilities);
        let capability = capabilities
            .into_iter()
            .find(|capability| capability.id == resolved_id)
            .ok_or_else(|| format!("capability not found: {resolved_id}"))?;
        let resolved =
            super::connector::resolve_connector_credentials_for_capability_with_redactions(
                package,
                &step.capability_id,
                input,
            )?;
        let mut input = capability_input(&capability.input_schema, &resolved.input);
        if step.approval_required
            && crate::app::extension_binding::prepare(&context, &resolved_id, &mut input)?.is_some()
        {
            let events = self.ledger.list_events(&self.run_id)?;
            let target = events
                .iter()
                .rev()
                .find(|e| {
                    e.step_id == step.id
                        && e.event_type
                            == crate::agent_core_contracts::RuntimeEventType::ApprovalRequested
                })
                .and_then(|e| e.payload.get("execution_target"))
                .ok_or("approved extension execution target missing")?;
            if target["capability_id"].as_str() != Some(&resolved_id)
                || target["args_digest"].as_str()
                    != Some(&crate::approval::policy::args_digest(&input)?)
            {
                return Err("extension target changed since Workflow approval; retry the step for a new approval".into());
            }
        }
        let output = self.gateway.invoke(&context, &resolved_id, input)?;
        let output = redact_connector_secret_values(output, &resolved.redactions);
        let (artifacts, usage) = workflow_result_metadata(&output)?;
        Ok(WorkflowStepExecution {
            output,
            artifacts,
            usage,
        })
    }
}

/// 这一步实际调用的能力：主能力可用就用主能力，否则按清单顺序挑第一个可用的候选。
/// 与 `workflow::preflight` 的解析口径保持一致（ControlPlane 且无工作台 provider 视为不可用）。
fn resolve_capability_id(
    package: &WorkflowPackage,
    requested: &str,
    available: &[crate::capability::types::CapabilityDescriptor],
) -> String {
    let usable = super::capability_is_usable;
    if available
        .iter()
        .any(|capability| capability.id == requested && usable(capability))
    {
        return requested.to_string();
    }
    package
        .capability_alternatives
        .get(requested)
        .into_iter()
        .flatten()
        .find(|candidate| {
            available
                .iter()
                .any(|capability| capability.id == candidate.as_str() && usable(capability))
        })
        .cloned()
        .unwrap_or_else(|| requested.to_string())
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
/// Resolve `input.*` placeholders embedded in a Workflow step's static input.
/// The Workflow JSON uses these placeholders for capability inputs, while
/// runtime steps receive the complete workflow context separately.
pub(crate) fn resolve_workflow_input_templates(value: &Value, workflow_input: &Value) -> Value {
    match value {
        Value::String(template) => match template.strip_prefix("input.") {
            Some(path) => lookup_workflow_input(workflow_input, path).unwrap_or(Value::Null),
            None => value.clone(),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_workflow_input_templates(item, workflow_input))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        resolve_workflow_input_templates(item, workflow_input),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn lookup_workflow_input(root: &Value, path: &str) -> Option<Value> {
    let mut current = root;
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        current = match current {
            Value::Object(object) => object.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current.clone())
}

/// Receipt references resolve only against the host's completed-step context.
/// Caller supplied `steps` inside launch input cannot impersonate a receipt.
pub(crate) fn resolve_workflow_step_templates(
    value: &Value,
    input: &Value,
    context: &Value,
) -> Value {
    match value {
        Value::String(text) if text.starts_with("steps.") => {
            lookup_workflow_input(context, text).unwrap_or(Value::Null)
        }
        Value::String(_) => resolve_workflow_input_templates(value, input),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_workflow_step_templates(item, input, context))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        resolve_workflow_step_templates(item, input, context),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// The runner owns workflow_context; resolve only its completed receipts here.
/// Missing optional receipts (e.g. a skipped builder) are omitted before schema validation.
pub(crate) fn resolve_completed_step_inputs(input: &Value) -> Value {
    let context = input
        .get("workflow_context")
        .cloned()
        .unwrap_or(Value::Null);
    match input.as_object() {
        Some(object) => Value::Object(
            object
                .iter()
                .filter_map(|(name, value)| {
                    if name == "workflow_context" {
                        return Some((name.clone(), value.clone()));
                    }
                    let resolved = resolve_workflow_step_templates(value, input, &context);
                    (!resolved.is_null()).then(|| (name.clone(), resolved))
                })
                .collect(),
        ),
        None => input.clone(),
    }
}

pub(crate) fn step_template_sources(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => text
            .strip_prefix("steps.")
            .map(|path| vec![path.split('.').next().unwrap_or_default().to_string()])
            .unwrap_or_default(),
        Value::Array(items) => items.iter().flat_map(step_template_sources).collect(),
        Value::Object(object) => object.values().flat_map(step_template_sources).collect(),
        _ => Vec::new(),
    }
}

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
    fn extension_approval_freezes_host_target_and_rejects_concurrent_rebinding() {
        use crate::capability::types::InvocationSource;
        use crate::store::types::LocalWorkerStatus;
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!("workflow-bound-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", root.join("private"));
        let (plugin, credential) = crate::app::secure_import::tests::fixture(
            &root.join("private"),
            "com.fixture.workflow",
            "fixture_workflow_",
        );
        let manifest_path = root.join("private/plugins/com.fixture.workflow/current/plugin.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        for capability in manifest["capabilities"].as_array_mut().unwrap() {
            capability["input_schema"] = serde_json::json!({"type":"object","additionalProperties":false,"properties":{
                "page_root":{"type":"string"},"resource":{"type":"string"},"credential_ref":{"type":"string"},"binding_revision":{"type":"string"}
            }});
        }
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        let workspace = root.join("repo/page");
        std::fs::create_dir_all(&workspace).unwrap();
        let context = InvocationContext::new(InvocationSource::Mcp, "workflow-test")
            .with_trusted_workspace(workspace.to_string_lossy().to_string());
        crate::store::plugin_credentials::save_instance(
            &plugin.id,
            &credential.id,
            "account",
            &BTreeMap::from([("endpoint".into(), "https://example.com".into())]),
            &BTreeMap::from([("token".into(), "fixture_workflow_private".into())]),
            &crate::capability::plugin::credential_declaration_fingerprint(&plugin.credentials),
        )
        .unwrap();
        let bind = |resource: &str| {
            let mut input = serde_json::json!({"page_root":workspace,"resource":resource,"credential_ref":"account"});
            let prepared =
                crate::app::extension_binding::prepare(&context, "fixture.target.bind", &mut input)
                    .unwrap()
                    .unwrap();
            let _guard = prepared.execution_lock().unwrap();
            prepared
                .complete(
                    &input,
                    &mut serde_json::json!({"binding_target":{"resource":resource}}),
                )
                .unwrap();
        };
        bind("first");
        let step: WorkflowStep = serde_json::from_value(serde_json::json!({
            "id":"PUBLISH","title":"Publish","kind":"capability","capability_id":"fixture.target.publish",
            "execution_mode":"sync","risk_level":"R3","approval_required":true
        })).unwrap();
        let package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version":super::super::WORKFLOW_PACKAGE_SCHEMA_VERSION,"id":"com.fixture.bound-workflow",
            "version":"1.0.0","name":"Bound workflow","description":"fixture","min_agent_version":"0.3.94",
            "capabilities":["fixture.target.publish"],"steps":[step],"ui":{"mode":"standard","entry":"","surfaces":[]}
        })).unwrap();
        let ledger = LocalRunLedger::new(root.join("runs.sqlite3"));
        let runner = super::super::WorkflowRunner::with_ledger(ledger.clone());
        let input = serde_json::json!({"page_root":workspace});
        let run = runner
            .start_in_workspace(
                "fixture",
                &package,
                "bound-test",
                &input,
                workspace.to_str().unwrap(),
            )
            .unwrap();
        // Resume recovers trust from the host-owned interaction, not input.
        let executor = WorkflowGatewayExecutor::new(
            CapabilityGateway::new(
                crate::Options::from_env(),
                Arc::new(Mutex::new(LocalWorkerStatus::default())),
            ),
            InvocationContext::new(InvocationSource::Mcp, "workflow-resume"),
            ledger.clone(),
            run.run_id.clone(),
        );
        let blocked = runner.run_ready(&package, run, &input, &executor).unwrap();
        assert_eq!(blocked.blocked_step_id, "PUBLISH");
        let events = ledger.list_events(&blocked.run.run_id).unwrap();
        let frozen = &events
            .iter()
            .find(|event| {
                event.event_type == crate::agent_core_contracts::RuntimeEventType::ApprovalRequested
            })
            .unwrap()
            .payload["execution_target"];
        assert_eq!(frozen["arguments"]["resource"], "first");
        assert_eq!(
            frozen,
            &executor
                .approval_target(&package, &package.steps[0], &input)
                .unwrap()
        );
        let _approved = runner.approve_step(blocked.run, "PUBLISH").unwrap();
        bind("second");
        let error = executor
            .execute(&package, &package.steps[0], &input)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("changed since Workflow approval"), "{error}");
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

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
            icon: String::new(),
            schema_version: super::super::WORKFLOW_PACKAGE_SCHEMA_VERSION.to_string(),
            id: "com.himind.workflow.test".to_string(),
            version: "1.0.0".to_string(),
            distribution_targets: Vec::new(),
            name: "Test".to_string(),
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: "0.3.47".to_string(),
            local_requirements: serde_json::json!({}),
            optional_providers: Vec::new(),
            capabilities: vec!["test.capability".to_string()],
            roles: Default::default(),
            capability_alternatives: Default::default(),
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
            delegate: None,
            when: None,
            fail_when: None,
            candidate_action: String::new(),
            input: serde_json::json!({}),
            execution_mode: "sync".to_string(),
            role_id: String::new(),
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
    fn resolves_nested_input_templates_for_capability_steps() {
        let input = serde_json::json!({
            "exhibit_id": "exh_1",
            "evidence_items": [
                {"path": "/tmp/shot.png", "kind": "development-screenshot"}
            ]
        });
        let template = serde_json::json!({
            "exhibit_id": "input.exhibit_id",
            "items": "input.evidence_items",
            "nested": {"missing": "input.not_present"}
        });
        assert_eq!(
            resolve_workflow_input_templates(&template, &input),
            serde_json::json!({
                "exhibit_id": "exh_1",
                "items": [
                    {"path": "/tmp/shot.png", "kind": "development-screenshot"}
                ],
                "nested": {"missing": null}
            })
        );
    }

    #[test]
    fn receipt_templates_use_host_context_and_preserve_types() {
        let input = serde_json::json!({"steps":{"PREVIEW":{"hash":"forged"}},"reason":"确认发布"});
        let context =
            serde_json::json!({"steps":{"PREVIEW":{"hash":"actual","revision":"20","ok":true}}});
        let template = serde_json::json!({"sha":"steps.PREVIEW.hash","receipt":"steps.PREVIEW","reason":"input.reason","missing":"steps.SKIPPED.output"});
        let resolved = resolve_workflow_step_templates(&template, &input, &context);
        assert_eq!(resolved["sha"], "actual");
        assert_eq!(resolved["receipt"]["ok"], true);
        assert_eq!(resolved["reason"], "确认发布");
        assert!(resolved["missing"].is_null());
        let mut sources = step_template_sources(&template);
        sources.sort();
        assert_eq!(sources, vec!["PREVIEW", "PREVIEW", "SKIPPED"]);
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

impl SharedStepExecutor for WorkflowGatewayExecutor {}
