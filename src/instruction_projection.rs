//! Cross-client projection contract for workspace instructions.
//!
//! DSH consumes the native instruction chain. Other clients need a managed
//! projection, but a discovered file is not proof that a client loaded it.
//! This contract keeps observation, planning, mutation and rollback separate so
//! adapters can stop on conflicts and report degraded support explicitly.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::workspace_instructions::{InstructionOverlay, InstructionSnapshot};

pub(crate) const PROJECTION_SCHEMA_VERSION: &str = "instruction_projection.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectionStatus {
    NativeLoaded,
    ProjectedManaged,
    ProjectedDegraded,
    Conflict,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionTarget {
    pub adapter_id: String,
    pub client_id: String,
    pub path: String,
    pub scope: String,
    pub format: String,
    #[serde(default)]
    pub supports_global: bool,
    #[serde(default)]
    pub supports_project: bool,
    #[serde(default)]
    pub supports_directory: bool,
    #[serde(default)]
    pub instruction_packs: Vec<InstructionPackRef>,
}

pub(crate) use crate::instruction_pack::InstructionPackRef;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionObservation {
    pub schema_version: String,
    pub target: ProjectionTarget,
    pub exists: bool,
    #[serde(default)]
    pub current_digest: String,
    #[serde(default)]
    pub managed_digest: String,
    #[serde(default)]
    pub expected_managed_digest: String,
    pub status: ProjectionStatus,
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionWrite {
    pub path: String,
    pub content_digest: String,
    pub content_bytes: usize,
    pub managed_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(default)]
    pub backup_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionPlan {
    pub schema_version: String,
    pub adapter_id: String,
    pub client_id: String,
    pub target_path: String,
    pub snapshot_digest: String,
    #[serde(default)]
    pub expected_current_digest: String,
    pub status: ProjectionStatus,
    #[serde(default)]
    pub writes: Vec<ProjectionWrite>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub unsupported: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub managed_block: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionReceipt {
    pub schema_version: String,
    pub adapter_id: String,
    pub client_id: String,
    pub target_path: String,
    pub status: ProjectionStatus,
    pub changed: bool,
    #[serde(default)]
    pub backup_path: String,
    #[serde(default)]
    pub previous_digest: String,
    #[serde(default)]
    pub new_digest: String,
    #[serde(default)]
    pub managed_digest: String,
    #[serde(default)]
    pub managed_keys: Vec<String>,
    #[serde(default)]
    pub message: String,
}

pub(crate) trait InstructionAdapter {
    fn inspect(&self, target: &ProjectionTarget) -> Result<ProjectionObservation, Box<dyn Error>>;
    fn plan(
        &self,
        snapshot: &InstructionSnapshot,
        target: &ProjectionTarget,
        observation: &ProjectionObservation,
    ) -> Result<ProjectionPlan, Box<dyn Error>>;
    fn apply(&self, plan: &ProjectionPlan) -> Result<ProjectionReceipt, Box<dyn Error>>;
    fn rollback(&self, receipt: &ProjectionReceipt) -> Result<(), Box<dyn Error>>;
}

/// Generic markdown-file adapter used by clients whose native instruction
/// format is an AGENTS.md/CLAUDE.md-style text file. It owns only the marker
/// block and preserves all user-authored text around it.
#[derive(Debug, Clone)]
pub(crate) struct ManagedMarkdownAdapter {
    pub adapter_id: String,
    pub begin_marker: String,
    pub end_marker: String,
}

impl ManagedMarkdownAdapter {
    pub(crate) fn himind(adapter_id: impl Into<String>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            begin_marker: crate::workspace_instructions::HIMIND_MANAGED_BEGIN.to_string(),
            end_marker: crate::workspace_instructions::HIMIND_MANAGED_END.to_string(),
        }
    }

    fn render_block(&self, snapshot: &InstructionSnapshot) -> String {
        let mut block = String::new();
        block.push_str(&self.begin_marker);
        block.push('\n');
        block
            .push_str("<!-- Generated by HiMind. Edit the source instruction files instead. -->\n");
        block.push_str("\n");
        block.push_str("## HiMind workspace instructions\n\n");
        block.push_str(&format!(
            "Source digest: `{}`\n\n",
            snapshot.rendered_digest
        ));
        if !snapshot.rendered_content.trim().is_empty() {
            block.push_str("### Resolved instruction content\n\n");
            block.push_str(snapshot.rendered_content.trim_end());
            block.push_str("\n\n");
        }
        for source in snapshot.sources.iter().filter(|source| {
            matches!(
                source.status,
                crate::workspace_instructions::InstructionSourceStatus::Loaded
                    | crate::workspace_instructions::InstructionSourceStatus::Truncated
            )
        }) {
            block.push_str(&format!("### {}\n\n", source.path));
            block.push_str(&format!("- Scope: `{}`\n", scope_name(&source.scope)));
            block.push_str(&format!("- Digest: `{}`\n", source.digest));
            block.push_str(&format!(
                "- Rendered bytes: `{}`\n\n",
                source.rendered_bytes
            ));
        }
        block.push_str(&self.end_marker);
        block
    }

    fn split_managed_block(
        &self,
        content: &str,
    ) -> Result<(String, Option<String>), Box<dyn Error>> {
        let begin = content
            .match_indices(&self.begin_marker)
            .collect::<Vec<_>>();
        let end = content.match_indices(&self.end_marker).collect::<Vec<_>>();
        if begin.len() > 1 || end.len() > 1 {
            return Err("multiple HiMind managed instruction blocks found".into());
        }
        match (begin.first(), end.first()) {
            (None, None) => Ok((content.to_string(), None)),
            (Some((begin_at, _)), Some((end_at, _))) if begin_at < end_at => {
                let end_at = *end_at + self.end_marker.len();
                let mut remainder = String::new();
                remainder.push_str(&content[..*begin_at]);
                remainder.push_str(&content[end_at..]);
                Ok((remainder, Some(content[*begin_at..end_at].to_string())))
            }
            _ => Err("HiMind managed instruction markers are unbalanced".into()),
        }
    }
}

impl InstructionAdapter for ManagedMarkdownAdapter {
    fn inspect(&self, target: &ProjectionTarget) -> Result<ProjectionObservation, Box<dyn Error>> {
        let path = PathBuf::from(target.path.trim());
        let content = if path.is_file() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let (_remainder, managed) = self.split_managed_block(&content)?;
        let status = if managed.is_some() {
            ProjectionStatus::ProjectedManaged
        } else {
            ProjectionStatus::NativeLoaded
        };
        Ok(ProjectionObservation {
            schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
            target: target.clone(),
            exists: path.is_file(),
            current_digest: digest(content.as_bytes()),
            managed_digest: managed
                .as_deref()
                .map(|value| digest(value.as_bytes()))
                .unwrap_or_default(),
            expected_managed_digest: String::new(),
            status,
            problems: Vec::new(),
        })
    }

    fn plan(
        &self,
        snapshot: &InstructionSnapshot,
        target: &ProjectionTarget,
        observation: &ProjectionObservation,
    ) -> Result<ProjectionPlan, Box<dyn Error>> {
        let block = self.render_block(snapshot);
        let mut plan = plan_managed_block(snapshot, target, observation, &block);
        if let Some(write) = plan.writes.first_mut() {
            write.content = block.clone();
        }
        plan.managed_block = block;
        Ok(plan)
    }

    fn apply(&self, plan: &ProjectionPlan) -> Result<ProjectionReceipt, Box<dyn Error>> {
        validate_plan(plan).map_err(std::io::Error::other)?;
        if plan.writes.is_empty() {
            return Ok(ProjectionReceipt {
                schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
                adapter_id: plan.adapter_id.clone(),
                client_id: plan.client_id.clone(),
                target_path: plan.target_path.clone(),
                status: plan.status.clone(),
                changed: false,
                backup_path: String::new(),
                previous_digest: String::new(),
                new_digest: String::new(),
                managed_digest: String::new(),
                managed_keys: Vec::new(),
                message: "no projection changes required".to_string(),
            });
        }
        let path = PathBuf::from(plan.target_path.trim());
        let existing = if path.is_file() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        if !plan.expected_current_digest.is_empty()
            && digest(existing.as_bytes()) != plan.expected_current_digest
        {
            return Err("target changed after planning; refusing projection".into());
        }
        let (remainder, _managed) = self.split_managed_block(&existing)?;
        let managed_block = plan
            .writes
            .first()
            .map(|write| write.content.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(plan.managed_block.as_str());
        let content = if remainder.trim().is_empty() {
            managed_block.to_string()
        } else {
            format!("{}\n\n{}\n", remainder.trim_end(), managed_block)
        };
        let backup_path = if path.is_file() {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_millis())
                .unwrap_or_default();
            let backup = path.with_file_name(format!(
                "{}.himind-instructions-backup-{stamp}.bak",
                path.file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("instructions")
            ));
            fs::copy(&path, &backup)?;
            backup.to_string_lossy().to_string()
        } else {
            String::new()
        };
        crate::store::atomic_file::atomic_write(&path, content.as_bytes())?;
        let receipt = ProjectionReceipt {
            schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
            adapter_id: plan.adapter_id.clone(),
            client_id: plan.client_id.clone(),
            target_path: plan.target_path.clone(),
            status: ProjectionStatus::ProjectedManaged,
            changed: true,
            backup_path,
            previous_digest: digest(existing.as_bytes()),
            new_digest: digest(content.as_bytes()),
            managed_digest: digest(managed_block.as_bytes()),
            managed_keys: plan
                .writes
                .iter()
                .map(|write| write.managed_key.clone())
                .collect(),
            message: "managed instruction block applied".to_string(),
        };
        validate_receipt(&receipt).map_err(std::io::Error::other)?;
        Ok(receipt)
    }

    fn rollback(&self, receipt: &ProjectionReceipt) -> Result<(), Box<dyn Error>> {
        validate_receipt(receipt).map_err(std::io::Error::other)?;
        if receipt.backup_path.trim().is_empty() {
            let path = PathBuf::from(receipt.target_path.trim());
            if path.is_file() {
                let current = fs::read(&path)?;
                if digest(&current) != receipt.new_digest {
                    return Err("target changed after projection; refusing rollback".into());
                }
                fs::remove_file(path)?;
            }
            return Ok(());
        }
        let backup = PathBuf::from(receipt.backup_path.trim());
        if !backup.is_file() {
            return Err("projection backup is missing".into());
        }
        let path = PathBuf::from(receipt.target_path.trim());
        let current = if path.is_file() {
            fs::read(&path)?
        } else {
            Vec::new()
        };
        if digest(&current) != receipt.new_digest {
            return Err("target changed after projection; refusing rollback".into());
        }
        let previous = fs::read(&backup)?;
        crate::store::atomic_file::atomic_write(&path, &previous)?;
        Ok(())
    }
}

pub(crate) fn plan_managed_block(
    snapshot: &InstructionSnapshot,
    target: &ProjectionTarget,
    observation: &ProjectionObservation,
    block: &str,
) -> ProjectionPlan {
    let mut unsupported = Vec::new();
    let scope_supported = match target.scope.as_str() {
        "global" => target.supports_global,
        "project" => target.supports_project,
        "directory" => target.supports_directory,
        _ => false,
    };
    if !scope_supported {
        unsupported.push(format!("target does not support {} scope", target.scope));
    }
    let mut conflicts = observation.problems.clone();
    if observation.status == ProjectionStatus::Conflict {
        conflicts.push("target managed content changed outside HiMind".to_string());
    }
    if !observation.expected_managed_digest.is_empty()
        && observation.managed_digest != observation.expected_managed_digest
    {
        conflicts.push("target managed content changed since the last HiMind receipt".to_string());
    }
    let content_digest = digest(block.as_bytes());
    let writes = if conflicts.is_empty()
        && unsupported.is_empty()
        && observation.managed_digest != content_digest
    {
        vec![ProjectionWrite {
            path: target.path.clone(),
            content_digest,
            content_bytes: block.len(),
            managed_key: format!("{}:{}", target.adapter_id, target.path),
            content: block.to_string(),
            backup_path: String::new(),
        }]
    } else {
        Vec::new()
    };
    let status = if !conflicts.is_empty() {
        ProjectionStatus::Conflict
    } else if !unsupported.is_empty() {
        ProjectionStatus::Blocked
    } else if writes.is_empty() {
        ProjectionStatus::ProjectedManaged
    } else if target.scope == "project" || target.scope == "directory" {
        ProjectionStatus::ProjectedManaged
    } else {
        ProjectionStatus::ProjectedDegraded
    };
    ProjectionPlan {
        schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
        adapter_id: target.adapter_id.clone(),
        client_id: target.client_id.clone(),
        target_path: target.path.clone(),
        snapshot_digest: snapshot.rendered_digest.clone(),
        expected_current_digest: observation.current_digest.clone(),
        status,
        writes,
        conflicts,
        unsupported,
        warnings: if target.format == "markdown" {
            Vec::new()
        } else {
            vec!["projection format is client-specific".to_string()]
        },
        managed_block: block.to_string(),
    }
}

pub(crate) fn validate_plan(plan: &ProjectionPlan) -> Result<(), String> {
    if plan.schema_version != PROJECTION_SCHEMA_VERSION {
        return Err("unsupported projection plan schema".to_string());
    }
    if plan.adapter_id.trim().is_empty() || plan.client_id.trim().is_empty() {
        return Err("projection plan identity is incomplete".to_string());
    }
    if !plan.conflicts.is_empty() && !plan.writes.is_empty() {
        return Err("conflicted projection plan cannot contain writes".to_string());
    }
    if !plan.unsupported.is_empty() && !plan.writes.is_empty() {
        return Err("blocked projection plan cannot contain writes".to_string());
    }
    Ok(())
}

pub(crate) fn validate_receipt(receipt: &ProjectionReceipt) -> Result<(), String> {
    if receipt.schema_version != PROJECTION_SCHEMA_VERSION {
        return Err("unsupported projection receipt schema".to_string());
    }
    if receipt.changed && receipt.new_digest.trim().is_empty() {
        return Err("changed projection receipt is missing new digest".to_string());
    }
    Ok(())
}

pub(crate) fn path_from_target(target: &ProjectionTarget) -> PathBuf {
    PathBuf::from(target.path.trim())
}

pub(crate) fn overlays_from_target(
    target: &ProjectionTarget,
) -> Result<Vec<InstructionOverlay>, Box<dyn Error>> {
    let mut overlays = Vec::with_capacity(target.instruction_packs.len());
    for reference in &target.instruction_packs {
        let published = crate::instruction_pack::read_published(&reference.id, &reference.version)?;
        if !reference.digest.is_empty() && reference.digest != published.digest {
            return Err(format!(
                "指令包摘要已变化，请重新读取已发布版本: {} v{}",
                reference.id, reference.version
            )
            .into());
        }
        overlays.push(InstructionOverlay {
            id: reference.id.clone(),
            version: reference.version.clone(),
            digest: published.digest,
            content: published.instructions,
        });
    }
    Ok(overlays)
}

fn digest(content: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(content))
}

