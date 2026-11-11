//! 持久化专家团队 Mission 领域。
//!
//! Mission 管业务目标、计划确认、任务依赖与用户可见事件；Task Attempt
//! 将引用 LocalRun，而不在这里复制步骤执行状态。

mod checks;
mod contract;
pub(crate) mod export;
pub(crate) use checks::{read_projection_usage, run_required_checks, HostCheck};
mod store;

pub(crate) use contract::{
    ExecutionBinding, MissionAssignment, MissionAssignmentInput, MissionAssignmentOptions,
    MissionBudget, MissionConfirmation, MissionCreateInput, MissionEvent, MissionHandoff,
    MissionHandoffInput, MissionPlanMode, MissionPlanProposal, MissionRoleAssignment,
    MissionRoleOption, MissionStatus, MissionTaskRoleMapping, MissionTaskSpec, MissionTaskView,
    MissionView, MissionWorkflowPin, MissionWorkspace, TaskStatus, validate_execution_binding,
};
pub(crate) use store::{MissionSnapshot, MissionStore};

/// Fail closed when a pinned collaboration workflow no longer authorizes the
/// task's role/provider. Authorization is re-evaluated at dispatch time even
/// though the plan was confirmed earlier; a changed pin or invalid assignment
/// must never silently fall through to another route.
pub(crate) fn validate_task_against_workflow(
    task: &MissionTaskSpec,
    package: &crate::workflow::WorkflowPackage,
) -> Result<(), Box<dyn std::error::Error>> {
    let declared_roles = if package.roles.is_empty() {
        package.steps.iter().map(|step| step.role_id.as_str())
            .filter(|role| !role.is_empty()).collect::<std::collections::BTreeSet<_>>()
    } else {
        package.roles.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>()
    };
    if !task.execution.role_id.trim().is_empty() && !declared_roles.contains(task.execution.role_id.trim()) {
        return Err(format!(
            "task {} role {} is not declared by pinned workflow",
            task.id, task.execution.role_id
        ).into());
    }
    if !package.accepts_provider(task.execution.executor.trim()) {
        return Err(format!(
            "task {} executor {} is not accepted by pinned workflow",
            task.id, task.execution.executor
        ).into());
    }
    Ok(())
}

pub(crate) fn validate_mission_workflow_binding(
    mission: &MissionView,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(pin) = mission.workflow.as_ref() else { return Ok(()); };
    let workflows = crate::workflow::WorkflowStore::open_default()?;
    let (package, _) = workflows.load_pinned_version(
        &pin.workflow_id, &pin.workflow_version, &pin.workflow_sha256,
    )?;
    for task in &mission.tasks {
        if task.status != crate::mission::TaskStatus::Canceled {
            validate_task_against_workflow(&task.spec, &package)?;
        }
    }
    Ok(())
}

fn assigned_role_contract(mission: &MissionView) -> (String, String) {
    if let Some(assignment) = mission.assignment.as_ref() {
        let role_ids = assignment.roles.iter()
            .map(|role| role.role_id.trim())
            .filter(|role| !role.is_empty())
            .collect::<Vec<_>>();
        if !role_ids.is_empty() {
            return (
                format!(
                    "已指派角色（每项任务的 role_id 必须严格取自这里）：{}
                     不得创建未列出的角色；executor/service/model 由宿主按角色绑定覆盖，                     不要在计划里自行引入其他执行器或模型。",
                    role_ids.join("、")
                ),
                role_ids[0].to_string(),
            );
        }
    }
    (
        "尚未完成角色指派。任务 role_id 可使用 implementer，但用户确认前仍必须完成固定工作流角色与服务指派。"
            .to_string(),
        "implementer".to_string(),
    )
}

