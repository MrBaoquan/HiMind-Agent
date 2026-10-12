//! 本机 OpenAI 兼容生图。
//!
//! 用用户在本机「AI 服务」里配置的服务（`custom:<id>`，缺省取当前激活服务）直接
//! 调用 `POST {base_url}/v1/images/generations`，把返回的图片落盘，并可选地以 MCP
//! image content 内联返回。整条链路只读本机状态库与用户自己的 API Key，不经过 AI
//! 工作台，因此独立（Independent）与已对接（Connected）两种模式都可用。
//!
//! 边界：
//! - 只用 OpenAI Images 兼容协议；Anthropic 协议的服务会被明确拒绝，而不是静默降级。
//! - 图片只写入显式 `output_dir`，或工作区 `.himind/media`，或 Agent 数据目录 `media`。
//! - 生成结果全部落盘；是否把图片内联进对话由调用方用 `inline` 决定。

use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::store::ai_services::AIServiceProtocol;
use crate::store::ai_services::{CAP_IMAGE_EDIT, CAP_IMAGE_GENERATE};

const DEFAULT_TIMEOUT_SECONDS: u64 = 180;
const MAX_REQUEST_COUNT: u64 = 4;
const MAX_RESPONSE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_INLINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_INPUT_IMAGES: usize = 4;
const MAX_INPUT_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 8_000;
const WORKSPACE_MEDIA_DIR: &str = ".himind";
const WORKSPACE_MEDIA_SUBDIR: &str = "media";
const AGENT_MEDIA_SUBDIR: &str = "media";
/// OpenAI Images 协议（`/v1/images/generations`）。
const ADAPTER_OPENAI_IMAGES: &str = "openai-images";
/// 火山方舟（豆包 Seedream）图像协议（`/api/v3/images/generations`）。
const ADAPTER_ARK_IMAGES: &str = "ark-images";
/// OpenAI 图生图（multipart `/v1/images/edits`）。
const ADAPTER_OPENAI_IMAGES_EDIT: &str = "openai-images-edit";

/// 能力入口：解析服务与参数，然后执行生图（`image.generate`）。
pub(crate) fn generate(input: &Value) -> Result<Value, Box<dyn Error>> {
    let request = ImageRequest::parse(input)?;
    let (service, api_key) =
        crate::store::ai_services::resolve_media_service(CAP_IMAGE_GENERATE, &request.service)?;
    let (adapter, model) = resolve_binding(&service, CAP_IMAGE_GENERATE, request.model.as_deref())?;
    let output_dir = resolve_output_dir(request.output_dir.as_deref())?;
    call_generate_api(
        &service.base_url,
        &adapter,
        &api_key,
        &service.id,
        &model,
        &request,
        &output_dir,
    )
}

/// 能力入口：图生图 / 改图（`image.edit`）。
///
/// 参考图可以是本机绝对路径，也可以是之前生成结果的 `media:<id>`；
/// 默认要求参考图位于当前会话工作区内。
pub(crate) fn edit(input: &Value) -> Result<Value, Box<dyn Error>> {
    let request = ImageRequest::parse(input)?;
    if request.input_images.is_empty() {
        return Err("input_images 不能为空：请给出参考图的本机路径或 media:<id>".into());
    }
    let (service, api_key) =
        crate::store::ai_services::resolve_media_service(CAP_IMAGE_EDIT, &request.service)?;
    let (adapter, model) = resolve_binding(&service, CAP_IMAGE_EDIT, request.model.as_deref())?;
    let output_dir = resolve_output_dir(request.output_dir.as_deref())?;
    call_edit_api(
        &service.base_url,
        &adapter,
        &api_key,
        &service.id,
        &model,
        &request,
        &output_dir,
    )
}

/// 取服务在指定能力上的「适配器 + 模型」；`model` 参数可覆盖槽位里的模型。
fn resolve_binding(
    service: &crate::store::ai_services::CustomAIService,
    capability: &str,
    requested_model: Option<&str>,
) -> Result<(String, String), Box<dyn Error>> {
    if matches!(
        service.protocol,
        crate::store::ai_services::AIServiceProtocol::Anthropic
    ) {
        return Err(format!(
            "服务「{}」使用 Anthropic 协议，不支持 OpenAI Images 媒体能力；请改用 openai-chat/openai-responses 服务",
            service.display_name
        )
        .into());
    }
    let binding = crate::store::ai_services::binding_for(service, capability);
    let requested = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (binding, requested) {
        (Some((adapter, model)), Some(override_model)) => Ok((adapter, override_model.to_string())),
        (Some(binding), None) => Ok(binding),
        (None, Some(override_model)) => Ok((
            crate::store::ai_services::DEFAULT_MEDIA_ADAPTER.to_string(),
            override_model.to_string(),
        )),
        (None, None) => Err(format!(
            "服务「{}」尚未配置「{capability}」能力；请在该服务上填写对应模型，或在调用时传入 model",
            service.display_name
        )
        .into()),
    }
}

