//! 后台视觉打标作业：一次提交多条素材，Agent 在后台并发调用多模态模型，
//! 结果按素材 ID 返回；视图关闭/重开不影响作业，结果落盘可恢复。
//!
//! 作业文件在 `agent_home()/media-ai-jobs/<job_id>.json`，进程重启后
//! `running` 会降级为 `interrupted`，未完成条目回到 `pending` 以便重试。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::vision_describe;

#[derive(Clone, Serialize, Deserialize)]
struct BatchItem {
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    image: String,
    status: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tokens: u64,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    usage: Value,
    #[serde(default)]
    service: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    error: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct BatchJob {
    id: String,
    status: String,
    total: usize,
    done: usize,
    failed: usize,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    budget_tokens: u64,
    #[serde(default)]
    used_tokens: u64,
    created_at: u64,
    updated_at: u64,
    items: Vec<BatchItem>,
}

static JOBS: OnceLock<Mutex<HashMap<String, Arc<Mutex<BatchJob>>>>> = OnceLock::new();

fn jobs() -> &'static Mutex<HashMap<String, Arc<Mutex<BatchJob>>>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

/// 视觉模型偶尔会返回空文本：按失败处理，并在重试时降分辨率。
fn describe_text_empty(outcome: &Result<Value, Box<dyn std::error::Error>>) -> bool {
    match outcome {
        Ok(value) => value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.trim().is_empty())
            .unwrap_or(true),
        Err(_) => false,
    }
}

fn job_directory() -> Result<PathBuf, String> {
    let directory = crate::store::paths::agent_home().join("media-ai-jobs");
    fs::create_dir_all(&directory).map_err(|error| format!("创建作业目录失败: {error}"))?;
    Ok(directory)
}

fn job_path(id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || id.len() > 120 || id.contains(['/', '\\', '.']) {
        return Err("job_id 不合法".to_string());
    }
    Ok(job_directory()?.join(format!("{id}.json")))
}

fn persist(job: &BatchJob) {
    let Ok(path) = job_path(&job.id) else {
        return;
    };
    if let Ok(payload) = serde_json::to_vec(job) {
        let _ = fs::write(path, payload);
    }
}

fn requeue_unfinished(job: &mut BatchJob) {
    // 进程重启后，运行中的条目没有线程在跑，标记回 pending 便于重试。
    for item in job.items.iter_mut() {
        if item.status == "running" {
            item.status = "pending".to_string();
            if item.error.is_empty() {
                item.error = "上次运行中断".to_string();
            }
        }
    }
}

fn load_job(id: &str) -> Result<Arc<Mutex<BatchJob>>, String> {
    if let Some(existing) = jobs().lock().ok().and_then(|map| map.get(id).cloned()) {
        return Ok(existing);
    }
    let path = job_path(id)?;
    let payload = fs::read(&path).map_err(|_| "作业不存在".to_string())?;
    let mut job: BatchJob =
        serde_json::from_slice(&payload).map_err(|error| format!("作业文件无法解析: {error}"))?;
    if job.status == "running" {
        job.status = "interrupted".to_string();
        requeue_unfinished(&mut job);
    }
    let shared = Arc::new(Mutex::new(job));
    if let Ok(mut map) = jobs().lock() {
        map.insert(id.to_string(), Arc::clone(&shared));
    }
    Ok(shared)
}

fn string_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn start(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let raw_items = input
        .get("items")
        .and_then(Value::as_array)
        .ok_or("items 不能为空")?;
    if raw_items.is_empty() || raw_items.len() > 500 {
        return Err("items 数量必须在 1 到 500 之间".into());
    }
    let mut items = Vec::with_capacity(raw_items.len());
    for raw in raw_items {
        let id = string_field(raw, "id").ok_or("每条 item 必须带 id")?;
        let path = string_field(raw, "path").unwrap_or_default();
        let image = string_field(raw, "image").unwrap_or_default();
        if path.is_empty() && image.is_empty() {
            return Err(format!("item {id} 缺少 path 或 image").into());
        }
        items.push(BatchItem {
            id,
            path,
            image,
            status: "pending".to_string(),
            text: String::new(),
            tokens: 0,
            attempts: 0,
            usage: Value::Null,
            service: String::new(),
            model: String::new(),
            error: String::new(),
        });
    }
    let profile = string_field(input, "profile").unwrap_or_else(|| "fast".to_string());
    if !matches!(profile.as_str(), "fast" | "full" | "ocr") {
        return Err("profile 只能是 fast、full 或 ocr".into());
    }
    let concurrency = input
        .get("concurrency")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 4) as usize)
        .unwrap_or(2);
    let budget_tokens = input
        .get("budget_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let attempts = input
        .get("attempts")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 3) as u32)
        .unwrap_or(2);
    let now = now_seconds();
    let id = format!("vision-{now}-{:08x}", rand::random::<u32>());
    let job = BatchJob {
        id: id.clone(),
        status: "running".to_string(),
        total: items.len(),
        done: 0,
        failed: 0,
        profile: profile.clone(),
        service: string_field(input, "service").unwrap_or_default(),
        budget_tokens,
        used_tokens: 0,
        created_at: now,
        updated_at: now,
        items,
    };
    let shared = Arc::new(Mutex::new(job));
    if let Ok(mut map) = jobs().lock() {
        map.insert(id.clone(), Arc::clone(&shared));
    }
    if let Ok(job) = shared.lock() {
        persist(&job);
    }
    let config = input.clone();
    let job_id = id.clone();
    thread::spawn(move || run_job(job_id, shared, config, concurrency, attempts));
    Ok(json!({ "job_id": id, "status": "running", "total": raw_items.len() }))
}