pub(crate) fn plan_prompt(mission: &MissionView) -> String {
    let (role_contract, example_role) = assigned_role_contract(mission);
    format!(
        "你是专家团队的协调 Agent。根据目标提出可执行整改计划。只输出 JSON，不修改文件、不派发任务。\n目标：{}\n工作区根：{}\n允许写入范围：{}\n验收：{}\n角色规则：{}\n路径规则：优先使用工作区相对路径；绝对路径可列入 read_paths/write_paths，但必须是完整规范路径且不得含 ..、设备命名空间、UNC 网络路径或文件系统根。绝对路径会展示给用户逐项选择允许或拒绝；不得假设已获准；工作区外路径必须单独请求授权，不得虚构与目标无关的路径。用户确认后绝对路径作为独立读/写授权，不受相对写入范围限制。工作区根请用相对路径 . 表示；只读任务的 write_paths 必须为空。\n任务规则：任务 ID 只能由 ASCII 字母数字、-、_、. 组成；依赖必须闭合且无环；每项任务必须有 executor=himind.builtin、role_id、acceptance_criteria、required_checks；required_checks 必须为可直接执行的命令，如 node --test、cargo test、npm test，禁止自然语言说明和 shell 管道；没有独立检查证据不得声称通过。\n输出结构：{{\"summary\":\"...\",\"tasks\":[{{\"id\":\"fix-1\",\"title\":\"...\",\"objective\":\"...\",\"depends_on\":[],\"read_paths\":[\".\"],\"write_paths\":[],\"acceptance_criteria\":[\"...\"],\"required_checks\":[\"...\"],\"execution\":{{\"executor\":\"himind.builtin\",\"model\":\"\",\"role_id\":\"{}\"}},\"repair_of\":\"\"}}],\"required_acceptance\":[\"...\"],\"budget\":{{\"max_rounds\":8,\"max_minutes\":120,\"max_repair_attempts\":2}}}}",
        format!("{}\n{}", mission.title, mission.objective),
        mission.workspace.root,
        mission.workspace.write_scope.join("、"),
        mission.acceptance_criteria.join("；"),
        role_contract,
        example_role,
    )
}

pub(crate) fn parse_plan_json(output: &str) -> Result<MissionPlanProposal, String> {
    let trimmed = output.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.rfind("```").map(|end| value[..end].trim()))
        .unwrap_or(trimmed);
    let start = unfenced.find('{').ok_or("协调 Agent 未返回 JSON 计划")?;
    let end = unfenced.rfind('}').ok_or("协调 Agent 未返回完整 JSON 计划")?;
    if end < start {
        return Err("协调 Agent 返回的计划 JSON 不完整".to_string());
    }
    serde_json::from_str(&unfenced[start..=end]).map_err(|error| format!("计划 JSON 无法解析: {error}"))
}

#[cfg(test)]
mod parser_tests {
    use super::parse_plan_json;

    pub(super) fn assigned_role_contract_for_test(
        assignment: Option<&crate::mission::MissionAssignment>,
    ) -> (String, String) {
        let mission = crate::mission::MissionView {
            schema_version: "mission.v1".to_string(),
            mission_id: "mission_test".to_string(),
            title: "test".to_string(),
            objective: String::new(),
            status: crate::mission::MissionStatus::Planned,
            workspace: crate::mission::MissionWorkspace {
                root: "F:/repo".to_string(), read_scope: vec![], write_scope: vec![],
            },
            acceptance_criteria: vec![], plan_revision: 0, plan_sha256: String::new(),
            plan_summary: String::new(), required_acceptance: vec![], proposed_tasks: vec![],
            budget: None, usage: None, handoff: None, workflow: None,
            assignment: assignment.cloned(), plan_mode: crate::mission::MissionPlanMode::Generated,
            confirmation: None, tasks: vec![],
            created_at: String::new(), updated_at: String::new(),
        };
        super::assigned_role_contract(&mission)
    }

    #[test]
    fn assigned_role_contract_limits_generated_plan_roles() {
        let assignment = crate::mission::MissionAssignment {
            roles: vec![crate::mission::MissionRoleAssignment {
                role_id: "ui.implementer.ia".to_string(),
                label: "IA".to_string(),
                executor: "himind.builtin".to_string(),
                service_id: "svc".to_string(),
                model: "himind:svc:model".to_string(),
            }],
            task_roles: vec![],
            applied_by: "local".to_string(),
            applied_at: "now".to_string(),
            recommended: true,
        };
        let (contract, example) = assigned_role_contract_for_test(Some(&assignment));
        assert!(contract.contains("ui.implementer.ia"));
        assert!(contract.contains("必须严格取自这里"));
        assert_eq!(example, "ui.implementer.ia");
    }

    #[test]
    fn parses_fenced_plan_with_surrounding_text() {
        let plan = parse_plan_json("说明\n```json\n{\"summary\":\"x\",\"tasks\":[],\"required_acceptance\":[],\"budget\":{\"max_rounds\":1,\"max_minutes\":1,\"max_repair_attempts\":0}}\n```").unwrap();
        assert_eq!(plan.summary, "x");
    }
}
/// Stable Agent entry for assigning one Mission.
pub(crate) fn assignment_deep_link(mission_id: &str) -> String {
    let encoded = url::form_urlencoded::byte_serialize(mission_id.as_bytes()).collect::<String>();
    format!("himind-agent://open?open=mission&mission_id={encoded}")
}