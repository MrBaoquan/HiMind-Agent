use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::api::client::TaskCancelGuard;
use crate::api::types::Task;
use crate::scan::service::detect_engine;
use crate::{report_task, Options};

use super::packaging::{collect_package_snapshot, sanitize_file_name, zip_directories, ZipStats};
use super::uploader::upload_inner_admin_package;

pub(crate) fn execute_upload_code(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    payload: Option<&Value>,
) -> Result<Value, Box<dyn Error>> {
    let pid = payload
        .and_then(|value| value.get("pid"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let exhibit_name = payload
        .and_then(|value| value.get("exhibit_name"))
        .and_then(|value| value.as_str())
        .unwrap_or("exhibit");
    let package_type = payload
        .and_then(|value| value.get("package_type"))
        .and_then(|value| value.as_str())
        .unwrap_or("source");
    let source_path = payload
        .and_then(|value| value.get("source_path"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let release_path = payload
        .and_then(|value| value.get("release_path"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let input_path = match package_type {
        "release" => {
            if release_path.trim().is_empty() {
                return Err("release package requires release_path".into());
            }
            release_path
        }
        _ => {
            if source_path.trim().is_empty() {
                return Err("source package requires source_path".into());
            }
            source_path
        }
    };

    if input_path.trim().is_empty() {
        return Err("missing source_path or release_path".into());
    }
    let inputs = parse_input_paths(input_path);
    if inputs.is_empty() {
        return Err("missing source_path or release_path".into());
    }
    for input in &inputs {
        if !input.exists() || !input.is_dir() {
            return Err(format!(
                "input path does not exist or is not a directory: {}",
                input.display()
            )
            .into());
        }
    }

    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        25,
        "检查待打包目录",
        None,
        None,
    )?;
    let mut cancel_guard = TaskCancelGuard::new();
    cancel_guard.check(client, options, agent_id, &task.id)?;
    let engine_type = inputs
        .iter()
        .map(|input| detect_engine(input))
        .find(|value| value != "Generic")
        .unwrap_or_else(|| "Generic".to_string());
    let output_dir = env::temp_dir().join("project-dashboard-packages");
    let cache_dir = env::temp_dir().join("project-dashboard-package-cache");
    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&cache_dir)?;
    let safe_name = sanitize_file_name(exhibit_name);
    let zip_path = output_dir.join(format!("{}-{}-{}.zip", pid, safe_name, package_type));
    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        32,
        "检查本地压缩缓存",
        None,
        None,
    )?;
    let snapshot = collect_package_snapshot(&inputs, &engine_type, package_type)?;
    let cache_path = cache_dir.join(format!("{}-{}.zip", snapshot.cache_key, package_type));
    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        40,
        "压缩发布包",
        None,
        None,
    )?;
    let (stats, cache_reused) = if cache_path.exists() {
        fs::copy(&cache_path, &zip_path)?;
        report_task(
            client,
            options,
            agent_id,
            &task.id,
            "running",
            69,
            &format!(
                "复用本地缓存压缩包：{} 个文件，{:.1} MB，跳过 {} 个",
                snapshot.included_files,
                snapshot.included_bytes as f64 / 1024.0 / 1024.0,
                snapshot.excluded_files
            ),
            None,
            None,
        )?;
        (
            ZipStats {
                included_files: snapshot.included_files,
                excluded_files: snapshot.excluded_files,
                included_bytes: snapshot.included_bytes,
            },
            true,
        )
    } else {
        let mut last_zip_report = Instant::now();
        let stats = zip_directories(
            &inputs,
            &zip_path,
            &engine_type,
            package_type,
            |stats, current| {
                cancel_guard.check(client, options, agent_id, &task.id)?;
                if stats.included_files == 1
                    || stats.included_files % 50 == 0
                    || last_zip_report.elapsed() >= Duration::from_secs(2)
                {
                    last_zip_report = Instant::now();
                    let mb = stats.included_bytes as f64 / 1024.0 / 1024.0;
                    let progress = std::cmp::min(69, 40 + (stats.included_files as i32 / 50));
                    report_task(
                        client,
                        options,
                        agent_id,
                        &task.id,
                        "running",
                        progress,
                        &format!(
                            "压缩中：已写入 {} 个文件，{:.1} MB，跳过 {} 个，当前 {}",
                            stats.included_files, mb, stats.excluded_files, current
                        ),
                        None,
                        None,
                    )?;
                }
                Ok(())
            },
        )?;
        fs::copy(&zip_path, &cache_path)?;
        (stats, false)
    };
    cancel_guard.check(client, options, agent_id, &task.id)?;
    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        70,
        "上传到内网管理系统",
        None,
        None,
    )?;
    let upload = upload_inner_admin_package(
        pid,
        &zip_path,
        !options.local_app,
        |chunk_index, chunk_count, uploaded_bytes, total_bytes| {
            cancel_guard.check(client, options, agent_id, &task.id)?;
            let progress = if chunk_count == 0 {
                95
            } else {
                70 + ((chunk_index as i32 * 25) / chunk_count as i32)
            };
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                std::cmp::min(95, progress),
                &format!(
                    "上传分片 {}/{}，{:.1}/{:.1} MB",
                    chunk_index,
                    chunk_count,
                    uploaded_bytes as f64 / 1024.0 / 1024.0,
                    total_bytes as f64 / 1024.0 / 1024.0
                ),
                None,
                None,
            )
        },
    )?;

    Ok(json!({
        "pid": pid,
        "exhibit_name": exhibit_name,
        "package_type": package_type,
        "stage": "uploaded",
        "engine_type": engine_type,
        "input_path": inputs.iter().map(|item| item.display().to_string()).collect::<Vec<_>>().join("\n"),
        "zip_path": zip_path.display().to_string(),
        "cache_path": cache_path.display().to_string(),
        "cache_key": snapshot.cache_key,
        "cache_reused": cache_reused,
        "zip_size": fs::metadata(&zip_path)?.len(),
        "included_files": stats.included_files,
        "excluded_files": stats.excluded_files,
        "upload": upload,
        "uploaded_at": chrono_like_now(),
        "message": "package created and uploaded to inner admin"
    }))
}

