//! Expert identities and selectable execution bindings shared by UI and MCP.
use std::error::Error;
use super::{MissionAssignmentInput, MissionAssignmentOptions, MissionExpertPin};

fn check_definition(pin: &MissionExpertPin, definition: &crate::expert::ExpertDefinition) -> Result<(), Box<dyn Error>> {
    if pin.expert_id != definition.id || pin.expert_version != definition.version
        || pin.expert_digest != crate::expert::digest(definition)? {
        return Err("所选专家版本或内容已变化，请重新指派专家".into());
    }
    Ok(())
}

pub(crate) fn validate_expert_pin(pin: &MissionExpertPin) -> Result<(), Box<dyn Error>> {
    let definition = crate::expert::get_pinned(&pin.expert_id, &pin.expert_version, &pin.expert_digest)?;
    check_definition(pin, &definition)
}

pub(crate) fn expert_instructions(pin: &MissionExpertPin) -> Result<String, Box<dyn Error>> {
    let expert = crate::expert::get_pinned(&pin.expert_id, &pin.expert_version, &pin.expert_digest)?;
    check_definition(pin, &expert)?;
    Ok(format!("所选专家：{} @ {}\n{}\n输出要求：{}\n工作阶段：{}\n证据要求：{}\n恢复规则：{}\n技能引用：{}\n能力引用：{}\n引用不是授权；仍须遵守任务的计划、工具和访问范围。",
        expert.name, expert.version, expert.instructions,
        expert.output_contract.required_sections.join("；"), expert.harness.behavior_phases.join(" → "),
        expert.harness.required_evidence.join("；"), expert.harness.recovery_guidance.join("；"),
        expert.skill_refs.join("、"), expert.capability_refs.join("、")))
}

/// Shared directory check for assignment and dispatch. Never resolves defaults here.
pub(crate) fn validate_execution_catalog(
    execution: &super::ExecutionBinding,
    executors: &[crate::app::mission_commands::MissionExecutionOption],
) -> Result<(), Box<dyn Error>> {
    let executor = executors.iter().find(|option| option.executor == execution.executor && option.available)
        .ok_or("所选执行器不可用或不受该工作流支持")?;
    // Existing ACP bindings may intentionally keep the client's current model.
    if crate::runtime::acp::is_provider(&execution.executor) && execution.model.is_empty() && execution.service_id.is_empty() {
        return Ok(());
    }
    if execution.executor == "himind.builtin" || !executor.models.is_empty() {
        if !executor.models.iter().any(|model| model.alias == execution.model && model.service_id == execution.service_id) {
            return Err("请选择该服务目录中的可用模型".into());
        }
    } else if !execution.model.is_empty() || !execution.service_id.is_empty() {
        return Err("该执行器不接受服务或模型覆盖".into());
    }
    Ok(())
}

fn validate_catalog_choices(input: &MissionAssignmentInput, catalog: &MissionAssignmentOptions) -> Result<(), Box<dyn Error>> {
    super::contract::validate_assignment(input)?;
    for role in &input.roles {
        if !catalog.roles.iter().any(|option| option.role_id == role.role_id) {
            return Err("请选择工作流声明的职责".into());
        }
        let pin = role.expert.as_ref().ok_or("请为每项职责选择已安装专家")?;
        if !catalog.experts.iter().any(|expert| expert.id == pin.expert_id && expert.version == pin.expert_version && expert.digest == pin.expert_digest) {
            return Err("所选专家不在当前已安装目录中，请刷新指派选项".into());
        }
        validate_execution_catalog(&super::ExecutionBinding {
            role_id: role.role_id.clone(), executor: role.executor.clone(),
            service_id: role.service_id.clone(), model: role.model.clone(), expert: role.expert.clone(),
        }, &catalog.executors)?;
    }
    if catalog.roles.iter().any(|option| !option.optional && !input.roles.iter().any(|role| role.role_id == option.role_id)) {
        return Err("请为固定工作流的每个职责槽位选择专家".into());
    }
    Ok(())
}

