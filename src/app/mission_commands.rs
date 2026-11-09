//! Tauri command surface for persistent expert-team missions.
//!
//! The store owns plan hashes, dependency validation and event sequencing. These
//! commands only marshal JSON across the local UI boundary and run SQLite work on
//! a blocking pool.

use crate::mission::{
    parse_plan_json, MissionCreateInput, MissionPlanProposal, MissionSnapshot, MissionStore,
    MissionTaskSpec, MissionView,
};

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
    tauri::async_runtime::spawn_blocking(move || {
        let store = MissionStore::open_default().map_err(|error| error.to_string())?;
        let mission = store.get(&mission_id).map_err(|error| error.to_string())?;
        let snapshot = store.snapshot(&mission_id, 0).map_err(|error| error.to_string())?;
        let failed = mission.tasks.iter()
            .filter(|task| task.status == crate::mission::TaskStatus::Failed)
            .collect::<Vec<_>>();
        if failed.is_empty() {
            return Err("当前没有验证失败的任务，不能生成跟进任务".to_string());
        }
        let failure_evidence = snapshot.events.iter()
            .filter(|event| matches!(event.event_type.as_str(), "verification_failed" | "task_failed"))
            .map(|event| serde_json::json!({
                "task_id": event.task_id,
                "reason": event.payload.get("reason").and_then(serde_json::Value::as_str).unwrap_or_default(),
                "evidence_refs": event.payload.get("evidence_refs"),
            }))
            .collect::<Vec<_>>();
        let prompt = format!(
            "你是专家团队的协调 Agent。根据以下失败验收证据，在已确认的修改范围内生成定向跟进任务。只输出 JSON，不直接修改文件或派发。\nMission：{}\n失败任务：{}\n失败证据：{}\n已批准写入范围：{}\n要求：每个新任务必须 repair_of 指向一个失败任务；新任务 ID 不同于现有任务；只能写入已批准范围；每项必须有 executor=himind.builtin、role_id、acceptance_criteria、required_checks；禁止在检查为空时声称通过。\n输出结构：{{\"summary\":\"...\",\"tasks\":[{{\"id\":\"repair-1\",\"title\":\"...\",\"objective\":\"...\",\"depends_on\":[],\"read_paths\":[],\"write_paths\":[],\"acceptance_criteria\":[\"...\"],\"required_checks\":[\"...\"],\"execution\":{{\"executor\":\"himind.builtin\",\"model\":\"\",\"role_id\":\"implementer\"}},\"repair_of\":\"failed-task-id\"}}],\"required_acceptance\":[],\"budget\":{{\"max_rounds\":4,\"max_minutes\":60,\"max_repair_attempts\":1}}}}",
            mission.title,
            failed.iter().map(|task| task.spec.id.clone()).collect::<Vec<_>>().join("、"),
            serde_json::to_string(&failure_evidence).unwrap_or_default(),
            mission.confirmation.as_ref().map(|value| value.approved_paths.join("、")).unwrap_or_default(),
        );
        let outcome = crate::runtime::deepseek_harness::execute_workflow_in_run_home(
            &options, &mission.workspace.root, &prompt, 240, true,
            &format!("mission-followup-{}", mission_id), "FOLLOWUP", None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,
            &|| Ok(false),
        ).map_err(|error| error.to_string())?;
        let proposal = parse_plan_json(&outcome.text)?;
        let failed_ids = failed.iter().map(|task| task.spec.id.as_str()).collect::<std::collections::HashSet<_>>();
        if proposal.tasks.is_empty() || proposal.tasks.iter().any(|task| !failed_ids.contains(task.repair_of.as_str())) {
            return Err("协调 Agent 的跟进任务未绑定到失败任务，已拒绝派发".to_string());
        }
        store.add_follow_up_tasks(&mission_id, &proposal.tasks, &proposal.summary)
            .map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}
