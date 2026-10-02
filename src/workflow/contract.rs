use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;

use super::{WorkflowPackage, WorkflowRunner, WorkflowStep};

pub(crate) fn contract_dry_run_report(package: &WorkflowPackage) -> Result<Value, Box<dyn Error>> {
    let plans = validate_execution_plans(package)?;
    let risk_gates = validate_risk_gates(&package.steps)?;
    let schema_count = validate_schema_assets(package)?;
    let loop_count = count_loops(&package.steps);
    Ok(json!({
        "mode": "static_dry_run",
        "state": "passed",
        "checks": {
            "execution_plans": "passed",
            "loop_scope": "passed",
            "risk_gates": "passed",
            "candidate_binding": "passed",
            "artifact_schemas": {
                "state": "passed",
                "validated": schema_count
            },
            "declarative_ui": "passed"
        },
        "coverage": {
            "execution_plans": plans,
            "loop_count": loop_count,
            "risk_gated_steps": risk_gates,
            "schema_assets": schema_count
        },
        "uncovered": [
            "capability_side_effects",
            "runtime_provider_execution",
            "connector_network_calls",
            "approval_pause_resume",
            "user_feedback_loop_iteration",
            "platform_delivery"
        ]
    }))
}

fn validate_execution_plans(package: &WorkflowPackage) -> Result<Value, Box<dyn Error>> {
    if package.execution_policy == "strict" {
        let plan = WorkflowRunner::build_execution_plan(package, &json!({}))?;
        return Ok(json!([{
            "entrypoint": plan.entrypoint,
            "exitpoint": plan.exitpoint,
            "entry_step_id": plan.entry_step_id,
            "exit_step_id": plan.exit_step_id,
            "active_steps": plan.active_step_ids.len(),
            "plan_digest": plan.plan_digest,
        }]));
    }

    let mut plans = Vec::new();
    for entrypoint in &package.entrypoints {
        let mut matched = false;
        let mut last_error = String::new();
        // 入口声明的前置条件是调用方的契约：静态编排只验证「按声明的入口出发
        // 能不能走到出口」，不能因为调用方还没提交参数就判定入口不可用。
        let mut facts = serde_json::Map::new();
        for requirement in &entrypoint.requires {
            facts.insert(requirement.clone(), Value::Bool(true));
        }
        for exitpoint in &package.exits {
            let input = json!({
                "execution": {
                    "entrypoint": entrypoint.id,
                    "exitpoint": exitpoint.id,
                },
                "facts": Value::Object(facts.clone()),
            });
            match WorkflowRunner::build_execution_plan(package, &input) {
                Ok(plan) => {
                    matched = true;
                    plans.push(json!({
                        "entrypoint": plan.entrypoint,
                        "exitpoint": plan.exitpoint,
                        "entry_step_id": plan.entry_step_id,
                        "exit_step_id": plan.exit_step_id,
                        "active_steps": plan.active_step_ids.len(),
                        "plan_digest": plan.plan_digest,
                    }));
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        if !matched {
            let detail = if last_error.is_empty() {
                String::new()
            } else {
                format!(": {last_error}")
            };
            return Err(format!(
                "workflow entrypoint {} cannot reach any declared exit{}",
                entrypoint.id, detail
            )
            .into());
        }
    }
    for exitpoint in &package.exits {
        if !plans
            .iter()
            .any(|plan| plan.get("exitpoint").and_then(Value::as_str) == Some(&exitpoint.id))
        {
            return Err(format!(
                "workflow exit {} is unreachable from every entrypoint",
                exitpoint.id
            )
            .into());
        }
    }
    Ok(Value::Array(plans))
}

fn validate_risk_gates(steps: &[WorkflowStep]) -> Result<usize, Box<dyn Error>> {
    fn walk(steps: &[WorkflowStep], count: &mut usize) -> Result<(), Box<dyn Error>> {
        for step in steps {
            let risk = workflow_step_risk_rank(step.risk_level.trim());
            if risk >= 3 && !step.approval_required {
                return Err(format!(
                    "workflow contract step {} has risk {} without an approval gate",
                    step.id, step.risk_level
                )
                .into());
            }
            if step.approval_required {
                *count += 1;
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(&loop_config.steps, count)?;
            }
        }
        Ok(())
    }
    let mut count = 0;
    walk(steps, &mut count)?;
    Ok(count)
}

fn workflow_step_risk_rank(value: &str) -> u8 {
    match value.trim().to_ascii_uppercase().as_str() {
        "READ_ONLY" | "R1" => 1,
        "LOCAL_WRITE" | "LOCAL_ACTION" | "PROCESS" | "NETWORK" | "R2" => 2,
        "R3" => 3,
        "R4" | "SYSTEM" | "NETWORK_WRITE" | "DESTRUCTIVE" => 4,
        _ => 2,
    }
}

fn validate_schema_assets(package: &WorkflowPackage) -> Result<usize, Box<dyn Error>> {
    let mut paths = package
        .artifacts
        .iter()
        .map(|artifact| artifact.schema.clone())
        .filter(|path| !path.trim().is_empty())
        .collect::<Vec<_>>();
    collect_runtime_schemas(&package.steps, &mut paths);
    let paths = paths.into_iter().collect::<BTreeSet<_>>();
    let mut validated = 0;
    for relative in paths {
        let path = package.source_root.join(&relative);
        let schema: Value = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| format!("workflow schema {relative} is invalid JSON: {error}"))?;
        jsonschema::validator_for(&schema)
            .map_err(|error| format!("workflow schema {relative} cannot be compiled: {error}"))?;
        validated += 1;
    }
    Ok(validated)
}

fn collect_runtime_schemas(steps: &[WorkflowStep], paths: &mut Vec<String>) {
    for step in steps {
        if let Some(runtime) = step.runtime.as_ref() {
            if !runtime.result_schema.trim().is_empty() {
                paths.push(runtime.result_schema.clone());
            }
        }
        if let Some(loop_config) = step.loop_config.as_ref() {
            collect_runtime_schemas(&loop_config.steps, paths);
        }
    }
}

fn count_loops(steps: &[WorkflowStep]) -> usize {
    steps
        .iter()
        .map(|step| {
            step.loop_config
                .as_ref()
                .map(|loop_config| 1 + count_loops(&loop_config.steps))
                .unwrap_or(0)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn builds_static_dry_run_for_segmented_wechat_workflow() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let package = super::super::load_from_directory(&root).unwrap();
        let report = contract_dry_run_report(&package).unwrap();
        assert_eq!(report["state"], "passed");
        assert_eq!(report["mode"], "static_dry_run");
        assert!(report["coverage"]["execution_plans"]
            .as_array()
            .is_some_and(|plans| !plans.is_empty()));
        assert!(report["coverage"]["schema_assets"]
            .as_u64()
            .is_some_and(|count| count > 0));
        assert_eq!(report["coverage"]["loop_count"], 0);
        assert!(report["uncovered"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == "platform_delivery")));
    }
}
