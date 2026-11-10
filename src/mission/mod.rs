//! 持久化专家团队 Mission 领域。
//!
//! Mission 管业务目标、计划确认、任务依赖与用户可见事件；Task Attempt
//! 将引用 LocalRun，而不在这里复制步骤执行状态。

mod contract;
mod store;

pub(crate) use contract::{
    ExecutionBinding, MissionBudget, MissionConfirmation, MissionCreateInput, MissionEvent,
    MissionPlanProposal, MissionStatus, MissionTaskSpec, MissionTaskView, MissionView,
    MissionWorkspace, TaskStatus,
};
pub(crate) use store::{MissionSnapshot, MissionStore};

pub(crate) fn plan_prompt(mission: &MissionView) -> String {
    format!(
        "你是专家团队的协调 Agent。根据目标提出可执行整改计划。只输出 JSON，不修改文件、不派发任务。\n目标：{}\n工作区根：{}\n允许写入范围：{}\n验收：{}\n路径规则：read_paths 和 write_paths 只能使用工作区相对路径；工作区根请写 .；禁止绝对路径、盘符和 ..；只读任务的 write_paths 必须为空。\n任务规则：任务 ID 只能由 ASCII 字母数字、-、_、. 组成；依赖必须闭合且无环；每项任务必须有 executor=himind.builtin、role_id、acceptance_criteria、required_checks；没有独立检查证据不得声称通过。\n输出结构：{{\"summary\":\"...\",\"tasks\":[{{\"id\":\"fix-1\",\"title\":\"...\",\"objective\":\"...\",\"depends_on\":[],\"read_paths\":[\".\"],\"write_paths\":[],\"acceptance_criteria\":[\"...\"],\"required_checks\":[\"...\"],\"execution\":{{\"executor\":\"himind.builtin\",\"model\":\"\",\"role_id\":\"implementer\"}},\"repair_of\":\"\"}}],\"required_acceptance\":[\"...\"],\"budget\":{{\"max_rounds\":8,\"max_minutes\":120,\"max_repair_attempts\":2}}}}",
        mission.title,
        mission.workspace.root,
        mission.workspace.write_scope.join("、"),
        mission.acceptance_criteria.join("；"),
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

    #[test]
    fn parses_fenced_plan_with_surrounding_text() {
        let plan = parse_plan_json("说明\n```json\n{\"summary\":\"x\",\"tasks\":[],\"required_acceptance\":[],\"budget\":{\"max_rounds\":1,\"max_minutes\":1,\"max_repair_attempts\":0}}\n```").unwrap();
        assert_eq!(plan.summary, "x");
    }
}
