//! Expert project candidate lifecycle and Dashboard submission.

use serde_json::json;
use std::error::Error;

use crate::expert::ExpertAuthoringDraft;
use crate::{Options, VERSION};

pub(crate) fn list() -> Result<Vec<ExpertAuthoringDraft>, Box<dyn Error>> {
    crate::expert::list_authoring_drafts()
}

pub(crate) fn read(id: &str, version: &str) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    crate::expert::read_authoring_draft(id, version)
}

pub(crate) fn test(id: &str, version: &str) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    crate::expert::test_authoring_draft(id, version)
}

pub(crate) fn confirm(id: &str, version: &str) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    crate::expert::confirm_authoring_draft(id, version)
}

pub(crate) fn submit(
    options: &Options,
    agent_id: &str,
    id: &str,
    version: &str,
) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    let mut draft = read(id, version)?;
    if draft.tested_at.is_none() { return Err("专家候选包尚未完成测试".into()); }
    if draft.confirmed_at.is_none() { return Err("专家候选版本尚未确认".into()); }
    crate::extension_projects::ensure_distribution_target(
        crate::extension_projects::ExtensionProjectKind::Expert,
        id,
        crate::extension_contracts::DistributionTarget::Workbench,
    )?;
    if agent_id.trim().is_empty() { return Err("HiMind 账号尚未授权".into()); }
    let access = crate::api::oauth::platform_access_token(options, crate::api::oauth::CREATIVE_SUBMIT_SCOPE)?;
    let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(180)).build()?;
    let source = crate::extension_projects::submission_source(crate::extension_projects::ExtensionProjectKind::Expert, id)?;
    let mut report = draft.test_report.clone().unwrap_or_else(|| json!({}));
    report["candidate_sha256"] = json!(draft.candidate_sha256);
    report["agent_version"] = json!(VERSION);
    report["tested_at"] = json!(draft.tested_at);
    let submitted = crate::api::distribution::submit_expert(&client, &options.api_base(), agent_id, &access.token, &draft.candidate_path, &report, &source)?;
    draft.dashboard_release_id = submitted.get("release_id").or_else(|| submitted.get("id")).and_then(|value| value.as_str()).map(str::to_string);
    draft.submitted_at = Some(crate::expert::now_stamp_public());
    draft.updated_at = crate::expert::now_stamp_public();
    crate::expert::persist_authoring_draft_public(&draft)?;
    Ok(draft)
}
