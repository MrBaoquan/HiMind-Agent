//! Host-built, informational provenance. An envelope never creates path authority.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{error::Error, path::{Component, Path}};
use crate::{mission::{MissionTaskSpec, MissionView}, store::local_runs::LocalRunLedger};

pub(super) fn producer<'a>(mission: &'a MissionView, logical: &str, events: &'a [crate::mission::MissionEvent])
    -> Result<(&'a crate::mission::MissionTaskView, &'a crate::mission::MissionEvent), Box<dyn Error>> {
    use std::collections::HashSet;
    let task = |id: &str| mission.tasks.iter().find(|task| task.spec.id == id).ok_or("handoff lineage task missing");
    // Validate every relevant ancestor chain, including failed intermediate
    // repairs. Native events can point straight from several ancestors to leaf.
    let mut descendants = Vec::new();
    for candidate in &mission.tasks {
        let mut current = candidate;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.spec.id.as_str()) { return Err("handoff repair ancestry cycle".into()); }
            if current.spec.id == logical { descendants.push(candidate); break; }
            if current.spec.repair_of.is_empty() { break; }
            current = task(&current.spec.repair_of)?;
        }
    }
    let event = |id: &str, kind: &str| events.iter().filter(|e| e.task_id == id && e.event_type == kind).max_by_key(|e| e.sequence);
    let mut current = task(logical)?;
    let mut seen = HashSet::new();
    while let Some(repaired) = event(&current.spec.id, "task_repaired") {
        if !seen.insert(current.spec.id.clone()) { return Err("handoff repair event cycle".into()); }
        let next = task(repaired.payload["repair_task_id"].as_str().ok_or("handoff repair target unknown")?)?;
        let mut ancestor = next;
        let mut ancestry = HashSet::new();
        while ancestor.spec.id != current.spec.id {
            if !ancestry.insert(&ancestor.spec.id) || ancestor.spec.repair_of.is_empty() {
                return Err("handoff repair event conflicts with ancestry".into());
            }
            ancestor = task(&ancestor.spec.repair_of)?;
        }
        if repaired.payload["verification_run_id"].as_str() != Some(next.verification_run_id.as_str()) {
            return Err("handoff repair verification binding unknown".into());
        }
        current = next;
    }
    // Multiple accepted terminal branches cannot be chosen by recency/guessing.
    let terminal = descendants.iter().filter(|t| t.verification_passed == Some(true)
        && event(&t.spec.id,"task_repaired").is_none()).collect::<Vec<_>>();
    if terminal.len() != 1 || terminal[0].spec.id != current.spec.id {
        return Err("handoff accepted repair leaf missing or ambiguous".into());
    }
    let host = event(&current.spec.id,"host_verification_checked").ok_or("handoff acceptance unknown")?;
    let completed = event(&current.spec.id,"task_completed").ok_or("handoff completion unknown")?;
    if current.status != crate::mission::TaskStatus::Completed || current.attempt_run_id.is_empty()
        || current.verification_run_id.is_empty() || current.attempt_run_id == current.verification_run_id
        || host.payload["passed"] != true || host.payload["plan_sha256"] != mission.plan_sha256
        || host.payload["implementation_run_id"] != current.attempt_run_id
        || host.payload["verification_run_id"] != current.verification_run_id
        || completed.payload["passed"] != true
        || completed.payload["implementation_run_id"] != current.attempt_run_id
        || completed.payload["verification_run_id"] != current.verification_run_id
        || completed.sequence <= host.sequence
        || !host.payload["candidate_before"].is_object()
        || host.payload["candidate_before"]["candidate_id"].as_str().is_none_or(str::is_empty)
        || host.payload["checks"].as_array().is_none_or(|checks| checks.is_empty() || checks.iter().any(|c| c["exit_code"].as_i64() != Some(0))) {
        return Err("handoff acceptance/run/check/candidate binding invalid or unknown".into());
    }
    if current.spec.required_checks.iter().any(|command| !host.payload["checks"].as_array().unwrap().iter()
        .any(|check|check["command"].as_str()==Some(command.as_str()))) {
        return Err("handoff required check binding missing".into());
    }
    Ok((current,host))
}

