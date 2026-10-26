//! 本机异步媒体能力（生视频）。
//!
//! 火山方舟 Seedance：`POST /api/v3/contents/generations/tasks` 提交，
//! `GET /api/v3/contents/generations/tasks/{id}` 查询，`DELETE .../{id}` 取消，
//! 成功后从 `video_url` 下载落盘。视频是分钟级任务，因此提交只返回 `job_id`，
//! 状态与产物由本机媒体任务表（`store::media_jobs`）维护。

use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::local_image::{data_url, resolve_input_image};
use crate::store::ai_services::CAP_VIDEO_GENERATE;
use crate::store::media_jobs::{self, MediaJob, MediaJobOutput};

pub(crate) const ADAPTER_ARK_VIDEO: &str = "ark-video";
pub(crate) const ADAPTER_WAN_VIDEO: &str = "wan-video";
const MAX_VIDEO_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_DOWNLOAD_TIMEOUT_SECONDS: u64 = 900;
const DEFAULT_WAIT_SECONDS: u64 = 300;
const MAX_WAIT_SECONDS: u64 = 900;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// `media.video.generate`：提交文生视频 / 图生视频任务（可选参考图）。
pub(crate) fn generate(input: &Value) -> Result<Value, Box<dyn Error>> {
    let prompt = string_field(input, "prompt").ok_or("prompt 不能为空")?;
    let requested_service = string_field(input, "service").unwrap_or_default();
    let (service, api_key) =
        crate::store::ai_services::resolve_media_service(CAP_VIDEO_GENERATE, &requested_service)?;
    let binding = crate::store::ai_services::binding_for(&service, CAP_VIDEO_GENERATE);
    let requested_model = string_field(input, "model");
    let (adapter, model) = match (binding, requested_model) {
        (Some((adapter, model)), Some(override_model)) => (adapter, override_model),
        (Some(binding), None) => binding,
        (None, Some(override_model)) => (ADAPTER_ARK_VIDEO.to_string(), override_model),
        (None, None) => {
            return Err(format!(
                "服务「{}」尚未配置「{CAP_VIDEO_GENERATE}」能力；请填写生视频模型后重试",
                service.display_name
            )
            .into())
        }
    };
    if adapter != ADAPTER_ARK_VIDEO && adapter != ADAPTER_WAN_VIDEO {
        return Err(format!(
            "适配器 {adapter} 暂不支持生视频；可选 {ADAPTER_ARK_VIDEO}、{ADAPTER_WAN_VIDEO}"
        )
        .into());
    }
    if api_key.trim().is_empty() {
        return Err("所选本机 AI 服务没有可用的 API Key".into());
    }

    let output_dir = resolve_output_dir(string_field(input, "output_dir"))?;
    let mut job = media_jobs::create(
        CAP_VIDEO_GENERATE,
        &service.id,
        &provider_label(&service.base_url),
        &adapter,
        &model,
        &prompt,
        &output_dir.to_string_lossy(),
    )?;

    match submit_task(
        &service.base_url,
        &api_key,
        &adapter,
        &model,
        input,
        &prompt,
    ) {
        Ok(submitted) => {
            job.provider_job_id = submitted.provider_job_id;
            job.provider_request_id = submitted.provider_request_id;
            job.status = media_jobs::STATUS_QUEUED.to_string();
            job.progress = 5;
            job.updated_at = now();
            media_jobs::save(&job)?;
            let mut value = job.to_json();
            value["hint"] =
                json!("视频为异步任务：稍后用 media.job.get 查询，或用 media.job.wait 等待完成。");
            Ok(value)
        }
        Err(error) => {
            job.status = media_jobs::STATUS_FAILED.to_string();
            job.error = error.to_string();
            job.updated_at = now();
            let _ = media_jobs::save(&job);
            Err(error)
        }
    }
}

/// `media.job.get`：拉取一次 provider 状态并返回（终态任务直接返回本地记录）。
pub(crate) fn job_get(input: &Value) -> Result<Value, Box<dyn Error>> {
    let id = string_field(input, "job_id").ok_or("job_id is required")?;
    let job = media_jobs::get(&id)?.ok_or_else(|| format!("媒体任务不存在：{id}"))?;
    Ok(refresh(job)?.to_json())
}

