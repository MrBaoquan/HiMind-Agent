use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    let conflict_policy = payload
        .get("conflict_policy")
        .and_then(Value::as_str)
        .unwrap_or("skip");
    if conflict_policy != "replace" && conflict_policy != "skip" {
        return Err("SMB conflict_policy must be replace or skip".into());
    }
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

    let source_sizes = sources
        .iter()
        .map(|(source, _)| fs::metadata(source).map(|metadata| metadata.len()))
        .collect::<Result<Vec<_>, _>>()?;
    let total_bytes = source_sizes.iter().copied().sum::<u64>();
    let target = Path::new(target_dir);
    fs::create_dir_all(target)?;
    let mut cancel_guard = TaskCancelGuard::new();
    let mut files = Vec::with_capacity(sources.len());
    let mut skipped = Vec::new();
    let mut transferred_bytes = 0_u64;
    let transfer_started = Instant::now();
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
        let existed = destination.exists();
        if existed && conflict_policy == "skip" {
            transferred_bytes = transferred_bytes.saturating_add(source_sizes[index]);
            skipped.push(json!({
                "file_name": file_name,
                "relative_path": relative_path.replace('\\', "/"),
                "reason": "already_exists",
            }));
            let progress =
                transfer_progress(transferred_bytes, total_bytes, index + 1, sources.len());
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                progress,
                &format!("已跳过 {}/{}：{}", index + 1, sources.len(), file_name),
                None,
                None,
            )?;
            continue;
        }
        let temporary = destination.with_file_name(format!(".{}.{}.uploading", file_name, task.id));
        let file_size = source_sizes[index];
        let file_start_bytes = transferred_bytes;
        let copy_result = copy_with_progress(source, &temporary, |file_bytes| {
            transferred_bytes = file_start_bytes.saturating_add(file_bytes);
            cancel_guard.check(client, options, agent_id, &task.id)?;
            let elapsed = transfer_started.elapsed();
            let speed = transfer_speed(transferred_bytes, elapsed);
            let progress =
                transfer_progress(transferred_bytes, total_bytes, index + 1, sources.len());
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                progress,
                &transfer_detail(
                    index + 1,
                    sources.len(),
                    file_name,
                    transferred_bytes,
                    total_bytes,
                    speed,
                ),
                None,
                None,
            )
        });
        let size = match copy_result {
            Ok(size) => size,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        transferred_bytes = file_start_bytes.saturating_add(file_size);
        if let Err(error) = commit_temporary_file(&temporary, &destination, &task.id) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        files.push(json!({
            "file_name": file_name,
            "relative_path": relative_path.replace('\\', "/"),
            "file_size": size,
            "action": if existed { "replaced" } else { "new" },
        }));
        let progress = transfer_progress(transferred_bytes, total_bytes, index + 1, sources.len());
        report_task(
            client,
            options,
            agent_id,
            &task.id,
            "running",
            progress,
            &format!(
                "已上传 {}/{}：{} · {}/{}",
                index + 1,
                sources.len(),
                file_name,
                format_bytes(transferred_bytes),
                format_bytes(total_bytes)
            ),
            None,
            None,
        )?;
    }

    Ok(json!({
        "stage": "uploaded",
        "category": payload.get("category").cloned().unwrap_or(Value::Null),
        "target_dir": target_dir,
        "transferred_bytes": transferred_bytes,
        "total_bytes": total_bytes,
        "files": files,
        "skipped": skipped,
    }))
}

fn copy_with_progress<F>(
    source: &Path,
    target: &Path,
    mut progress: F,
) -> Result<u64, Box<dyn Error>>
where
    F: FnMut(u64) -> Result<(), Box<dyn Error>>,
{
    const BUFFER_SIZE: usize = 1024 * 1024;
    let mut reader = fs::File::open(source)?;
    let mut writer = fs::File::create(target)?;
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let mut copied = 0_u64;
    let mut last_report = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        copied = copied.saturating_add(read as u64);
        if last_report.elapsed() >= Duration::from_secs(1) {
            progress(copied)?;
            last_report = Instant::now();
        }
    }
    writer.flush()?;
    progress(copied)?;
    Ok(copied)
}

fn commit_temporary_file(
    temporary: &Path,
    destination: &Path,
    task_id: &str,
) -> Result<(), Box<dyn Error>> {
    if !destination.exists() {
        fs::rename(temporary, destination)?;
        return Ok(());
    }
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("destination file name is invalid")?;
    let backup = destination.with_file_name(format!(".{}.{}.backup", file_name, task_id));
    if backup.exists() {
        fs::remove_file(&backup)?;
    }
    fs::rename(destination, &backup)?;
    if let Err(error) = fs::rename(temporary, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(error.into());
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn transfer_progress(
    bytes: u64,
    total_bytes: u64,
    completed_files: usize,
    total_files: usize,
) -> i32 {
    if total_bytes > 0 {
        return (15 + ((bytes.saturating_mul(80) / total_bytes) as i32)).clamp(15, 95);
    }
    (15 + (((completed_files as u64).saturating_mul(80) / total_files.max(1) as u64) as i32))
        .clamp(15, 95)
}

fn transfer_speed(bytes: u64, elapsed: Duration) -> u64 {
    let millis = elapsed.as_millis().max(1) as u64;
    bytes.saturating_mul(1000) / millis
}

fn transfer_detail(
    current_file: usize,
    total_files: usize,
    file_name: &str,
    bytes: u64,
    total_bytes: u64,
    speed: u64,
) -> String {
    let remaining = total_bytes.saturating_sub(bytes);
    let eta = if speed > 0 {
        format_duration(remaining / speed)
    } else {
        "计算中".to_string()
    };
    format!(
        "正在传输 {current_file}/{total_files}：{file_name} · {}/{} · {}/s · 剩余 {eta}",
        format_bytes(bytes),
        format_bytes(total_bytes),
        format_bytes(speed)
    )
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes / KB)
    } else {
        format!("{} B", bytes as u64)
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} 秒")
    } else if seconds < 3600 {
        format!("{} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!("{} 小时 {} 分", seconds / 3600, (seconds % 3600) / 60)
    }
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

#[cfg(test)]
mod tests {
    use super::{commit_temporary_file, transfer_detail, transfer_progress};
    use std::fs;

    #[test]
    fn reports_byte_weighted_progress() {
        assert_eq!(transfer_progress(0, 1000, 0, 1), 15);
        assert_eq!(transfer_progress(500, 1000, 0, 1), 55);
        assert_eq!(transfer_progress(1000, 1000, 1, 1), 95);
    }

    #[test]
    fn transfer_detail_contains_actionable_metrics() {
        let detail = transfer_detail(
            1,
            2,
            "video.mp4",
            5 * 1024 * 1024,
            10 * 1024 * 1024,
            1024 * 1024,
        );
        assert!(detail.contains("1/2"));
        assert!(detail.contains("5.0 MB/10.0 MB"));
        assert!(detail.contains("1.0 MB/s"));
        assert!(detail.contains("剩余 5 秒"));
    }

    #[test]
    fn replacing_a_file_keeps_the_new_content() {
        let root = std::env::temp_dir().join(format!("himind-smb-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("asset.bin");
        let temporary = root.join(".asset.bin.task.uploading");
        fs::write(&destination, b"old").unwrap();
        fs::write(&temporary, b"new").unwrap();
        commit_temporary_file(&temporary, &destination, "task").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert!(!root.join(".asset.bin.task.backup").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