fn scope_name(scope: &crate::workspace_instructions::InstructionScope) -> &'static str {
    match scope {
        crate::workspace_instructions::InstructionScope::Global => "global",
        crate::workspace_instructions::InstructionScope::Project => "project",
        crate::workspace_instructions::InstructionScope::Directory => "directory",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_instructions::{InstructionProjection, InstructionSourceSnapshot};
    use crate::workspace_instructions::{InstructionScope, InstructionSourceStatus};

    fn snapshot() -> InstructionSnapshot {
        InstructionSnapshot {
            schema_version: "instruction_snapshot.v1".to_string(),
            resolver_version: "test".to_string(),
            workspace_root: "C:/workspace".to_string(),
            project_root: "C:/workspace".to_string(),
            resolved_at_ms: 1,
            rendered_bytes: 1,
            rendered_digest: "sha256:snapshot".to_string(),
            sources: vec![InstructionSourceSnapshot {
                path: "AGENTS.md".to_string(),
                scope: InstructionScope::Project,
                precedence: 0,
                digest: "sha256:source".to_string(),
                source_bytes: 1,
                rendered_bytes: 1,
                status: InstructionSourceStatus::Loaded,
                reason: String::new(),
            }],
            projection: InstructionProjection {
                adapter_id: "himind-dsh".to_string(),
                status: "native_loaded".to_string(),
                dsh_max_bytes: 1,
                dsh_max_source_bytes: 1,
                reason: String::new(),
                project_root_markers: vec![".git".to_string()],
                instruction_file_candidates: vec!["AGENTS.md".to_string()],
                local_instruction_file_candidates: vec![],
            },
            instruction_packs: Vec::new(),
            rendered_content: "instructions".to_string(),
        }
    }

    fn target(scope: &str) -> ProjectionTarget {
        ProjectionTarget {
            adapter_id: "codex".to_string(),
            client_id: "codex".to_string(),
            path: "C:/workspace/AGENTS.md".to_string(),
            scope: scope.to_string(),
            format: "markdown".to_string(),
            supports_global: false,
            supports_project: true,
            supports_directory: false,
            instruction_packs: Vec::new(),
        }
    }

    #[test]
    fn conflicting_target_stops_writes() {
        let observation = ProjectionObservation {
            schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
            target: target("project"),
            exists: true,
            current_digest: "sha256:current".to_string(),
            managed_digest: "sha256:old".to_string(),
            expected_managed_digest: String::new(),
            status: ProjectionStatus::Conflict,
            problems: vec![],
        };
        let plan = plan_managed_block(&snapshot(), &target("project"), &observation, "managed");
        assert_eq!(plan.status, ProjectionStatus::Conflict);
        assert!(plan.writes.is_empty());
        assert!(validate_plan(&plan).is_ok());
    }

    #[test]
    fn unsupported_scope_is_blocked() {
        let observation = ProjectionObservation {
            schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
            target: target("global"),
            exists: false,
            current_digest: String::new(),
            managed_digest: String::new(),
            expected_managed_digest: String::new(),
            status: ProjectionStatus::Blocked,
            problems: vec![],
        };
        let plan = plan_managed_block(&snapshot(), &target("global"), &observation, "managed");
        assert_eq!(plan.status, ProjectionStatus::Blocked);
        assert!(plan.writes.is_empty());
    }

    #[test]
    fn markdown_adapter_applies_managed_block_and_rolls_back_new_file() {
        let root = std::env::temp_dir().join(format!(
            "himind-instruction-projection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("AGENTS.md");
        let adapter = ManagedMarkdownAdapter::himind("codex");
        let target = ProjectionTarget {
            adapter_id: "codex".to_string(),
            client_id: "codex".to_string(),
            path: path.to_string_lossy().to_string(),
            scope: "project".to_string(),
            format: "markdown".to_string(),
            supports_global: false,
            supports_project: true,
            supports_directory: false,
            instruction_packs: Vec::new(),
        };
        let observation = adapter.inspect(&target).unwrap();
        let plan = adapter.plan(&snapshot(), &target, &observation).unwrap();
        assert_eq!(plan.writes.len(), 1);
        let receipt = adapter.apply(&plan).unwrap();
        assert!(receipt.changed);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("HIMIND:BEGIN"));
        adapter.rollback(&receipt).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_rejects_target_changed_after_plan() {
        let root = std::env::temp_dir().join(format!(
            "himind-instruction-projection-stale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("AGENTS.md");
        std::fs::write(&path, "user content\n").unwrap();
        let adapter = ManagedMarkdownAdapter::himind("codex");
        let target = ProjectionTarget {
            path: path.to_string_lossy().to_string(),
            ..target("project")
        };
        let observation = adapter.inspect(&target).unwrap();
        let plan = adapter.plan(&snapshot(), &target, &observation).unwrap();
        std::fs::write(&path, "changed by user\n").unwrap();
        let error = adapter.apply(&plan).unwrap_err();
        assert!(error.to_string().contains("changed after planning"));
        let _ = std::fs::remove_dir_all(root);
    }
}