/// `media.job.wait`：轮询到终态或超时。
pub(crate) fn job_wait(input: &Value) -> Result<Value, Box<dyn Error>> {
    let id = string_field(input, "job_id").ok_or("job_id is required")?;
    let timeout = input
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|value| (1..=MAX_WAIT_SECONDS).contains(value))
        .unwrap_or(DEFAULT_WAIT_SECONDS);
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        let job = media_jobs::get(&id)?.ok_or_else(|| format!("媒体任务不存在：{id}"))?;
        let job = refresh(job)?;
        if job.terminal() {
            return Ok(job.to_json());
        }
        if std::time::Instant::now() >= deadline {
            let mut value = job.to_json();
            value["timeout"] = json!(true);
            value["hint"] = json!("等待超时，任务仍在进行；可继续用 media.job.get 查询。");
            return Ok(value);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// `media.job.cancel`：请求 provider 取消并把本地任务置为已取消。
pub(crate) fn job_cancel(input: &Value) -> Result<Value, Box<dyn Error>> {
    let id = string_field(input, "job_id").ok_or("job_id is required")?;
    let mut job = media_jobs::get(&id)?.ok_or_else(|| format!("媒体任务不存在：{id}"))?;
    if job.terminal() {
        return Ok(job.to_json());
    }
    let (service, api_key) = crate::store::ai_services::load_secret(&job.service_id)?;
    if !job.provider_job_id.trim().is_empty() {
        cancel_task(
            &service.base_url,
            &api_key,
            &job.adapter,
            &job.provider_job_id,
        )?;
    }
    job.status = media_jobs::STATUS_CANCELED.to_string();
    job.updated_at = now();
    media_jobs::save(&job)?;
    Ok(job.to_json())
}

/// 按需拉取 provider 状态；成功时下载视频并登记 `media_id`。
fn refresh(mut job: MediaJob) -> Result<MediaJob, Box<dyn Error>> {
    if job.terminal() || job.provider_job_id.trim().is_empty() {
        return Ok(job);
    }
    let (service, api_key) = crate::store::ai_services::load_secret(&job.service_id)?;
    let status = fetch_status(
        &service.base_url,
        &api_key,
        &job.adapter,
        &job.provider_job_id,
    )?;
    job.updated_at = now();
    match status.state.as_str() {
        "queued" => {
            job.status = media_jobs::STATUS_QUEUED.to_string();
            job.progress = job.progress.max(5);
        }
        "running" => {
            job.status = media_jobs::STATUS_RUNNING.to_string();
            job.progress = status.progress.unwrap_or(job.progress.max(30));
        }
        "succeeded" => {
            job.progress = 100;
            match status.video_url.as_deref() {
                Some(url) if !url.trim().is_empty() => {
                    let output = download_video(url, &job)?;
                    job.outputs = vec![output];
                    job.status = media_jobs::STATUS_SUCCEEDED.to_string();
                }
                _ => {
                    job.status = media_jobs::STATUS_FAILED.to_string();
                    job.error = "provider 报告成功但没有返回视频地址".to_string();
                }
            }
        }
        "canceled" => job.status = media_jobs::STATUS_CANCELED.to_string(),
        "failed" => {
            job.status = media_jobs::STATUS_FAILED.to_string();
            job.error = status
                .error
                .unwrap_or_else(|| "provider 报告生成失败".to_string());
        }
        other => {
            job.status = media_jobs::STATUS_RUNNING.to_string();
            job.error = format!("未知的 provider 状态：{other}");
        }
    }
    media_jobs::save(&job)?;
    Ok(job)
}

struct ProviderStatus {
    state: String,
    progress: Option<u8>,
    video_url: Option<String>,
    error: Option<String>,
}

struct SubmittedTask {
    provider_job_id: String,
    provider_request_id: String,
}

fn submit_task(
    base_url: &str,
    api_key: &str,
    adapter: &str,
    model: &str,
    input: &Value,
    prompt: &str,
) -> Result<SubmittedTask, Box<dyn Error>> {
    match adapter {
        ADAPTER_WAN_VIDEO => submit_wan_task(base_url, api_key, model, input, prompt),
        _ => submit_ark_task(base_url, api_key, model, input, prompt),
    }
}

fn submit_ark_task(
    base_url: &str,
    api_key: &str,
    model: &str,
    input: &Value,
    prompt: &str,
) -> Result<SubmittedTask, Box<dyn Error>> {
    let mut body = input
        .get("parameters")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    body.insert("model".to_string(), json!(model));
    let mut content = vec![json!({ "type": "text", "text": prompt })];
    if let Some(images) = input.get("input_images").and_then(Value::as_array) {
        for item in images {
            let reference = item.as_str().ok_or("input_images 只能是字符串数组")?;
            let image = resolve_input_image(reference)?;
            content.push(json!({
                "type": "image_url",
                "image_url": { "url": data_url(&image) },
            }));
        }
    }
    body.insert("content".to_string(), json!(content));
    for (key, slot) in [
        ("duration", "duration_seconds"),
        ("ratio", "ratio"),
        ("resolution", "resolution"),
    ] {
        if body.contains_key(key) {
            continue;
        }
        if let Some(value) = input.get(slot) {
            body.insert(key.to_string(), value.clone());
        }
    }
    body.entry("watermark".to_string())
        .or_insert_with(|| json!(false));

    let client = client_for(base_url, request_timeout())?;
    let response = client
        .post(tasks_url(base_url)?)
        .bearer_auth(api_key.trim())
        .json(&Value::Object(body))
        .send()
        .map_err(|error| format!("提交生视频任务失败：{error}"))?;
    let status = response.status();
    let payload = read_json(response)?;
    if !status.is_success() {
        return Err(format!("生视频接口返回 {status}：{}", describe_error(&payload)).into());
    }
    let provider_job_id = payload
        .get("id")
        .or_else(|| payload.get("job_id"))
        .or_else(|| payload.get("task_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or("生视频响应缺少任务 ID")?;
    Ok(SubmittedTask {
        provider_job_id,
        provider_request_id: payload
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn submit_wan_task(
    base_url: &str,
    api_key: &str,
    model: &str,
    input: &Value,
    prompt: &str,
) -> Result<SubmittedTask, Box<dyn Error>> {
    let images = input
        .get("input_images")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if images.len() > 1 {
        return Err("万相生视频当前只接受 1 张首帧参考图".into());
    }
    let lower_model = model.to_ascii_lowercase();
    if images.is_empty() && lower_model.contains("i2v") {
        return Err("所选万相图生视频模型需要 1 张 input_images 首帧图".into());
    }
    if !images.is_empty() && lower_model.contains("t2v") {
        return Err("所选万相文生视频模型不接受 input_images；请改用 i2v 模型".into());
    }
    let mut input_body = json!({"prompt": prompt});
    if let Some(negative) = string_field(input, "negative_prompt") {
        input_body["negative_prompt"] = json!(negative);
    }
    if let Some(reference) = images.first().and_then(Value::as_str) {
        let image = resolve_input_image(reference)?;
        input_body["img_url"] = json!(data_url(&image));
    }
    let mut parameters = input
        .get("parameters")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, slot) in [
        ("duration", "duration_seconds"),
        ("ratio", "ratio"),
        ("resolution", "resolution"),
        ("prompt_extend", "prompt_extend"),
    ] {
        if parameters.contains_key(key) {
            continue;
        }
        if let Some(value) = input.get(slot) {
            parameters.insert(key.to_string(), value.clone());
        }
    }
    parameters
        .entry("watermark".to_string())
        .or_insert_with(|| json!(false));
    let body = json!({
        "model": model,
        "input": input_body,
        "parameters": parameters,
    });
    let client = client_for(base_url, request_timeout())?;
    let response = client
        .post(wan_synthesis_url(base_url)?)
        .bearer_auth(api_key.trim())
        .header("X-DashScope-Async", "enable")
        .json(&body)
        .send()
        .map_err(|error| format!("提交万相视频任务失败：{error}"))?;
    let status = response.status();
    let payload = read_json(response)?;
    if !status.is_success() {
        return Err(format!("万相视频接口返回 {status}：{}", describe_error(&payload)).into());
    }
    let provider_job_id = payload
        .get("output")
        .and_then(|output| output.get("task_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or("万相视频响应缺少 output.task_id")?;
    Ok(SubmittedTask {
        provider_job_id,
        provider_request_id: payload
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn fetch_status(
    base_url: &str,
    api_key: &str,
    adapter: &str,
    provider_job_id: &str,
) -> Result<ProviderStatus, Box<dyn Error>> {
    match adapter {
        ADAPTER_WAN_VIDEO => fetch_wan_status(base_url, api_key, provider_job_id),
        _ => fetch_ark_status(base_url, api_key, provider_job_id),
    }
}

fn fetch_ark_status(
    base_url: &str,
    api_key: &str,
    provider_job_id: &str,
) -> Result<ProviderStatus, Box<dyn Error>> {
    let client = client_for(base_url, request_timeout())?;
    let url = format!("{}/{}", tasks_url(base_url)?, provider_job_id);
    let response = client
        .get(&url)
        .bearer_auth(api_key.trim())
        .send()
        .map_err(|error| format!("查询生视频任务失败：{error}"))?;
    let status = response.status();
    let payload = read_json(response)?;
    if !status.is_success() {
        return Err(format!("查询生视频任务返回 {status}：{}", describe_error(&payload)).into());
    }
    Ok(parse_provider_status(&payload))
}

fn fetch_wan_status(
    base_url: &str,
    api_key: &str,
    provider_job_id: &str,
) -> Result<ProviderStatus, Box<dyn Error>> {
    let client = client_for(base_url, request_timeout())?;
    let response = client
        .get(wan_task_url(base_url, provider_job_id)?)
        .bearer_auth(api_key.trim())
        .send()
        .map_err(|error| format!("查询万相视频任务失败：{error}"))?;
    let status = response.status();
    let payload = read_json(response)?;
    if !status.is_success() {
        return Err(format!(
            "查询万相视频任务返回 {status}：{}",
            describe_error(&payload)
        )
        .into());
    }
    Ok(parse_provider_status(&payload))
}

fn parse_provider_status(payload: &Value) -> ProviderStatus {
    let output = payload.get("output").unwrap_or(payload);
    let raw_state = output
        .get("task_status")
        .or_else(|| output.get("status"))
        .or_else(|| output.get("state"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let state = match raw_state.as_str() {
        "queued" | "pending" | "created" => "queued",
        "running" | "processing" | "in_progress" => "running",
        "succeeded" | "completed" | "success" => "succeeded",
        "failed" | "error" => "failed",
        "canceled" | "cancelled" => "canceled",
        other => other,
    }
    .to_string();
    ProviderStatus {
        state,
        progress: output
            .get("progress")
            .or_else(|| payload.get("progress"))
            .and_then(Value::as_u64)
            .map(|value| value.min(100) as u8),
        video_url: find_video_url(payload),
        error: output
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| output.get("code").and_then(Value::as_str))
            .or_else(|| {
                payload.get("error").and_then(|error| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| error.as_str())
                })
            })
            .map(str::to_string),
    }
}

fn cancel_task(
    base_url: &str,
    api_key: &str,
    adapter: &str,
    provider_job_id: &str,
) -> Result<(), Box<dyn Error>> {
    let client = client_for(base_url, request_timeout())?;
    let response = if adapter == ADAPTER_WAN_VIDEO {
        client
            .post(wan_cancel_url(base_url, provider_job_id)?)
            .bearer_auth(api_key.trim())
            .send()
            .map_err(|error| format!("取消万相视频任务失败：{error}"))?
    } else {
        let url = format!("{}/{}", tasks_url(base_url)?, provider_job_id);
        client
            .delete(&url)
            .bearer_auth(api_key.trim())
            .send()
            .map_err(|error| format!("取消生视频任务失败：{error}"))?
    };
    if !response.status().is_success() {
        let status = response.status();
        let payload = read_json(response).unwrap_or(Value::Null);
        return Err(format!("取消生视频任务返回 {status}：{}", describe_error(&payload)).into());
    }
    Ok(())
}

fn wan_api_root(base_url: &str) -> Result<String, Box<dyn Error>> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("AI 服务 base_url 为空".into());
    }
    let parsed = url::Url::parse(base).map_err(|_| "AI 服务 base_url 不是合法 URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("AI 服务 base_url 仅支持 http/https".into());
    }
    let root = base
        .strip_suffix("/compatible-mode/v1")
        .or_else(|| base.strip_suffix("/api/v1"))
        .unwrap_or(base);
    Ok(root.trim_end_matches('/').to_string())
}

fn wan_synthesis_url(base_url: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{}/api/v1/services/aigc/video-generation/video-synthesis",
        wan_api_root(base_url)?
    ))
}

fn wan_task_url(base_url: &str, task_id: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{}/api/v1/tasks/{}",
        wan_api_root(base_url)?,
        task_id.trim()
    ))
}

fn wan_cancel_url(base_url: &str, task_id: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!("{}/cancel", wan_task_url(base_url, task_id)?))
}

/// 从 provider 返回里尽力找出视频地址（不同厂商字段名不一）。
fn find_video_url(payload: &Value) -> Option<String> {
    let candidates = [
        payload
            .get("output")
            .and_then(|value| value.get("video_url")),
        payload
            .get("content")
            .and_then(|value| value.get("video_url")),
        payload.get("video_url"),
        payload.get("output_url"),
        payload
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("video_url")),
        payload
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("url")),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

fn download_video(url: &str, job: &MediaJob) -> Result<MediaJobOutput, Box<dyn Error>> {
    let parsed = url::Url::parse(url).map_err(|_| "provider 返回的视频地址无效")?;
    if parsed.scheme() != "https"
        && !matches!(parsed.host_str(), Some("127.0.0.1") | Some("localhost"))
    {
        return Err("视频地址必须使用 HTTPS（环回地址除外）".into());
    }
    let client = client_for(url, Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECONDS))?;
    let response = client
        .get(parsed)
        .send()
        .map_err(|error| format!("下载生成视频失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载生成视频失败：HTTP {}", response.status()).into());
    }
    let bytes = read_limited(response, MAX_VIDEO_BYTES)?;
    if bytes.len() as u64 > MAX_VIDEO_BYTES {
        return Err("生成视频超过 512 MiB 上限".into());
    }
    let mime = sniff_video_mime(&bytes).to_string();
    let extension = if mime == "video/webm" { "webm" } else { "mp4" };
    let dir = PathBuf::from(&job.output_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建视频输出目录失败：{}（{error}）", dir.display()))?;
    let path = dir.join(format!(
        "{}-0.{}",
        sanitize_stem(&job.capability),
        extension
    ));
    let path = unique_path(path);
    crate::store::atomic_file::atomic_write(&path, &bytes)
        .map_err(|error| format!("视频写入失败：{}（{error}）", path.display()))?;
    let sha256 = hex_sha256(&bytes);
    let record = crate::store::media_registry::register(crate::store::media_registry::NewMedia {
        path: &path.to_string_lossy(),
        kind: "video",
        mime_type: &mime,
        size_bytes: bytes.len() as u64,
        sha256: &sha256,
        width: 0,
        height: 0,
        duration_seconds: 0.0,
        capability: &job.capability,
        service_id: &job.service_id,
        prompt: &job.prompt,
    })?;
    Ok(MediaJobOutput {
        media_id: record.id,
        path: path.to_string_lossy().to_string(),
        mime_type: mime,
        size_bytes: bytes.len() as u64,
        duration_seconds: 0.0,
        width: 0,
        height: 0,
        cover_path: String::new(),
    })
}

fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp4");
    let mut random = [0u8; 4];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    path.with_file_name(format!("{stem}-{suffix}.{extension}"))
}

