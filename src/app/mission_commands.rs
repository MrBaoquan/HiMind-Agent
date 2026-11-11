//! Tauri command surface for persistent expert-team missions.
//!
//! The store owns plan hashes, dependency validation and event sequencing. These
//! commands only marshal JSON across the local UI boundary and run SQLite work on
//! a blocking pool.

use crate::capability::service::CapabilityGateway;
use std::fs;
use crate::mission::{
    parse_plan_json, MissionAssignmentInput, MissionAssignmentOptions, MissionCreateInput,
    MissionHandoffInput, MissionPlanProposal, MissionRoleOption, MissionSnapshot, MissionStore,
    MissionTaskSpec, MissionView,
};

#[derive(serde::Serialize)]
pub(crate) struct MissionExecutionOption {
    pub executor: String,
    pub label: String,
    pub available: bool,
    pub reason: String,
    pub models: Vec<MissionModelOption>,
}

#[derive(serde::Serialize)]
pub(crate) struct MissionModelOption {
    pub alias: String,
    pub label: String,
}

fn list_mission_execution_options_sync() -> Result<Vec<MissionExecutionOption>, String> {
        let mut options = vec![MissionExecutionOption {
            executor: "himind.builtin".to_string(),
            label: "HiMind built-in Runtime".to_string(),
            available: true,
            reason: String::new(),
            models: crate::app::builtin_ai_router::model_catalog()
                .unwrap_or_default()
                .into_iter()
                .map(|route| MissionModelOption {
                    alias: route.alias,
                    label: route.display_name,
                })
                .collect(),
        }];
        if std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1") {
            options.push(MissionExecutionOption {
                executor: "himind.fixture".to_string(),
                label: "HiMind deterministic fixture".to_string(),
                available: true,
                reason: String::new(),
                models: Vec::new(),
            });
        }
        for (executor, label, ready) in [
            ("personal.codex", "Codex CLI", crate::runtime::codex::probe().status == "ready"),
            ("personal.github-copilot", "GitHub Copilot CLI", crate::runtime::copilot::probe().status == "ready"),
        ] {
            options.push(MissionExecutionOption {
                executor: executor.to_string(),
                label: label.to_string(),
                available: ready,
                reason: if ready { String::new() } else { "local CLI unavailable".to_string() },
                models: Vec::new(),
            });
        }
        let reports = crate::runtime::acp::probe_installations()
            .into_iter()
            .map(|report| (report.provider.clone(), report))
            .collect::<std::collections::BTreeMap<_, _>>();
        for profile in crate::store::acp_profiles::list().map_err(|error| error.to_string())? {
            if !profile.enabled {
                continue;
            }
            let report = reports.get(&profile.provider_id);
            let available = report.is_some_and(|report| report.status == "ready");
            options.push(MissionExecutionOption {
                executor: profile.provider_id,
                label: profile.display_name,
                available,
                reason: if available {
                    String::new()
                } else {
                    report.map(|report| format!("{:?}", report.capabilities))
                        .unwrap_or_else(|| "ACP profile 未通过运行时探测".to_string())
                },
                models: Vec::new(),
            });
        }
        Ok(options)
}

#[tauri::command]
pub(crate) async fn list_mission_execution_options() -> Result<Vec<MissionExecutionOption>, String> {
    tauri::async_runtime::spawn_blocking(list_mission_execution_options_sync)
        .await
        .map_err(|error| error.to_string())?
}