#[derive(Debug, Clone)]
struct ImageRequest {
    prompt: String,
    service: String,
    model: Option<String>,
    size: Option<String>,
    quality: Option<String>,
    n: u64,
    output_dir: Option<String>,
    filename: Option<String>,
    inline: bool,
    /// provider 专属参数（如方舟的 `watermark`、`sequential_image_generation`），
    /// 直接合并进请求体；同名时以显式字段（model/prompt/size/…）为准。
    parameters: Option<serde_json::Map<String, Value>>,
    /// 图生图的参考图：本机绝对路径或 `media:<id>`。
    input_images: Vec<String>,
    /// 可选蒙版（仅 `openai-images-edit` 使用）。
    mask: Option<String>,
}

impl ImageRequest {
    fn parse(input: &Value) -> Result<Self, Box<dyn Error>> {
        let object = input
            .as_object()
            .ok_or("生图参数必须是 JSON 对象")?;
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if prompt.is_empty() {
            return Err("prompt 不能为空".into());
        }
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(format!("prompt 过长（最多 {MAX_PROMPT_CHARS} 个字符）").into());
        }
        let n = match input.get("n") {
            None | Some(Value::Null) => 1,
            Some(value) => value
                .as_u64()
                .filter(|value| (1..=MAX_REQUEST_COUNT).contains(value))
                .ok_or_else(|| format!("n 必须是 1-{MAX_REQUEST_COUNT} 的整数"))?,
        };
        Ok(Self {
            prompt,
            service: optional_string(input, "service", 80)?.unwrap_or_default(),
            model: optional_string(input, "model", 160)?,
            size: optional_token(input, "size", 32)?,
            quality: optional_token(input, "quality", 32)?,
            n,
            output_dir: optional_string(input, "output_dir", 2000)?,
            filename: optional_string(input, "filename", 120)?,
            inline: object
                .get("inline")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            parameters: match input.get("parameters") {
                None | Some(Value::Null) => None,
                Some(Value::Object(map)) => Some(map.clone()),
                Some(_) => return Err("parameters 必须是对象".into()),
            },
            input_images: match input.get("input_images") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => {
                    let mut result = Vec::new();
                    for item in items {
                        let value = item
                            .as_str()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .ok_or("input_images 只能是字符串数组")?;
                        result.push(value.to_string());
                    }
                    if result.len() > MAX_INPUT_IMAGES {
                        return Err(format!("input_images 最多 {MAX_INPUT_IMAGES} 张").into());
                    }
                    result
                }
                Some(_) => return Err("input_images 必须是字符串数组".into()),
            },
            mask: optional_string(input, "mask", 2000)?,
        })
    }
}

fn optional_string(
    input: &Value,
    key: &str,
    max_len: usize,
) -> Result<Option<String>, Box<dyn Error>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.chars().count() > max_len {
                return Err(format!("{key} 过长（最多 {max_len} 个字符）").into());
            }
            Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
        }
        Some(_) => Err(format!("{key} 必须是字符串").into()),
    }
}

/// `size`/`quality` 只作为参数透传给 provider，这里限制字符集，避免拼出畸形请求。
fn optional_token(
    input: &Value,
    key: &str,
    max_len: usize,
) -> Result<Option<String>, Box<dyn Error>> {
    let Some(value) = optional_string(input, key, max_len)? else {
        return Ok(None);
    };
    let allowed = value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, 'x' | 'X' | '*' | '-' | '.' | '_'));
    if !allowed {
        return Err(format!("{key} 只允许字母、数字与 x X * - . _ 字符").into());
    }
    Ok(Some(value))
}

