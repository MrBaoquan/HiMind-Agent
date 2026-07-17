use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::api::client::TaskCancelGuard;
use crate::api::types::Task;
use crate::{report_task, Options};

pub(crate) fn execute_smb_upload(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    payload: Option<&Value>,
) -> Result<Value, Box<dyn Error>> {
    let payload = payload.ok_or("missing SMB upload payload")?;
    let target_dir = payload
        .get("target_dir")
        .and_then(Value::as_str)
        .ok_or("missing SMB target directory")?;
    if !target_dir.starts_with(r"\\") {
        return Err("SMB target must be a UNC path".into());
    }
    let source_paths = payload
        .get("source_paths")
        .and_then(Value::as_array)
        .ok_or("missing SMB source files")?;
    let relative_paths = payload
        .get("relative_paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if source_paths.is_empty() || source_paths.len() > 50 {
        return Err("SMB upload requires 1 to 50 files".into());
    }

    let selected_sources = source_paths
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or("invalid SMB source file")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut sources = Vec::new();
    for (index, source) in selected_sources.iter().enumerate() {
        if source.is_file() {
            let relative = relative_paths
                .get(index)
                .and_then(Value::as_str)
                .unwrap_or_default();
            sources.push((source.clone(), relative.to_string()));
        } else if source.is_dir() {
            collect_files(source, source, &mut sources)?;
        } else {
            return Err(format!("source path does not exist: {}", source.display()).into());
        }
    }
    if sources.is_empty() || sources.len() > 10_000 {
        return Err("SMB upload requires 1 to 10000 files after folder expansion".into());
    }

    let target = Path::new(target_dir);
    fs::create_dir_all(target)?;
    let mut cancel_guard = TaskCancelGuard::new();
    let mut files = Vec::with_capacity(sources.len());
    for (index, (source, selected_relative_path)) in sources.iter().enumerate() {
        cancel_guard.check(client, options, agent_id, &task.id)?;
        let file_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("source file name is invalid")?;
        let relative_path = (!selected_relative_path.trim().is_empty())
            .then_some(selected_relative_path.as_str())
            .unwrap_or(file_name)
            .replace('/', "\\");
        if relative_path.contains("..") || Path::new(&relative_path).is_absolute() {
            return Err("invalid SMB relative path".into());
        }
        let destination = target.join(&relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_file_name(format!(".{}.{}.uploading", file_name, task.id));
        let size = fs::copy(source, &temporary)?;
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        fs::rename(&temporary, &destination)?;
        files.push(json!({
            "file_name": file_name,
            "relative_path": relative_path.replace('\\', "/"),
            "file_size": size,
        }));
        let progress = 20 + (((index + 1) as i32 * 75) / sources.len() as i32);
        report_task(
            client,
            options,
            agent_id,
            &task.id,
            "running",
            progress,
            &format!("已上传 {}/{}：{}", index + 1, sources.len(), file_name),
            None,
            None,
        )?;
    }

    Ok(json!({
        "stage": "uploaded",
        "category": payload.get("category").cloned().unwrap_or(Value::Null),
        "target_dir": target_dir,
        "files": files,
    }))
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(PathBuf, String)>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            let root_name = root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("folder");
            let nested = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((path, format!("{root_name}/{nested}")));
        }
        if files.len() > 10_000 {
            return Err("selected folder contains more than 10000 files".into());
        }
    }
    Ok(())
}
