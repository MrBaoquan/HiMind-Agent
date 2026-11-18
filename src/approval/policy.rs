use super::types::RequestType;
use sha2::{Digest, Sha256};

pub(crate) const APPROVAL_POLICY_VERSION: &str = "approval-policy-v1";

pub(crate) fn approval_generation(contract_generation: Option<&str>) -> i64 {
    let Some(generation) = contract_generation
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return 1;
    };
    let digest = Sha256::digest(generation.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    // Keep the derived generation exact when it crosses JSON/TypeScript
    // boundaries. Values above 2^53-1 cannot be represented losslessly by a
    // JavaScript number.
    (u64::from_be_bytes(bytes) & ((1u64 << 53) - 1)).max(1) as i64
}

pub(crate) fn args_digest(input: &serde_json::Value) -> Result<String, serde_json::Error> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(input)?)))
}

/// Returns the stable approval class for capabilities whose execution can
/// remove data or sever a business relationship. Unknown capabilities are not
/// treated as destructive solely because their display metadata says write.
pub(crate) fn destructive_request_type(capability_id: &str) -> Option<RequestType> {
    let id = capability_id.trim().to_ascii_lowercase();
    match id.as_str() {
        "system.filesystem.delete" | "workspace.delete" => Some(RequestType::FilesystemDelete),
        "workbench.business.project.delete" | "project.delete" => {
            Some(RequestType::BusinessProjectDelete)
        }
        "workbench.business.exhibit.delete" | "exhibit.delete" => {
            Some(RequestType::BusinessExhibitDelete)
        }
        // Full replacement and detach operations can remove existing business
        // relationships even though they are exposed as network writes.
        "workbench.business.project.managers.replace"
        | "workbench.business.project.owners.replace"
        | "workbench.business.exhibit.crew.replace"
        | "workbench.business.exhibit.crew.remove"
        | "workbench.business.project.exhibit.detach" => Some(RequestType::DestructiveAction),
        "mcp.server.remove" | "mcp.registration.remove" | "mcp.registration.remove-all" => {
            Some(RequestType::DestructiveAction)
        }
        _ if id.ends_with(".delete") || id.ends_with(".delete-all") => {
            Some(RequestType::DestructiveAction)
        }
        _ => None,
    }
}

pub(crate) fn is_destructive_capability(capability_id: &str) -> bool {
    destructive_request_type(capability_id).is_some()
}

/// AI 连接域能力（`ai.client.*` / `ai.service.*`）只读写本机 AI 客户端配置与
/// 本机 AI 服务状态，不触碰 Dashboard 业务数据，也不依赖控制平面。它们由
/// Agent 本机自管：审批只在本机确认，不创建 Dashboard 审批记录。
pub(crate) fn is_local_ai_configuration_capability(capability_id: &str) -> bool {
    let id = capability_id.trim();
    id.starts_with("ai.client.") || id.starts_with("ai.service.") || id == "ai.service.list"
}

/// R3 is the minimum tier for deleting user or business data. The only R4
/// operation in this family is an unsafe target, which is rejected by the
/// filesystem handler before any approval request is shown.
pub(crate) fn effective_risk_level(capability_id: &str, declared: &str) -> &'static str {
    let capability_id = capability_id.trim().to_ascii_lowercase();
    if is_destructive_capability(&capability_id)
        || matches!(
            capability_id.as_str(),
            "distribution.release.publish"
                | "extension.review.decide"
                | "mission.handoff.create"
                | "mission.assignment.apply"
                | "mission.plan.confirm"
                | "mission.plan-change.apply"
                | "mission.follow-up.add"
                | "mission.follow_up.add"
                | "mission.task.cancel-pending"
                | "mission.task.cancel_pending"
                | "mission.budget.extend"
                | "mission.verification.retry"
                | "mission.control"
                | "mission.workflow.export"
                | "mission.workflow.save"
        )
    {
        "R3"
    } else if capability_id == "extension.plugin.build"
        && declared.trim().eq_ignore_ascii_case("process")
    {
        // Older extension-development-tools manifests used `process` for a
        // fixed Go test/build command. Keep that known capability at the
        // local R2 tier while unknown process vocabularies still fail closed.
        "R2"
    } else if capability_id.starts_with("wechat.miniprogram.")
        && declared.trim().eq_ignore_ascii_case("process")
    {
        // The WeChat plugin runs fixed package-manager scripts without a
        // shell. Upload, release and platform actions remain network/system
        // risks and require an explicit Workflow approval gate.
        "R2"
    } else {
        match declared.trim().to_ascii_uppercase().as_str() {
            "READ_ONLY" => "R1",
            "NETWORK_WRITE" | "LOCAL_WRITE" | "LOCAL_ACTION" => "R2",
            "ADMIN_ACTION" => "R3",
            "R1" => "R1",
            "R2" => "R2",
            "R3" => "R3",
            "R4" => "R4",
            // An approval-required capability with an unknown risk vocabulary
            // must not become eligible for ordinary wildcard delegation.
            _ => "R3",
        }
    }
}