/// 真正调用 provider 的入口；把「解析服务」与「发起请求」分开，便于单测。
fn call_generate_api(
    base_url: &str,
    adapter: &str,
    api_key: &str,
    service_id: &str,
    model: &str,
    request: &ImageRequest,
    output_dir: &Path,
) -> Result<Value, Box<dyn Error>> {
    if api_key.trim().is_empty() {
        return Err("所选本机 AI 服务没有可用的 API Key".into());
    }
    let adapter = normalize_image_adapter(adapter)?;
    let url = images_url_for(adapter, base_url)?;
    // 先铺 provider 专属参数，再用显式字段覆盖，避免调用方参数悄悄改掉核心字段。
    let mut body = request.parameters.clone().unwrap_or_default();
    body.insert("model".to_string(), json!(model));
    body.insert("prompt".to_string(), json!(request.prompt));
    body.insert("n".to_string(), json!(request.n));
    body.insert("response_format".to_string(), json!("b64_json"));
    if let Some(size) = &request.size {
        body.insert("size".to_string(), json!(size));
    }
    if let Some(quality) = &request.quality {
        body.insert("quality".to_string(), json!(quality));
    }
    if adapter == ADAPTER_ARK_IMAGES {
        // 方舟默认可能给结果加水印；业务素材默认关闭，可在 parameters 里覆盖。
        body.entry("watermark".to_string())
            .or_insert_with(|| json!(false));
    }

    let client = Client::builder().timeout(request_timeout()).build()?;
    let mut payload = Value::Object(body);
    let (mut status, mut body_value) = post_images(&client, &url, api_key, &payload)?;
    // `gpt-image-1` 等新模型不接受 `response_format`（默认就回 b64_json），
    // 部分兼容网关也会拒绝未知参数。这里只在明确是参数问题时去掉它重试一次，
    // 不做其它静默降级。
    if !status.is_success() && rejects_response_format(&body_value) {
        if let Some(object) = payload.as_object_mut() {
            object.remove("response_format");
        }
        let retry = post_images(&client, &url, api_key, &payload)?;
        status = retry.0;
        body_value = retry.1;
    }
    if !status.is_success() {
        return Err(format!("生图接口返回 {status}：{}", describe_error(&body_value)).into());
    }
    let payload = body_value;

    materialize_outputs(
        &client,
        &payload,
        request,
        output_dir,
        CAP_IMAGE_GENERATE,
        service_id,
        model,
        base_url,
        "已生成",
    )
}

/// 把 provider 返回的图片落盘、登记为 `media_id`，并组装成能力返回值。
#[allow(clippy::too_many_arguments)]
fn materialize_outputs(
    client: &Client,
    payload: &Value,
    request: &ImageRequest,
    output_dir: &Path,
    capability: &str,
    service_id: &str,
    model: &str,
    base_url: &str,
    verb: &str,
) -> Result<Value, Box<dyn Error>> {
    let items = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or("生图响应缺少 data 数组")?;
    if items.is_empty() {
        return Err("生图响应没有返回任何图片".into());
    }

    let mut files: Vec<Value> = Vec::new();
    let mut inline_blocks: Vec<Value> = Vec::new();
    for (index, item) in items.iter().take(request.n as usize).enumerate() {
        let bytes = match item.get("b64_json").and_then(Value::as_str) {
            Some(encoded) => decode_base64(encoded)?,
            None => {
                let raw_url = item
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("生图返回项既没有 b64_json 也没有 url")?;
                download_image(&client, raw_url)?
            }
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "生成图片超过 {} MiB 上限",
                MAX_IMAGE_BYTES / (1024 * 1024)
            )
            .into());
        }
        let mime = sniff_mime(&bytes);
        let path = output_dir.join(output_file_name(
            request.filename.as_deref(),
            index,
            extension_for_mime(mime),
        ));
        crate::store::atomic_file::atomic_write(&path, &bytes)
            .map_err(|error| format!("图片写入失败：{}（{error}）", path.display()))?;
        let (width, height) = image_dimensions(&bytes);
        let sha256 = hex_sha256(&bytes);
        let path_text = path.to_string_lossy().to_string();
        let record = crate::store::media_registry::register(crate::store::media_registry::NewMedia {
            path: &path_text,
            kind: "image",
            mime_type: mime,
            size_bytes: bytes.len() as u64,
            sha256: &sha256,
            width: width.unwrap_or_default(),
            height: height.unwrap_or_default(),
            duration_seconds: 0.0,
            capability,
            service_id,
            prompt: &request.prompt,
        })?;
        if request.inline && bytes.len() <= MAX_INLINE_BYTES {
            inline_blocks.push(json!({
                "type": "image",
                "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "mimeType": mime,
            }));
        }
        files.push(json!({
            "media_id": record.id,
            "path": path_text,
            "name": path.file_name().and_then(|value| value.to_str()).unwrap_or_default(),
            "mime_type": mime,
            "size_bytes": bytes.len(),
            "sha256": sha256,
            "width": width,
            "height": height,
            "revised_prompt": item.get("revised_prompt").and_then(Value::as_str),
        }));
    }

    let paths: Vec<String> = files
        .iter()
        .filter_map(|file| file.get("path").and_then(Value::as_str).map(str::to_string))
        .collect();
    let structured = json!({
        "mode": "local",
        "provider": provider_label(base_url),
        "service_id": service_id,
        "capability": capability,
        "model": model,
        "output_dir": output_dir.to_string_lossy(),
        "files": files,
    });
    if inline_blocks.is_empty() {
        return Ok(structured);
    }
    let mut content = vec![json!({
        "type": "text",
        "text": format!("{verb} {} 张图片：\n{}\n后续可用 media:<id> 引用继续改图。", paths.len(), paths.join("\n")),
    })];
    content.extend(inline_blocks);
    Ok(json!({
        "content": content,
        "structuredContent": structured,
    }))
}