fn run_job(
    _job_id: String,
    shared: Arc<Mutex<BatchJob>>,
    config: Value,
    concurrency: usize,
    attempts: u32,
) {
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..concurrency {
        let shared = Arc::clone(&shared);
        let next = Arc::clone(&next);
        let config = config.clone();
        handles.push(thread::spawn(move || {
            worker(shared, next, config, attempts)
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    if let Ok(mut job) = shared.lock() {
        if job.status == "running" {
            job.status = "done".to_string();
        }
        let reason = match job.status.as_str() {
            "canceled" => "已取消",
            "budget_exceeded" => "预算上限已达，未执行",
            _ => "作业未完成",
        }
        .to_string();
        for item in job.items.iter_mut() {
            if item.status == "pending" || item.status == "running" {
                item.status = "canceled".to_string();
                if item.error.is_empty() {
                    item.error = reason.clone();
                }
            }
        }
        job.updated_at = now_seconds();
        persist(&job);
    }
}

fn worker(shared: Arc<Mutex<BatchJob>>, next: Arc<AtomicUsize>, config: Value, attempts: u32) {
    loop {
        let index = next.fetch_add(1, Ordering::SeqCst);
        if index >= shared.lock().map(|job| job.total).unwrap_or(0) {
            break;
        }
        let should_stop = {
            let Ok(job) = shared.lock() else { break };
            if job.status != "running" {
                true
            } else {
                job.budget_tokens > 0 && job.used_tokens >= job.budget_tokens
            }
        };
        if should_stop {
            break;
        }
        {
            let Ok(mut job) = shared.lock() else { break };
            if job.items[index].status != "pending" {
                continue;
            }
            job.items[index].status = "running".to_string();
        }
        let mut request = {
            let Ok(job) = shared.lock() else { break };
            let item = &job.items[index];
            let mut request = json!({
                "profile": config.get("profile").cloned().unwrap_or(Value::String("fast".into())),
                "max_edge": config.get("max_edge").cloned().unwrap_or(json!(512)),
                "max_output_tokens": config.get("max_output_tokens").cloned().unwrap_or(json!(200)),
            });
            if let Some(service) = string_field(&config, "service") {
                request["service"] = json!(service);
            }
            if let Some(model) = string_field(&config, "model") {
                request["model"] = json!(model);
            }
            if let Some(prompt) = string_field(&config, "prompt") {
                request["prompt"] = json!(prompt);
            }
            if let Some(effort) = string_field(&config, "reasoning_effort") {
                request["reasoning_effort"] = json!(effort);
            }
            if item.path.is_empty() {
                request["image"] = json!(item.image);
            } else {
                request["path"] = json!(item.path);
            }
            request
        };
        let mut outcome = vision_describe::describe(&request);
        let mut used = 1;
        while (outcome.is_err() || describe_text_empty(&outcome)) && used < attempts {
            if let Err(error) = &outcome {
                let message = error.to_string().to_lowercase();
                // 个别网关不认 reasoning_effort；失败后去掉它再试一次，别让整批卡死。
                let unsupported = message.contains("reasoning")
                    || message.contains("unsupported")
                    || message.contains("http 400")
                    || message.contains("http 422");
                if unsupported {
                    if let Some(object) = request.as_object_mut() {
                        object.remove("reasoning_effort");
                    }
                }
            } else if describe_text_empty(&outcome) {
                // 空返回多半是图太大或提示过长：降一档分辨率再试。
                request["max_edge"] = json!(384);
            }
            thread::sleep(Duration::from_millis(1500));
            if let Ok(job) = shared.lock() {
                if job.status != "running" {
                    break;
                }
            }
            used += 1;
            outcome = vision_describe::describe(&request);
        }
        if let Ok(mut job) = shared.lock() {
            let mut resolved_service = String::new();
            let mut item_tokens = 0u64;
            let mut finished_ok = false;
            {
                let item = &mut job.items[index];
                item.attempts = used;
                match outcome {
                    Ok(value) => {
                        let text = value
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if text.trim().is_empty() {
                            item.status = "failed".to_string();
                            item.error = "视觉模型返回了空内容（已降分辨率重试）".to_string();
                        } else {
                            item.status = "done".to_string();
                            item.text = text;
                            item.tokens = value
                                .get("usage")
                                .and_then(|usage| usage.get("total_tokens"))
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            item.usage = value.get("usage").cloned().unwrap_or(Value::Null);
                            item.service = value
                                .get("service")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            item.model = value
                                .get("model")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            item.error.clear();
                            resolved_service = item.service.clone();
                            item_tokens = item.tokens;
                            finished_ok = true;
                        }
                    }
                    Err(error) => {
                        item.status = "failed".to_string();
                        item.error = error.to_string();
                    }
                }
            }
            if finished_ok {
                job.done += 1;
                job.used_tokens = job.used_tokens.saturating_add(item_tokens);
                // 把 Agent 实际解析到的服务回填到作业上，界面不用再自己猜。
                if job.service.is_empty() && !resolved_service.is_empty() {
                    job.service = resolved_service;
                }
            } else {
                job.failed += 1;
            }
            if job.budget_tokens > 0 && job.used_tokens >= job.budget_tokens {
                job.status = "budget_exceeded".to_string();
            }
            job.updated_at = now_seconds();
            persist(&job);
        }
    }
}

pub(crate) fn status(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let id = string_field(input, "job_id").ok_or("job_id 不能为空")?;
    let shared = load_job(&id)?;
    let job = shared.lock().map_err(|_| "作业锁不可用")?;
    let items = job
        .items
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "status": item.status,
                "text": item.text,
                "tokens": item.tokens,
                "attempts": item.attempts,
                "usage": item.usage,
                "service": item.service,
                "model": item.model,
                "error": item.error,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "job_id": job.id,
        "status": job.status,
        "total": job.total,
        "done": job.done,
        "failed": job.failed,
        "profile": job.profile,
        "service": job.service,
        "budget_tokens": job.budget_tokens,
        "used_tokens": job.used_tokens,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "items": items,
    }))
}

pub(crate) fn cancel(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let id = string_field(input, "job_id").ok_or("job_id 不能为空")?;
    let shared = load_job(&id)?;
    let mut job = shared.lock().map_err(|_| "作业锁不可用")?;
    if job.status == "running" {
        job.status = "canceled".to_string();
        job.updated_at = now_seconds();
        persist(&job);
    }
    Ok(json!({ "job_id": job.id, "status": job.status }))
}

pub(crate) fn resume(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    // 重启后把中断作业里仍为 pending 的条目重新排队。
    let id = string_field(input, "job_id").ok_or("job_id 不能为空")?;
    let shared = load_job(&id)?;
    let mut job = shared.lock().map_err(|_| "作业锁不可用")?;
    if job.status == "running" {
        return Ok(json!({ "job_id": job.id, "status": job.status, "resumed": false }));
    }
    let pending = job
        .items
        .iter()
        .filter(|item| matches!(item.status.as_str(), "pending" | "running"))
        .count();
    if pending == 0 {
        return Ok(
            json!({ "job_id": job.id, "status": job.status, "resumed": false, "pending": 0 }),
        );
    }
    for item in job.items.iter_mut() {
        if item.status == "running" {
            item.status = "pending".to_string();
        }
    }
    job.status = "running".to_string();
    job.updated_at = now_seconds();
    persist(&job);
    let config = json!({
        "profile": job.profile,
        "service": job.service,
        "max_edge": input.get("max_edge").cloned().unwrap_or(json!(512)),
        "max_output_tokens": input.get("max_output_tokens").cloned().unwrap_or(json!(200)),
        "budget_tokens": job.budget_tokens,
    });
    let attempts = input
        .get("attempts")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 3) as u32)
        .unwrap_or(2);
    let concurrency = input
        .get("concurrency")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 4) as usize)
        .unwrap_or(2);
    drop(job);
    let job_id = id.clone();
    thread::spawn(move || run_job(job_id, shared, config, concurrency, attempts));
    Ok(json!({ "job_id": id, "status": "running", "resumed": true, "pending": pending }))
}