pub(crate) fn risk_rank(value: &str) -> u8 {
    match value.trim().to_ascii_uppercase().as_str() {
        "R1" => 1,
        "R2" => 2,
        "R3" => 3,
        "R4" => 4,
        _ => 4,
    }
}

pub(crate) fn target_description(capability_id: &str, input: &serde_json::Value) -> String {
    if matches!(
        capability_id,
        "extension.plugin.candidate.activate"
            | "extension.workflow.candidate.activate"
            | "extension.activation.rollback"
    ) {
        return serde_json::json!({
            "id":input.get("id"),"version":input.get("version"),"kind":input.get("kind"),
            "candidate_sha256":input.get("candidate_sha256"),"workspace_root":input.get("workspace_root"),
            "runtime_scope":"current Agent Profile; workspace identifies the candidate origin"
        }).to_string();
    }
    if capability_id == "mission.plan.confirm" {
        return mission_plan_confirmation_target(input)
            .unwrap_or_else(|_| "计划确认信息无效或超出显示限制".to_string());
    }
    if capability_id == "mission.plan-change.apply" {
        return mission_plan_change_apply_target(input)
            .unwrap_or_else(|_| "计划变更授权信息无效或超出显示限制".to_string());
    }
    if matches!(
        capability_id,
        "mission.follow-up.add" | "mission.follow_up.add"
    ) {
        return mission_follow_up_add_target(input)
            .unwrap_or_else(|_| "跟进请求无效或超出审批显示限制".to_string());
    }
    if matches!(
        capability_id,
        "mission.task.cancel-pending" | "mission.task.cancel_pending"
    ) {
        return mission_pending_repair_cancellation_target(input)
            .unwrap_or_else(|_| "撤销请求无效或超出审批显示限制".to_string());
    }
    if capability_id == "mission.budget.extend" {
        return mission_budget_extension_target(input)
            .unwrap_or_else(|_| "预算调整信息无效或超出显示限制".to_string());
    }
    let key = if capability_id.contains("project") {
        "project_id"
    } else if capability_id.contains("exhibit") {
        "exhibit_id"
    } else if input
        .get("target")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        "target"
    } else if input
        .get("id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        "id"
    } else {
        "path"
    };
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("未提供目标")
        .chars()
        .take(500)
        .collect()
}

pub(crate) fn mission_pending_repair_cancellation_target(
    input: &serde_json::Value,
) -> Result<String, String> {
    crate::mission::MissionStore::validate_pending_repair_cancellation(input)
        .map_err(|error| error.to_string())?;
    let target = serde_json::to_string_pretty(input).map_err(|error| error.to_string())?;
    if target.len() > super::manager::MAX_APPROVAL_TEXT_BYTES - 2 * 1024 {
        return Err("pending repair cancellation exceeds the approval display limit".to_string());
    }
    Ok(target)
}

pub(crate) fn mission_plan_confirmation_target(
    input: &serde_json::Value,
) -> Result<String, String> {
    let field = |name: &str| {
        input
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing {name}"))
    };
    let details = serde_json::json!({
        "mission_id": field("mission_id")?,
        "plan_sha256": field("plan_sha256")?,
        "selected_task_ids": field("selected_task_ids")?,
        "approved_paths": field("approved_paths")?,
        "approved_absolute_read_paths": field("approved_absolute_read_paths")?,
        "approved_absolute_write_paths": field("approved_absolute_write_paths")?,
    });
    let text = serde_json::to_string_pretty(&details).map_err(|error| error.to_string())?;
    if text.len() > 4_000 {
        return Err("plan confirmation scope exceeds the approval display limit".to_string());
    }
    Ok(text)
}

pub(crate) fn mission_plan_change_apply_target(
    input: &serde_json::Value,
) -> Result<String, String> {
    let field = |name: &str| {
        input
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing {name}"))
    };
    let details = serde_json::json!({
        "mission_id": field("mission_id")?,
        "request_id": field("request_id")?,
        "expected_plan_revision": field("expected_plan_revision")?,
        "expected_plan_sha256": field("expected_plan_sha256")?,
        "additional_read_paths": field("read_paths")?,
        "additional_write_paths": field("write_paths")?,
    });
    let text = serde_json::to_string_pretty(&details).map_err(|error| error.to_string())?;
    if text.len() > 4_000 {
        return Err("plan change approval details exceed the display limit".to_string());
    }
    Ok(text)
}

