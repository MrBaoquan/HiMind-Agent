//! 本机视觉理解：把一张图片交给已接入的多模态模型，返回标签/描述/质量分。
//!
//! 这是 Agent 侧能力（`media.ai.describe`），插件只负责提供本地图片路径和解析结果。
//! 图片会先缩到最长边 1024 再编码 JPEG，减少 token 与上传体积。

use base64::Engine as _;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::store::ai_services::{self, AIServiceProtocol, CustomAIService, CAP_VISION};

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EDGE: u32 = 1024;

pub(crate) fn describe(input: &Value) -> Result<Value, Box<dyn Error>> {
    let path = string_field(input, "path").unwrap_or_default();
    let image_input = string_field(input, "image");
    if path.is_empty() && image_input.is_none() {
        return Err("path 或 image 至少提供一个".into());
    }
    let profile = string_field(input, "profile").unwrap_or_else(|| "full".to_string());
    let prompt = string_field(input, "prompt").unwrap_or_else(|| default_prompt(&profile));
    let requested_service = string_field(input, "service").unwrap_or_default();
    // 服务归属 Agent：调用方不指定时，按「vision.describe」能力绑定路由，
    // 没有绑定才退回到当前激活服务。`service` 参数仅为旧调用方保留，新调用方不应传。
    let mut resolved_by = "binding";
    let (service, api_key) = if let Some(id) = requested_service.strip_prefix("custom:") {
        resolved_by = "request";
        ai_services::load_secret(id.trim())?
    } else if requested_service.is_empty() {
        let bound = ai_services::configured_capability_service(CAP_VISION)?;
        match bound {
            Some(id) => ai_services::load_secret(&id)?,
            None => {
                resolved_by = "active";
                ai_services::active_service_with_secret()?.ok_or("没有激活的 AI 服务")?
            }
        }
    } else {
        return Err("service 必须是 custom:<id>，留空由 Agent 按能力绑定选择".into());
    };
    // 模型解析顺序必须与路由计划一致（ADR 0023 第七条）：
    // 显式传入 → `vision.describe` 的媒体绑定 → 服务对话模型。
    // 计划里 `capability_models(CAP_VISION)` 用的也是这个顺序，三处同源。
    let model = string_field(input, "model")
        .or_else(|| {
            ai_services::capability_models(&service, CAP_VISION)
                .into_iter()
                .next()
        })
        .unwrap_or_else(|| service.model.clone());
    if model.trim().is_empty() {
        return Err("当前 AI 服务没有可用模型".into());
    }
    let max_edge = input
        .get("max_edge")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(128, 1536) as u32)
        .unwrap_or(MAX_EDGE);
    let max_output_tokens = input
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(64, 2000))
        .unwrap_or(400);
    let reasoning_effort = string_field(input, "reasoning_effort");
    let (payload, mime) = match image_input {
        Some(value) => encode_data_url(&value, max_edge)?,
        None => encode_image(&path, max_edge)?,
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let data_url = format!("data:{mime};base64,{encoded}");
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let (text, usage) = match service.protocol {
        AIServiceProtocol::OpenaiChat => call_openai_chat(
            &client,
            &service,
            &api_key,
            &model,
            &prompt,
            &data_url,
            max_output_tokens,
            reasoning_effort.as_deref(),
        )?,
        AIServiceProtocol::OpenaiResponses => call_openai_responses(
            &client,
            &service,
            &api_key,
            &model,
            &prompt,
            &data_url,
            max_output_tokens,
            reasoning_effort.as_deref(),
        )?,
        AIServiceProtocol::Anthropic => call_anthropic(
            &client,
            &service,
            &api_key,
            &model,
            &prompt,
            &encoded,
            &mime,
            max_output_tokens,
        )?,
    };
    if text.trim().is_empty() {
        return Err("视觉模型返回了空内容".into());
    }
    Ok(json!({
        "text": text,
        "model": model,
        "service": format!("custom:{}", service.id),
        "protocol": service.protocol.as_str(),
        "resolved_by": resolved_by,
        "display_name": service.display_name,
        "profile": profile,
        "max_edge": max_edge,
        "usage": usage,
    }))
}

fn default_prompt(profile: &str) -> String {
    if profile == "fast" || profile == "tags" {
        return "只输出 JSON：{\"tags\":[\"5-8个2-8字中文标签\"]}。标签只描述画面内容（场景、主体、动作、氛围），不要文件格式、分辨率、构图、配图用途等技术或空泛词。不要描述、不要解释、不要推理。"
            .to_string();
    }
    if profile == "ocr" {
        return "只识别图片中的文字，按行输出纯文本，不要解释，不要加引号。没有文字时输出空。"
            .to_string();
    }
    "请用中文分析这张素材画面，只输出 JSON：{\"tags\":[\"5-10个简短标签\"],\"description\":\"一句话描述\",\"score\":1到5的整数}".to_string()
}

fn string_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn encode_image(path: &str, max_edge: u32) -> Result<(Vec<u8>, &'static str), Box<dyn Error>> {
    let target = PathBuf::from(path);
    if !target.is_absolute() {
        return Err("path 必须是绝对路径".into());
    }
    let info = fs::metadata(&target)?;
    if !info.is_file() {
        return Err("path 不是文件".into());
    }
    if info.len() > MAX_IMAGE_BYTES {
        return Err(format!("图片超过 {} MiB 上限", MAX_IMAGE_BYTES / 1024 / 1024).into());
    }
    let image = image::open(&target)?;
    encode_pixels(image, max_edge)
}