fn summarize(job: &BatchJob) -> Value {
    json!({
        "job_id": job.id,
        "status": job.status,
        "total": job.total,
        "done": job.done,
        "failed": job.failed,
        "profile": job.profile,
        "used_tokens": job.used_tokens,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

pub(crate) fn list() -> Result<Value, Box<dyn std::error::Error>> {
    let mut seen = std::collections::HashSet::new();
    let mut summaries = Vec::new();
    if let Ok(map) = jobs().lock() {
        for shared in map.values() {
            if let Ok(job) = shared.lock() {
                seen.insert(job.id.clone());
                summaries.push(summarize(&job));
            }
        }
    }
    // 进程重启后内存里没有作业，从磁盘补齐，界面才能接着看上次的结果。
    if let Ok(directory) = job_directory() {
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if seen.contains(stem) {
                    continue;
                }
                let Ok(payload) = fs::read(&path) else {
                    continue;
                };
                let Ok(job) = serde_json::from_slice::<BatchJob>(&payload) else {
                    continue;
                };
                seen.insert(job.id.clone());
                summaries.push(summarize(&job));
            }
        }
    }
    summaries.sort_by_key(|item| {
        std::cmp::Reverse(item.get("created_at").and_then(Value::as_u64).unwrap_or(0))
    });
    summaries.truncate(50);
    Ok(json!({ "jobs": summaries }))
}
