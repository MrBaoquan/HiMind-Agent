//! Discovery and durable receipts for workspace-instruction projections.
//!
//! Discovery is intentionally read-only. A target describes what a client can
//! consume and where its native instruction file would live; it does not imply
//! that the client loaded the file or authorize a write.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::instruction_projection::{ProjectionReceipt, ProjectionTarget};
use crate::store::atomic_file::atomic_write;

const RECEIPT_DIRECTORY: &str = "instruction-projection-receipts";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionTargetDescriptor {
    pub target: ProjectionTarget,
    pub detected: bool,
    pub native: bool,
    pub degraded: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ProjectionReceipt>,
}

/// Discover project and user instruction targets for the supported clients.
///
/// The returned list includes absent files so the UI can preview a first
/// projection. Paths are resolved from the supplied workspace and the current
/// user profile; no files are created or modified.
pub(crate) fn discover_instruction_targets(
    workspace: &Path,
) -> Result<Vec<InstructionTargetDescriptor>, Box<dyn Error>> {
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(format!("workspace is not a directory: {}", workspace.display()).into());
    }
    let project_root = project_root(&workspace);
    let mut targets = Vec::new();

    let codex_project = project_root.join("AGENTS.md");
    targets.push(descriptor(
        "codex-instructions",
        "codex",
        codex_project,
        "project",
        true,
        true,
        false,
        true,
        "Codex project instructions are natively discovered by AGENTS.md",
    ));
    let codex_global = first_existing_or_default(
        &[
            user_home().join(".codex").join("AGENTS.md"),
            user_home().join(".agents").join("AGENTS.md"),
        ],
        ".codex/AGENTS.md",
    );
    targets.push(descriptor(
        "codex-global-instructions",
        "codex",
        codex_global,
        "global",
        true,
        false,
        false,
        true,
        "Codex global instructions are loaded from the user Codex home",
    ));

    let claude_project = project_root.join("CLAUDE.md");
    targets.push(descriptor(
        "claude-code-instructions",
        "claude-code",
        claude_project,
        "project",
        true,
        true,
        true,
        true,
        "Claude Code project instructions are natively discovered by CLAUDE.md",
    ));
    let claude_global = user_home().join(".claude").join("CLAUDE.md");
    targets.push(descriptor(
        "claude-code-global-instructions",
        "claude-code",
        claude_global,
        "global",
        true,
        false,
        false,
        true,
        "Claude Code user instructions are loaded from ~/.claude/CLAUDE.md",
    ));

    let copilot_project = project_root.join(".github").join("copilot-instructions.md");
    targets.push(descriptor(
        "github-copilot-instructions",
        "github-copilot",
        copilot_project,
        "project",
        true,
        true,
        false,
        true,
        "GitHub Copilot repository custom instructions",
    ));
    // AGENTS.md is understood by some Copilot agent surfaces, but it is not
    // equivalent to repository custom instructions in every Copilot host.
    targets.push(descriptor(
        "github-copilot-agents-fallback",
        "github-copilot",
        project_root.join("AGENTS.md"),
        "project",
        true,
        true,
        false,
        false,
        "degraded fallback: this Copilot host may ignore AGENTS.md",
    ));

    for item in &mut targets {
        item.receipt = load_projection_receipt(&item.target)?;
    }

    Ok(targets)
}

pub(crate) fn descriptor_for_target(
    workspace: &Path,
    adapter_id: &str,
) -> Result<Option<InstructionTargetDescriptor>, Box<dyn Error>> {
    Ok(discover_instruction_targets(workspace)?
        .into_iter()
        .find(|item| item.target.adapter_id == adapter_id))
}

fn descriptor(
    adapter_id: &str,
    client_id: &str,
    path: PathBuf,
    scope: &str,
    supports_global: bool,
    supports_project: bool,
    supports_directory: bool,
    native: bool,
    reason: &str,
) -> InstructionTargetDescriptor {
    let detected = path.is_file();
    InstructionTargetDescriptor {
        target: ProjectionTarget {
            adapter_id: adapter_id.to_string(),
            client_id: client_id.to_string(),
            path: path.to_string_lossy().to_string(),
            scope: scope.to_string(),
            format: "markdown".to_string(),
            supports_global,
            supports_project,
            supports_directory,
            instruction_packs: Vec::new(),
        },
        detected,
        native,
        degraded: !native,
        reason: reason.to_string(),
        receipt: None,
    }
}

fn user_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn instruction_home_for_client(client_id: &str) -> PathBuf {
    let home = user_home();
    match client_id {
        "codex" => home.join(".codex"),
        "claude-code" => home.join(".claude"),
        _ => home,
    }
}

