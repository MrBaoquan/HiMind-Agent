use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::upload::packaging::sanitize_file_name;

use super::types::{PathCandidate, ScanCacheEntry, ScanTarget};

pub(crate) fn execute_scan(payload: Option<&Value>) -> Result<Value, Box<dyn Error>> {
    let source_roots = read_roots(payload, "source_roots", &["F:\\U3DProjects"]);
    let release_roots = read_roots(payload, "release_roots", &["F:\\Project Released Files"]);
    let targets = read_scan_targets(payload);

    let mut candidates = Vec::new();
    for root in source_roots {
        candidates.extend(scan_root("source", Path::new(&root), 3, &targets)?);
    }
    for root in release_roots {
        candidates.extend(scan_root("release", Path::new(&root), 3, &targets)?);
    }

    Ok(json!({
        "candidate_count": candidates.len(),
        "target_count": targets.len(),
        "candidates": candidates,
    }))
}

fn read_roots(payload: Option<&Value>, key: &str, fallback: &[&str]) -> Vec<String> {
    payload
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| fallback.iter().map(|item| item.to_string()).collect())
}

fn read_scan_targets(payload: Option<&Value>) -> Vec<ScanTarget> {
    payload
        .and_then(|value| value.get("scan_targets"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<ScanTarget>(item.clone()).ok())
                .filter(|target| {
                    !target.exhibit_name.trim().is_empty() || !target.project_name.trim().is_empty()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn scan_root(
    root_type: &str,
    root: &Path,
    max_depth: usize,
    targets: &[ScanTarget],
) -> Result<Vec<PathCandidate>, Box<dyn Error>> {
    let mut candidates = Vec::new();
    if !root.exists() {
        return Ok(candidates);
    }
    let target_key = scan_target_key(targets);
    let root_mtime = path_mtime_seconds(root).unwrap_or(0);
    if let Some(cached) = load_scan_cache(root_type, root, max_depth, &target_key, root_mtime) {
        return Ok(cached);
    }
    scan_dir(
        root_type,
        root,
        0,
        max_depth,
        targets,
        targets.is_empty(),
        &mut candidates,
    )?;
    let _ = save_scan_cache(
        root_type,
        root,
        max_depth,
        &target_key,
        root_mtime,
        &candidates,
    );
    Ok(candidates)
}

fn scan_target_key(targets: &[ScanTarget]) -> String {
    let mut parts: Vec<String> = targets
        .iter()
        .map(|target| {
            format!(
                "{}:{}",
                normalize_scan_text(&target.project_name),
                normalize_scan_text(&target.exhibit_name)
            )
        })
        .collect();
    parts.sort();
    parts.join("|")
}

fn scan_cache_path(root_type: &str, root: &Path, max_depth: usize, target_key: &str) -> PathBuf {
    let cache_dir = env::var("HIMIND_SCAN_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::temp_dir().join("himind-agent-cache"));
    let key = sanitize_file_name(&format!(
        "{}-{}-{}-{}",
        root_type,
        root.display(),
        max_depth,
        target_key
    ));
    cache_dir.join(format!("{}.json", key))
}

fn load_scan_cache(
    root_type: &str,
    root: &Path,
    max_depth: usize,
    target_key: &str,
    root_mtime: u64,
) -> Option<Vec<PathCandidate>> {
    let path = scan_cache_path(root_type, root, max_depth, target_key);
    let data = fs::read_to_string(path).ok()?;
    let entry = serde_json::from_str::<ScanCacheEntry>(&data).ok()?;
    if entry.root_type == root_type
        && entry.root == root.display().to_string()
        && entry.max_depth == max_depth
        && entry.target_key == target_key
        && entry.root_mtime == root_mtime
    {
        Some(entry.candidates)
    } else {
        None
    }
}

fn save_scan_cache(
    root_type: &str,
    root: &Path,
    max_depth: usize,
    target_key: &str,
    root_mtime: u64,
    candidates: &[PathCandidate],
) -> Result<(), Box<dyn Error>> {
    let path = scan_cache_path(root_type, root, max_depth, target_key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = ScanCacheEntry {
        root_type: root_type.to_string(),
        root: root.display().to_string(),
        max_depth,
        target_key: target_key.to_string(),
        root_mtime,
        candidates: candidates.to_vec(),
    };
    fs::write(path, serde_json::to_vec_pretty(&entry)?)?;
    Ok(())
}

fn path_mtime_seconds(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn scan_dir(
    root_type: &str,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    targets: &[ScanTarget],
    in_target_branch: bool,
    candidates: &mut Vec<PathCandidate>,
) -> Result<(), Box<dyn Error>> {
    let target_match = targets.is_empty() || in_target_branch || path_matches_targets(dir, targets);
    if depth > 0 {
        let name = dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_string();
        let engine_type = detect_engine(dir);
        let has_source_hint = has_source_hint(dir);
        let has_release_hint = has_release_hint(dir);
        if has_source_hint || has_release_hint || target_match || (targets.is_empty() && depth <= 2)
        {
            candidates.push(PathCandidate {
                root_type: root_type.to_string(),
                path: dir.display().to_string(),
                name,
                engine_type,
                has_source_hint,
                has_release_hint,
            });
        }
    }

    if depth >= max_depth {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && !is_ignored_dir(&path) {
            let child_matches_target = path_name_matches_targets(&path, targets);
            let should_enter_child = targets.is_empty() || in_target_branch || child_matches_target;
            if !should_enter_child {
                continue;
            }
            let child_in_target_branch = targets.is_empty()
                || in_target_branch
                || path_name_matches_target_exhibit(&path, targets);
            scan_dir(
                root_type,
                &path,
                depth + 1,
                max_depth,
                targets,
                child_in_target_branch,
                candidates,
            )?;
        }
    }

    Ok(())
}

fn path_matches_targets(path: &Path, targets: &[ScanTarget]) -> bool {
    let text = normalize_scan_text(&path.display().to_string());
    targets.iter().any(|target| {
        let exhibit = normalize_scan_text(&target.exhibit_name);
        let project = normalize_scan_text(&target.project_name);
        (!exhibit.is_empty() && text.contains(&exhibit))
            || (!project.is_empty() && text.contains(&project))
    })
}

fn path_name_matches_targets(path: &Path, targets: &[ScanTarget]) -> bool {
    let text = normalize_scan_text(
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(""),
    );
    targets.iter().any(|target| {
        let exhibit = normalize_scan_text(&target.exhibit_name);
        let project = normalize_scan_text(&target.project_name);
        (!exhibit.is_empty() && text.contains(&exhibit))
            || (!project.is_empty() && text.contains(&project))
    })
}

fn path_name_matches_target_exhibit(path: &Path, targets: &[ScanTarget]) -> bool {
    let text = normalize_scan_text(
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(""),
    );
    targets.iter().any(|target| {
        let exhibit = normalize_scan_text(&target.exhibit_name);
        !exhibit.is_empty() && text.contains(&exhibit)
    })
}

pub(crate) fn normalize_scan_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

pub(crate) fn detect_engine(dir: &Path) -> String {
    if dir.join("Assets").is_dir() && dir.join("ProjectSettings").is_dir() {
        return "Unity".to_string();
    }
    if contains_extension(dir, "uproject")
        || (dir.join("Content").is_dir() && dir.join("Config").is_dir())
    {
        return "Unreal".to_string();
    }
    "Unknown".to_string()
}

fn has_source_hint(dir: &Path) -> bool {
    dir.join("Assets").is_dir()
        || dir.join("ProjectSettings").is_dir()
        || dir.join("Source").is_dir()
        || contains_extension(dir, "uproject")
}

fn has_release_hint(dir: &Path) -> bool {
    contains_extension(dir, "exe") || dir.join("Windows").is_dir() || dir.join("Binaries").is_dir()
}

fn contains_extension(dir: &Path, extension: &str) -> bool {
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(extension))
                .unwrap_or(false)
        })
}

fn is_ignored_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".git"
            | ".svn"
            | ".vs"
            | "library"
            | "temp"
            | "intermediate"
            | "saved"
            | "deriveddatacache"
            | "node_modules"
    )
}
