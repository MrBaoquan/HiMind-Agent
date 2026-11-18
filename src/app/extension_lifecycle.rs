//! Local candidate activation API. Never confirms, publishes or submits candidates.
use super::extension_activation::{ActivationKind, ActivationStatus, ActivationStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

fn validate_id(id: &str) -> Result<(), Box<dyn Error>> {
    if id.is_empty()
        || id.len() > 200
        || matches!(id, "." | "..")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("invalid extension candidate id".into());
    }
    Ok(())
}

pub(crate) const CONTRACT_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationInput {
    pub id: String,
    pub version: String,
    pub candidate_sha256: String,
    pub workspace_root: PathBuf,
}

impl ActivationInput {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        validate_id(&self.id)?;
        crate::extension_contracts::validate_sha256("candidate_sha256", &self.candidate_sha256)?;
        semver::Version::parse(&self.version)?;
        if !self.workspace_root.is_absolute() || !self.workspace_root.is_dir() {
            return Err("activation workspace_root must be an existing absolute directory".into());
        }
        Ok(())
    }

    fn check(
        &self,
        id: &str,
        version: &str,
        hash: &str,
        workspace: &Path,
        candidate: &Path,
        tested: bool,
        confirmed: bool,
    ) -> Result<(), Box<dyn Error>> {
        self.validate()?;
        if id != self.id
            || version != self.version
            || !hash.eq_ignore_ascii_case(&self.candidate_sha256)
        {
            return Err("activation candidate identity changed; inspect and test the intended candidate again".into());
        }
        if !workspace.is_dir() || workspace.canonicalize()? != self.workspace_root.canonicalize()? {
            return Err("activation candidate belongs to a different workspace".into());
        }
        if !tested || !confirmed {
            return Err("activation requires a tested and confirmed candidate".into());
        }
        let metadata = std::fs::metadata(candidate)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 512 * 1024 * 1024 {
            return Err("activation candidate archive is empty or exceeds 512 MiB".into());
        }
        let mut file = File::open(candidate)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&self.candidate_sha256) {
            return Err("activation candidate archive changed; save and test it again".into());
        }
        Ok(())
    }
}

/// Serializes API activation and rollback for this profile/extension. Does not lock source code.
pub(crate) fn operation_lock(
    kind: ActivationKind,
    id: &str,
) -> Result<crate::store::atomic_file::AtomicFileLock, Box<dyn Error>> {
    validate_id(id)?;
    let path = crate::store::paths::agent_home()
        .join("data/extension-lifecycle-locks")
        .join(format!("{}-{id}", kind.as_str()));
    Ok(crate::store::atomic_file::lock(&path)?)
}

pub(crate) fn activate_plugin(input: ActivationInput) -> Result<Value, Box<dyn Error>> {
    input.validate()?;
    let draft = crate::plugin_authoring::read(&input.id, &input.version)?;
    let workspace = draft
        .workspace_path
        .as_deref()
        .ok_or("plugin candidate has no workspace identity")?;
    input.check(
        &draft.manifest.id,
        &draft.manifest.version,
        &draft.candidate_sha256,
        workspace,
        &draft.candidate_path,
        draft.tested_at.is_some(),
        draft.confirmed_at.is_some(),
    )?;
    Ok(serde_json::to_value(
        crate::plugin_authoring::activate_pinned_draft(draft)?,
    )?)
}

pub(crate) fn activate_workflow(input: ActivationInput) -> Result<Value, Box<dyn Error>> {
    input.validate()?;
    let draft = crate::workflow::read_authoring_draft(&input.id, &input.version)?;
    input.check(
        &draft.package_id,
        &draft.version,
        &draft.candidate_sha256,
        &draft.source_root,
        &draft.candidate_path,
        draft.tested_at.is_some(),
        draft.confirmed_at.is_some(),
    )?;
    if !matches!(
        draft.state,
        crate::extension_contracts::ExtensionCandidateState::Confirmed
            | crate::extension_contracts::ExtensionCandidateState::Submitted
    ) || draft.test_report["state"] != "passed"
        || draft.test_report["candidate_sha256"] != draft.candidate_sha256
    {
        return Err("workflow candidate test does not match the confirmed archive".into());
    }
    Ok(serde_json::to_value(
        crate::workflow::activate_authoring_draft(draft)?,
    )?)
}

pub(crate) fn list() -> Result<Value, Box<dyn Error>> {
    let profile = crate::store::paths::profile_name();
    let store = ActivationStore::new(
        crate::store::paths::agent_home().join("data/extension-activations.json"),
    );
    let mut items = store
        .load()?
        .into_values()
        .filter(|r| {
            r.target.profile == profile
                && matches!(
                    r.status,
                    ActivationStatus::Active | ActivationStatus::Partial
                )
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(
        json!({"profile":profile,"activations":items,"runtime_scope":"profile","schema_version":"extension_activation.v1"}),
    )
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RollbackKind {
    Plugin,
    Workflow,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RollbackInput {
    pub kind: RollbackKind,
    pub id: String,
    pub candidate_sha256: String,
    pub workspace_root: PathBuf,
}
pub(crate) fn rollback(input: RollbackInput) -> Result<Value, Box<dyn Error>> {
    crate::extension_contracts::validate_sha256("candidate_sha256", &input.candidate_sha256)?;
    let kind = match input.kind {
        RollbackKind::Plugin => ActivationKind::Plugin,
        RollbackKind::Workflow => ActivationKind::Workflow,
    };
    if !input.workspace_root.is_absolute() || !input.workspace_root.is_dir() {
        return Err("rollback workspace_root must be an existing absolute directory".into());
    }
    let target = super::extension_activation::resolve_activation_target(
        &crate::store::paths::profile_name(),
        &input.workspace_root,
        &[],
        None,
    )?;
    let store = ActivationStore::new(
        crate::store::paths::agent_home().join("data/extension-activations.json"),
    );
    let active = store
        .active_for(&target, kind, &input.id)?
        .filter(|r| r.status == ActivationStatus::Active)
        .ok_or("extension has no active development candidate")?;
    super::extension_activation::ensure_receipt_workspace(&active, &input.workspace_root)?;
    if !active
        .candidate_sha256
        .eq_ignore_ascii_case(&input.candidate_sha256)
    {
        return Err(
            "active candidate changed; inspect the activation receipt before rollback".into(),
        );
    }
    let result = match input.kind {
        RollbackKind::Plugin => {
            crate::plugin_authoring::rollback(&input.id, &input.workspace_root)?
        }
        RollbackKind::Workflow => {
            crate::workflow::rollback_authoring_activation(&input.id, &input.workspace_root)?
        }
    };
    Ok(serde_json::to_value(result)?)
}

pub(crate) fn activation_schema() -> Value {
    json!({"type":"object","properties":{
        "id":{"type":"string","minLength":1,"maxLength":200},
        "version":{"type":"string","minLength":1,"maxLength":80},
        "candidate_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"},
        "workspace_root":{"type":"string","minLength":1,"maxLength":4096}},
        "required":["id","version","candidate_sha256","workspace_root"],"additionalProperties":false})
}
pub(crate) fn rollback_schema() -> Value {
    json!({"type":"object","properties":{
        "kind":{"enum":["plugin","workflow"]},"id":{"type":"string","minLength":1,"maxLength":200},
        "candidate_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"},"workspace_root":{"type":"string","minLength":1,"maxLength":4096}},
        "required":["kind","id","candidate_sha256","workspace_root"],"additionalProperties":false})
}

#[cfg(test)]
#[path = "extension_lifecycle_tests.rs"]
mod tests;
