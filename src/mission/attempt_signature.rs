//! Compatibility across incremental revisions is scoped to an immutable Attempt.
use super::{MissionEvent, MissionTaskView, MissionView, TaskStatus};
use serde_json::json;
use sha2::{Digest, Sha256};

pub(crate) fn input_signature(mission: &MissionView, task: &MissionTaskView) -> String {
    let dependencies=task.spec.depends_on.iter().map(|id| {
        let producer=mission.tasks.iter().find(|item|item.spec.id==*id);
        // Retain the accepted repair lineage as well as the logical dependency.
        let mut lineage=std::collections::BTreeSet::from([id.clone()]);
        loop {
            let before=lineage.len();
            for peer in &mission.tasks {
                if lineage.contains(&peer.spec.repair_of) { lineage.insert(peer.spec.id.clone()); }
            }
            if lineage.len()==before { break; }
        }
        let repairs=mission.tasks.iter().filter(|peer|peer.spec.id!=*id && lineage.contains(&peer.spec.id))
            .map(|peer|json!({"spec":peer.spec,"run":peer.attempt_run_id,"review":peer.verification_run_id,
                "passed":peer.verification_passed,"evidence":peer.evidence_refs})).collect::<Vec<_>>();
        json!({"dependency":id,"spec":producer.map(|item|&item.spec),"run":producer.map(|item|&item.attempt_run_id),
            "review":producer.map(|item|&item.verification_run_id),"passed":producer.and_then(|item|item.verification_passed),
            "evidence":producer.map(|item|&item.evidence_refs),"repairs":repairs})
    }).collect::<Vec<_>>();
    let input = json!({"contract":"mission.attempt-input.v1","workspace":mission.workspace.root,
        "permission_mode":mission.workspace.permission_mode,"objective":mission.objective,
        "task":task.spec,"dependencies":dependencies});
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&input).expect("JSON input signature serialization"))
    )
}

/// Every link is host-persisted in the same transaction as its plan delta.
/// A matching latest Mission hash alone never revives a canceled/superseded run.
pub(crate) fn evidence_plan_compatible(
    mission: &MissionView,
    task: &MissionTaskView,
    evidence_plan: &str,
    events: &[MissionEvent],
) -> bool {
    if evidence_plan.is_empty()
        || task.attempt_run_id.is_empty()
        || task.status == TaskStatus::Canceled
    {
        return false;
    }
    if evidence_plan == mission.plan_sha256 {
        return true;
    }
    let signature = input_signature(mission, task);
    let mut plan = evidence_plan.to_string();
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(plan.clone()) {
        let Some(link) = events
            .iter()
            .filter(|event| {
                event.event_type == "mission_attempt_plan_retained"
                    && event.task_id == task.spec.id
                    && event.payload["from_plan_sha256"] == plan
            })
            .max_by_key(|event| event.sequence)
        else {
            return false;
        };
        if link.payload["implementation_run_id"] != task.attempt_run_id
            || link.payload["input_sha256"] != signature
            || link.payload["verification_run_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id != task.verification_run_id)
        {
            return false;
        }
        let Some(next) = link.payload["to_plan_sha256"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        plan = next.to_string();
        if plan == mission.plan_sha256 {
            return true;
        }
    }
    false
}