pub(crate) fn execute_upload_placeholder(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    payload: Option<&Value>,
) -> Result<Value, Box<dyn Error>> {
    let pid = payload
        .and_then(|value| value.get("pid"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let exhibit_name = payload
        .and_then(|value| value.get("exhibit_name"))
        .and_then(|value| value.as_str())
        .unwrap_or("exhibit");
    let file_name = payload
        .and_then(|value| value.get("file_name"))
        .and_then(|value| value.as_str())
        .unwrap_or("代码上传占位.txt");
    let content = payload
        .and_then(|value| value.get("content"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        return Err("placeholder content is empty".into());
    }
    let mut cancel_guard = TaskCancelGuard::new();
    cancel_guard.check(client, options, agent_id, &task.id)?;

    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        35,
        "生成占位说明文件",
        None,
        None,
    )?;
    let output_dir = env::temp_dir().join("project-dashboard-packages");
    fs::create_dir_all(&output_dir)?;
    let safe_exhibit = sanitize_file_name(exhibit_name);
    let mut safe_file_name = sanitize_file_name(file_name);
    if safe_file_name.trim().is_empty() {
        safe_file_name = "代码上传占位.txt".to_string();
    }
    let placeholder_path = output_dir.join(format!("{}-{}-{}", pid, safe_exhibit, safe_file_name));
    fs::write(&placeholder_path, content.as_bytes())?;

    report_task(
        client,
        options,
        agent_id,
        &task.id,
        "running",
        70,
        "上传占位说明文件到内网管理系统",
        None,
        None,
    )?;
    let upload = upload_inner_admin_package(
        pid,
        &placeholder_path,
        !options.local_app,
        |chunk_index, chunk_count, uploaded_bytes, total_bytes| {
            cancel_guard.check(client, options, agent_id, &task.id)?;
            let progress = if chunk_count == 0 {
                95
            } else {
                70 + ((chunk_index as i32 * 25) / chunk_count as i32)
            };
            report_task(
                client,
                options,
                agent_id,
                &task.id,
                "running",
                std::cmp::min(95, progress),
                &format!(
                    "上传分片 {}/{}，{:.1}/{:.1} KB",
                    chunk_index,
                    chunk_count,
                    uploaded_bytes as f64 / 1024.0,
                    total_bytes as f64 / 1024.0
                ),
                None,
                None,
            )
        },
    )?;

    Ok(json!({
        "pid": pid,
        "exhibit_name": exhibit_name,
        "stage": "placeholder_uploaded",
        "file_name": safe_file_name,
        "placeholder_path": placeholder_path.display().to_string(),
        "file_size": fs::metadata(&placeholder_path)?.len(),
        "upload": upload,
        "uploaded_at": chrono_like_now(),
        "message": "placeholder file uploaded to inner admin"
    }))
}

fn parse_input_paths(value: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for item in value.split(|ch| ch == '\n' || ch == '\r') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let path = PathBuf::from(item);
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn chrono_like_now() -> String {
    format!("{:?}", std::time::SystemTime::now())
}
