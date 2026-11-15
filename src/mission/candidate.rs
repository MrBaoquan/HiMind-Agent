//! Freeze only the files in a task's confirmed scope, including Git-ignored files.
//! Missions can use ordinary folders and must never stage an enclosing repository.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::io::Read;
use std::path::{Component, Path};

pub(crate) fn scoped_candidate_identity(
    workspace: &Path,
    paths: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<Value, Box<dyn Error>> {
    let workspace = workspace.canonicalize()?;
    let mut entries = BTreeSet::new();
    for raw in paths {
        let raw = Path::new(raw.as_ref());
        if raw.components().any(|part| matches!(part, Component::ParentDir)) {
            return Err("candidate scope must not contain parent traversal".into());
        }
        let path = if raw.is_absolute() { raw.to_path_buf() } else { workspace.join(raw) };
        // Refuse symlink ancestors as well as symlink leaves; walking a scoped
        // directory must not silently start reading a different directory.
        for ancestor in path.ancestors() {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err("candidate symlinks require a resolved explicit path".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                entries.insert(path);
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {
                for entry in walkdir::WalkDir::new(&path).follow_links(false).into_iter()
                    .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
                {
                    let entry = entry?;
                    if entry.file_type().is_symlink() {
                        return Err("candidate symlinks require a resolved explicit path".into());
                    }
                    entries.insert(entry.path().to_path_buf());
                    if entries.len() > 10_000 {
                        return Err("candidate scope exceeds 10000 entries".into());
                    }
                }
            }
        }
    }
    if entries.is_empty() {
        return Err("candidate requires at least one task read or write path".into());
    }
    let mut content = Sha256::new();
    let mut manifest = Sha256::new();
    let mut total_bytes = 0u64;
    let mut files = serde_json::Map::new();
    for path in entries {
        let label = path.strip_prefix(&workspace).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        hash_field(&mut content, path.to_string_lossy().as_bytes());
        hash_field(&mut manifest, path.to_string_lossy().as_bytes());
        let kind = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err("candidate path became a symlink".into()),
            Ok(metadata) if metadata.is_file() => {
                total_bytes = total_bytes.checked_add(metadata.len()).ok_or("candidate size overflow")?;
                if total_bytes > 256 * 1024 * 1024 { return Err("candidate scope exceeds 256 MiB".into()); }
                let mut file = std::fs::File::open(&path)?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 65536];
                let mut read_bytes = 0u64;
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 { break; }
                    read_bytes += count as u64;
                    if read_bytes > metadata.len() { return Err("candidate file changed while reading".into()); }
                    digest.update(&buffer[..count]);
                }
                if read_bytes != metadata.len() { return Err("candidate file changed while reading".into()); }
                let digest = digest.finalize();
                files.insert(label.clone(), json!(format!("{digest:x}")));
                hash_field(&mut content, &digest);
                b"file".as_slice()
            }
            Ok(metadata) if metadata.is_dir() => b"directory".as_slice(),
            Ok(_) => return Err("candidate scope contains an unsupported file type".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                files.insert(label, Value::Null);
                b"missing".as_slice()
            }
            Err(error) => return Err(error.into()),
        };
        hash_field(&mut content, kind);
        hash_field(&mut manifest, kind);
    }
    let tree_digest = format!("{:x}", content.finalize());
    let status_digest = format!("{:x}", manifest.finalize());
    let candidate_id = format!("{:x}", Sha256::digest(format!("{}:{tree_digest}:{status_digest}", workspace.display()).as_bytes()));
    Ok(json!({"candidate": {
        "schema_version": "mission_candidate.v1", "identity_kind": "scoped_files_sha256",
        "workspace_root": workspace, "candidate_id": candidate_id,
        // Kept as an explicit marker for the existing verification envelope.
        "commit_sha": "not-applicable", "tree_digest": tree_digest, "status_digest": status_digest,
    }, "files": files}))
}

