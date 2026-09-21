use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEVELOPMENT_CHECKPOINT_SCHEMA_VERSION: &str = "development_checkpoint.v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevelopmentCheckpointCreatedBy {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevelopmentCheckpoint {
    pub schema_version: String,
    pub checkpoint_id: String,
    pub project_id: String,
    #[serde(default)]
    pub target_id: String,
    #[serde(default)]
    pub environment: String,
    pub workspace_root: String,
    pub repository_root: String,
    #[serde(default)]
    pub branch: String,
    pub commit_sha: String,
    pub tree_digest: String,
    pub dirty: bool,
    #[serde(default)]
    pub status_digest: String,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub created_by: DevelopmentCheckpointCreatedBy,
    #[serde(default)]
    pub artifacts: Vec<Value>,
    pub created_at: String,
}

pub(crate) fn create(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = input
        .get("workspace_root")
        .or_else(|| input.get("project_root"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("development checkpoint requires workspace_root")?;
    let project_id = input
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("development checkpoint requires project_id")?;
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let created_by: DevelopmentCheckpointCreatedBy = input
        .get("created_by")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if !created_by.lease_id.trim().is_empty() {
        crate::workspace_lease::validate_active(&created_by.lease_id, &workspace, "write")?;
    }
    let repository_root =
        PathBuf::from(git_output(&workspace, &["rev-parse", "--show-toplevel"])?.trim())
            .canonicalize()?;
    if !workspace.starts_with(&repository_root) {
        return Err("development checkpoint workspace is outside the Git repository".into());
    }
    let commit_sha = git_output(&workspace, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let branch = git_output(&workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let status = git_output(&workspace, &crate::worktree_identity::status_arguments())?;
    let dirty = !status.trim().is_empty();
    let tree_digest = if dirty {
        worktree_tree_digest(&workspace)?
    } else {
        git_output(&workspace, &["rev-parse", "HEAD^{tree}"])?
            .trim()
            .to_string()
    };
    let status_digest = format!("{:x}", Sha256::digest(status.as_bytes()));
    let checkpoint_id = format!(
        "devcp_{:x}",
        Sha256::digest(
            format!(
                "{}:{}:{}:{}",
                project_id, commit_sha, tree_digest, status_digest
            )
            .as_bytes()
        )
    );
    let tests = input
        .get("tests")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let checkpoint = DevelopmentCheckpoint {
        schema_version: DEVELOPMENT_CHECKPOINT_SCHEMA_VERSION.to_string(),
        checkpoint_id,
        project_id: project_id.to_string(),
        target_id: input
            .get("target_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        environment: input
            .get("environment")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        workspace_root: display_path(&workspace),
        repository_root: display_path(&repository_root),
        branch,
        commit_sha,
        tree_digest,
        dirty,
        status_digest,
        tests,
        created_by,
        artifacts: Vec::new(),
        created_at: unix_timestamp_string(),
    };
    let directory = repository_root.join(".himind").join("checkpoints");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", checkpoint.checkpoint_id));
    std::fs::write(&path, serde_json::to_vec_pretty(&checkpoint)?)?;
    let bytes = std::fs::read(&path)?;
    let artifact = json!({
        "artifact_id": "development-checkpoint",
        "artifact_type": "development_checkpoint",
        "name": "Development Checkpoint",
        "uri": path.to_string_lossy(),
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
        "size_bytes": bytes.len(),
    });
    Ok(json!({
        "ok": true,
        "checkpoint": checkpoint,
        "artifacts": [artifact],
    }))
}

fn worktree_tree_digest(workspace: &Path) -> Result<String, Box<dyn Error>> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let index_path = std::env::temp_dir().join(format!(
        "himind-development-index-{}-{unique}",
        std::process::id()
    ));
    let lock_path = PathBuf::from(format!("{}.lock", index_path.to_string_lossy()));
    let _ = std::fs::remove_file(&index_path);
    let _ = std::fs::remove_file(&lock_path);
    let result = (|| -> Result<String, Box<dyn Error>> {
        git_with_index(workspace, &index_path, &["read-tree", "HEAD"])?;
        git_with_index(
            workspace,
            &index_path,
            &crate::worktree_identity::stage_arguments(),
        )?;
        Ok(git_with_index(workspace, &index_path, &["write-tree"])?
            .trim()
            .to_string())
    })();
    let _ = std::fs::remove_file(index_path);
    let _ = std::fs::remove_file(lock_path);
    result
}

fn git_with_index(
    workspace: &Path,
    index_path: &Path,
    args: &[&str],
) -> Result<String, Box<dyn Error>> {
    let output = crate::runtime::process::hidden_command("git")
        .current_dir(workspace)
        .env("GIT_INDEX_FILE", index_path)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Git checkpoint index command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_output(workspace: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = crate::runtime::process::hidden_command("git")
        .args(["-C"])
        .arg(workspace)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Git checkpoint command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(path: &Path, args: &[&str]) {
        let status = crate::runtime::process::hidden_command("git")
            .current_dir(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn creates_checkpoint_for_clean_repository() {
        let root = std::env::temp_dir().join(format!(
            "himind-development-checkpoint-{}-{}",
            std::process::id(),
            unix_timestamp_string()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("app.js"), "console.log('ok');\n").unwrap();
        git(&root, &["add", "app.js"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        let lease = crate::workspace_lease::acquire(&json!({
            "workspace_root": root.clone(),
            "project_id": "demo",
            "target_id": "main",
            "owner_client": "himind-ai",
            "owner_session": "session-1",
            "mode": "write"
        }))
        .unwrap();
        let lease_id = lease["lease"]["lease_id"].as_str().unwrap();

        let result = create(&json!({
            "workspace_root": root.clone(),
            "project_id": "demo",
            "target_id": "main",
            "environment": "development",
            "tests": ["unit"],
            "created_by": {
                "client": "himind-ai",
                "session_id": "session-1",
                "lease_id": lease_id
            }
        }))
        .unwrap();
        assert_eq!(
            result["checkpoint"]["schema_version"],
            DEVELOPMENT_CHECKPOINT_SCHEMA_VERSION
        );
        assert_eq!(result["checkpoint"]["dirty"], false);
        assert_eq!(result["checkpoint"]["tests"][0], "unit");
        let second = create(&json!({
            "workspace_root": root.clone(),
            "project_id": "demo",
            "target_id": "main",
            "environment": "development",
            "created_by": {
                "client": "himind-ai",
                "session_id": "session-1",
                "lease_id": lease_id
            }
        }))
        .unwrap();
        assert_eq!(
            result["checkpoint"]["tree_digest"],
            second["checkpoint"]["tree_digest"]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