// The manager accepts 16 KiB for the complete approval description. Reserve
// 2 KiB for its trusted capability/risk/source wrapper; do not change that gate.
const FOLLOW_UP_REVIEW_TARGET_BYTES: usize = super::manager::MAX_APPROVAL_TEXT_BYTES - 2 * 1024;
const FOLLOW_UP_INLINE_REVIEW_BYTES: usize = FOLLOW_UP_REVIEW_TARGET_BYTES - 2 * 1024;

pub(crate) fn mission_follow_up_add_target(input: &serde_json::Value) -> Result<String, String> {
    mission_follow_up_target_with_root(
        input,
        &crate::store::paths::agent_home().join("mission-follow-up-reviews"),
    )
}

fn mission_follow_up_target_with_root(
    input: &serde_json::Value,
    review_root: &std::path::Path,
) -> Result<String, String> {
    let request: crate::mission::follow_up::MissionFollowUpAdd =
        serde_json::from_value(input.clone()).map_err(|error| error.to_string())?;
    request.validate().map_err(|error| error.to_string())?;
    // Match the unchanged Grant args_digest exactly, including every objective,
    // path, check, criterion, dependency, reason and complete read extension
    // (paths, authorized_by, reason). No caller-supplied reference or digest.
    let payload = serde_json::to_vec(input).map_err(|error| error.to_string())?;
    let digest = args_digest(input).map_err(|error| error.to_string())?;
    let binding = "store inherits the complete repair_of execution binding; no caller override";
    let scope_policy = if request.read_scope_extension.is_some() {
        "Explicit user read-only extension, bound to this R3 request: paths must exactly equal new reads outside parent scope; waiting with no running/awaiting_verification work; no write, execution, original-check, workspace or confirmation expansion"
    } else {
        "store rejects wider parent read/write paths and missing original required_checks"
    };
    let inline = serde_json::to_string(&serde_json::json!({
        "review_mode":"inline_full_request", "payload_sha256":digest,
        "binding":binding, "scope_policy":scope_policy, "request":input,
    }))
    .map_err(|error| error.to_string())?;
    if inline.len() <= FOLLOW_UP_INLINE_REVIEW_BYTES {
        return Ok(inline);
    }

    // A review receipt is application state, not a Mission task or a path grant.
    // The path is host-chosen and content-addressed, and its bytes must match the
    // exact input. Replays reuse it; a changed existing file fails closed.
    let review_path = review_root.join(format!("{digest}.json"));
    let tasks = request.tasks.iter().enumerate().map(|(index, task)| {
        let mut summary = serde_json::json!({
            "id":task.id,"title":task.title,"repair_of":task.repair_of,
            "task_sha256":args_digest(&input["tasks"][index]).expect("validated task serialization"),
            "review_task_index":index, "field_counts":{},
            "objective":{"characters":task.objective.chars().count(),
                "sha256":format!("{:x}",Sha256::digest(task.objective.as_bytes()))},
        });
        for (name, values) in [("depends_on", &task.depends_on), ("read_paths", &task.read_paths),
            ("write_paths", &task.write_paths), ("acceptance_criteria", &task.acceptance_criteria),
            ("required_checks", &task.required_checks)] {
            summary["field_counts"][name] = serde_json::json!(values.len());
        }
        summary
    }).collect::<Vec<_>>();
    // Keep the large manifest bounded even with 128 long selectors/reasons.
    // Its explicit read-only summary points to the entire digest-bound grant.
    let extension_review = request.read_scope_extension.as_ref().map(|extension| serde_json::json!({
        "access":"read_only", "explicit_user_authorization_required":true,
        "paths_count":extension.paths.len(), "authorized_by":extension.authorized_by,
        "reason_characters":extension.reason.chars().count(),
        "sha256":args_digest(&input["read_scope_extension"]).expect("validated extension serialization"),
        "review_json_pointer":"/read_scope_extension",
        "full_fields_in_review":["paths","authorized_by","reason"],
    }));
    let target = serde_json::to_string(&serde_json::json!({
        "review_mode":"full_review_required", "mission_id":request.mission_id,
        "expected_plan_sha256":request.expected_plan_sha256, "payload_sha256":digest,
        "binding":binding, "full_fields_in_review":true,
        "task_reference_format":"review JSON /tasks/{review_task_index}; full objective, paths, checks and criteria",
        "scope_policy":scope_policy, "read_scope_extension":extension_review,
        "review":{"path":review_path.to_string_lossy(),"sha256":digest,
            "format":"complete compact UTF-8 JSON, exact MCP arguments", "required":true},
        "review_instruction":"Read complete review JSON: every objective, path, check, dependency, criterion and reason; for read_scope_extension verify all paths, authorized_by and reason against existing explicit user read-only permission. Verify payload_sha256 against source MCP arguments before R3 approval. Trusted client source is in the host wrapper.",
        "tasks":tasks,
    })).map_err(|error| error.to_string())?;
    if target.len() > FOLLOW_UP_REVIEW_TARGET_BYTES {
        return Err("follow-up review manifest exceeds the approval display limit".to_string());
    }
    std::fs::create_dir_all(review_root).map_err(|error| error.to_string())?;
    reject_review_link(review_root)?;
    reject_review_link(&review_path)?;
    reject_review_link(&crate::store::atomic_file::lock_path(&review_path))?;
    let _lock = crate::store::atomic_file::lock(&review_path).map_err(|error| error.to_string())?;
    if review_path.exists() {
        if std::fs::read(&review_path).map_err(|error| error.to_string())? != payload {
            return Err(
                "follow-up review receipt content does not match the exact request".to_string(),
            );
        }
    } else {
        crate::store::atomic_file::atomic_write(&review_path, &payload)
            .map_err(|error| error.to_string())?;
    }
    Ok(target)
}

