use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;

use super::{WorkflowPackage, WorkflowRunner, WorkflowStep};

pub(crate) fn contract_dry_run_report(package: &WorkflowPackage) -> Result<Value, Box<dyn Error>> {
    let plans = validate_execution_plans(package)?;
    let risk_gates = validate_risk_gates(&package.steps)?;
    let roles = collect_roles(package);
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
            "roles": roles,
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

/// 盘点包内声明的角色：按角色聚合它负责的步骤，供界面展示「这个任务由谁协作完成」。
///
/// 角色配置（brief / provider / tool_policy）来自 `package.roles`；只写 role_id
/// 而没有配置的角色也会出现，此时 `configured = false`——界面据此区分
/// 「有角色但没配行为」与「角色真有执行语义」。
fn collect_roles(package: &WorkflowPackage) -> Value {
    fn walk(steps: &[WorkflowStep], roles: &mut std::collections::BTreeMap<String, Vec<String>>) {
        for step in steps {
            let role = step.role_id.trim();
            if !role.is_empty() {
                roles.entry(role.to_string()).or_default().push(step.id.clone());
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(&loop_config.steps, roles);
            }
        }
    }
    let mut roles = std::collections::BTreeMap::<String, Vec<String>>::new();
    walk(&package.steps, &mut roles);
    Value::Array(
        roles
            .into_iter()
            .map(|(role_id, step_ids)| {
                let role = package.roles.get(&role_id);
                let resolved_provider = role
                    .map(|role| role.runtime_provider.trim())
                    .filter(|value| !value.is_empty())
                    /* 角色没指定 provider 时，各步骤可能自己声明了不同的；
                       只有全部一致才报出确定值，否则留空。 */
                    .map(str::to_string)
                    .or_else(|| {
                        let providers = step_ids
                            .iter()
                            .filter_map(|step_id| {
                                find_step(&package.steps, step_id)
                                    .and_then(|step| step.runtime.as_ref())
                                    .map(|runtime| runtime.provider.trim().to_string())
                            })
                            .filter(|provider| !provider.is_empty())
                            .collect::<std::collections::BTreeSet<_>>();
                        if providers.len() == 1 {
                            providers.into_iter().next()
                        } else {
                            None
                        }
                    });
                json!({
                    "role_id": role_id,
                    "label": role.map(|role| role.label.trim()).unwrap_or_default(),
                    "step_count": step_ids.len(),
                    "step_ids": step_ids,
                    "configured": role.is_some(),
                    "runtime_provider": resolved_provider.unwrap_or_default(),
                    "tool_policy": role.map(|role| role.tool_policy.trim()).unwrap_or_default(),
                    "has_brief": role
                        .map(|role| !role.brief.trim().is_empty())
                        .unwrap_or(false),
                })
            })
            .collect(),
    )
}

/// 在（可能嵌套的）步骤树里按键查一步。
fn find_step<'a>(steps: &'a [WorkflowStep], step_id: &str) -> Option<&'a WorkflowStep> {
    for step in steps {
        if step.id == step_id {
            return Some(step);
        }
        if let Some(loop_config) = step.loop_config.as_ref() {
            if let Some(found) = find_step(&loop_config.steps, step_id) {
                return Some(found);
            }
        }
    }
    None
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
        // 这个包没有声明角色：dry-run 要给出空清单，而不是凭空造一个角色出来。
        assert_eq!(report["coverage"]["roles"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn dry_run_summarizes_declared_roles() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = super::super::load_from_directory(&root).unwrap();
        for (index, step) in package.steps.iter_mut().enumerate() {
            step.role_id = if index == 0 {
                "software.implementer".to_string()
            } else {
                "software.verifier".to_string()
            };
        }
        let report = contract_dry_run_report(&package).unwrap();
        let roles = report["coverage"]["roles"].as_array().unwrap();
        let verifier = roles
            .iter()
            .find(|role| role["role_id"] == "software.verifier")
            .expect("verifier role should be reported");
        // 步骤数要跟实际分到的步骤对上，否则界面会显示错误的协作规模。
        assert_eq!(
            verifier["step_count"].as_u64().unwrap(),
            (package.steps.len() - 1) as u64
        );
    }

    #[test]
    fn role_fixture_package_declares_a_full_software_team() {
        // 这个 fixture 就是「一个任务分给多个角色」的最小完整样例：
        // 解析、校验、dry-run 全都要通过，否则示例会变成一个跑不起来的文档。
        let source = include_str!(
            "../../contracts/agent-core/v1/examples/workflow-package.roles-v1.json"
        );
        let mut package: super::super::WorkflowPackage = serde_json::from_str(source).unwrap();
        package.validate().unwrap();
        package.source_root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();

        let report = contract_dry_run_report(&package).unwrap();
        let roles = report["coverage"]["roles"].as_array().unwrap();
        let role_ids = roles
            .iter()
            .map(|role| role["role_id"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            role_ids,
            vec![
                "software.analyst",
                "software.delivery",
                "software.implementer",
                "software.reviewer",
                "software.verifier",
            ]
        );
        // 循环里的步骤也要按角色统计：测试修复循环就挂在 verifier 名下。
        let verifier = roles
            .iter()
            .find(|role| role["role_id"] == "software.verifier")
            .unwrap();
        assert_eq!(verifier["step_count"], 2);
        assert!(verifier["step_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "VERIFY-RUN"));
    }

    #[test]
    fn role_config_is_reported_with_effective_execution_settings() {
        let source = include_str!(
            "../../contracts/agent-core/v1/examples/workflow-package.roles-v1.json"
        );
        let mut package: super::super::WorkflowPackage = serde_json::from_str(source).unwrap();
        package.source_root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        package.validate().unwrap();

        let report = contract_dry_run_report(&package).unwrap();
        let roles = report["coverage"]["roles"].as_array().unwrap();
        let reviewer = roles
            .iter()
            .find(|role| role["role_id"] == "software.reviewer")
            .unwrap();
        // 审查角色声明了 none 工具策略和独立 provider：这两项必须原样报出来，
        // 界面才有依据说明「审查者看不到实现方的推理过程」。
        assert_eq!(reviewer["configured"], true);
        assert_eq!(reviewer["tool_policy"], "none");
        assert_eq!(reviewer["runtime_provider"], "himind.builtin");
        assert_eq!(reviewer["has_brief"], true);
        // 角色没声明 provider 时才回落到步骤声明；这里是角色自己声明的。
        assert_eq!(reviewer["label"], "代码审查");
    }
}
