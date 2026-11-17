//! Goal-first entry shared by the desktop command and the MCP capability.
//! Team definition creation never admits execution or grants access.
use super::{
    MissionCreateInput, MissionHandoffInput, MissionPlanMode, MissionStore, MissionView,
    MissionWorkflowPin, MissionWorkspace,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{error::Error, fs};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionIntentInput {
    pub idempotency_key: String,
    pub source_client: String,
    pub source_reference: String,
    #[serde(default)]
    pub source_message_refs: Vec<String>,
    #[serde(default)]
    pub context_summary: String,
    pub title: String,
    pub objective: String,
    pub workspace: MissionWorkspace,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

pub(crate) fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "idempotency_key":{"type":"string","minLength":1,"maxLength":200},
        "source_client":{"type":"string","minLength":1},
        "source_reference":{"type":"string","minLength":1},
        "source_message_refs":{"type":"array","items":{"type":"string"},"maxItems":200},
        "context_summary":{"type":"string","maxLength":2000},
        "title":{"type":"string","minLength":1,"maxLength":240},
        "objective":{"type":"string","minLength":1,"maxLength":2000},
        "workspace":{"type":"object","properties":{
            "root":{"type":"string","minLength":1},
            "permission_mode":{"enum":["workspace_write","full_access"]},
            "read_scope":{"type":"array","items":{"type":"string"}},
            "write_scope":{"type":"array","items":{"type":"string"}}
        },"required":["root"],"additionalProperties":false},
        "acceptance_criteria":{"type":"array","items":{"type":"string"}}
    },"required":["idempotency_key","source_client","source_reference","title","objective","workspace"],"additionalProperties":false})
}

fn fingerprint(input: &MissionIntentInput) -> Result<String, Box<dyn Error>> {
    let mut hash = Sha256::new();
    hash.update(b"mission_intent.v1\0");
    hash.update(serde_json::to_vec(input)?);
    Ok(format!("{:x}", hash.finalize()))
}

fn validate(input: &MissionIntentInput) -> Result<(), Box<dyn Error>> {
    super::contract::validate_create(&MissionCreateInput {
        title: input.title.clone(),
        objective: input.objective.clone(),
        workspace: input.workspace.clone(),
        acceptance_criteria: input.acceptance_criteria.clone(),
    })?;
    super::contract::require_text("objective", &input.objective, 2000)?;
    super::contract::require_text("idempotency_key", &input.idempotency_key, 200)?;
    super::contract::require_id("source_client", &input.source_client)?;
    super::contract::require_text("source_reference", &input.source_reference, 1000)?;
    if input.source_message_refs.len() > 200 || input.context_summary.chars().count() > 2000 {
        return Err("source context exceeds the handoff limits".into());
    }
    Ok(())
}