pub(crate) fn validate_catalog_assignment(input: &MissionAssignmentInput, catalog: &MissionAssignmentOptions) -> Result<(), Box<dyn Error>> {
    validate_catalog_choices(input, catalog)?;
    for role in &input.roles {
        validate_expert_pin(role.expert.as_ref().ok_or("请选择已安装专家")?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expert_pin_rejects_same_version_with_changed_instructions() {
        let definition: crate::expert::ExpertDefinition = serde_json::from_value(serde_json::json!({
            "schema_version":"expert.v1", "id":"com.himind.expert.test", "name":"测试专家", "version":"1.0.0", "instructions":"Read real checks"
        })).unwrap();
        let pin = MissionExpertPin {expert_id:definition.id.clone(), expert_version:definition.version.clone(), expert_digest:crate::expert::digest(&definition).unwrap()};
        check_definition(&pin,&definition).unwrap();
        let mut drifted=definition;
        drifted.instructions="Changed".into();
        assert!(check_definition(&pin,&drifted).is_err());
    }

    fn directory_fixture() -> (MissionAssignmentInput, MissionAssignmentOptions) {
        let catalog: MissionAssignmentOptions = serde_json::from_value(serde_json::json!({
            "workflow_id":"com.himind.workflow.test", "workflow_version":"1.0.0", "workflow_sha256":"pin", "source":"workflow",
            "roles":[{"role_id":"reviewer", "label":"Review", "brief":"", "recommended_executor":"himind.builtin",
                "recommended_service_id":"svc", "recommended_model":"himind:svc:model", "model_override_supported":true,
                "availability":"available", "unavailable_reason":""}],
            "experts":[{"id":"com.himind.expert.test", "version":"1.0.0", "digest":"sha256:pin", "name":"Test",
                "description":"", "author":"", "categories":[], "supported_clients":[], "skill_count":0, "workflow_count":0,
                "capability_count":0, "builtin":false, "active":false}],
            "executors":[{"executor":"himind.builtin", "label":"HiMind AI", "available":true, "reason":"",
                "models":[{"alias":"himind:svc:model", "label":"model", "service_id":"svc", "service_label":"Service"}]}]
        })).unwrap();
        let input: MissionAssignmentInput = serde_json::from_value(serde_json::json!({
            "roles":[{"role_id":"reviewer", "label":"Review", "executor":"himind.builtin", "model":"himind:svc:model", "service_id":"svc",
                "expert":{"expert_id":"com.himind.expert.test", "expert_version":"1.0.0", "expert_digest":"sha256:pin"}}],
            "task_roles":[], "applied_by":"test", "recommended":false
        })).unwrap();
        (input, catalog)
    }

    #[test]
    fn directory_rejects_missing_expert_slot_unavailable_executor_and_model_mismatch() {
        let (input, catalog) = directory_fixture();
        validate_catalog_choices(&input, &catalog).unwrap();
        let mut missing = input.clone(); missing.roles[0].expert = None;
        assert!(validate_catalog_choices(&missing, &catalog).unwrap_err().to_string().contains("已安装专家"));
        let mut stale = input.clone(); stale.roles[0].expert.as_mut().unwrap().expert_digest = "changed".into();
        assert!(validate_catalog_choices(&stale, &catalog).is_err());
        let mut extra_slot = catalog.clone(); let mut slot = extra_slot.roles[0].clone(); slot.role_id = "implementer".into(); extra_slot.roles.push(slot);
        assert!(validate_catalog_choices(&input, &extra_slot).unwrap_err().to_string().contains("每个职责"));
        extra_slot.roles.last_mut().unwrap().optional = true;
        validate_catalog_choices(&input, &extra_slot).unwrap();
        let mut unavailable = catalog.clone(); unavailable.executors[0].available = false;
        assert!(validate_catalog_choices(&input, &unavailable).is_err());
        let mut wrong_model = input.clone(); wrong_model.roles[0].model = "himind:svc:missing".into();
        assert!(validate_catalog_choices(&wrong_model, &catalog).is_err());
        let mut wrong_service = input.clone(); wrong_service.roles[0].service_id = "other".into();
        assert!(validate_catalog_choices(&wrong_service, &catalog).is_err());
        let mut empty_catalog = catalog.clone(); empty_catalog.executors[0].models.clear();
        let mut empty_model = input.clone(); empty_model.roles[0].model.clear(); empty_model.roles[0].service_id.clear();
        assert!(validate_catalog_choices(&empty_model, &empty_catalog).is_err());
    }

    #[test]
    fn old_execution_and_assignment_json_without_expert_remain_readable() {
        let execution: super::super::ExecutionBinding = serde_json::from_value(serde_json::json!({
            "executor":"personal.codex", "role_id":"reviewer", "model":""
        })).unwrap();
        assert!(execution.expert.is_none());
        let role: super::super::MissionRoleAssignment = serde_json::from_value(serde_json::json!({
            "executor":"personal.codex", "role_id":"reviewer"
        })).unwrap();
        assert!(role.expert.is_none());
    }
}