fn first_existing_or_default(candidates: &[PathBuf], fallback: &str) -> PathBuf {
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| user_home().join(fallback.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

fn project_root(workspace: &Path) -> PathBuf {
    let mut current = workspace.to_path_buf();
    loop {
        if current.join(".git").exists()
            || current.join("AGENTS.md").is_file()
            || current.join("CLAUDE.md").is_file()
        {
            return current;
        }
        let Some(parent) = current.parent() else {
            return workspace.to_path_buf();
        };
        if parent == current {
            return workspace.to_path_buf();
        }
        current = parent.to_path_buf();
    }
}

pub(crate) fn projection_receipt_path(target: &ProjectionTarget) -> PathBuf {
    let normalized_path = normalized_target_path(&target.path);
    let key = format!(
        "{}\n{}\n{}",
        target.adapter_id,
        target.client_id,
        normalized_path.to_string_lossy()
    );
    let digest = Sha256::digest(key.as_bytes());
    crate::store::paths::agent_home()
        .join(RECEIPT_DIRECTORY)
        .join(format!("{:x}.json", digest))
}

fn normalized_target_path(path: &str) -> PathBuf {
    let target = PathBuf::from(path.trim());
    if let Ok(canonical) = fs::canonicalize(&target) {
        return canonical;
    }
    let Some(file_name) = target.file_name() else {
        return target;
    };
    target
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .map(|parent| parent.join(file_name))
        .unwrap_or(target)
}

pub(crate) fn save_projection_receipt(
    receipt: &ProjectionReceipt,
) -> Result<PathBuf, Box<dyn Error>> {
    let target = ProjectionTarget {
        adapter_id: receipt.adapter_id.clone(),
        client_id: receipt.client_id.clone(),
        path: receipt.target_path.clone(),
        scope: String::new(),
        format: "markdown".to_string(),
        supports_global: false,
        supports_project: false,
        supports_directory: false,
        instruction_packs: Vec::new(),
    };
    let path = projection_receipt_path(&target);
    let payload = serde_json::to_vec_pretty(receipt)?;
    atomic_write(&path, &payload)?;
    Ok(path)
}

pub(crate) fn load_projection_receipt(
    target: &ProjectionTarget,
) -> Result<Option<ProjectionReceipt>, Box<dyn Error>> {
    let path = projection_receipt_path(target);
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(
        content.trim_start_matches('\u{feff}'),
    )?))
}

/// Remove the durable receipt after a projection has been rolled back.
///
/// A receipt describes the digest that was written by HiMind. Keeping it after
/// rollback would make the next planning pass compare the restored target
/// against stale managed content and report a false conflict.
pub(crate) fn remove_projection_receipt(receipt: &ProjectionReceipt) -> Result<(), Box<dyn Error>> {
    let target = ProjectionTarget {
        adapter_id: receipt.adapter_id.clone(),
        client_id: receipt.client_id.clone(),
        path: receipt.target_path.clone(),
        scope: String::new(),
        format: "markdown".to_string(),
        supports_global: false,
        supports_project: false,
        supports_directory: false,
        instruction_packs: Vec::new(),
    };
    let path = projection_receipt_path(&target);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;

    #[test]
    fn discovers_project_targets_without_writing() {
        let root =
            std::env::temp_dir().join(format!("himind-instruction-targets-{}", std::process::id()));
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::write(root.join(".github/copilot-instructions.md"), "user").unwrap();
        let targets = discover_instruction_targets(&root).unwrap();
        let copilot = targets
            .iter()
            .find(|item| item.target.adapter_id == "github-copilot-instructions")
            .unwrap();
        assert!(copilot.detected);
        assert!(copilot.native);
        assert!(!root.join("AGENTS.md").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn receipt_round_trip_uses_profile_home() {
        let _guard = test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-instruction-receipts-{}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".git")).unwrap();
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let receipt = ProjectionReceipt {
            schema_version: "instruction_projection.v1".to_string(),
            adapter_id: "codex-instructions".to_string(),
            client_id: "codex".to_string(),
            target_path: root.join("AGENTS.md").to_string_lossy().to_string(),
            status: crate::instruction_projection::ProjectionStatus::ProjectedManaged,
            changed: true,
            backup_path: String::new(),
            previous_digest: "sha256:old".to_string(),
            new_digest: "sha256:new".to_string(),
            managed_digest: "sha256:block".to_string(),
            managed_keys: vec!["codex-instructions:x".to_string()],
            message: "ok".to_string(),
        };
        let path = save_projection_receipt(&receipt).unwrap();
        assert!(path.is_file());
        let target = ProjectionTarget {
            adapter_id: receipt.adapter_id.clone(),
            client_id: receipt.client_id.clone(),
            path: receipt.target_path.clone(),
            scope: "project".to_string(),
            format: "markdown".to_string(),
            supports_global: false,
            supports_project: true,
            supports_directory: false,
            instruction_packs: Vec::new(),
        };
        assert_eq!(
            load_projection_receipt(&target).unwrap(),
            Some(receipt.clone())
        );
        let discovered = discover_instruction_targets(&root).unwrap();
        let codex_project = discovered
            .iter()
            .find(|item| item.target.adapter_id == "codex-instructions")
            .unwrap();
        assert_eq!(codex_project.receipt, Some(receipt.clone()));
        remove_projection_receipt(&receipt).unwrap();
        assert_eq!(load_projection_receipt(&target).unwrap(), None);
        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(root);
    }
}