pub(crate) fn mission_assignment_options_sync(mission_id: &str) -> Result<MissionAssignmentOptions, String> {
    let store = MissionStore::open_default().map_err(|error| error.to_string())?;
    let mission = store.get(mission_id).map_err(|error| error.to_string())?;
    let pin = mission.workflow.as_ref().ok_or("mission has no pinned collaboration workflow")?;
    let workflows = crate::workflow::WorkflowStore::open_default().map_err(|error| error.to_string())?;
    let (package, digest) = workflows
        .load_pinned_version(&pin.workflow_id, &pin.workflow_version, &pin.workflow_sha256)
        .map_err(|error| error.to_string())?;
    let execution_options = crate::app::mission_commands::list_mission_execution_options_sync()?;
    let routes = crate::app::builtin_ai_router::model_catalog().unwrap_or_default();
    let default_executor = execution_options
        .iter()
        .find(|option| option.executor == "himind.builtin" && option.available)
        .or_else(|| execution_options.iter().find(|option| option.available));
    let default_route = routes.first();
    let mut roles = package.roles.into_iter().map(|(role_id, role)| {
        let recommended_executor = if role.runtime_provider.trim().is_empty() {
            default_executor.map(|option| option.executor.clone()).unwrap_or_default()
        } else {
            role.runtime_provider.trim().to_string()
        };
        let available = execution_options.iter().find(|option| option.executor == recommended_executor);
        let model_override_supported = recommended_executor == "himind.builtin";
        let recommended_model = if model_override_supported {
            if role.model.trim().is_empty() {
                default_route.map(|route| route.alias.clone()).unwrap_or_default()
            } else {
                role.model.trim().to_string()
            }
        } else {
            String::new()
        };
        let recommended_service_id = crate::workflow::model_alias_parts(&recommended_model)
            .map(|(_, service, _)| service.to_string()).unwrap_or_default();
        let (availability, unavailable_reason) = match available {
            Some(option) if option.available => ("available".to_string(), String::new()),
            Some(option) => ("unavailable".to_string(), option.reason.clone()),
            None if recommended_executor.is_empty() => ("unavailable".to_string(), "没有可用的执行器".to_string()),
            None => ("unavailable".to_string(), format!("执行器未安装：{recommended_executor}")),
        };
        MissionRoleOption {
            role_id, label: role.label, brief: role.brief, recommended_executor,
            recommended_service_id, recommended_model, model_override_supported,
            availability, unavailable_reason,
        }
    }).collect::<Vec<_>>();
    roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
    Ok(MissionAssignmentOptions {
        workflow_id: pin.workflow_id.clone(), workflow_version: pin.workflow_version.clone(),
        workflow_sha256: digest, roles, source: "workflow".to_string(),
    })
}

#[tauri::command]
pub(crate) async fn create_mission_handoff(input: MissionHandoffInput) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.create_handoff(&input))
            .map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn list_mission_assignment_options(mission_id: String) -> Result<MissionAssignmentOptions, String> {
    tauri::async_runtime::spawn_blocking(move || mission_assignment_options_sync(&mission_id))
        .await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn assign_mission_roles(mission_id: String, input: MissionAssignmentInput) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.assign_roles(&mission_id, &input))
            .map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}

fn export_mission_workflow_sync(
    gateway: &CapabilityGateway,
    mission_id: &str,
    workflow_id: &str,
    version: &str,
    name: &str,
    entrypoint: &str,
) -> Result<crate::workflow::WorkflowDraft, String> {
    let store = MissionStore::open_default().map_err(|error| error.to_string())?;
    let mission = store.get(mission_id).map_err(|error| error.to_string())?;
    let safe_id = workflow_id.trim();
    let safe_version = version.trim();
    if safe_id.is_empty() || safe_version.is_empty() {
        return Err("workflow_id and version are required".to_string());
    }
    let root = crate::store::paths::agent_home()
        .join("mission-exports")
        .join(mission_id)
        .join(format!("{safe_id}-{safe_version}-{}", rand::random::<u64>()));
    let result = (|| {
        crate::mission::export::write_export_package(
            &mission, &root, safe_id, safe_version, name, entrypoint,
        )?;
        let draft = crate::workflow::save_authoring_candidate(&root)?;
        let capabilities = gateway
            .list_capabilities(&crate::capability::types::InvocationContext::tauri())?;
        crate::workflow::test_authoring_candidate_with_capabilities(
            &draft.package_id,
            &draft.version,
            &capabilities,
        )
    })();
    // The persisted authoring candidate no longer needs the transient package
    // staging tree. Keeping it would accumulate full workspaces and private
    // mission material on every export.
    if let Err(error) = fs::remove_dir_all(&root) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("mission workflow export staging cleanup failed: {error}");
        }
    }
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn export_mission_workflow(
    mission_id: String,
    workflow_id: String,
    version: String,
    name: String,
    entrypoint: String,
    options: tauri::State<'_, crate::app::commands::AgentState>,
) -> Result<crate::workflow::WorkflowDraft, String> {
    let gateway = options.capability_gateway.clone();
    tauri::async_runtime::spawn_blocking(move || {
        export_mission_workflow_sync(
            &gateway, &mission_id, &workflow_id, &version, &name, &entrypoint,
        )
    }).await.map_err(|error| error.to_string())?
}

