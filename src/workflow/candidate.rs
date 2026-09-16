use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn freeze_candidate(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = input
        .get("project_root")
        .or_else(|| input.get("repository_root"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("candidate freeze requires project_root or repository_root")?;
    let candidate_artifact_id = input
        .get("candidate_artifact_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("candidate freeze requires candidate_artifact_id")?;
    let allow_dirty = input
        .get("allow_dirty")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let workspace = PathBuf::from(workspace).canonicalize()?;
    let repository_root = git_output(&workspace, &["rev-parse", "--show-toplevel"])?;
    let repository_root = PathBuf::from(repository_root.trim()).canonicalize()?;
    if !workspace.starts_with(&repository_root) {
        return Err("candidate workspace is outside the Git repository".into());
    }

    let commit_sha = git_output(&workspace, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let head_tree_digest = git_output(&workspace, &["rev-parse", "HEAD^{tree}"])?
        .trim()
        .to_string();
    let status = git_output(
        &workspace,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    let dirty = !status.trim().is_empty();
    if dirty && !allow_dirty {
        return Err("candidate workspace has uncommitted changes".into());
    }
    let tree_digest = if dirty {
        worktree_tree_digest(&workspace)?
    } else {
        head_tree_digest
    };
    if commit_sha.is_empty() || tree_digest.is_empty() {
        return Err("candidate Git identity is incomplete".into());
    }

    let status_digest = format!("{:x}", Sha256::digest(status.as_bytes()));
    let candidate_id = format!(
        "{:x}",
        Sha256::digest(format!("{commit_sha}:{tree_digest}:{status_digest}").as_bytes())
    );
    let payload = json!({
        "schema_version": "workflow_candidate.v1",
        "candidate_id": candidate_id,
        "repository_root": repository_root,
        "commit_sha": commit_sha,
        "tree_digest": tree_digest,
        "dirty": dirty,
        "status_digest": status_digest,
        "frozen_at": unix_timestamp_string(),
    });
    let artifact_dir = repository_root
        .join(".himind")
        .join("artifacts")
        .join("workflow");
    std::fs::create_dir_all(&artifact_dir)?;
    let artifact_path = artifact_dir.join("candidate.json");
    std::fs::write(&artifact_path, serde_json::to_vec_pretty(&payload)?)?;
    let metadata = std::fs::metadata(&artifact_path)?;
    let sha256 = format!("{:x}", Sha256::digest(std::fs::read(&artifact_path)?));
    Ok(json!({
        "ok": true,
        "candidate": payload,
        "artifacts": [{
            "artifact_id": candidate_artifact_id,
            "artifact_type": "candidate",
            "name": "不可变交付候选",
            "uri": artifact_path.to_string_lossy(),
            "sha256": sha256,
            "size_bytes": metadata.len(),
        }]
    }))
}

fn worktree_tree_digest(workspace: &Path) -> Result<String, Box<dyn Error>> {
    let index_path = std::env::temp_dir().join(format!(
        "himind-candidate-index-{}-{}",
        std::process::id(),
        unix_timestamp_string()
    ));
    let _ = std::fs::remove_file(&index_path);
    let result = (|| -> Result<String, Box<dyn Error>> {
        git_with_index(workspace, &index_path, &["read-tree", "HEAD"])?;
        git_with_index(workspace, &index_path, &["add", "-A"])?;
        Ok(git_with_index(workspace, &index_path, &["write-tree"])?
            .trim()
            .to_string())
    })();
    let _ = std::fs::remove_file(index_path);
    result
}

fn git_with_index(
    workspace: &Path,
    index_path: &Path,
    args: &[&str],
) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .env("GIT_INDEX_FILE", index_path)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Git candidate index command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) fn read_candidate(uri: &str) -> Result<Value, Box<dyn Error>> {
    let path = candidate_artifact_path(uri).ok_or("candidate artifact URI is not local")?;
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if value.get("schema_version").and_then(Value::as_str) != Some("workflow_candidate.v1") {
        return Err("candidate artifact schema_version is invalid".into());
    }
    if value
        .get("commit_sha")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
        || value
            .get("tree_digest")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err("candidate artifact identity is incomplete".into());
    }
    Ok(value)
}

fn candidate_artifact_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    if let Ok(parsed) = url::Url::parse(uri) {
        if parsed.scheme() == "file" {
            return parsed.to_file_path().ok();
        }
    }
    let path = Path::new(uri);
    path.is_absolute().then(|| path.to_path_buf())
}

fn git_output(workspace: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("Git is unavailable for candidate freeze: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git candidate command failed: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn freezes_clean_git_head() {
        let root = std::env::temp_dir().join(format!(
            "himind-candidate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("app.js"), "console.log('ok');\n").unwrap();
        git(&root, &["add", "app.js"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);

        let result = freeze_candidate(&json!({
            "project_root": root,
            "candidate_artifact_id": "candidate",
            "allow_dirty": false
        }))
        .unwrap();
        assert_eq!(result["candidate"]["dirty"], false);
        assert_eq!(
            result["candidate"]["commit_sha"].as_str().unwrap().len(),
            40
        );
        assert_eq!(result["artifacts"][0]["artifact_id"], "candidate");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_dirty_workspace_when_disallowed() {
        let root = std::env::temp_dir().join(format!(
            "himind-candidate-dirty-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("app.js"), "console.log('ok');\n").unwrap();
        git(&root, &["add", "app.js"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(root.join("app.js"), "console.log('dirty');\n").unwrap();

        let error = freeze_candidate(&json!({
            "project_root": root,
            "candidate_artifact_id": "candidate",
            "allow_dirty": false
        }))
        .unwrap_err();
        assert!(error.to_string().contains("uncommitted changes"));
        let dirty = freeze_candidate(&json!({
            "project_root": root,
            "candidate_artifact_id": "candidate",
            "allow_dirty": true
        }))
        .unwrap();
        assert_eq!(dirty["candidate"]["dirty"], true);
        assert!(!dirty["candidate"]["tree_digest"]
            .as_str()
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