/// 归一化生图适配器；空值等价于 `openai-images`（保持旧配置可直接用）。
fn normalize_image_adapter(raw: &str) -> Result<&'static str, Box<dyn Error>> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "openai" | "openai-images" | "openai_images" => Ok(ADAPTER_OPENAI_IMAGES),
        "openai-edit" | "openai-images-edit" | "openai_images_edit" => {
            Ok(ADAPTER_OPENAI_IMAGES_EDIT)
        }
        "ark" | "ark-images" | "ark_images" | "volcengine" | "doubao" | "seedream" => {
            Ok(ADAPTER_ARK_IMAGES)
        }
        other => Err(format!(
            "不支持的媒体适配器：{other}；可选 openai-images、openai-images-edit、ark-images"
        )
        .into()),
    }
}

/// 图生图 / 改图的参考图。
pub(crate) struct InputImage {
    pub(crate) name: String,
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
}

/// 解析参考图：`media:<id>` 走媒体登记表，否则按本机绝对路径读取。
///
/// 当会话工作区可知时，参考图必须位于该工作区内，避免把工作区外的任意文件
/// 悄悄发给第三方模型。
pub(crate) fn resolve_input_image(reference: &str) -> Result<InputImage, Box<dyn Error>> {
    let reference = reference.trim();
    let (path, source) = match crate::store::media_registry::resolve_reference(reference)? {
        Some(record) => (PathBuf::from(&record.path), format!("media:{}", record.id)),
        None => (PathBuf::from(reference), reference.to_string()),
    };
    if !path.is_absolute() {
        return Err(format!("参考图必须是绝对路径或 media:<id>：{reference}").into());
    }
    if let Some(workspace) = crate::extension_workspace::session_root() {
        let resolved = crate::path_guard::resolve_real_path(&path)?;
        if !crate::path_guard::path_is_within(&workspace, &resolved) {
            return Err(format!(
                "参考图必须位于当前会话工作区内：{}（如需工作区外素材请先用 media.asset.import 登记）",
                path.display()
            )
            .into());
        }
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("读取参考图失败：{}（{error}）", path.display()))?;
    if bytes.len() > MAX_INPUT_IMAGE_BYTES {
        return Err(format!(
            "参考图 {} 超过 {} MiB 上限",
            source,
            MAX_INPUT_IMAGE_BYTES / (1024 * 1024)
        )
        .into());
    }
    let mime = sniff_mime(&bytes).to_string();
    if !mime.starts_with("image/") {
        return Err(format!("参考图 {source} 不是受支持的图片格式（PNG/JPEG/WEBP/GIF）").into());
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("input.png")
        .to_string();
    Ok(InputImage { name, mime, bytes })
}

pub(crate) fn data_url(image: &InputImage) -> String {
    format!(
        "data:{};base64,{}",
        image.mime,
        base64::engine::general_purpose::STANDARD.encode(&image.bytes)
    )
}

/// 按适配器执行图生图：方舟走 JSON（`image` 字段），OpenAI 走 multipart edits。
fn call_edit_api(
    base_url: &str,
    adapter: &str,
    api_key: &str,
    service_id: &str,
    model: &str,
    request: &ImageRequest,
    output_dir: &Path,
) -> Result<Value, Box<dyn Error>> {
    if api_key.trim().is_empty() {
        return Err("所选本机 AI 服务没有可用的 API Key".into());
    }
    let adapter = normalize_image_adapter(adapter)?;
    let images = request
        .input_images
        .iter()
        .map(|reference| resolve_input_image(reference))
        .collect::<Result<Vec<_>, _>>()?;
    let client = Client::builder().timeout(request_timeout()).build()?;

    match adapter {
        ADAPTER_OPENAI_IMAGES_EDIT => {
            if images.len() != 1 {
                return Err(
                    "openai-images-edit 只接受 1 张参考图；多图改图请使用 ark-images".into(),
                );
            }
            let mut form = reqwest::blocking::multipart::Form::new()
                .text("model", model.to_string())
                .text("prompt", request.prompt.clone())
                .text("n", request.n.to_string());
            if let Some(size) = &request.size {
                form = form.text("size", size.clone());
            }
            if let Some(quality) = &request.quality {
                form = form.text("quality", quality.clone());
            }
            let image = &images[0];
            let part = reqwest::blocking::multipart::Part::bytes(image.bytes.clone())
                .file_name(image.name.clone())
                .mime_str(&image.mime)?;
            form = form.part("image", part);
            if let Some(mask) = request.mask.as_deref() {
                let mask = resolve_input_image(mask)?;
                let part = reqwest::blocking::multipart::Part::bytes(mask.bytes.clone())
                    .file_name(mask.name.clone())
                    .mime_str(&mask.mime)?;
                form = form.part("mask", part);
            }
            let url = images_edit_url(base_url)?;
            let response = client
                .post(&url)
                .bearer_auth(api_key.trim())
                .multipart(form)
                .send()
                .map_err(|error| format!("调用图生图接口失败：{error}"))?;
            let status = response.status();
            let payload = read_json(response)?;
            if !status.is_success() {
                return Err(format!("图生图接口返回 {status}：{}", describe_error(&payload)).into());
            }
            materialize_outputs(
                &client,
                &payload,
                request,
                output_dir,
                CAP_IMAGE_EDIT,
                service_id,
                model,
                base_url,
                "已生成",
            )
        }
        ADAPTER_ARK_IMAGES => {
            let mut body = request.parameters.clone().unwrap_or_default();
            body.insert("model".to_string(), json!(model));
            body.insert("prompt".to_string(), json!(request.prompt));
            body.insert("n".to_string(), json!(request.n));
            body.insert("response_format".to_string(), json!("b64_json"));
            if let Some(size) = &request.size {
                body.insert("size".to_string(), json!(size));
            }
            if let Some(quality) = &request.quality {
                body.insert("quality".to_string(), json!(quality));
            }
            body.entry("watermark".to_string())
                .or_insert_with(|| json!(false));
            // 方舟用 `image` 承载参考图，支持单张字符串或多张数组。
            let encoded = images.iter().map(data_url).collect::<Vec<_>>();
            body.insert(
                "image".to_string(),
                if encoded.len() == 1 {
                    json!(encoded[0])
                } else {
                    json!(encoded)
                },
            );
            let url = images_url_for(adapter, base_url)?;
            let mut payload = Value::Object(body);
            let (mut status, mut body_value) = post_images(&client, &url, api_key, &payload)?;
            if !status.is_success() && rejects_response_format(&body_value) {
                if let Some(object) = payload.as_object_mut() {
                    object.remove("response_format");
                }
                let retry = post_images(&client, &url, api_key, &payload)?;
                status = retry.0;
                body_value = retry.1;
            }
            if !status.is_success() {
                return Err(format!("图生图接口返回 {status}：{}", describe_error(&body_value)).into());
            }
            materialize_outputs(
                &client,
                &body_value,
                request,
                output_dir,
                CAP_IMAGE_EDIT,
                service_id,
                model,
                base_url,
                "已生成",
            )
        }
        other => Err(format!("适配器 {other} 暂不支持图生图（image.edit）").into()),
    }
}

/// `{base_url}/v1/images/edits`；`base_url` 已经带 `/v1` 时不重复追加。
fn images_edit_url(base_url: &str) -> Result<String, Box<dyn Error>> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("AI 服务 base_url 为空".into());
    }
    let parsed = url::Url::parse(base).map_err(|_| "AI 服务 base_url 不是合法 URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("AI 服务 base_url 仅支持 http/https".into());
    }
    let root = if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{base}/v1")
    };
    Ok(format!("{root}/images/edits"))
}