fn encode_data_url(value: &str, max_edge: u32) -> Result<(Vec<u8>, &'static str), Box<dyn Error>> {
    let (_, encoded) = value.split_once(',').ok_or("image 必须是 data URL")?;
    let raw = base64::engine::general_purpose::STANDARD.decode(encoded.trim())?;
    if raw.len() as u64 > MAX_IMAGE_BYTES {
        return Err(format!("图片超过 {} MiB 上限", MAX_IMAGE_BYTES / 1024 / 1024).into());
    }
    let image = image::load_from_memory(&raw)?;
    encode_pixels(image, max_edge)
}

fn encode_pixels(
    image: image::DynamicImage,
    max_edge: u32,
) -> Result<(Vec<u8>, &'static str), Box<dyn Error>> {
    let resized = if image.width() > max_edge || image.height() > max_edge {
        let scale = max_edge as f32 / image.width().max(image.height()) as f32;
        let width = ((image.width() as f32 * scale).round() as u32).max(1);
        let height = ((image.height() as f32 * scale).round() as u32).max(1);
        image.resize(width, height, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut output = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 86);
    encoder.encode_image(&resized)?;
    Ok((output, "image/jpeg"))
}

fn endpoint(base: &str, suffix: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with(suffix.trim_start_matches('/')) {
        return base.to_string();
    }
    if suffix.starts_with('/') {
        if base.ends_with("/v1") && suffix.starts_with("/v1/") {
            return format!("{}{}", base, &suffix[3..]);
        }
        format!("{base}{suffix}")
    } else {
        format!("{base}/{suffix}")
    }
}

fn call_openai_chat(
    client: &Client,
    service: &CustomAIService,
    api_key: &str,
    model: &str,
    prompt: &str,
    data_url: &str,
    max_output_tokens: u64,
    reasoning_effort: Option<&str>,
) -> Result<(String, Value), Box<dyn Error>> {
    let mut body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": data_url}}
            ]
        }],
        "max_tokens": max_output_tokens,
        "temperature": 0
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    let response = client
        .post(endpoint(&service.base_url, "/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    if !status.is_success() {
        return Err(format!(
            "视觉模型调用失败 HTTP {status}: {}",
            truncate(&body.to_string())
        )
        .into());
    }
    let text = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .map(content_text)
        .unwrap_or_default();
    Ok((text, body.get("usage").cloned().unwrap_or(Value::Null)))
}

fn call_openai_responses(
    client: &Client,
    service: &CustomAIService,
    api_key: &str,
    model: &str,
    prompt: &str,
    data_url: &str,
    max_output_tokens: u64,
    reasoning_effort: Option<&str>,
) -> Result<(String, Value), Box<dyn Error>> {
    let mut body = json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": prompt},
                {"type": "input_image", "image_url": data_url}
            ]
        }],
        "max_output_tokens": max_output_tokens
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning"] = json!({"effort": effort});
    }
    let response = client
        .post(endpoint(&service.base_url, "/responses"))
        .bearer_auth(api_key)
        .json(&body)
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    if !status.is_success() {
        return Err(format!(
            "视觉模型调用失败 HTTP {status}: {}",
            truncate(&body.to_string())
        )
        .into());
    }
    let mut text = body
        .get("output_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if text.is_empty() {
        if let Some(outputs) = body.get("output").and_then(Value::as_array) {
            for output in outputs {
                if let Some(parts) = output.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(value) = part.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                        }
                    }
                }
            }
        }
    }
    Ok((text, body.get("usage").cloned().unwrap_or(Value::Null)))
}

fn call_anthropic(
    client: &Client,
    service: &CustomAIService,
    api_key: &str,
    model: &str,
    prompt: &str,
    encoded: &str,
    mime: &str,
    max_output_tokens: u64,
) -> Result<(String, Value), Box<dyn Error>> {
    let response = client
        .post(endpoint(&service.base_url, "/v1/messages"))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": model,
            "max_tokens": max_output_tokens,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": mime, "data": encoded}},
                    {"type": "text", "text": prompt}
                ]
            }]
        }))
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    if !status.is_success() {
        return Err(format!(
            "视觉模型调用失败 HTTP {status}: {}",
            truncate(&body.to_string())
        )
        .into());
    }
    let mut text = String::new();
    if let Some(parts) = body.get("content").and_then(Value::as_array) {
        for part in parts {
            if let Some(value) = part.get("text").and_then(Value::as_str) {
                text.push_str(value);
            }
        }
    }
    Ok((text, body.get("usage").cloned().unwrap_or(Value::Null)))
}

fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    let mut output = String::new();
    if let Some(parts) = value.as_array() {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                output.push_str(text);
            }
        }
    }
    output
}

fn truncate(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() > 400 {
        let shortened: String = value.chars().take(400).collect();
        format!("{shortened}…")
    } else {
        value.to_string()
    }
}

#[allow(dead_code)]
fn ensure_path(value: &str) -> PathBuf {
    Path::new(value).to_path_buf()
}
