//! Read-only ECC repository inspection.
//!
//! ECC files are treated as untrusted source material. This module classifies
//! them and extracts metadata for a review/import plan; it never executes
//! shell commands, hooks, installers, or model/tool declarations.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_FILES: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EccArtifactKind {
    WorkspaceInstruction,
    SubagentTemplate,
    InstructionPack,
    Skill,
    Workflow,
    HookCandidate,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EccArtifact {
    pub path: String,
    pub kind: EccArtifactKind,
    pub bytes: usize,
    pub digest: String,
    pub title: String,
    #[serde(default)]
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EccInspection {
    pub root: String,
    pub artifacts: Vec<EccArtifact>,
    pub warnings: Vec<String>,
    pub executable_files_ignored: usize,
}

pub(crate) fn inspect_repository(root: &Path) -> Result<EccInspection, Box<dyn Error>> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(format!("ECC root is not a directory: {}", root.display()).into());
    }
    let mut artifacts = Vec::new();
    let mut warnings = Vec::new();
    let mut executable_files_ignored = 0usize;
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.path() == root || !entry.file_type().is_file() {
            continue;
        }
        if artifacts.len() >= MAX_FILES {
            warnings.push(format!(
                "file limit reached ({MAX_FILES}); remaining files omitted"
            ));
            break;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        let kind = classify(&relative);
        if matches!(kind, EccArtifactKind::HookCandidate)
            || relative.ends_with(".sh")
            || relative.ends_with(".ps1")
            || relative.ends_with(".bat")
        {
            executable_files_ignored = executable_files_ignored.saturating_add(1);
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.len() > MAX_FILE_BYTES {
            warnings.push(format!("omitted oversized file: {relative}"));
            continue;
        }
        let content = fs::read(entry.path())?;
        if content.contains(&0) {
            warnings.push(format!("omitted binary file: {relative}"));
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        let (frontmatter, title) = parse_markdown_metadata(&text);
        artifacts.push(EccArtifact {
            path: relative,
            kind,
            bytes: content.len(),
            digest: format!("sha256:{:x}", Sha256::digest(&content)),
            title,
            frontmatter,
            warnings: Vec::new(),
        });
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(EccInspection {
        root: root.to_string_lossy().to_string(),
        artifacts,
        warnings,
        executable_files_ignored,
    })
}

fn classify(path: &str) -> EccArtifactKind {
    let normalized = path.to_ascii_lowercase();
    let file = normalized.rsplit('/').next().unwrap_or(&normalized);
    if file == "agents.md" || file == "claude.md" || file == "agents.local.md" {
        return EccArtifactKind::WorkspaceInstruction;
    }
    if normalized.starts_with("agents/") && file.ends_with(".md") {
        return EccArtifactKind::SubagentTemplate;
    }
    if normalized.starts_with("rules/") && file.ends_with(".md") {
        return EccArtifactKind::InstructionPack;
    }
    if normalized.starts_with("skills/") && file.ends_with(".md") {
        return EccArtifactKind::Skill;
    }
    if normalized.starts_with("commands/") && file.ends_with(".md") {
        return EccArtifactKind::Workflow;
    }
    if normalized.starts_with("hooks/") {
        return EccArtifactKind::HookCandidate;
    }
    EccArtifactKind::Unknown
}

fn parse_markdown_metadata(content: &str) -> (serde_json::Map<String, serde_json::Value>, String) {
    let mut frontmatter = serde_json::Map::new();
    let mut title = String::new();
    let mut lines = content.lines();
    if lines.next().map(str::trim) == Some("---") {
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed == "---" {
                break;
            }
            if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if !key.is_empty() {
                    frontmatter.insert(
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
            }
        }
    }
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("# ") {
            title = value.trim().to_string();
            break;
        }
    }
    (frontmatter, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_ecc_surfaces_without_executing_hooks() {
        let root =
            std::env::temp_dir().join(format!("himind-ecc-inspection-{}", std::process::id()));
        for directory in ["agents", "rules/common", "skills/demo", "commands", "hooks"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("AGENTS.md"), "# Project\nFollow rules").unwrap();
        fs::write(
            root.join("agents/reviewer.md"),
            "---\nmodel: fast\n---\n# Reviewer",
        )
        .unwrap();
        fs::write(root.join("rules/common/agents.md"), "# Rules").unwrap();
        fs::write(root.join("skills/demo/SKILL.md"), "# Skill").unwrap();
        fs::write(root.join("commands/check.md"), "# Check").unwrap();
        fs::write(root.join("hooks/install.sh"), "echo unsafe").unwrap();
        let report = inspect_repository(&root).unwrap();
        assert!(report
            .artifacts
            .iter()
            .any(|item| item.kind == EccArtifactKind::SubagentTemplate));
        assert!(report
            .artifacts
            .iter()
            .any(|item| item.kind == EccArtifactKind::Skill));
        assert_eq!(report.executable_files_ignored, 1);
        assert!(!root.join("installed").exists());
        let _ = fs::remove_dir_all(root);
    }
}