pub(crate) fn build(mission: &MissionView, receiver: &MissionTaskSpec, ledger: &LocalRunLedger, store: &crate::mission::MissionStore)
    -> Result<Vec<Value>, Box<dyn Error>> {
    let (snapshot, lineage_events) = store.executor_handoff_context(&mission.mission_id)?;
    if snapshot.plan_sha256 != mission.plan_sha256 { return Err("handoff plan changed".into()); }
    let mission = &snapshot;
    let mut upstream_ids = receiver.depends_on.clone();
    if !receiver.repair_of.is_empty() && !upstream_ids.contains(&receiver.repair_of) {
        upstream_ids.push(receiver.repair_of.clone());
    }
    let mut envelopes = Vec::new();
    for id in upstream_ids {
        let logical = mission.tasks.iter().find(|task| task.spec.id == id).ok_or("handoff upstream task missing")?;
        let failure_context = receiver.repair_of == id;
        let (source, acceptance) = if failure_context { (logical,None) }
            else { let (task,host) = producer(mission,&id,&lineage_events)?; (task,Some(host)) };
        let run = ledger.get_run(&source.attempt_run_id)?.ok_or("handoff upstream LocalRun missing")?;
        if !failure_context && run.status != crate::agent_core_contracts::LocalRunStatus::Succeeded { return Err("handoff accepted producer run did not succeed".into()); }
        if let Some(host) = acceptance {
            let verification = ledger.get_run(&source.verification_run_id)?.ok_or("handoff verification LocalRun missing")?;
            if verification.status != crate::agent_core_contracts::LocalRunStatus::Succeeded || host.payload["verification_run_id"] != verification.run_id {
                return Err("handoff verification run did not succeed".into());
            }
        }
        let events = ledger.list_events(&run.run_id)?;
        let (_, baseline_event) = store.verification_event_context(&mission.mission_id,&source.spec.id,&run.run_id)?;
        let baseline = baseline_event.as_ref().and_then(|event|event.payload.get("protected_baseline")).filter(|v|!v.is_null());
        let output = events.iter().rev().find_map(|event|
            event.payload.get("output").or_else(|| event.payload.get("runtime_report")));
        let refs = output.and_then(|output| output.get("evidence")).and_then(Value::as_array);
        let mut artifacts = Vec::new();
        let root = Path::new(&mission.workspace.root).canonicalize()?;
        // Evidence strings are untrusted. Only intersected, confirmed, relative
        // business paths may be inspected. Generated absolute paths need their
        // separate task-bound resource contract and are not followed here.
        for reference in refs.into_iter().flatten().filter_map(Value::as_str).take(128) {
            let Some(path) = authorized_artifact(&root, reference, &source.spec.write_paths, &receiver.read_paths)? else { continue; };
            let approved = mission.confirmation.as_ref().is_some_and(|confirmation|
                covered(reference, &confirmation.approved_paths) && path.strip_prefix(&root).ok()
                    .is_some_and(|path|covered(&path.to_string_lossy(),&confirmation.approved_paths)));
            if !approved { continue; }
            let metadata = std::fs::metadata(&path)?;
            if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 { continue; }
            let bytes = std::fs::read(path)?;
            artifacts.push(json!({"path":reference,"sha256":format!("{:x}",Sha256::digest(&bytes)),"size_bytes":bytes.len()}));
        }
        artifacts.sort_by_key(|artifact| artifact["path"].as_str().unwrap_or_default().to_string());
        artifacts.dedup_by(|a,b| a["path"] == b["path"]);
        let manifest_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&artifacts)?));
        envelopes.push(json!({
            "schema_version":"executor_handoff.v1","authority":"provenance_only_no_path_grant",
            "upstream_task_id":id,"upstream_run_id":run.run_id,"upstream_status":run.status,
            "logical_dependency_task_id":id,"actual_producer_task_id":source.spec.id,
            "context_kind":if failure_context {"failed_parent_context_not_accepted_output"} else {"accepted_producer"},
            "logical_failed_history":{"attempt_run_id":logical.attempt_run_id,
                "attempt_status":ledger.get_run(&logical.attempt_run_id)?.map(|run|run.status),
                "verification_status":ledger.get_run(&logical.verification_run_id)?.map(|run|run.status),
                "verification_run_id":logical.verification_run_id,"verification_passed_projection":logical.verification_passed,
                "note":"original run fields retained; completed parent projection does not attest its failed run"},
            "acceptance":acceptance.map(|host|json!({"event_id":host.event_id,"sequence":host.sequence,
                "implementation_run_id":host.payload["implementation_run_id"],"verification_run_id":host.payload["verification_run_id"],
                "checks":host.payload["checks"],"candidate":host.payload["candidate_before"]})),
            "attempts":run.steps.iter().map(|step| json!({"step_id":step.step_id,"attempt":step.attempt})).collect::<Vec<_>>(),
            "repair_of":receiver.repair_of,"plan_sha256":mission.plan_sha256,
            "confirmation_plan_sha256":mission.confirmation.as_ref().map(|confirmation|&confirmation.plan_sha256),
            "source_baseline":"host protected baseline retained in upstream run; not inferred from current files",
            "source_baseline_sha256":baseline.map(serde_json::to_vec).transpose()?.map(|bytes|format!("{:x}",Sha256::digest(bytes))),
            "source_baseline_identity":baseline_event.as_ref().map(|event|json!({"event_id":event.event_id,
                "task_id":event.task_id,"implementation_run_id":source.attempt_run_id,
                "verification_run_id":source.verification_run_id})),
            "artifacts":artifacts,"artifact_manifest_sha256":manifest_hash,
            "hash_observation":"current bytes at receiver input generation; not historical output attestation",
            "source":{"requested":source.spec.execution,"actual_executor":run.runtime_provider,
                "actual_model":run.steps.iter().map(|step|&step.model).filter(|model|!model.is_empty()).collect::<Vec<_>>(),
                "service_source":output.and_then(|output|output.get("service_source")),
                "model_source":if source.spec.execution.executor=="personal.codex" {
                    if source.spec.execution.model.is_empty() {"personal_cli_config"} else {"explicit_cli"}
                } else if source.spec.execution.executor=="himind.builtin" {"host_pinned_himind_service"} else {"unreported"},
                "permission_mode":mission.workspace.permission_mode,
                "permission_source":"confirmed_mission_policy; not an OS filesystem sandbox",
                "read_paths":source.spec.read_paths,"write_paths":source.spec.write_paths},
            "receiver":{"execution":receiver.execution,"permission_mode":mission.workspace.permission_mode,
                "read_paths":receiver.read_paths,"write_paths":receiver.write_paths},
            "usage":run.usage,"usage_dedup_key":format!("{}:{}",mission.mission_id,run.run_id),
            "cost_status":if run.usage.as_ref().is_some_and(|usage|!usage.currency.is_empty()) {"reported"} else {"unknown"},
            "implementation_output_sha256":output.map(|output|serde_json::to_vec(output)).transpose()?.map(|bytes|format!("{:x}",Sha256::digest(bytes))),
            "verification_run_id":source.verification_run_id,
            "verification_authority":"host_only; implementation success is not independent acceptance"
        }));
    }
    Ok(envelopes)
}

