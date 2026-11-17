//! Sanitized Mission -> reusable Workflow export.
//!
//! A completed Mission is evidence that a collaboration method worked. Export
//! keeps the reusable method only: task structure, declared roles, checks and
//! acceptance. It must never carry instance identity, grants, transcripts,
//! local paths, run ids, events or usage.

use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::fs;

use crate::mission::{ExecutionBinding, MissionStatus, MissionView, TaskStatus};
use crate::workflow::{WorkflowPackage, WorkflowRole, WorkflowRuntimeStep, WorkflowStep};

fn safe_component(value: &str) -> String {
    let cleaned = value.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect::<String>();
    let cleaned = cleaned.trim_matches('.').trim_matches('-').to_string();
    if cleaned.is_empty() { "collaboration".to_string() } else { cleaned.chars().take(80).collect() }
}

fn safe_role_id(value: &str) -> String {
    let cleaned = safe_component(value);
    if cleaned.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) { cleaned } else { format!("role.{cleaned}") }
}

fn task_id(value: &str) -> String {
    let cleaned = safe_component(value).replace('.', "-");
    if cleaned.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) { cleaned.chars().take(80).collect() } else { "task".to_string() }
}

fn prompt(task: &crate::mission::MissionTaskSpec) -> String {
    format!(
        "执行协作任务：{}\n目标：{}\n验收：{}\n必须检查：{}\n遵守本次 Mission 已确认的读取、写入与权限边界；不得扩大范围，不得自行验证通过。",
        task.title, task.objective, task.acceptance_criteria.join("；"), task.required_checks.join("；")
    )
}

fn role_from_binding(role_id: &str, binding: &ExecutionBinding) -> WorkflowRole {
    WorkflowRole {
        optional: false,
        label: role_id.to_string(),
        brief: format!("承担 {role_id} 职责；只执行计划内任务，不扩大范围。"),
        runtime_provider: binding.executor.clone(),
        prompt: String::new(),
        tool_policy: "default".to_string(),
        model: binding.model.clone(),
    }
}

fn scrubbed_task(task: &crate::mission::MissionTaskSpec, role_id: &str, id: &str, deps: &BTreeMap<String, String>) -> Result<WorkflowStep, Box<dyn Error>> {
    let mut depends_on = task.depends_on.iter().map(|dependency| {
        deps.get(dependency).cloned().ok_or_else(|| format!(
            "task {} references missing exported delivery dependency {dependency}", task.id
        ))
    }).collect::<Result<Vec<_>, _>>()?;
    depends_on.sort(); depends_on.dedup();
    Ok(WorkflowStep {
        id: id.to_string(), title: task.title.clone(), kind: "runtime".to_string(),
        capability_id: String::new(), runtime: Some(WorkflowRuntimeStep {
            permission_mode: String::new(), provider: task.execution.executor.clone(), prompt: prompt(task),
            workspace_path: String::new(), result_schema: String::new(),
            allow_network: None, input_artifacts: vec![], tool_policy: "default".to_string(),
            model: task.execution.model.clone(), model_fallbacks: vec![], timeout_seconds: 900,
        }), loop_config: None, delegate: None, when: None, fail_when: None,
        candidate_action: String::new(), input: json!({}), execution_mode: "long_running".to_string(),
        risk_level: if task.write_paths.is_empty() { "read_only" } else { "local_write" }.to_string(),
        role_id: role_id.to_string(), approval_required: false, on_failure: String::new(),
        depends_on,
    })
}