fn reject_review_link(path: &std::path::Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if linked {
        return Err("follow-up review storage must not use links or reparse points".to_string());
    }
    Ok(())
}

/// Read only a hash-addressed native follow-up receipt. Callers cannot pass a
/// path; its exact bytes and typed contract must both match the approval hash.
pub(crate) fn read_mission_follow_up_review(digest: &str) -> Result<serde_json::Value, String> {
    read_follow_up_review_at(
        digest,
        &crate::store::paths::agent_home().join("mission-follow-up-reviews"),
    )
}

fn read_follow_up_review_at(
    digest: &str,
    root: &std::path::Path,
) -> Result<serde_json::Value, String> {
    use std::io::Read;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("任务说明摘要无效".to_string());
    }
    let path = root.join(format!("{digest}.json"));
    reject_review_link(root)?;
    reject_review_link(&path)?;
    let file = std::fs::File::open(path).map_err(|_| "完整任务说明暂不可读，请重试".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "无法核对任务说明".to_string())?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err("任务说明格式或长度无效".to_string());
    }
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "完整任务说明读取失败".to_string())?;
    if bytes.len() > 4 * 1024 * 1024 || format!("{:x}", Sha256::digest(&bytes)) != digest {
        return Err("任务说明与审批摘要不一致".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "任务说明格式无效".to_string())?;
    let request: crate::mission::follow_up::MissionFollowUpAdd =
        serde_json::from_value(value.clone()).map_err(|_| "任务说明契约无效".to_string())?;
    request
        .validate()
        .map_err(|_| "任务说明契约无效".to_string())?;
    Ok(serde_json::json!({"payload_sha256":digest,"request":value}))
}