fn covered(path: &str, scopes: &[String]) -> bool {
    scopes.iter().any(|scope| Path::new(path).starts_with(Path::new(scope)))
}

fn authorized_artifact(root: &Path, reference: &str, writes: &[String], reads: &[String])
    -> Result<Option<std::path::PathBuf>, Box<dyn Error>> {
    let path = Path::new(reference);
    if path.is_absolute() || path.components().any(|part| !matches!(part, Component::Normal(_)))
        || !covered(reference, writes) || !covered(reference, reads) { return Ok(None); }
    let joined = root.join(path);
    if !joined.is_file() { return Ok(None); }
    let resolved = joined.canonicalize()?;
    if !resolved.starts_with(root) { return Ok(None); }
    let relative=resolved.strip_prefix(root)?.to_string_lossy();
    if !covered(&relative,writes) || !covered(&relative,reads) {return Ok(None);}
    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_paths_require_both_scopes_and_never_follow_absolute_evidence() {
        let root = std::env::temp_dir().join(format!("handoff-scope-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("reports")).unwrap();
        std::fs::write(root.join("reports/result.json"), b"{}").unwrap();
        let root = root.canonicalize().unwrap();
        let scopes = vec!["reports".into()];
        assert!(authorized_artifact(&root,"reports/result.json",&scopes,&scopes).unwrap().is_some());
        for path in ["../reports/result.json", "reports/../reports/result.json", "C:/outside.json"] {
            assert!(authorized_artifact(&root,path,&scopes,&scopes).unwrap().is_none());
        }
        assert!(authorized_artifact(&root,"reports/result.json",&scopes,&["src".into()]).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