/// Build a sanitized package from a completed Mission. The caller supplies the
/// reusable package identity/version so this function never reuses Mission ID as
/// the distributable identity. No source path is written into the package.
/// The legacy export entrypoint is accepted for API compatibility; strict
/// packages always execute the full delivery graph and declare no endpoints.
pub(crate) fn package_from_completed_mission(
    mission: &MissionView,
    package_id: &str,
    version: &str,
    name: &str,
    _entrypoint: &str,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    if mission.status != MissionStatus::Completed {
        return Err("only a completed Mission can be exported as a workflow".into());
    }
    // Successful repairs project completion/verification onto their ancestors
    // without replacing the original specs (MissionStore::record_verification_result).
    // Keep those delivery specs and edges: repair specs describe instance rework
    // and may omit original dependencies, so neither replay nor substitute them.
    let reusable = mission.tasks.iter().filter(|task|
        task.spec.repair_of.is_empty() && task.status != TaskStatus::Canceled
    ).collect::<Vec<_>>();
    if reusable.is_empty() {
        return Err("Mission has no independently verified reusable tasks".into());
    }
    for task in &reusable {
        if task.status != TaskStatus::Completed || task.verification_passed != Some(true) {
            return Err(format!("delivery task {} is not completed and independently verified", task.spec.id).into());
        }
    }
    let mut task_ids = BTreeMap::new();
    let mut exported_ids = BTreeMap::new();
    for task in &reusable {
        let id = task_id(&task.spec.id);
        if task_ids.insert(task.spec.id.clone(), id.clone()).is_some() {
            return Err(format!("duplicate delivery task id: {}", task.spec.id).into());
        }
        if let Some(previous) = exported_ids.insert(id.clone(), task.spec.id.clone()) {
            return Err(format!("task ID normalization collision: {previous} and {} both become {id}", task.spec.id).into());
        }
    }
    let mut role_bindings = BTreeMap::<String, ExecutionBinding>::new();
    for task in &reusable {
        let role_id = safe_role_id(&task.spec.execution.role_id);
        if let Some(existing) = role_bindings.get(&role_id) {
            if existing.role_id != task.spec.execution.role_id {
                return Err(format!("role ID normalization collision: {} and {} both become {role_id}", existing.role_id, task.spec.execution.role_id).into());
            }
            if existing != &task.spec.execution {
                return Err(format!("role {role_id} has conflicting execution bindings").into());
            }
        } else {
            role_bindings.insert(role_id, task.spec.execution.clone());
        }
    }
    let roles = role_bindings.iter().map(|(id, binding)| (id.clone(), role_from_binding(id, binding))).collect::<BTreeMap<_, _>>();
    let steps = reusable.iter().map(|task| {
        let role_id = safe_role_id(&task.spec.execution.role_id);
        scrubbed_task(&task.spec, &role_id, task_ids.get(&task.spec.id).expect("mapped task"), &task_ids)
    }).collect::<Result<Vec<_>, _>>()?;
    let mut requirements = mission.required_acceptance.clone();
    requirements.extend(mission.acceptance_criteria.clone());
    requirements.sort(); requirements.dedup();
    let exports = BTreeSet::from(["workbench".to_string(), "github".to_string()]);
    let package: WorkflowPackage = serde_json::from_value(json!({
        "schema_version": "workflow_package.v1",
        "id": package_id, "version": version, "name": name,
        "description": "从已验收协作任务沉淀的可复用工作流。",
        "release_notes": "由已完成 Mission 脱敏生成；仅保留角色、步骤、检查与验收，不自动提交或发布。",
        "min_agent_version": "0.3.92",
        "supported_runtimes": role_bindings.values().map(|binding| binding.executor.clone()).collect::<BTreeSet<_>>(),
        "distribution_targets": exports,
        "roles": roles, "steps": steps, "artifacts": [],
        "execution_policy": "strict",
        "entrypoints": [], "default_entrypoint": "",
        "exits": [], "default_exitpoint": "",
        "ui": {"mode": "standard", "entry": "", "surfaces": ["agent"]},
        "dependencies": {"skills": [], "plugins": [], "connectors": [], "runtimes": []},
        "created_at": ""
    }))?;
    package.validate()?;
    Ok(package)
}

/// Write a validated package into a staging directory. The writer intentionally
/// takes no Mission workspace and never copies from it.
pub(crate) fn write_package(root: &Path, package: &WorkflowPackage) -> Result<PathBuf, Box<dyn Error>> {
    package.validate()?;
    fs::create_dir_all(root)?;
    fs::write(root.join("workflow.json"), serde_json::to_vec_pretty(package)?)?;
    Ok(root.join("workflow.json"))
}