/// OpenAI Images 协议地址（保留给旧调用点）：`{base_url}/v1/images/generations`。
fn images_url(base_url: &str) -> Result<String, Box<dyn Error>> {
    images_url_for(ADAPTER_OPENAI_IMAGES, base_url)
}

/// 按适配器推导生图地址；`base_url` 已经带协议根时不重复追加。
fn images_url_for(adapter: &str, base_url: &str) -> Result<String, Box<dyn Error>> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("AI 服务 base_url 为空".into());
    }
    let parsed = url::Url::parse(base).map_err(|_| "AI 服务 base_url 不是合法 URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("AI 服务 base_url 仅支持 http/https".into());
    }
    let root = match adapter {
        ADAPTER_ARK_IMAGES => {
            if base.ends_with("/api/v3") {
                base.to_string()
            } else {
                format!("{base}/api/v3")
            }
        }
        _ => {
            if base.ends_with("/v1") {
                base.to_string()
            } else {
                format!("{base}/v1")
            }
        }
    };
    Ok(format!("{root}/images/generations"))
}

fn provider_label(base_url: &str) -> String {
    url::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.trim().to_string())
}

fn request_timeout() -> Duration {
    let seconds = std::env::var("HIMIND_LOCAL_IMAGE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| (10..=900).contains(value))
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
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

fn post_images(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &Value,
) -> Result<(reqwest::StatusCode, Value), Box<dyn Error>> {
    let response = client
        .post(url)
        .bearer_auth(api_key.trim())
        .json(body)
        .send()
        .map_err(|error| format!("调用生图接口失败：{error}"))?;
    let status = response.status();
    let payload = read_json(response)?;
    Ok((status, payload))
}

/// 判断 4xx 错误是否只因为 provider 不认识 `response_format`。
fn rejects_response_format(payload: &Value) -> bool {
    let message = payload
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    message.contains("response_format") || message.contains("response format")
}

fn read_json(response: reqwest::blocking::Response) -> Result<Value, Box<dyn Error>> {
    let bytes = read_limited(response, MAX_RESPONSE_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("生图响应解析失败：{error}").into())
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

fn decode_base64(value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| format!("生图返回的 base64 无法解码：{error}").into())
}

fn download_image(client: &Client, raw_url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let parsed = url::Url::parse(raw_url).map_err(|_| "生图返回的图片 URL 无效")?;
    if parsed.scheme() != "https" && !is_loopback_host(parsed.host_str()) {
        return Err("生图返回的图片 URL 必须使用 HTTPS（环回地址除外）".into());
    }
    let response = client
        .get(parsed)
        .send()
        .map_err(|error| format!("下载生成图片失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载生成图片失败：HTTP {}", response.status()).into());
    }
    read_limited(response, MAX_IMAGE_BYTES as u64)
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("127.0.0.1") | Some("localhost") | Some("::1"))
}

fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else {
        "application/octet-stream"
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 只读 PNG IHDR 与 JPEG SOF 的宽高；其他格式返回 `None`，不影响生图本身。
fn image_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return (Some(width), Some(height));
    }
    if bytes.len() > 4 && bytes.starts_with(&[0xff, 0xd8]) {
        let mut index = 2usize;
        while index + 9 < bytes.len() {
            if bytes[index] != 0xff {
                index += 1;
                continue;
            }
            let marker = bytes[index + 1];
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                index += 2;
                continue;
            }
            let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
            if length < 2 {
                break;
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
                let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]) as u32;
                return (Some(width), Some(height));
            }
            index += 2 + length;
        }
    }
    (None, None)
}

fn output_file_name(stem: Option<&str>, index: usize, extension: &str) -> String {
    let stem = stem
        .map(sanitize_stem)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "image".to_string());
    let mut random = [0u8; 4];
    OsRng.fill_bytes(&mut random);
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{stem}-{index}-{suffix}.{extension}")
}

fn sanitize_stem(value: &str) -> String {
    let mut result = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            result.push(ch);
            last_dash = false;
        } else if !last_dash {
            result.push('-');
            last_dash = true;
        }
        if result.chars().count() >= 48 {
            break;
        }
    }
    result.trim_matches('-').to_string()
}

/// 显式 `output_dir` 优先；否则落到工作区 `.himind/media`，再退回 Agent 数据目录 `media`。
fn resolve_output_dir(explicit: Option<&str>) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(raw) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err("output_dir 必须是绝对路径".into());
        }
        if is_blocked_output_dir(&path) {
            return Err(format!("拒绝写入系统目录：{}", path.display()).into());
        }
        fs::create_dir_all(&path)
            .map_err(|error| format!("创建 output_dir 失败：{}（{error}）", path.display()))?;
        return Ok(path);
    }
    let base = crate::extension_workspace::session_root()
        .map(|root| root.join(WORKSPACE_MEDIA_DIR).join(WORKSPACE_MEDIA_SUBDIR))
        .unwrap_or_else(|| crate::store::paths::agent_home().join(AGENT_MEDIA_SUBDIR));
    fs::create_dir_all(&base)
        .map_err(|error| format!("创建图片目录失败：{}（{error}）", base.display()))?;
    Ok(base)
}