pub(crate) fn mission_budget_extension_target(input: &serde_json::Value) -> Result<String, String> {
    let mission_id = input
        .get("mission_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing mission_id".to_string())?;
    let request = input
        .get("input")
        .cloned()
        .ok_or_else(|| "missing input".to_string())?;
    let details = serde_json::json!({ "mission_id": mission_id, "budget_increase": request });
    let text = serde_json::to_string_pretty(&details).map_err(|error| error.to_string())?;
    if text.len() > 4_000 {
        return Err("budget extension details exceed the approval display limit".to_string());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_and_filesystem_deletes_are_r3() {
        assert_eq!(
            effective_risk_level("workbench.business.project.delete", "R1"),
            "R3"
        );
        assert_eq!(
            effective_risk_level("system.filesystem.delete", "read_only"),
            "R3"
        );
    }

    fn follow_up_review_input() -> serde_json::Value {
        serde_json::json!({"schema_version":"mission.follow_up.add.v1","mission_id":"mission_test",
            "expected_plan_sha256":"a".repeat(64),"reason":"Explicit user correction",
            "tasks":[{"id":"repair-6","title":"Correct delivery","objective":"Consume authorized runtime resources",
                "repair_of":"repair-5","depends_on":[],"read_paths":["src"],"write_paths":["src"],
                "acceptance_criteria":["Real delivery passes"],"required_checks":["cargo test"]}]})
    }

    fn follow_up_review_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("himind-follow-up-review-{}", rand::random::<u64>()))
    }

    #[test]
    fn mission_follow_up_review_supports_real_ten_kib_payload_inline_without_truncation() {
        let mut input = follow_up_review_input();
        input["tasks"][0]["objective"] = serde_json::json!("x".repeat(5_000));
        let paths = (0..16)
            .map(|i| format!("src/{i}/{}", "p".repeat(150)))
            .collect::<Vec<_>>();
        input["tasks"][0]["read_paths"] = serde_json::json!(paths);
        input["tasks"][0]["write_paths"] = serde_json::json!(paths);
        input["tasks"][0]["required_checks"] = serde_json::json!(vec!["cargo test"; 32]);
        let payload = serde_json::to_vec(&input).unwrap();
        assert!(payload.len() > 10 * 1024);
        let root = follow_up_review_root();
        let target = mission_follow_up_target_with_root(&input, &root).unwrap();
        let review: serde_json::Value = serde_json::from_str(&target).unwrap();
        assert_eq!(review["review_mode"], "inline_full_request");
        assert_eq!(review["request"], input);
        assert_eq!(review["payload_sha256"], args_digest(&input).unwrap());
        assert!(!root.exists());
        let description = format!("能力：mission.follow-up.add\n风险等级：R3\n目标：{target}\n来源：dashboard_worker\n\n拒绝、超时或 Agent 中断都会阻止实际执行。");
        assert!(description.len() <= super::super::manager::MAX_APPROVAL_TEXT_BYTES);
    }

    #[test]
    fn mission_follow_up_review_keeps_full_large_objective_paths_checks_and_criteria() {
        let mut input = follow_up_review_input();
        input["tasks"][0]["objective"] = serde_json::json!("界".repeat(8_000));
        input["tasks"][0]["acceptance_criteria"] = serde_json::json!(vec!["界".repeat(2_000); 32]);
        input["tasks"][0]["required_checks"] = serde_json::json!(vec!["cargo test"; 32]);
        let root = follow_up_review_root();
        let target = mission_follow_up_target_with_root(&input, &root).unwrap();
        let review: serde_json::Value = serde_json::from_str(&target).unwrap();
        assert_eq!(review["review_mode"], "full_review_required");
        assert_eq!(review["review"]["required"], true);
        assert_eq!(review["payload_sha256"], args_digest(&input).unwrap());
        let file = std::path::PathBuf::from(review["review"]["path"].as_str().unwrap());
        assert_eq!(file.parent(), Some(root.as_path()));
        assert_eq!(
            file.file_name().unwrap().to_str().unwrap(),
            format!("{}.json", args_digest(&input).unwrap())
        );
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            args_digest(&input).unwrap()
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            input
        );
        let full = read_follow_up_review_at(&args_digest(&input).unwrap(), &root).unwrap();
        assert_eq!(full["request"], input);
        assert_eq!(full["payload_sha256"], args_digest(&input).unwrap());
        for invalid in ["../secret".to_string(), "A".repeat(64), "z".repeat(64)] {
            assert!(read_follow_up_review_at(&invalid, &root).is_err());
        }
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(
            mission_follow_up_target_with_root(&input, &root).unwrap(),
            target
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            modified
        );
        let mut changed = input.clone();
        changed["tasks"][0]["objective"] = serde_json::json!("异".repeat(8_000));
        let changed_target = mission_follow_up_target_with_root(&changed, &root).unwrap();
        assert_ne!(changed_target, target);
        // Only this unit test's isolated review directory is modified.
        std::fs::write(&file, b"tampered").unwrap();
        assert!(read_follow_up_review_at(&args_digest(&input).unwrap(), &root).is_err());
        assert!(mission_follow_up_target_with_root(&input, &root)
            .unwrap_err()
            .contains("does not match"));
        assert_eq!(std::fs::read(&file).unwrap(), b"tampered");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_review_manifest_fits_actual_approval_limit_at_maximum_task_count() {
        let mut input = follow_up_review_input();
        let template = input["tasks"][0].clone();
        input["tasks"] = serde_json::json!((0..8)
            .map(|i| {
                let mut t = template.clone();
                t["id"] = serde_json::json!(format!("repair-{i}-{}", "x".repeat(150)));
                t["repair_of"] = serde_json::json!(format!("parent-{i}-{}", "x".repeat(150)));
                t["title"] = serde_json::json!("界".repeat(240));
                t["objective"] = serde_json::json!("界".repeat(8_000));
                t
            })
            .collect::<Vec<_>>());
        let root = follow_up_review_root();
        let target = mission_follow_up_target_with_root(&input, &root).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&target).unwrap();
        assert_eq!(manifest["tasks"].as_array().unwrap().len(), 8);
        for (i, t) in input["tasks"].as_array().unwrap().iter().enumerate() {
            assert_eq!(manifest["tasks"][i]["id"], t["id"]);
            assert_eq!(manifest["tasks"][i]["title"], t["title"]);
            assert_eq!(manifest["tasks"][i]["repair_of"], t["repair_of"]);
            assert_eq!(manifest["tasks"][i]["task_sha256"], args_digest(t).unwrap());
            assert_eq!(manifest["tasks"][i]["field_counts"]["required_checks"], 1);
            assert_eq!(manifest["full_fields_in_review"], true);
        }
        let description = format!("能力：mission.follow-up.add\n风险等级：R3\n目标：{target}\n来源：dashboard_worker\n\n拒绝、超时或 Agent 中断都会阻止实际执行。");
        assert!(description.len() <= super::super::manager::MAX_APPROVAL_TEXT_BYTES);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_read_extension_approval_binds_every_field_inline_and_in_full_review() {
        for large in [false, true] {
            let mut input = follow_up_review_input();
            input["tasks"][0]["read_paths"] = serde_json::json!(["src", "contracts"]);
            input["read_scope_extension"] = serde_json::json!({"paths":["contracts"],
                "authorized_by":"user:existing-explicit-permission", "reason":"Independent review compilation inputs"});
            if large {
                input["tasks"][0]["objective"] = serde_json::json!("界".repeat(8_000));
            }
            let root = follow_up_review_root();
            let target = mission_follow_up_target_with_root(&input, &root).unwrap();
            let review: serde_json::Value = serde_json::from_str(&target).unwrap();
            assert_eq!(review["payload_sha256"], args_digest(&input).unwrap());
            assert!(review["scope_policy"]
                .as_str()
                .unwrap()
                .contains("read-only"));
            if large {
                assert_eq!(review["read_scope_extension"]["access"], "read_only");
                assert_eq!(
                    review["read_scope_extension"]["authorized_by"],
                    input["read_scope_extension"]["authorized_by"]
                );
                assert_eq!(
                    review["read_scope_extension"]["sha256"],
                    args_digest(&input["read_scope_extension"]).unwrap()
                );
                assert_eq!(
                    read_follow_up_review_at(&args_digest(&input).unwrap(), &root).unwrap()
                        ["request"],
                    input
                );
            } else {
                assert_eq!(review["request"], input);
                assert!(!root.exists());
            }
            for field in ["paths", "authorized_by", "reason"] {
                let mut changed = input.clone();
                if field == "paths" {
                    changed["read_scope_extension"][field] = serde_json::json!(["workflows"]);
                } else {
                    changed["read_scope_extension"][field] =
                        serde_json::json!("different explicit authorization");
                }
                assert_ne!(
                    args_digest(&changed).unwrap(),
                    args_digest(&input).unwrap(),
                    "{field}"
                );
                assert_ne!(
                    mission_follow_up_target_with_root(&changed, &root).unwrap(),
                    target,
                    "{field}"
                );
            }
            if large {
                let path = root.join(format!("{}.json", args_digest(&input).unwrap()));
                let mut tampered = input.clone();
                tampered["read_scope_extension"]["authorized_by"] = serde_json::json!("spoofed");
                std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
                assert!(read_follow_up_review_at(&args_digest(&input).unwrap(), &root).is_err());
                assert!(mission_follow_up_target_with_root(&input, &root).is_err());
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn mission_follow_up_read_extension_large_manifest_fits_with_maximum_batch_and_authorization() {
        let mut input = follow_up_review_input();
        let paths = (0..128)
            .map(|i| format!("inputs/{i}/{}", "界".repeat(990)))
            .collect::<Vec<_>>();
        input["read_scope_extension"] = serde_json::json!({"paths":paths,
            "authorized_by":"界".repeat(160), "reason":"界".repeat(2000)});
        let template = input["tasks"][0].clone();
        input["tasks"] = serde_json::json!((0..8)
            .map(|i| {
                let mut task = template.clone();
                task["id"] = serde_json::json!(format!("repair-{i}-{}", "x".repeat(150)));
                task["repair_of"] = serde_json::json!(format!("parent-{i}-{}", "x".repeat(150)));
                task["title"] = serde_json::json!("界".repeat(240));
                task["objective"] = serde_json::json!("界".repeat(8000));
                if i == 0 {
                    task["read_paths"] = serde_json::json!(paths);
                }
                task
            })
            .collect::<Vec<_>>());
        let root = follow_up_review_root();
        let target = mission_follow_up_target_with_root(&input, &root).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&target).unwrap();
        assert_eq!(manifest["read_scope_extension"]["access"], "read_only");
        assert_eq!(manifest["read_scope_extension"]["paths_count"], 128);
        assert_eq!(
            manifest["read_scope_extension"]["review_json_pointer"],
            "/read_scope_extension"
        );
        assert_eq!(manifest["read_scope_extension"]["reason_characters"], 2000);
        assert_eq!(
            manifest["read_scope_extension"]["sha256"],
            args_digest(&input["read_scope_extension"]).unwrap()
        );
        assert_eq!(
            read_follow_up_review_at(&args_digest(&input).unwrap(), &root).unwrap()["request"],
            input
        );
        let description = format!("能力：mission.follow-up.add\n风险等级：R3\n目标：{target}\n来源：dashboard_worker\n\n拒绝、超时或 Agent 中断都会阻止实际执行。");
        assert!(target.len() <= FOLLOW_UP_REVIEW_TARGET_BYTES);
        assert!(description.len() <= super::super::manager::MAX_APPROVAL_TEXT_BYTES);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mission_follow_up_policy_requires_r3_and_displays_the_exact_request() {
        for declared in ["R1", "read_only", "local_write"] {
            assert_eq!(
                effective_risk_level("mission.follow-up.add", declared),
                "R3"
            );
        }
        assert!(!is_local_ai_configuration_capability(
            "mission.follow-up.add"
        ));
        assert!(destructive_request_type("mission.follow-up.add").is_none());
        let input = serde_json::json!({"schema_version":"mission.follow_up.add.v1","mission_id":"mission_test",
            "expected_plan_sha256":"a".repeat(64),"reason":"Explicit user correction",
            "tasks":[{"id":"repair-6","title":"Correct delivery","objective":"Consume authorized runtime resources",
                "repair_of":"repair-5","depends_on":[],"read_paths":["src"],"write_paths":["src"],
                "acceptance_criteria":["Real delivery passes"],"required_checks":["cargo test"]}]});
        let target = mission_follow_up_add_target(&input).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&target).unwrap()["request"],
            input
        );
        assert_eq!(target_description("mission.follow-up.add", &input), target);
        let mut changed = input.clone();
        changed["tasks"][0]["objective"] = serde_json::json!("A precise different objective");
        assert_ne!(args_digest(&input).unwrap(), args_digest(&changed).unwrap());
        assert!(target.len() < FOLLOW_UP_INLINE_REVIEW_BYTES);
    }

    #[test]
    fn mission_budget_extension_requires_r3_and_shows_the_requested_limits() {
        assert_eq!(effective_risk_level("mission.budget.extend", "R1"), "R3");
        let target = mission_budget_extension_target(&serde_json::json!({
            "mission_id": "mission_test",
            "input": {"additional_tokens": 1000, "unlimited_tokens": false}
        }))
        .unwrap();
        assert!(target.contains("mission_test"));
        assert!(target.contains("1000"));
    }

    #[test]
    fn pending_repair_cancellation_requires_r3_and_exact_validated_target() {
        for declared in ["R1", "read_only", "local_write"] {
            assert_eq!(
                effective_risk_level("mission.task.cancel-pending", declared),
                "R3"
            );
        }
        let input = serde_json::json!({"schema_version":"mission.task.cancel_pending.v1",
            "mission_id":"mission_test", "expected_plan_sha256":"a".repeat(64),
            "task_id":"obsolete-repair", "request_id":"human-scope-1", "reason":"用户调整当前验收范围"});
        let target = mission_pending_repair_cancellation_target(&input).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&target).unwrap(),
            input
        );
        assert_eq!(
            target_description("mission.task.cancel-pending", &input),
            target
        );
        let mut invalid = input.clone();
        invalid["execution"] = serde_json::json!("override");
        assert!(mission_pending_repair_cancellation_target(&invalid).is_err());
        let mut changed = input.clone();
        changed["task_id"] = serde_json::json!("different-repair");
        assert_ne!(args_digest(&input).unwrap(), args_digest(&changed).unwrap());
        assert_ne!(
            target,
            mission_pending_repair_cancellation_target(&changed).unwrap()
        );
    }

    #[test]
    fn ordinary_writes_are_not_promoted_to_destructive() {
        assert!(!is_destructive_capability(
            "workbench.business.project.update"
        ));
        assert_eq!(
            effective_risk_level("workbench.business.project.update", "network_write"),
            "R2"
        );
    }

    #[test]
    fn relationship_replacements_and_detach_are_r3() {
        for capability_id in [
            "workbench.business.project.managers.replace",
            "workbench.business.project.owners.replace",
            "workbench.business.exhibit.crew.replace",
            "workbench.business.exhibit.crew.remove",
            "workbench.business.project.exhibit.detach",
        ] {
            assert_eq!(effective_risk_level(capability_id, "network_write"), "R3");
            assert!(is_destructive_capability(capability_id));
        }
    }

    #[test]
    fn publish_and_admin_decisions_are_r3() {
        assert_eq!(
            effective_risk_level("distribution.release.publish", "network_write"),
            "R3"
        );
        assert_eq!(
            effective_risk_level("extension.review.decide", "admin_action"),
            "R3"
        );
        assert_eq!(
            effective_risk_level("mission.plan.confirm", "local_write"),
            "R3"
        );
    }

    #[test]
    fn unknown_risk_vocabulary_fails_closed_at_r3() {
        assert_eq!(effective_risk_level("third.party.write", "mystery"), "R3");
    }

    #[test]
    fn fixed_extension_build_process_alias_is_local_r2() {
        assert_eq!(
            effective_risk_level("extension.plugin.build", "process"),
            "R2"
        );
        assert_eq!(
            effective_risk_level("wechat.miniprogram.test", "process"),
            "R2"
        );
        assert_eq!(
            effective_risk_level("wechat.miniprogram.preview", "network"),
            "R3"
        );
        assert_eq!(effective_risk_level("third.party.process", "process"), "R3");
    }

    #[test]
    fn approval_generation_is_stable_and_changes_with_contract() {
        assert_eq!(approval_generation(None), 1);
        assert_eq!(
            approval_generation(Some("generation-a")),
            approval_generation(Some("generation-a"))
        );
        assert_ne!(
            approval_generation(Some("generation-a")),
            approval_generation(Some("generation-b"))
        );
        assert!(approval_generation(Some("generation-a")) <= (1i64 << 53) - 1);
    }

    #[test]
    fn legacy_catalog_risk_aliases_map_to_unified_tiers() {
        assert_eq!(
            effective_risk_level("workbench.context.resolve", "read_only"),
            "R1"
        );
        assert_eq!(
            effective_risk_level("project.update", "network_write"),
            "R2"
        );
    }

    #[test]
    fn target_description_prefers_explicit_target_or_id() {
        assert_eq!(
            target_description("ai.client.import", &serde_json::json!({"target":"codex"})),
            "codex"
        );
        assert_eq!(
            target_description(
                "extension.review.decide",
                &serde_json::json!({"id":"skill-1"})
            ),
            "skill-1"
        );
    }

    #[test]
    fn plan_confirmation_target_shows_hash_tasks_and_exact_path_grants() {
        let input = serde_json::json!({
            "mission_id":"mission-test",
            "plan_sha256":"a".repeat(64),
            "selected_task_ids":["fix", "review"],
            "approved_paths":["src/attention.mjs", "REVIEW.md"],
            "approved_absolute_read_paths":[],
            "approved_absolute_write_paths":[]
        });
        let target = target_description("mission.plan.confirm", &input);
        for value in [
            "mission-test",
            "plan_sha256",
            "selected_task_ids",
            "src/attention.mjs",
            "REVIEW.md",
            "approved_absolute_write_paths",
        ] {
            assert!(
                target.contains(value),
                "approval summary omitted {value}: {target}"
            );
        }
    }

    #[test]
    fn plan_confirmation_target_fails_closed_when_full_scope_cannot_be_displayed() {
        let input = serde_json::json!({
            "mission_id":"mission-test",
            "plan_sha256":"a".repeat(64),
            "selected_task_ids":["fix"],
            "approved_paths":["x".repeat(4_100)],
            "approved_absolute_read_paths":[],
            "approved_absolute_write_paths":[]
        });
        assert!(mission_plan_confirmation_target(&input).is_err());
    }

    #[test]
    fn ai_connection_domain_is_local_owned_configuration() {
        // AI 连接域只读写本机客户端配置与本机服务状态，不触碰 Dashboard 业务
        // 数据：识别为本机配置能力、不视为破坏性、保持声明风险等级（R2）。
        for capability_id in [
            "ai.client.list",
            "ai.client.status",
            "ai.client.import",
            "ai.client.remove",
            "ai.client.import.plan",
            "ai.client.remove.plan",
            "ai.service.list",
            "ai.service.custom.upsert",
            "ai.service.custom.remove",
            "ai.service.custom.model.list",
        ] {
            assert!(
                is_local_ai_configuration_capability(capability_id),
                "AI 连接域能力必须识别为本机配置: {capability_id}"
            );
            assert!(
                !is_destructive_capability(capability_id),
                "AI 连接域不得视为破坏性: {capability_id}"
            );
            assert_eq!(
                effective_risk_level(capability_id, "local_write"),
                "R2",
                "AI 连接域应保持声明风险等级: {capability_id}"
            );
        }
        for unrelated in [
            "workbench.business.project.delete",
            "system.filesystem.delete",
            "workbench.context.resolve",
        ] {
            assert!(!is_local_ai_configuration_capability(unrelated));
        }
    }
}