pub(crate) fn export_mission_workflow_for_mcp(
    gateway: &CapabilityGateway,
    input: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mission_id = input.get("mission_id").and_then(serde_json::Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty()).ok_or("mission.workflow.export requires mission_id")?;
    let workflow_id = input.get("workflow_id").and_then(serde_json::Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty()).ok_or("mission.workflow.export requires workflow_id")?;
    let version = input.get("version").and_then(serde_json::Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty()).ok_or("mission.workflow.export requires version")?;
    let name = input.get("name").and_then(serde_json::Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty()).unwrap_or(workflow_id);
    let entrypoint = input.get("entrypoint").and_then(serde_json::Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty()).unwrap_or("start");
    let draft = export_mission_workflow_sync(gateway, mission_id, workflow_id, version, name, entrypoint)
        .map_err(std::io::Error::other)?;
    Ok(serde_json::to_value(draft)?)
}

#[tauri::command]
pub(crate) async fn update_mission_task_execution(
    mission_id: String,
    task_id: String,
    expected_plan_sha256: String,
    execution: crate::mission::ExecutionBinding,
) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| {
                store.update_task_execution(
                    &mission_id,
                    &task_id,
                    &expected_plan_sha256,
                    &execution,
                )
            })
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn create_mission(input: MissionCreateInput) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.create(&input))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn list_missions(limit: Option<usize>) -> Result<Vec<MissionView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.list(limit.unwrap_or(50)))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_mission(mission_id: String) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.get(&mission_id))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn propose_mission_plan(
    mission_id: String,
    proposal: MissionPlanProposal,
) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.propose_plan(&mission_id, &proposal))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn confirm_mission_plan(
    mission_id: String,
    plan_sha256: String,
    selected_task_ids: Vec<String>,
    approved_paths: Vec<String>,
    approved_absolute_read_paths: Vec<String>,
    approved_absolute_write_paths: Vec<String>,
    confirmed_by: String,
) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| {
                store.confirm_plan(
                    &mission_id,
                    &plan_sha256,
                    &selected_task_ids,
                    &approved_paths,
                    &approved_absolute_read_paths,
                    &approved_absolute_write_paths,
                    &confirmed_by,
                )
            })
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn add_mission_follow_up_tasks(
    mission_id: String,
    tasks: Vec<MissionTaskSpec>,
    reason: String,
) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.add_follow_up_tasks(&mission_id, &tasks, &reason))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn get_mission_snapshot(
    mission_id: String,
    after_sequence: Option<u64>,
) -> Result<MissionSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        MissionStore::open_default()
            .and_then(|store| store.snapshot(&mission_id, after_sequence.unwrap_or(0)))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn generate_mission_plan(
    mission_id: String,
    options: tauri::State<'_, crate::app::commands::AgentState>,
) -> Result<MissionView, String> {
    let options = options.options.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = MissionStore::open_default().map_err(|error| error.to_string())?;
        let mission = store.get(&mission_id).map_err(|error| error.to_string())?;
        if !matches!(mission.status, crate::mission::MissionStatus::Draft | crate::mission::MissionStatus::Planned | crate::mission::MissionStatus::AwaitingConfirmation) {
            return Err("当前 Mission 状态不允许重新生成计划".to_string());
        }
        let prompt = crate::mission::plan_prompt(&mission);
        let outcome = crate::runtime::deepseek_harness::execute_workflow_in_run_home(
            &options, &mission.workspace.root, &prompt, 240, true,
            &format!("mission-plan-{}", mission_id), "PLAN", None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,
            &|| Ok(false),
        ).map_err(|error| error.to_string())?;
        let proposal = parse_plan_json(&outcome.text)?;
        store.propose_plan(&mission_id, &proposal).map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn generate_mission_follow_up(
    mission_id: String,
    options: tauri::State<'_, crate::app::commands::AgentState>,
) -> Result<MissionView, String> {
    let options = options.options.clone();
    tauri::async_runtime::spawn_blocking(move || generate_mission_follow_up_sync(&options, &mission_id))
        .await.map_err(|error| error.to_string())?
}

pub(crate) fn generate_mission_follow_up_sync(options: &crate::Options, mission_id: &str) -> Result<MissionView, String> {
        let store = MissionStore::open_default().map_err(|error| error.to_string())?;
        let mission = store.get(mission_id).map_err(|error| error.to_string())?;
        if let Some(reason) = store.budget_exhaustion_reason(&mission).map_err(|error| error.to_string())? { return Err(reason); }
        let snapshot = store.snapshot(mission_id, 0).map_err(|error| error.to_string())?;
        let failed_all = mission.tasks.iter()
            .filter(|task| task.status == crate::mission::TaskStatus::Failed)
            .collect::<Vec<_>>();
        if failed_all.is_empty() {
            return Err("当前没有验证失败的任务，不能生成跟进任务".to_string());
        }
        // A repair must target the newest failed leaf. A failed repair is itself
        // a leaf and must be retried; an ancestor with a repair child is not.
        let parent_ids = mission.tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty())
            .map(|task| task.spec.repair_of.as_str())
            .collect::<std::collections::HashSet<_>>();
        let failed_repair_ids = mission.tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty() && task.status == crate::mission::TaskStatus::Failed)
            .map(|task| task.spec.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let leaf_failures = failed_all.iter().copied()
            .filter(|task| {
                let id = task.spec.id.as_str();
                // Retry failed repairs themselves, and only use tasks with no
                // repair child as roots for a new repair chain.
                failed_repair_ids.contains(id) || !parent_ids.contains(id)
            })
            .collect::<Vec<_>>();
        let targets = if leaf_failures.is_empty() { failed_all.clone() } else { leaf_failures };
        let failure_evidence = snapshot.events.iter()
            .filter(|event| matches!(event.event_type.as_str(), "verification_failed" | "task_failed"))
            .map(|event| serde_json::json!({
                "task_id": event.task_id,
                "reason": event.payload.get("reason").and_then(serde_json::Value::as_str).unwrap_or_default(),
                "evidence_refs": event.payload.get("evidence_refs"),
            }))
            .collect::<Vec<_>>();
        let mut current_checks = Vec::new();
        for failed_task in &targets {
            let checks = crate::mission::run_required_checks(std::path::Path::new(&mission.workspace.root),
                &failed_task.spec.required_checks, std::time::Duration::from_secs(60), &|| Ok(false));
            current_checks.push(serde_json::json!({"task_id": failed_task.spec.id, "checks": checks}));
        }
        let observation = store.append_event(mission_id, "host_required_checks_observed", "", "跟进前宿主检查已完成",
            serde_json::json!({"plan_sha256": mission.plan_sha256, "checks": current_checks})).map_err(|error| error.to_string())?;
        let mut prompt = format!(
            "你是专家团队的协调 Agent。根据以下失败验收证据，在已确认的修改范围内生成定向跟进任务。只输出 JSON，不直接修改文件或派发。\nMission：{}\n失败任务：{}\n失败证据：{}\n已批准写入范围：{}\n要求：每个新任务必须 repair_of 指向一个失败任务；新任务 ID 不同于现有任务；只能写入已批准范围；每项必须有 executor=himind.builtin、role_id、acceptance_criteria、required_checks；禁止在检查为空时声称通过。\n输出结构：{{\"summary\":\"...\",\"tasks\":[{{\"id\":\"repair-1\",\"title\":\"...\",\"objective\":\"...\",\"depends_on\":[],\"read_paths\":[],\"write_paths\":[],\"acceptance_criteria\":[\"...\"],\"required_checks\":[\"...\"],\"execution\":{{\"executor\":\"himind.builtin\",\"model\":\"\",\"role_id\":\"implementer\"}},\"repair_of\":\"failed-task-id\"}}],\"required_acceptance\":[],\"budget\":{{\"max_rounds\":4,\"max_minutes\":60,\"max_repair_attempts\":1}}}}",
            mission.title,
            targets.iter().map(|task| task.spec.id.clone()).collect::<Vec<_>>().join("、"),
            serde_json::to_string(&failure_evidence).unwrap_or_default(),
            mission.confirmation.as_ref().map(|value| value.approved_paths.join("、")).unwrap_or_default(),
        );
        prompt.push_str(&format!("\n原失败任务（必须保留 required_checks，可按 Mission 目标修订阶段性验收）：{}\n所有已确认路径授权：{}\nrequired_checks 只允许可执行命令，不得添加自然语言。",
            serde_json::to_string(&targets).unwrap_or_default(), serde_json::to_string(&mission.confirmation).unwrap_or_default()));
        prompt.push_str(&format!("\nMission objective: {}\nMission acceptance: {}\nThe follow-up must advance the Mission to its acceptance, not merely rephrase the failure. If the host observed required check is red and the acceptance requires it green, the repair task must change the implementation within the confirmed write scope so the check can pass; do not return another documentation-only task unless the confirmed scope truly has no source path capable of fixing it.",
            mission.objective, serde_json::to_string(&mission.acceptance_criteria).unwrap_or_default()));
        prompt.push_str(&format!("\nHost observed current checks (not developer self-report), event {}: {}\nWhen the worker sandbox cannot run these commands, use this host observation as a cited transcript; copy its numeric TAP totals and actual exit code into the repair objective. Do not fabricate a worker execution. Return only one repair task for the most recent failed leaf task; do not repair an ancestor when its existing repair task failed.",
            observation.event_id, serde_json::to_string(&current_checks).unwrap_or_default()));
        let outcome = crate::runtime::deepseek_harness::execute_workflow_in_run_home(
            &options, &mission.workspace.root, &prompt, 240, true,
            &format!("mission-followup-{}", mission_id), "FOLLOWUP", None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,
            &|| Ok(false),
        ).map_err(|error| error.to_string())?;
        let proposal = parse_plan_json(&outcome.text)?;
        let failed_ids = targets.iter().map(|task| task.spec.id.as_str()).collect::<std::collections::HashSet<_>>();
        if proposal.tasks.is_empty() || proposal.tasks.iter().any(|task| !failed_ids.contains(task.repair_of.as_str())) {
            return Err("协调 Agent 的跟进任务未绑定到失败任务，已拒绝派发".to_string());
        }
        store.add_follow_up_tasks(mission_id, &proposal.tasks, &proposal.summary)
            .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, repair_of: &str, status: crate::mission::TaskStatus) -> crate::mission::MissionTaskView {
        crate::mission::MissionTaskView {
            spec: crate::mission::MissionTaskSpec {
                id: id.to_string(),
                title: id.to_string(),
                objective: String::new(),
                depends_on: Vec::new(),
                read_paths: Vec::new(),
                write_paths: Vec::new(),
                acceptance_criteria: vec!["ok".to_string()],
                required_checks: vec!["node --test".to_string()],
                execution: crate::mission::ExecutionBinding {
                    executor: "himind.builtin".to_string(),
                    model: String::new(),
                    role_id: "implementer".to_string(),
                    service_id: String::new(),
                },
                repair_of: repair_of.to_string(),
            },
            status,
            attempt_run_id: String::new(),
            verification_run_id: String::new(),
            verification_passed: None,
            evidence_refs: Vec::new(),
        }
    }

    #[test]
    fn follow_up_targets_latest_failed_repair_leaf_not_ancestor() {
        let tasks = vec![
            task("doc-1", "", crate::mission::TaskStatus::Failed),
            task("repair-1", "doc-1", crate::mission::TaskStatus::Failed),
        ];
        let parent_ids = tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty())
            .map(|task| task.spec.repair_of.as_str())
            .collect::<std::collections::HashSet<_>>();
        let failed_repair_ids = tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty() && task.status == crate::mission::TaskStatus::Failed)
            .map(|task| task.spec.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let targets = tasks.iter()
            .filter(|task| task.status == crate::mission::TaskStatus::Failed)
            .filter(|task| {
                let id = task.spec.id.as_str();
                failed_repair_ids.contains(id) || !parent_ids.contains(id)
            })
            .map(|task| task.spec.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(targets, vec!["repair-1"]);
    }

    #[test]
    fn follow_up_retries_a_failed_repair_chain_leaf() {
        let tasks = vec![
            task("doc-1", "", crate::mission::TaskStatus::Failed),
            task("repair-1", "doc-1", crate::mission::TaskStatus::Failed),
            task("repair-2", "repair-1", crate::mission::TaskStatus::Failed),
        ];
        let parent_ids = tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty())
            .map(|task| task.spec.repair_of.as_str())
            .collect::<std::collections::HashSet<_>>();
        let failed_repair_ids = tasks.iter()
            .filter(|task| !task.spec.repair_of.is_empty() && task.status == crate::mission::TaskStatus::Failed)
            .map(|task| task.spec.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let targets = tasks.iter()
            .filter(|task| task.status == crate::mission::TaskStatus::Failed)
            .filter(|task| {
                let id = task.spec.id.as_str();
                failed_repair_ids.contains(id) || !parent_ids.contains(id)
            })
            .map(|task| task.spec.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(targets, vec!["repair-1", "repair-2"]);
    }
}

#[tauri::command]
pub(crate) async fn control_mission(mission_id: String, action: String) -> Result<MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::app::mission_dispatch::control_mission(&mission_id, &action).map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}