fn is_blocked_output_dir(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    let normalized = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let trimmed = normalized.trim_end_matches('\\');
    trimmed.ends_with(":\\")
        || trimmed.contains("\\windows")
        || trimmed.contains("\\program files")
        || trimmed.contains("\\program files (x86)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use std::net::TcpListener;
    use std::thread;

    /// 1x1 PNG，用于在测试里冒充 provider 返回的 `b64_json`。
    const TINY_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    /// 读取请求直到客户端停止发送为止。
    ///
    /// 服务端若在请求体还没读完时就关连接，Windows 会回 RST，客户端可能拿不到
    /// 响应体（表现为空 body）。测试里显式把请求读干净再回包，避免偶发失败。
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
        let mut buffer = [0u8; 8192];
        let mut text = String::new();
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                Err(_) => break,
            }
        }
        text
    }

    fn drain_request(stream: &mut std::net::TcpStream) {
        let _ = read_request(stream);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "himind-local-image-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fake_provider(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                drain_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{address}/v1")
    }

    #[test]
    fn images_url_appends_v1_once() {
        assert_eq!(
            images_url("https://api.example.com").unwrap(),
            "https://api.example.com/v1/images/generations"
        );
        assert_eq!(
            images_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/images/generations"
        );
        assert!(images_url("file:///tmp/model").is_err());
    }

    #[test]
    fn request_requires_prompt_and_bounds_count() {
        assert!(ImageRequest::parse(&json!({})).is_err());
        assert!(ImageRequest::parse(&json!({ "prompt": "图", "n": 9 })).is_err());
        let request = ImageRequest::parse(&json!({ "prompt": "图" })).unwrap();
        assert_eq!(request.n, 1);
        assert!(request.inline);
    }

    #[test]
    fn inline_content_is_emitted_for_generated_image() {
        let body = format!("{{\"data\":[{{\"b64_json\":\"{TINY_PNG_B64}\"}}]}}");
        let body: &'static str = Box::leak(body.into_boxed_str());
        let base_url = fake_provider(body);
        let output_dir = temp_dir("inline");
        let request = ImageRequest::parse(&json!({ "prompt": "一只猫" })).unwrap();
        let result = call_generate_api(
            &base_url,
            ADAPTER_OPENAI_IMAGES,
            "test-key",
            "svc_1",
            "gpt-image-1",
            &request,
            &output_dir,
        )
        .unwrap();

        assert_eq!(result["structuredContent"]["mode"], "local");
        assert_eq!(result["structuredContent"]["model"], "gpt-image-1");
        let file = &result["structuredContent"]["files"][0];
        assert_eq!(file["mime_type"], "image/png");
        assert_eq!(file["width"], 1);
        assert_eq!(file["height"], 1);
        let path = PathBuf::from(file["path"].as_str().unwrap());
        assert!(path.is_file());
        assert_eq!(fs::read(&path).unwrap(), decode_base64(TINY_PNG_B64).unwrap());
        assert_eq!(result["content"][1]["type"], "image");
        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn inline_can_be_disabled_and_files_still_written() {
        let body = format!("{{\"data\":[{{\"b64_json\":\"{TINY_PNG_B64}\"}}]}}");
        let body: &'static str = Box::leak(body.into_boxed_str());
        let base_url = fake_provider(body);
        let output_dir = temp_dir("plain");
        let request = ImageRequest::parse(&json!({ "prompt": "一只猫", "inline": false })).unwrap();
        let result = call_generate_api(
            &base_url,
            ADAPTER_OPENAI_IMAGES,
            "test-key",
            "svc_1",
            "gpt-image-1",
            &request,
            &output_dir,
        )
        .unwrap();
        assert_eq!(result["mode"], "local");
        assert!(result.get("content").is_none());
        let path = PathBuf::from(result["files"][0]["path"].as_str().unwrap());
        assert!(path.is_file());
        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn adapter_aliases_normalize_to_known_values() {
        assert_eq!(normalize_image_adapter("").unwrap(), ADAPTER_OPENAI_IMAGES);
        assert_eq!(
            normalize_image_adapter("OpenAI-Images").unwrap(),
            ADAPTER_OPENAI_IMAGES
        );
        assert_eq!(normalize_image_adapter("doubao").unwrap(), ADAPTER_ARK_IMAGES);
        assert_eq!(
            normalize_image_adapter("ark").unwrap(),
            ADAPTER_ARK_IMAGES
        );
        assert!(normalize_image_adapter("midjourney").is_err());
    }

    #[test]
    fn ark_adapter_targets_v3_images_and_defaults_watermark_off() {
        let body = format!("{{\"data\":[{{\"b64_json\":\"{TINY_PNG_B64}\"}}]}}");
        let body: &'static str = Box::leak(body.into_boxed_str());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                *sink.lock().unwrap() = read_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("ark-home");
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let output_dir = temp_dir("ark");
        let request =
            ImageRequest::parse(
                &json!({ "prompt": "一只猫", "inline": false, "parameters": { "watermark": true } }),
            )
                .unwrap();
        let result = call_generate_api(
            &format!("http://{address}"),
            ADAPTER_ARK_IMAGES,
            "test-key",
            "svc_ark",
            "doubao-seedream-4-0",
            &request,
            &output_dir,
        )
        .unwrap();

        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let text = captured.lock().unwrap().clone();
        assert!(text.contains("POST /api/v3/images/generations"), "{text}");
        // 调用方显式给的 watermark 优先于默认值。
        assert!(text.contains("\"watermark\":true"), "{text}");
        assert!(PathBuf::from(result["files"][0]["path"].as_str().unwrap()).is_file());
        let _ = fs::remove_dir_all(&output_dir);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ark_edit_sends_the_reference_image_and_registers_media_id() {
        let body = format!("{{\"data\":[{{\"b64_json\":\"{TINY_PNG_B64}\"}}]}}");
        let body: &'static str = Box::leak(body.into_boxed_str());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                *sink.lock().unwrap() = read_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("edit-home");
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &home);

        let output_dir = temp_dir("edit-out");
        let source_dir = temp_dir("edit-src");
        let source = source_dir.join("ref.png");
        fs::write(&source, decode_base64(TINY_PNG_B64).unwrap()).unwrap();
        let request = ImageRequest::parse(&json!({
            "prompt": "把背景换成夜景",
            "input_images": [source.to_string_lossy()],
            "inline": false,
        }))
        .unwrap();
        let result = call_edit_api(
            &format!("http://{address}"),
            ADAPTER_ARK_IMAGES,
            "test-key",
            "svc_ark",
            "doubao-seedream-5-0-pro",
            &request,
            &output_dir,
        )
        .unwrap();

        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }

        let text = captured.lock().unwrap().clone();
        assert!(text.contains("POST /api/v3/images/generations"), "{text}");
        assert!(text.contains(r#""image":"#), "{text}");
        assert!(text.contains("data:image/png;base64,"), "{text}");
        assert_eq!(result["capability"], "image.edit");
        let media_id = result["files"][0]["media_id"].as_str().unwrap();
        assert!(media_id.starts_with("media_"), "{media_id}");
        assert!(PathBuf::from(result["files"][0]["path"].as_str().unwrap()).is_file());
        let _ = fs::remove_dir_all(&output_dir);
        let _ = fs::remove_dir_all(&source_dir);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn relative_output_dir_is_rejected() {
        assert!(resolve_output_dir(Some("images")).is_err());
        assert!(resolve_output_dir(Some("C:\\Windows\\Temp")).is_err());
    }

    #[test]
    fn retries_without_response_format_when_provider_rejects_it() {
        const REJECT: &str = "{\"error\":{\"message\":\"Unknown parameter: 'response_format'.\"}}";
        const SUCCESS: &str = "{\"data\":[]}";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for (index, body) in [REJECT, SUCCESS].into_iter().enumerate() {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                drain_request(&mut stream);
                let status = if index == 0 { "400 Bad Request" } else { "200 OK" };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        let output_dir = temp_dir("retry");
        let request = ImageRequest::parse(&json!({ "prompt": "图" })).unwrap();
        let error = call_generate_api(
            &format!("http://{address}/v1"),
            ADAPTER_OPENAI_IMAGES,
            "test-key",
            "svc_1",
            "gpt-image-1",
            &request,
            &output_dir,
        )
        .unwrap_err()
        .to_string();
        // 第二次请求已经去掉 response_format；这里用空 data 结束，证明重试真的发生了。
        assert!(error.contains("没有返回任何图片"), "{error}");
        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn provider_errors_are_surfaced() {
        let body: &'static str = "{\"error\":{\"message\":\"model not found\"}}";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                drain_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        let output_dir = temp_dir("error");
        let request = ImageRequest::parse(&json!({ "prompt": "图" })).unwrap();
        let error = call_generate_api(
            &format!("http://{address}/v1"),
            ADAPTER_OPENAI_IMAGES,
            "test-key",
            "svc_1",
            "unknown-model",
            &request,
            &output_dir,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("model not found"), "{error}");
        let _ = fs::remove_dir_all(&output_dir);
    }
}