fn tasks_url(base_url: &str) -> Result<String, Box<dyn Error>> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("AI 服务 base_url 为空".into());
    }
    let parsed = url::Url::parse(base).map_err(|_| "AI 服务 base_url 不是合法 URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("AI 服务 base_url 仅支持 http/https".into());
    }
    let root = if base.ends_with("/api/plan/v3") || base.ends_with("/api/v3") {
        base.to_string()
    } else {
        format!("{base}/api/v3")
    };
    Ok(format!("{root}/contents/generations/tasks"))
}

fn resolve_output_dir(explicit: Option<String>) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(raw) = explicit
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err("output_dir 必须是绝对路径".into());
        }
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("创建 output_dir 失败：{}（{error}）", path.display()))?;
        return Ok(path);
    }
    let base = crate::extension_workspace::session_root()
        .map(|root| root.join(".himind").join("media"))
        .unwrap_or_else(|| crate::store::paths::agent_home().join("media"));
    std::fs::create_dir_all(&base)
        .map_err(|error| format!("创建视频目录失败：{}（{error}）", base.display()))?;
    Ok(base)
}

fn client_for(url: &str, timeout: Duration) -> Result<Client, Box<dyn Error>> {
    let mut builder = Client::builder().timeout(timeout);
    if url::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"))
    {
        builder = builder.no_proxy();
    }
    Ok(builder.build()?)
}

