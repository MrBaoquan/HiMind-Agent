use reqwest::blocking::{multipart, Client};
use serde_json::{json, Value};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::remote::client::{
    extract_input_value, extract_meta_content, inner_admin_base, inner_admin_client,
    inner_admin_login, is_login_page, truncate_text,
};

pub(crate) fn upload_inner_admin_package<F>(
    pid: &str,
    zip_path: &Path,
    allow_env_fallback: bool,
    mut progress: F,
) -> Result<Value, Box<dyn Error>>
where
    F: FnMut(u64, u64, u64, u64) -> Result<(), Box<dyn Error>>,
{
    let base = inner_admin_base();
    let client = inner_admin_client()?;
    inner_admin_login(&client, &base, allow_env_fallback)?;
    let csrf_token = inner_admin_csrf_token(&client, &base, pid)?;
    let file_name = zip_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("invalid zip file name")?
        .to_string();
    let file_size = fs::metadata(zip_path)?.len();
    let preprocess_url = format!("{}/admin/api/upload_preprocess", base);
    let preprocess: Value = client
        .post(&preprocess_url)
        .header("X-CSRF-TOKEN", csrf_token.clone())
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Accept", "application/json, text/javascript, */*; q=0.01")
        .form(&vec![
            ("file_name".to_string(), file_name.clone()),
            ("file_size".to_string(), file_size.to_string()),
            ("params[p_id]".to_string(), pid.to_string()),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("inner admin preprocess request failed: {error}"))?
        .json()
        .map_err(|error| format!("inner admin preprocess response is not valid json: {error}"))?;
    if let Some(error) = preprocess.get("error").and_then(Value::as_str) {
        if !error.trim().is_empty() {
            return Err(format!("inner admin preprocess failed: {}", error).into());
        }
    }
    if let Some(saved_path) = preprocess.get("savedPath").and_then(Value::as_str) {
        if !saved_path.is_empty() {
            progress(0, 0, file_size, file_size)?;
            return Ok(json!({"saved_path": saved_path, "instant": true, "chunks": 0}));
        }
    }
    let upload_ext = string_field(&preprocess, "uploadExt")?;
    let upload_basename = string_field(&preprocess, "uploadBaseName")?;
    let sub_dir = string_field(&preprocess, "subDir")?;
    let chunk_size = number_field(&preprocess, "chunkSize")?;
    if chunk_size == 0 {
        return Err("inner admin returned zero chunk size".into());
    }
    let chunk_count = (file_size + chunk_size - 1) / chunk_size;
    let uploading_url = format!("{}/admin/api/uploading", base);
    let mut file = File::open(zip_path)?;
    let mut last_response = Value::Null;
    for chunk_index in 1..=chunk_count {
        let start = (chunk_index - 1) * chunk_size;
        let length = std::cmp::min(chunk_size, file_size - start) as usize;
        let mut buffer = vec![0_u8; length];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer)?;
        let mut response = None;
        let mut last_error = String::new();
        for attempt in 1..=5 {
            let part = multipart::Part::bytes(buffer.clone()).file_name(file_name.clone());
            let form = multipart::Form::new()
                .part("file", part)
                .text("upload_ext", upload_ext.clone())
                .text("chunk_total", chunk_count.to_string())
                .text("chunk_index", chunk_index.to_string())
                .text("upload_basename", upload_basename.clone())
                .text("sub_dir", sub_dir.clone());
            match client
                .post(&uploading_url)
                .header("X-CSRF-TOKEN", csrf_token.clone())
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Accept", "application/json, text/javascript, */*; q=0.01")
                .multipart(form)
                .send()
            {
                Ok(upload_response) => {
                    let status = upload_response.status();
                    match upload_response.text() {
                        Ok(response_body) if status.is_success() => {
                            response = Some(serde_json::from_str::<Value>(&response_body).map_err(|error| format!(
                                "inner admin upload returned invalid json at chunk {} of {}: {}; body: {}",
                                chunk_index,
                                chunk_count,
                                error,
                                truncate_text(&response_body, 300)
                            ))?);
                            break;
                        }
                        Ok(response_body) => {
                            return Err(format!(
                                "inner admin upload failed at chunk {} of {} with status {}: {}",
                                chunk_index,
                                chunk_count,
                                status.as_u16(),
                                truncate_text(&response_body, 300)
                            )
                            .into());
                        }
                        Err(error) => {
                            last_error = format!("inner admin upload response read failed at chunk {} of {} attempt {}: {error}", chunk_index, chunk_count, attempt);
                        }
                    }
                }
                Err(error) => {
                    last_error = format!(
                        "inner admin upload request failed at chunk {} of {} attempt {}: {error}",
                        chunk_index, chunk_count, attempt
                    );
                }
            }
            if attempt < 5 {
                thread::sleep(Duration::from_secs(2));
            }
        }
        let response = response.ok_or(last_error)?;
        if let Some(error) = response.get("error").and_then(Value::as_str) {
            if !error.trim().is_empty() {
                return Err(format!(
                    "inner admin upload failed at chunk {}: {}",
                    chunk_index, error
                )
                .into());
            }
        }
        last_response = response;
        progress(
            chunk_index,
            chunk_count,
            std::cmp::min(chunk_index * chunk_size, file_size),
            file_size,
        )?;
    }
    Ok(json!({
        "saved_path": last_response.get("savedPath").and_then(Value::as_str).unwrap_or(""),
        "instant": false,
        "chunks": chunk_count,
    }))
}

fn inner_admin_csrf_token(
    client: &Client,
    base: &str,
    pid: &str,
) -> Result<String, Box<dyn Error>> {
    let edit_url = format!("{}/admin/software/product/{}/edit", base, pid);
    let html = client.get(&edit_url).send()?.error_for_status()?.text()?;
    if is_login_page(&html) {
        return Err("inner admin session expired before upload".into());
    }
    extract_meta_content(&html, "csrf-token")
        .or_else(|| extract_input_value(&html, "_token"))
        .ok_or_else(|| "missing inner admin csrf token".into())
}

fn string_field(value: &Value, key: &str) -> Result<String, Box<dyn Error>> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing inner admin field {}", key).into())
}

fn number_field(value: &Value, key: &str) -> Result<u64, Box<dyn Error>> {
    if let Some(n) = value.get(key).and_then(Value::as_u64) {
        return Ok(n);
    }
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| format!("missing inner admin field {}", key).into())
}