/// A sanitized method template: no objective, machine paths, transcripts,
/// service bindings, permission choice or expert instructions are copied.
fn team_package(
    duties: &[(String, String, String)],
) -> Result<crate::workflow::WorkflowPackage, Box<dyn Error>> {
    if duties.is_empty() {
        return Err("专家库没有适合本需求的专家；请先安装相应专家或选择固定工作流".into());
    }
    let roles = duties
        .iter()
        .map(|(id, label, brief)| {
            (
                id.clone(),
                json!({
                    "label": label, "brief": brief, "tool_policy":"default"
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let steps = duties.iter().enumerate().map(|(index, (role, label, brief))| json!({
        "id":format!("duty-{}", index + 1), "title":label, "role_id":role,
        "kind":"runtime", "execution_mode":"long_running", "risk_level":"read_only",
        "runtime":{"provider":"auto","prompt":brief,"tool_policy":"default","timeout_seconds":900},
        "input":{},
        // This is a reusable default method, not the runtime's generated DAG.
        "depends_on": if index == 0 { Vec::<String>::new() } else { vec![format!("duty-{index}")] }
    })).collect::<Vec<_>>();
    let package: crate::workflow::WorkflowPackage = serde_json::from_value(json!({
        "schema_version":"workflow_package.v1", "id":"com.himind.workflow.team-template",
        "version":"1.0.0", "name":"按需求组建的专家团队", "description":"从真实专家目录推荐的可复用职责方法。每次运行重新生成计划并确认环境和权限。",
        "min_agent_version":crate::VERSION, "roles":roles, "steps":steps,
        "supported_runtimes":["himind.builtin"],
        "optional_providers":["personal.codex","personal.github-copilot","acp.claude","acp.opencode"],
        "execution_policy":"segmented",
        "ui":{"mode":"standard","entry":"","surfaces":["agent"]},
        "entrypoints":[{"id":"start","at_step":"duty-1","label":"按需求生成计划"}], "default_entrypoint":"start",
        "exits":[{"id":"completed","at_step":format!("duty-{}", duties.len()),"label":"交付并复核"}], "default_exitpoint":"completed"
    }))?;
    identify_team_template(package)
}

/// Hash every serialized method field except the derived identity itself. This
/// function receives no Mission input, expert pin, path or authorization data.
fn identify_team_template(
    mut package: crate::workflow::WorkflowPackage,
) -> Result<crate::workflow::WorkflowPackage, Box<dyn Error>> {
    let mut template = serde_json::to_value(&package)?;
    template
        .as_object_mut()
        .ok_or("team template must be an object")?
        .remove("id");
    let mut hash = Sha256::new();
    hash.update(b"mission_team_template.v1\0");
    hash.update(serde_json::to_vec(&template)?);
    package.id = format!("com.himind.workflow.team-{:x}", hash.finalize());
    package.validate()?;
    Ok(package)
}

fn recommended_team_package(
    recommendation: &super::team::TeamRecommendation,
) -> Result<crate::workflow::WorkflowPackage, Box<dyn Error>> {
    if !recommendation.missing_required_roles.is_empty() {
        return Err(format!(
            "缺少必要专家职责，未创建团队方法或任务：{}；请安装相应专家后重试。",
            recommendation
                .missing_required_roles
                .iter()
                .map(|role| role.reason.as_str())
                .collect::<Vec<_>>()
                .join("；")
        )
        .into());
    }
    if recommendation.roles.is_empty() {
        return Err(format!(
            "没有可推荐的团队职责，未创建团队方法或任务：{}",
            recommendation.warnings.join("；")
        )
        .into());
    }
    let duties = recommendation
        .roles
        .iter()
        .map(|role| (role.role_id.clone(), role.label.clone(), role.brief.clone()))
        .collect::<Vec<_>>();
    team_package(&duties)
}

pub(crate) fn create(input: &MissionIntentInput) -> Result<MissionView, Box<dyn Error>> {
    let store = MissionStore::open_default()?;
    let workflows = crate::workflow::WorkflowStore::open_default()?;
    create_with_stores(input, &store, &workflows)
}

fn create_with_stores(
    input: &MissionIntentInput,
    store: &MissionStore,
    workflows: &crate::workflow::WorkflowStore,
) -> Result<MissionView, Box<dyn Error>> {
    validate(input)?;
    let request_hash = fingerprint(input)?;
    if let Some(existing) = store.handoff_replay(&input.idempotency_key, &request_hash)? {
        return Ok(existing);
    }
    let experts = crate::expert::list_team_candidates()?;
    let recommendation = super::team::recommend_team(&input.objective, &experts);
    let package = recommended_team_package(&recommendation)?;
    // Generated definitions live in the Mission-private method store. They
    // become ordinary capabilities only after an explicit reusable export.
    let staging = crate::store::paths::agent_home()
        .join("mission-team-drafts")
        .join(format!("{}-{}", std::process::id(), rand::random::<u64>()));
    let result = (|| {
        super::export::write_package(&staging, &package)?;
        let installed = workflows.install_mission_method(&staging)?;
        store.create_handoff_with_fingerprint(
            &MissionHandoffInput {
                idempotency_key: input.idempotency_key.clone(),
                source_client: input.source_client.clone(),
                source_reference: input.source_reference.clone(),
                source_message_refs: input.source_message_refs.clone(),
                context_summary: input.context_summary.clone(),
                title: input.title.clone(),
                objective: input.objective.clone(),
                workspace: input.workspace.clone(),
                acceptance_criteria: input.acceptance_criteria.clone(),
                workflow: MissionWorkflowPin {
                    workflow_id: installed.package.id,
                    workflow_version: installed.package.version,
                    workflow_sha256: installed.package_digest,
                    entrypoint: "start".into(),
                },
                plan_mode: MissionPlanMode::Generated,
                imported_plan: None,
            },
            workflows,
            Some(&request_hash),
        )
    })();
    // Only the host-generated staging directory is removed, never the workspace.
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_team_method_is_valid_and_contains_no_instance_authority() {
        let package = team_package(&[
            (
                "architecture".into(),
                "技术架构".into(),
                "检查边界及恢复证据".into(),
            ),
            (
                "quality".into(),
                "独立复核".into(),
                "根据真实产物和检查核验".into(),
            ),
        ])
        .unwrap();
        let serialized = serde_json::to_string(&package).unwrap();
        assert!(package.accepts_provider("personal.codex"));
        assert!(package.accepts_provider("himind.builtin"));
        assert!(package
            .roles
            .values()
            .all(|role| role.runtime_provider.is_empty() && role.model.is_empty()));
        assert!(!serialized.contains("full_access"));
        assert!(!serialized.contains("source_reference"));
        assert!(team_package(&[]).is_err());
    }

    #[test]
    fn team_template_identity_tracks_full_method_content_without_instance_data() {
        let duties = [(
            "reviewer".into(),
            "独立复核".into(),
            "核验真实交付证据".into(),
        )];
        let original = team_package(&duties).unwrap();
        assert_eq!(original.id, team_package(&duties).unwrap().id);
        assert_eq!(
            original.id,
            identify_team_template(original.clone()).unwrap().id
        );
        let mut changed_version = original.clone();
        changed_version.min_agent_version = "0.3.0".into();
        if changed_version.min_agent_version == original.min_agent_version {
            changed_version.min_agent_version = "0.2.0".into();
        }
        let changed_version = identify_team_template(changed_version).unwrap();
        assert_ne!(original.id, changed_version.id);
        let mut changed_step = original.clone();
        changed_step.steps[0]
            .runtime
            .as_mut()
            .unwrap()
            .timeout_seconds += 1;
        let changed_step = identify_team_template(changed_step).unwrap();
        assert_ne!(original.id, changed_step.id);
        assert_eq!(
            original.roles, changed_step.roles,
            "step changes alone must affect identity"
        );
        let serialized = serde_json::to_string(&original).unwrap();
        for field in [
            "idempotency_key",
            "source_reference",
            "expert_digest",
            "permission_mode",
        ] {
            assert!(
                !serialized.contains(field),
                "instance field {field} leaked into method"
            );
        }
        assert!(original
            .steps
            .iter()
            .all(|step| step.runtime.as_ref().is_some_and(|runtime| runtime
                .workspace_path
                .is_empty()
                && runtime.permission_mode.is_empty()
                && runtime.model.is_empty())));
        // Different complete definitions can coexist under immutable versioning.
        let root = std::env::temp_dir().join(format!(
            "team-templates-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let workflows = crate::workflow::WorkflowStore::new(root.join("workflows"));
        for (index, package) in [original, changed_version, changed_step].iter().enumerate() {
            let staging = root.join(format!("template-{index}"));
            super::super::export::write_package(&staging, package).unwrap();
            workflows.install_from_directory(&staging).unwrap();
        }
        assert_eq!(workflows.list().unwrap().len(), 3);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn intent_replay_survives_catalog_changes_without_authorizing_execution() {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "mission-intent-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        let previous_development = std::env::var_os("HIMIND_EXPERT_DEVELOPMENT_DIR");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        std::env::remove_var("HIMIND_EXPERT_DEVELOPMENT_DIR");
        struct Restore(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (key, value) in [
                    ("HIMIND_AGENT_HOME", &self.0),
                    ("HIMIND_EXPERT_DEVELOPMENT_DIR", &self.1),
                ] {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
            }
        }
        let _restore = Restore(previous, previous_development);
        let expert_root = root.join("experts/com.himind.expert.software-engineer/versions/1.0.0");
        fs::create_dir_all(&expert_root).unwrap();
        fs::write(expert_root.join("expert.json"),serde_json::to_vec(&json!({
            "schema_version":"expert.v1","id":"com.himind.expert.software-engineer","version":"1.0.0",
            "name":"软件工程师","instructions":"实现并提供真实检查证据"
        })).unwrap()).unwrap();
        let store = MissionStore::new(root.join("test-missions.db"));
        let workflows = crate::workflow::WorkflowStore::new(root.join("workflows"));
        let input:MissionIntentInput=serde_json::from_value(json!({
            "idempotency_key":"intent-test","source_client":"codex","source_reference":"test-thread",
            "title":"测试入口","objective":"实现软件修复","workspace":{"root":"C:/isolated-project","read_scope":["."],"write_scope":["src"],"permission_mode":"full_access"}
        })).unwrap();
        let missing = create_with_stores(&input, &store, &workflows)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("reviewer"), "{missing}");
        assert!(
            store.list(20).unwrap().is_empty(),
            "a missing reviewer must not create a partial Mission"
        );
        assert!(
            workflows.list().unwrap().is_empty(),
            "a missing reviewer must not install a partial method"
        );
        let review_root = root.join("experts/com.himind.expert.quality-engineer/versions/1.0.0");
        fs::create_dir_all(&review_root).unwrap();
        fs::write(review_root.join("expert.json"),serde_json::to_vec(&json!({
            "schema_version":"expert.v1","id":"com.himind.expert.quality-engineer","version":"1.0.0",
            "name":"测试工程师","instructions":"独立复核交付与检查证据"
        })).unwrap()).unwrap();
        let first = create_with_stores(&input, &store, &workflows).unwrap();
        assert_eq!(
            first.status,
            super::super::MissionStatus::AwaitingAssignment
        );
        assert!(first.confirmation.is_none());
        assert!(first.tasks.is_empty());
        assert!(
            workflows.list().unwrap().is_empty(),
            "generated methods must stay out of capabilities"
        );
        assert!(first.assignment.is_none());
        let pin = first.workflow.as_ref().unwrap();
        let (method, _) = workflows
            .load_pinned_version(
                &pin.workflow_id,
                &pin.workflow_version,
                &pin.workflow_sha256,
            )
            .unwrap();
        assert!(method.roles.contains_key("reviewer"));
        let recommendation = super::super::team::recommend_team(
            &input.objective,
            &crate::expert::list_team_candidates().unwrap(),
        );
        let assignment = serde_json::from_value(json!({
            "roles":recommendation.roles.iter().map(|role|json!({
                "role_id":role.role_id,"label":role.label,"expert":role.expert,
                "executor":"personal.codex","service_id":"","model":""
            })).collect::<Vec<_>>(), "task_roles":[],"applied_by":"test","recommended":true
        }))
        .unwrap();
        let assigned = store
            .assign_roles_with_store(&first.mission_id, &assignment, &workflows)
            .unwrap();
        let event_count = store.snapshot(&first.mission_id, 0).unwrap().events.len();
        fs::remove_file(expert_root.join("expert.json")).unwrap();
        fs::remove_file(review_root.join("expert.json")).unwrap();
        let replay = create_with_stores(&input, &store, &workflows).unwrap();
        assert_eq!(replay.mission_id, first.mission_id);
        assert_eq!(replay.workflow, first.workflow);
        assert_eq!(
            replay.assignment, assigned.assignment,
            "retry must preserve the existing exact expert bindings"
        );
        assert_eq!(
            store.snapshot(&first.mission_id, 0).unwrap().events.len(),
            event_count,
            "retry does not append another creation or assignment"
        );
        let mut changed = input.clone();
        changed.workspace.permission_mode = "workspace_write".into();
        assert!(create_with_stores(&changed, &store, &workflows)
            .unwrap_err()
            .to_string()
            .contains("idempotency"));
        fs::remove_dir_all(root).unwrap();
    }
}