/// Files readable by this task but outside its declared write scope. A baseline
/// is captured before execution; candidate identity alone only binds the review.
pub(crate) fn protected_files(
    workspace: &Path,
    task: &super::MissionTaskSpec,
) -> Result<Value, Box<dyn Error>> {
    let root = workspace.canonicalize()?;
    let writes = task.write_paths.iter().map(|raw| {
        let path = Path::new(raw);
        if path.is_absolute() { path.to_path_buf() } else { root.join(path) }
    }).collect::<Vec<_>>();
    let identity = scoped_candidate_identity(workspace, &task.read_paths)?;
    let files = identity["files"].as_object().ok_or("candidate file manifest missing")?;
    Ok(Value::Object(files.iter().filter(|(label, value)| {
        let path = Path::new(label);
        let path = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
        // A missing read directory can be a required parent of an authorized
        // output. Creating that directory must not look like deleting a
        // protected file. Existing files at that path and new siblings still
        // remain protected; unrelated missing inputs retain their null entry.
        !writes.iter().any(|write| path.starts_with(write)
            || (value.is_null() && write.starts_with(&path)))
    }).map(|(key, value)| (key.clone(), value.clone())).collect()))
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_nested_workspace_is_bound_without_reading_its_parent() {
        let parent = std::env::temp_dir().join(format!("mission-candidate-{}", rand::random::<u64>()));
        let root = parent.join(".himind/test");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(parent.join(".gitignore"), ".himind/\n").unwrap();
        std::fs::write(root.join("src/app.js"), "before").unwrap();
        let freeze = || scoped_candidate_identity(&root, ["src", "REVIEW.md"]).unwrap();
        let before = freeze();
        std::fs::write(parent.join("unrelated.txt"), "outside scope").unwrap();
        assert_eq!(before, freeze());
        std::fs::write(root.join("src/app.js"), "after!").unwrap();
        assert_ne!(before, freeze());
        let before_review = freeze();
        std::fs::write(root.join("REVIEW.md"), "review").unwrap();
        assert_ne!(before_review, freeze());
        assert!(scoped_candidate_identity(&root, ["../unrelated.txt"]).is_err());
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn protected_manifest_excludes_writes_and_detects_changes_and_added_files() {
        let root = std::env::temp_dir().join(format!("mission-protected-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("test")).unwrap();
        std::fs::write(root.join("src/app.js"), "before").unwrap();
        std::fs::write(root.join("test/app.js"), "fixed check").unwrap();
        let task = serde_json::from_value(json!({"id":"fix", "title":"fix", "read_paths":["src", "test"],
            "write_paths":["src/app.js"], "execution":{"executor":"himind.fixture","role_id":"fix"}})).unwrap();
        let baseline = protected_files(&root, &task).unwrap();
        assert_eq!(baseline.as_object().unwrap().len(), 1);
        std::fs::write(root.join("src/app.js"), "implementation").unwrap();
        assert_eq!(baseline, protected_files(&root, &task).unwrap());
        std::fs::write(root.join("test/added.js"), "new test").unwrap();
        assert_ne!(baseline, protected_files(&root, &task).unwrap());
        std::fs::remove_file(root.join("test/added.js")).unwrap();
        std::fs::write(root.join("test/app.js"), "changed check").unwrap();
        assert_ne!(baseline, protected_files(&root, &task).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protected_manifest_allows_creation_of_authorized_parent_directories() {
        let root = std::env::temp_dir().join(format!("mission-protected-parent-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        let task = serde_json::from_value(json!({"id":"plan", "title":"plan",
            "read_paths":["reports", "unavailable-input.txt"],
            "write_paths":["reports/nested/plan.json"],
            "execution":{"executor":"himind.fixture","role_id":"plan"}})).unwrap();
        let baseline = protected_files(&root, &task).unwrap();
        assert_eq!(baseline, json!({"unavailable-input.txt":null}));
        std::fs::create_dir_all(root.join("reports/nested")).unwrap();
        std::fs::write(root.join("reports/nested/plan.json"), "authorized report").unwrap();
        assert_eq!(baseline, protected_files(&root, &task).unwrap());
        std::fs::write(root.join("reports/read-only.txt"), "unauthorized sibling").unwrap();
        assert_ne!(baseline, protected_files(&root, &task).unwrap());
        std::fs::remove_file(root.join("reports/read-only.txt")).unwrap();
        std::fs::write(root.join("unavailable-input.txt"), "created outside write scope").unwrap();
        assert_ne!(baseline, protected_files(&root, &task).unwrap());
        std::fs::remove_file(root.join("unavailable-input.txt")).unwrap();
        std::fs::remove_dir_all(root.join("reports")).unwrap();
        std::fs::write(root.join("reports"), "parent is an unauthorized file").unwrap();
        assert_ne!(baseline, protected_files(&root, &task).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_external_inputs_are_also_bound() {
        let parent = std::env::temp_dir().join(format!("mission-external-candidate-{}", rand::random::<u64>()));
        let root = parent.join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let external = parent.join("input.txt");
        std::fs::write(&external, "before").unwrap();
        let freeze = || scoped_candidate_identity(&root, [external.to_str().unwrap()]).unwrap();
        let before = freeze();
        std::fs::write(&external, "after").unwrap();
        assert_ne!(before, freeze());
        let _ = std::fs::remove_dir_all(parent);
    }
}