fn request_timeout() -> Duration {
    let seconds = std::env::var("HIMIND_LOCAL_VIDEO_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| (10..=900).contains(value))
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECONDS);
    Duration::from_secs(seconds)
}

fn read_limited(
    response: reqwest::blocking::Response,
    limit: u64,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut buffer = Vec::new();
    response.take(limit + 1).read_to_end(&mut buffer)?;
    if buffer.len() as u64 > limit {
        return Err(format!("响应超过 {limit} 字节上限").into());
    }
    Ok(buffer)
}

fn read_json(response: reqwest::blocking::Response) -> Result<Value, Box<dyn Error>> {
    let bytes = read_limited(response, 8 * 1024 * 1024)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("provider 响应解析失败：{error}").into())
}

fn describe_error(payload: &Value) -> String {
    let message = payload
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .map(str::to_string)
        .unwrap_or_else(|| payload.to_string());
    let mut truncated: String = message.chars().take(500).collect();
    if message.chars().count() > 500 {
        truncated.push('…');
    }
    truncated
}

fn provider_label(base_url: &str) -> String {
    url::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.trim().to_string())
}

fn string_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn sniff_video_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBM" {
        "video/webm"
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        "video/mp4"
    } else {
        "application/octet-stream"
    }
}

fn sanitize_stem(value: &str) -> String {
    let stem: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = stem.trim_matches('-');
    if trimmed.is_empty() {
        "video".to_string()
    } else {
        trimmed.to_string()
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[allow(dead_code)]
fn output_dir_of(job: &MediaJob) -> &Path {
    Path::new(&job.output_dir)
}
#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// 方舟生视频接口的最小假实现：提交 → 状态（成功 + 视频地址）→ 视频下载。
    fn fake_ark_video() -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        let port = address.port();
        thread::spawn(move || {
            for index in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut buffer = [0u8; 8192];
                let mut text = String::new();
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                        Err(_) => break,
                    }
                }
                if index == 0 {
                    *sink.lock().unwrap() = text.clone();
                    let body = "{\"id\":\"cgt-test-1\"}";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                } else if index == 1 {
                    let body = format!(
                        "{{\"status\":\"succeeded\",\"progress\":100,\"content\":{{\"video_url\":\"http://127.0.0.1:{port}/video.mp4\"}}}}"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                } else {
                    let video: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        video.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(video);
                }
                let _ = stream.flush();
            }
        });
        (format!("http://{address}"), captured)
    }

    fn fake_wan_video() -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        let port = address.port();
        thread::spawn(move || {
            for index in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut buffer = [0u8; 16384];
                let mut text = String::new();
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                        Err(_) => break,
                    }
                }
                if index == 0 {
                    *sink.lock().unwrap() = text.clone();
                    let body = "{\"request_id\":\"req-wan-1\",\"output\":{\"task_id\":\"wan-task-1\",\"task_status\":\"PENDING\"}}";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                } else if index == 1 {
                    let body = format!(
                        "{{\"request_id\":\"req-wan-2\",\"output\":{{\"task_id\":\"wan-task-1\",\"task_status\":\"SUCCEEDED\",\"video_url\":\"http://127.0.0.1:{port}/video.mp4\",\"usage\":{{\"duration\":5}}}}}}"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                } else {
                    let video: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        video.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(video);
                }
                let _ = stream.flush();
            }
        });
        (format!("http://{address}"), captured)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "himind-local-video-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn video_job_runs_submit_status_download() {
        let (base_url, captured) = fake_ark_video();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("home");
        let workspace = temp_dir("ws");
        let out_dir = workspace.join("media-out");
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        let previous_ws = std::env::var_os("HIMIND_AI_WORKSPACE");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        std::env::set_var("HIMIND_AI_WORKSPACE", &workspace);

        let result = (|| -> Result<(Value, Value), Box<dyn Error>> {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_VIDEO_GENERATE.to_string(),
                crate::store::ai_services::MediaModelBinding {
                    adapter: ADAPTER_ARK_VIDEO.to_string(),
                    model: "doubao-seedance-test".to_string(),
                    execution_mode: None,
                },
            );
            crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
                id: "ark-video".to_string(),
                display_name: "方舟视频".to_string(),
                base_url: base_url.clone(),
                protocol: crate::store::ai_services::AIServiceProtocol::OpenaiChat,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-video".to_string(),
            })?;
            crate::store::ai_services::set_media_service(CAP_VIDEO_GENERATE, "ark-video")?;

            let submitted = generate(&json!({
                "prompt": "一只柯基在雪地里奔跑",
                "duration_seconds": 5,
                "ratio": "16:9",
                "output_dir": out_dir.to_string_lossy(),
            }))?;
            let job_id = submitted["job_id"].as_str().unwrap().to_string();
            let finished = job_get(&json!({ "job_id": job_id }))?;
            Ok((submitted, finished))
        })();

        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        match previous_ws {
            Some(value) => std::env::set_var("HIMIND_AI_WORKSPACE", value),
            None => std::env::remove_var("HIMIND_AI_WORKSPACE"),
        }

        let (submitted, finished) = result.expect("video pipeline");
        assert_eq!(submitted["status"], "queued");
        assert!(submitted["job_id"]
            .as_str()
            .unwrap()
            .starts_with("media_job_"));
        assert_eq!(finished["status"], "succeeded");
        let output = &finished["outputs"][0];
        assert!(output["media_id"].as_str().unwrap().starts_with("media_"));
        assert!(PathBuf::from(output["path"].as_str().unwrap()).is_file());

        let text = captured.lock().unwrap().clone();
        assert!(
            text.contains("POST /api/v3/contents/generations/tasks"),
            "{text}"
        );
        assert!(text.contains("\"content\""), "{text}");
        assert!(text.contains("一只柯基在雪地里奔跑"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn wan_image_to_video_runs_submit_status_download() {
        let (base_url, captured) = fake_wan_video();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("wan-home");
        let workspace = temp_dir("wan-ws");
        let out_dir = workspace.join("media-out");
        let image_path = workspace.join("first-frame.png");
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(&image_path, png).unwrap();
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        let previous_ws = std::env::var_os("HIMIND_AI_WORKSPACE");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        std::env::set_var("HIMIND_AI_WORKSPACE", &workspace);

        let result = (|| -> Result<(Value, Value), Box<dyn Error>> {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_VIDEO_GENERATE.to_string(),
                crate::store::ai_services::MediaModelBinding {
                    adapter: ADAPTER_WAN_VIDEO.to_string(),
                    model: "wan2.6-i2v-flash".to_string(),
                    execution_mode: None,
                },
            );
            crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
                id: "wan-video".to_string(),
                display_name: "万相视频".to_string(),
                base_url: base_url.clone(),
                protocol: crate::store::ai_services::AIServiceProtocol::OpenaiChat,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-wan".to_string(),
            })?;
            crate::store::ai_services::set_media_service(CAP_VIDEO_GENERATE, "wan-video")?;
            let submitted = generate(&json!({
                "prompt": "镜头缓慢推进",
                "input_images": [image_path.to_string_lossy()],
                "duration_seconds": 5,
                "resolution": "720P",
                "output_dir": out_dir.to_string_lossy(),
            }))?;
            let job_id = submitted["job_id"].as_str().unwrap().to_string();
            let finished = job_get(&json!({ "job_id": job_id }))?;
            Ok((submitted, finished))
        })();

        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        match previous_ws {
            Some(value) => std::env::set_var("HIMIND_AI_WORKSPACE", value),
            None => std::env::remove_var("HIMIND_AI_WORKSPACE"),
        }
        let (submitted, finished) = result.expect("Wan video pipeline");
        assert_eq!(submitted["provider_request_id"], "req-wan-1");
        assert_eq!(finished["provider_request_id"], "req-wan-1");
        assert_eq!(finished["status"], "succeeded");
        assert!(finished["provider_job_id"]
            .as_str()
            .unwrap()
            .contains("wan-task-1"));
        let text = captured.lock().unwrap().clone();
        assert!(
            text.contains("POST /api/v1/services/aigc/video-generation/video-synthesis"),
            "{text}"
        );
        assert!(
            text.to_ascii_lowercase()
                .contains("x-dashscope-async: enable"),
            "{text}"
        );
        assert!(text.contains("\"img_url\""), "{text}");
        assert!(text.contains("wan2.6-i2v-flash"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn ark_plan_video_url_keeps_plan_prefix() {
        assert_eq!(
            tasks_url("https://ark.cn-beijing.volces.com/api/plan/v3").unwrap(),
            "https://ark.cn-beijing.volces.com/api/plan/v3/contents/generations/tasks"
        );
        assert_eq!(
            tasks_url("https://ark.cn-beijing.volces.com").unwrap(),
            "https://ark.cn-beijing.volces.com/api/v3/contents/generations/tasks"
        );
    }
}