/// Export a completed Mission into a caller-specified staging directory and
/// prove the directory can be loaded by the normal workflow package loader.
/// This function writes only the sanitized package; admission to the authoring
/// pipeline remains a separate, explicit call.
pub(crate) fn write_export_package(
    mission: &MissionView,
    root: &Path,
    package_id: &str,
    version: &str,
    name: &str,
    entrypoint: &str,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let package = package_from_completed_mission(mission, package_id, version, name, entrypoint)?;
    if root.exists() {
        let entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
        if !entries.is_empty() {
            return Err("workflow export directory must be empty".into());
        }
    }
    write_package(root, &package)?;
    let loaded = crate::workflow::load_from_directory(root)?;
    let expected = serde_json::to_value(&package)?;
    let actual = serde_json::to_value(&loaded)?;
    if expected != actual {
        return Err("workflow export changed while round-tripping through the package loader".into());
    }
    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::{MissionBudget, MissionConfirmation, MissionPlanMode, MissionTaskView, MissionWorkspace};
    use crate::workflow::WorkflowRunner;

    fn completed_mission() -> MissionView {
        let spec = crate::mission::MissionTaskSpec {
            id: "fix-1".to_string(), title: "Fix".to_string(), objective: "Change code".to_string(),
            depends_on: vec![], read_paths: vec!["src".to_string()],
            write_paths: vec!["src/lib.rs".to_string()], acceptance_criteria: vec!["tests pass".to_string()],
            required_checks: vec!["node --test".to_string()],
            execution: ExecutionBinding { expert: None, executor: "himind.builtin".to_string(), service_id: "svc".to_string(), model: "himind:svc:model".to_string(), role_id: "implementer".to_string() },
            repair_of: String::new(),
        };
        MissionView {
            scheduling: None, schema_version: "mission.v1".to_string(), mission_id: "mission_secret".to_string(), title: "Secret title".to_string(),
            objective: "Secret objective".to_string(), status: MissionStatus::Completed,
            workspace: MissionWorkspace { permission_mode: String::new(), root: "F:/secret/workspace".to_string(), read_scope: vec!["secret".to_string()], write_scope: vec!["secret".to_string()] },
            acceptance_criteria: vec!["tests pass".to_string()], plan_revision: 1, plan_sha256: "a".repeat(64),
            plan_summary: "secret plan".to_string(), required_acceptance: vec!["tests pass".to_string()], proposed_tasks: vec![],
            budget: Some(MissionBudget { max_rounds: 1, max_minutes: 1, max_repair_attempts: 0, max_total_tokens: None, max_cost: None, currency: None }),
            cost_usage: Default::default(), effective_wait: None,
            usage: Some(crate::agent_core_contracts::LocalRunUsage { input_tokens: 1, output_tokens: 1, estimated_cost: 1.0, currency: "USD".to_string(), billing_owner: "secret".to_string() }),
            handoff: Some(crate::mission::MissionHandoff { idempotency_key: "secret-key".to_string(), source_client: "codex".to_string(), source_reference: "secret-thread".to_string(), source_message_refs: vec![], context_summary: "secret transcript".to_string(), request_sha256: "b".repeat(64), received_at: "2026-11-10T00:00:00Z".to_string() }),
            workflow: None, assignment: None, plan_mode: MissionPlanMode::Generated,
            confirmation: Some(MissionConfirmation { plan_sha256: "a".repeat(64), selected_task_ids: vec!["fix-1".to_string()], approved_absolute_read_paths: vec![], approved_absolute_write_paths: vec![], approved_paths: vec!["secret".to_string()], confirmed_by: "secret".to_string(), confirmed_at: "2026-11-10T00:00:00Z".to_string() }),
            tasks: vec![MissionTaskView { spec, status: TaskStatus::Completed, attempt_run_id: "run-secret".to_string(), verification_run_id: "verify-secret".to_string(), verification_passed: Some(true), evidence_refs: vec!["secret-evidence".to_string()] }],
            created_at: "2026-11-10T00:00:00Z".to_string(), updated_at: "2026-11-10T00:00:00Z".to_string(),
        }
    }

    fn delivery(id: &str, depends_on: &[&str]) -> MissionTaskView {
        let mut task = completed_mission().tasks.remove(0);
        task.spec.id = id.to_string();
        task.spec.title = format!("Deliver {id}");
        task.spec.depends_on = depends_on.iter().map(|id| id.to_string()).collect();
        task
    }

    #[test]
    fn only_completed_missions_can_be_exported() {
        let mut mission = completed_mission();
        mission.status = MissionStatus::Active;
        assert!(package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap_err().to_string().contains("completed"));
    }

    #[test]
    fn default_plan_covers_three_independent_deliveries_and_their_downstream() {
        let mut mission = completed_mission();
        mission.tasks = vec![
            delivery("analysis", &[]), delivery("design", &[]), delivery("presentation", &[]),
            delivery("synthesis", &["analysis", "design"]), delivery("review", &["synthesis"]),
        ];
        let package = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap();
        assert_eq!(package.execution_policy, "strict");
        assert!(package.entrypoints.is_empty());
        assert!(package.exits.is_empty());
        assert!(package.default_entrypoint.is_empty());
        assert!(package.default_exitpoint.is_empty());
        assert_eq!(package.steps.len(), 5);
        assert_eq!(package.steps[3].depends_on, ["analysis", "design"]);
        assert_eq!(package.steps[4].depends_on, ["synthesis"]);
        let plan = WorkflowRunner::build_execution_plan(&package, &json!({})).unwrap();
        assert_eq!(plan.active_step_ids, ["analysis", "design", "presentation", "synthesis", "review"]);
        assert_eq!(plan.entrypoint, "full");
        assert_eq!(plan.exitpoint, "full");
        assert!(plan.assumptions.is_empty());
    }

    #[test]
    fn legacy_export_entrypoint_is_compatible_with_strict_full_graph() {
        let mission = completed_mission();
        let expected = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "").unwrap();
        for entrypoint in ["start", "custom-entry"] {
            let package = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", entrypoint).unwrap();
            assert_eq!(serde_json::to_value(&package).unwrap(), serde_json::to_value(&expected).unwrap());
            let plan = WorkflowRunner::build_execution_plan(&package, &json!({})).unwrap();
            assert_eq!(plan.active_step_ids, ["fix-1"]);
            let error = WorkflowRunner::build_execution_plan(&package, &json!({"execution": {"entrypoint": entrypoint}})).unwrap_err();
            assert!(error.to_string().contains("strict workflow does not allow selecting"));
        }
    }

    #[test]
    fn multilevel_repairs_export_original_delivery_graph_without_instance_history() {
        let mut mission = completed_mission();
        mission.tasks.push(delivery("review", &["fix-1"]));
        let expected = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap();
        // These ancestors are completed/verified by repair projection, while
        // their original failed run identities remain in the Mission view.
        mission.tasks[0].attempt_run_id = "failed-original-run".into();
        mission.tasks[0].verification_run_id = "failed-original-verification".into();
        let mut first = delivery("repair-first", &[]);
        first.spec.repair_of = "fix-1".into();
        first.spec.title = "Instance-only repair instructions".into();
        first.spec.objective = "Fix the previous failed-original-run using old-instance-output".into();
        first.spec.required_checks.push("node --test old-instance-output.test.js".into());
        let mut final_repair = first.clone();
        final_repair.spec.id = "repair-final".into();
        final_repair.spec.repair_of = "repair-first".into();
        let mut review_retry = delivery("review-retry", &["repair-final"]);
        review_retry.spec.repair_of = "review".into();
        let mut withdrawn = first.clone();
        withdrawn.spec.id = "repair-withdrawn".into();
        withdrawn.status = TaskStatus::Canceled;
        withdrawn.verification_passed = None;
        mission.tasks.extend([first, final_repair, review_retry, withdrawn]);
        let original = mission.clone();
        let package = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap();
        assert_eq!(mission, original);
        assert_eq!(serde_json::to_value(&package).unwrap(), serde_json::to_value(&expected).unwrap());
        let plan = WorkflowRunner::build_execution_plan(&package, &json!({})).unwrap();
        assert_eq!(plan.active_step_ids, ["fix-1", "review"]);
        assert_eq!(package.steps[1].depends_on, ["fix-1"]);
        let serialized = serde_json::to_string(&package).unwrap();
        for forbidden in ["repair-first", "repair-final", "review-retry", "repair-withdrawn", "Instance-only", "failed-original-run", "failed-original-verification", "old-instance-output"] {
            assert!(!serialized.contains(forbidden), "export replayed history: {forbidden}");
        }
    }

    #[test]
    fn export_rejects_missing_or_excluded_delivery_dependencies() {
        for dependency in ["missing", "canceled", "repair-history", "fix.1"] {
            let mut mission = completed_mission();
            let mut canceled = delivery("canceled", &[]);
            canceled.status = TaskStatus::Canceled;
            canceled.verification_passed = None;
            let mut repair = delivery("repair-history", &[]);
            repair.spec.repair_of = "fix-1".into();
            mission.tasks.extend([canceled, repair, delivery("review", &[dependency])]);
            let error = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap_err().to_string();
            assert!(error.contains("missing exported delivery dependency"), "{error}");
            assert!(error.contains(dependency), "{error}");
        }
    }

    #[test]
    fn export_rejects_task_id_normalization_collisions() {
        let prefix = "a".repeat(80);
        for (first, second) in [
            ("task.one".to_string(), "task-one".to_string()),
            (format!("{prefix}a"), format!("{prefix}b")),
            ("_private".to_string(), "task".to_string()),
        ] {
            let mut mission = completed_mission();
            mission.tasks = vec![delivery(&first, &[]), delivery(&second, &[])];
            let error = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap_err().to_string();
            assert!(error.contains("task ID normalization collision"), "{error}");
            assert!(error.contains(&first) && error.contains(&second), "{error}");
        }
    }

    #[test]
    fn export_rejects_role_id_normalization_collisions() {
        let mut mission = completed_mission();
        mission.tasks.push(delivery("review", &["fix-1"]));
        mission.tasks[0].spec.execution.role_id = ".writer".into();
        mission.tasks[1].spec.execution.role_id = "writer".into();
        let error = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap_err().to_string();
        assert!(error.contains("role ID normalization collision"), "{error}");
    }

    #[test]
    fn export_rejects_unverified_original_deliveries_instead_of_omitting_them() {
        for status in [TaskStatus::Completed, TaskStatus::Failed] {
            let mut mission = completed_mission();
            let mut unverified = delivery("independent", &[]);
            unverified.status = status;
            unverified.verification_passed = Some(false);
            mission.tasks.push(unverified);
            let error = package_from_completed_mission(&mission, "com.example.export", "1.0.0", "Export", "start").unwrap_err().to_string();
            assert!(error.contains("delivery task independent is not completed and independently verified"), "{error}");
        }
    }

    #[test]
    fn export_writes_a_loadable_sanitized_package() {
        let root = std::env::temp_dir().join(format!(
            "himind-mission-export-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let package = write_export_package(
            &completed_mission(), &root, "com.example.export-file", "1.0.0",
            "Export file", "start",
        ).unwrap();
        assert_eq!(package.id, "com.example.export-file");
        let text = fs::read_to_string(root.join("workflow.json")).unwrap();
        assert!(!text.contains("F:/secret/workspace"));
        assert!(!text.contains("mission_secret"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn export_drops_instance_and_private_data() {
        let package = package_from_completed_mission(&completed_mission(), "com.example.export", "1.0.0", "Export", "start").unwrap();
        let serialized = serde_json::to_string(&package).unwrap();
        for forbidden in ["mission_secret", "run-secret", "verify-secret", "secret transcript", "F:/secret/workspace", "secret-evidence", "USD", "secret-key"] {
            assert!(!serialized.contains(forbidden), "export leaked {forbidden}");
        }
        assert_eq!(package.steps.len(), 1);
        assert_eq!(package.roles["implementer"].model, "himind:svc:model");
        assert_eq!(package.steps[0].runtime.as_ref().unwrap().workspace_path, "");
        assert!(package.steps[0].runtime.as_ref().unwrap().prompt.contains("node --test"));
    }
}
