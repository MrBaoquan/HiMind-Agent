//! Responses 请求中的媒体规格化。
//!
//! AI 客户端会把本地图片转换成 `data:image/...;base64,...` 并写回会话历史。
//! 如果原样转发，同一批图片会在每一轮请求中重复出现，很快超过远端网关的
//! 32 MiB 请求体上限。这里在本机网关把原图落到媒体登记表，只保留最近少量
//! 图片的小预览和稳定 `media_id`，从源头控制请求体大小。

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageFormat};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::store::media_registry::{self, MediaRecord, NewMedia};

/// 历史里只有最近这些图片保留内联预览，更早的图片只留引用和元数据。
const RECENT_INLINE_IMAGE_PREVIEWS: usize = 6;
/// 预览图最长边。既保留可读性，又避免把小截图重新放大成请求负担。
const PREVIEW_MAX_DIMENSION: u32 = 768;
const PREVIEW_JPEG_QUALITY: u8 = 72;
const PREVIEW_TARGET_BYTES: usize = 512 * 1024;
/// 低于这个大小且尺寸合理的最近图片保持原样，减少不必要的画质损失。
const KEEP_ORIGINAL_MAX_BYTES: usize = 256 * 1024;
const KEEP_ORIGINAL_MAX_DIMENSION: u32 = 1280;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MediaNormalizationStats {
    pub images_total: usize,
    pub images_offloaded: usize,
    pub images_replaced_with_reference: usize,
    pub inline_bytes_before: usize,
    pub inline_bytes_after: usize,
    pub request_bytes_before: usize,
    pub request_bytes_after: usize,
}

#[derive(Debug)]
pub(crate) struct NormalizedResponsesRequest {
    pub request: Value,
    pub stats: MediaNormalizationStats,
    pub warnings: Vec<String>,
}

struct NormalizeState {
    total_images: usize,
    image_index: usize,
    seen_hashes: HashSet<String>,
    stats: MediaNormalizationStats,
    warnings: Vec<String>,
}

struct ParsedImageData {
    mime_type: String,
    bytes: Vec<u8>,
}

/// 规格化一个 Responses 请求。非 data URL 图片和普通文本不会改变。
pub(crate) fn normalize_request(mut request: Value) -> Result<NormalizedResponsesRequest, String> {
    let before = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    let inline_before = data_url_bytes(&request);
    let total_images = count_data_images(&request);
    let mut state = NormalizeState {
        total_images,
        image_index: 0,
        seen_hashes: HashSet::new(),
        stats: MediaNormalizationStats {
            images_total: total_images,
            inline_bytes_before: inline_before,
            request_bytes_before: before.len(),
            ..Default::default()
        },
        warnings: Vec::new(),
    };
    normalize_value(&mut request, &mut state);
    let after = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    state.stats.inline_bytes_after = data_url_bytes(&request);
    state.stats.request_bytes_after = after.len();
    Ok(NormalizedResponsesRequest {
        request,
        stats: state.stats,
        warnings: state.warnings,
    })
}

fn normalize_value(value: &mut Value, state: &mut NormalizeState) {
    match value {
        Value::Array(items) => {
            let previous = std::mem::take(items);
            let mut next = Vec::with_capacity(previous.len());
            for mut item in previous {
                if is_data_image(&item) {
                    state.image_index += 1;
                    match normalize_image_object(item, state) {
                        Ok(mut parts) => next.append(&mut parts),
                        Err(error) => {
                            state.warnings.push(error);
                            next.push(json!({
                                "type": "input_text",
                                "text": "[HiMind media] 图片数据无法解析，已从请求中移除以避免请求体超限。"
                            }));
                        }
                    }
                } else {
                    normalize_value(&mut item, state);
                    next.push(item);
                }
            }
            *items = next;
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                normalize_value(value, state);
            }
        }
        _ => {}
    }
}

fn normalize_image_object(
    mut image: Value,
    state: &mut NormalizeState,
) -> Result<Vec<Value>, String> {
    let data_url =
        image_url_string(&image).ok_or_else(|| "input_image 缺少可用 image_url".to_string())?;
    let parsed = parse_data_image(data_url)?;
    let sha256 = format!("{:x}", Sha256::digest(&parsed.bytes));
    let format = image::guess_format(&parsed.bytes).ok();
    let decoded = format.and_then(|format| {
        image::load_from_memory_with_format(&parsed.bytes, format)
            .ok()
            .map(|image| (format, image))
    });
    let dimensions = decoded
        .as_ref()
        .map(|(_, image)| image.dimensions())
        .unwrap_or((0, 0));
    let mime_type = decoded
        .as_ref()
        .map(|(format, _)| image_mime(*format).to_string())
        .unwrap_or_else(|| parsed.mime_type.clone());
    let record = store_original(
        &parsed.bytes,
        &mime_type,
        &sha256,
        dimensions.0,
        dimensions.1,
        decoded.as_ref().map(|(format, _)| *format),
    )?;
    state.stats.images_offloaded += 1;

    let recent = state.image_index + RECENT_INLINE_IMAGE_PREVIEWS > state.total_images;
    let duplicate = !state.seen_hashes.insert(sha256);
    if !recent || duplicate {
        state.stats.images_replaced_with_reference += 1;
        return Ok(vec![media_note(
            &record,
            dimensions,
            parsed.bytes.len(),
            true,
        )]);
    }

    let should_preview = parsed.bytes.len() > KEEP_ORIGINAL_MAX_BYTES
        || dimensions.0 > KEEP_ORIGINAL_MAX_DIMENSION
        || dimensions.1 > KEEP_ORIGINAL_MAX_DIMENSION;
    if !should_preview {
        return Ok(vec![
            image,
            media_note(&record, dimensions, parsed.bytes.len(), false),
        ]);
    }

    let Some((format, dynamic)) = decoded else {
        state.stats.images_replaced_with_reference += 1;
        return Ok(vec![media_note(
            &record,
            dimensions,
            parsed.bytes.len(),
            true,
        )]);
    };
    let Some(preview) = cached_preview(&record, &parsed.bytes, format, dynamic)? else {
        state.stats.images_replaced_with_reference += 1;
        return Ok(vec![media_note(
            &record,
            dimensions,
            parsed.bytes.len(),
            true,
        )]);
    };
    set_image_url(
        &mut image,
        &format!(
            "data:image/jpeg;base64,{}",
            BASE64_STANDARD.encode(&preview)
        ),
    );
    if let Some(object) = image.as_object_mut() {
        object.insert("detail".to_string(), json!("low"));
    }
    Ok(vec![
        image,
        media_note(&record, dimensions, parsed.bytes.len(), false),
    ])
}

fn is_data_image(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("input_image")
        && image_url_string(value).is_some_and(|url| url.starts_with("data:image/"))
}

fn image_url_string(value: &Value) -> Option<&str> {
    match value.get("image_url") {
        Some(Value::String(url)) => Some(url),
        Some(Value::Object(object)) => object.get("url").and_then(Value::as_str),
        _ => None,
    }
}

fn set_image_url(value: &mut Value, url: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    match object.get_mut("image_url") {
        Some(Value::String(current)) => *current = url.to_string(),
        Some(Value::Object(current)) => {
            current.insert("url".to_string(), json!(url));
        }
        _ => {
            object.insert("image_url".to_string(), json!(url));
        }
    }
}

fn parse_data_image(value: &str) -> Result<ParsedImageData, String> {
    let rest = value
        .strip_prefix("data:")
        .ok_or_else(|| "图片不是 data URL".to_string())?;
    let (metadata, payload) = rest
        .split_once(',')
        .ok_or_else(|| "图片 data URL 缺少数据段".to_string())?;
    if !metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return Err("图片 data URL 不是 Base64 编码".to_string());
    }
    let mime_type = metadata
        .split(';')
        .next()
        .unwrap_or("image/png")
        .trim()
        .to_ascii_lowercase();
    let bytes = BASE64_STANDARD
        .decode(payload)
        .map_err(|error| format!("图片 Base64 解码失败：{error}"))?;
    Ok(ParsedImageData { mime_type, bytes })
}

fn count_data_images(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_data_images).sum(),
        Value::Object(object) => {
            if is_data_image(value) {
                1
            } else {
                object.values().map(count_data_images).sum()
            }
        }
        _ => 0,
    }
}

fn data_url_bytes(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(data_url_bytes).sum(),
        Value::Object(object) => {
            if is_data_image(value) {
                image_url_string(value).map(str::len).unwrap_or_default()
            } else {
                object.values().map(data_url_bytes).sum()
            }
        }
        _ => 0,
    }
}

fn store_original(
    bytes: &[u8],
    mime_type: &str,
    sha256: &str,
    width: u32,
    height: u32,
    format: Option<ImageFormat>,
) -> Result<MediaRecord, String> {
    if let Some(existing) =
        media_registry::find_by_sha256(sha256).map_err(|error| error.to_string())?
    {
        if Path::new(&existing.path).is_file() {
            return Ok(existing);
        }
    }
    let dir = media_cache_dir();
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let extension = format
        .map(image_extension)
        .unwrap_or_else(|| extension_from_mime(mime_type));
    let path = dir.join(format!("{sha256}.{extension}"));
    if !path.is_file() {
        crate::store::atomic_file::atomic_write(&path, bytes).map_err(|error| error.to_string())?;
    }
    media_registry::register(NewMedia {
        path: &path.to_string_lossy(),
        kind: "image",
        mime_type,
        size_bytes: bytes.len() as u64,
        sha256,
        width,
        height,
        duration_seconds: 0.0,
        capability: "inference.media-normalize",
        service_id: "local-gateway",
        prompt: "AI 客户端会话图片自动外置",
    })
    .map_err(|error| error.to_string())
}

fn cached_preview(
    record: &MediaRecord,
    bytes: &[u8],
    format: ImageFormat,
    image: DynamicImage,
) -> Result<Option<Vec<u8>>, String> {
    let path = preview_cache_path(record, format);
    if let Ok(preview) = std::fs::read(&path) {
        return Ok(Some(preview));
    }
    for (max_dimension, quality) in [
        (PREVIEW_MAX_DIMENSION, PREVIEW_JPEG_QUALITY),
        (512, 66),
        (384, 60),
    ] {
        let Some(preview) = encode_preview(&image, max_dimension, quality)? else {
            continue;
        };
        if preview.len() < bytes.len() || preview.len() <= PREVIEW_TARGET_BYTES {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            crate::store::atomic_file::atomic_write(&path, &preview)
                .map_err(|error| error.to_string())?;
            return Ok(Some(preview));
        }
    }
    Ok(None)
}

fn encode_preview(
    image: &DynamicImage,
    max_dimension: u32,
    quality: u8,
) -> Result<Option<Vec<u8>>, String> {
    let resized = image.thumbnail(max_dimension, max_dimension);
    if resized.width() == 0 || resized.height() == 0 {
        return Ok(None);
    }
    let rgb = resized.to_rgb8();
    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, quality)
        .encode_image(&rgb)
        .map_err(|error| error.to_string())?;
    Ok(Some(output))
}

fn media_note(
    record: &MediaRecord,
    dimensions: (u32, u32),
    bytes: usize,
    reference_only: bool,
) -> Value {
    let action = if reference_only {
        "该历史图片已从内联上下文移除；原图仍在本地媒体登记表中。"
    } else {
        "请求中仅附降采样预览；原图仍在本地媒体登记表中。"
    };
    json!({
        "type": "input_text",
        "text": format!(
            "[HiMind media] media_id={}; {}×{}; {} bytes; {}. 需要读取细节时使用 media:{} 调用本地媒体工具，不要重新内联原始 Base64。",
            record.id,
            dimensions.0,
            dimensions.1,
            bytes,
            action,
            record.id,
        )
    })
}

fn media_cache_dir() -> PathBuf {
    crate::store::paths::agent_home().join("media-inbox")
}

fn preview_cache_path(record: &MediaRecord, format: ImageFormat) -> PathBuf {
    media_cache_dir().join(format!(
        "{}-{}.preview.jpg",
        record.sha256,
        image_extension(format)
    ))
}

fn image_mime(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Gif => "image/gif",
        _ => "application/octet-stream",
    }
}

fn image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        ImageFormat::Gif => "gif",
        _ => "bin",
    }
}

fn extension_from_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use image::{ImageBuffer, Rgb};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn with_isolated_home<T>(run: impl FnOnce() -> T) -> T {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-responses-media-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let result = run();
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
        result
    }

    fn png_data_url(width: u32, height: u32, seed: u8) -> String {
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            let value = (x.wrapping_mul(31) ^ y.wrapping_mul(17) ^ seed as u32) as u8;
            Rgb([value, value.wrapping_add(37), value.wrapping_add(73)])
        });
        let mut bytes = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        format!("data:image/png;base64,{}", BASE64_STANDARD.encode(bytes))
    }

    #[test]
    fn keeps_only_recent_image_previews_and_old_images_become_references() {
        with_isolated_home(|| {
            let input = (0..8)
                .map(|index| {
                    json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_image",
                            "image_url": png_data_url(32, 32, index),
                            "detail": "original"
                        }]
                    })
                })
                .collect::<Vec<_>>();
            let normalized = normalize_request(json!({ "input": input })).unwrap();
            let items = normalized.request["input"].as_array().unwrap();
            assert!(items[0]["content"][0]["type"] == "input_text");
            assert!(items[1]["content"][0]["type"] == "input_text");
            assert!(items[7]["content"][0]["type"] == "input_image");
            assert_eq!(normalized.stats.images_total, 8);
            assert_eq!(normalized.stats.images_offloaded, 8);
            assert!(normalized.stats.request_bytes_after < normalized.stats.request_bytes_before);
        });
    }

    #[test]
    fn large_recent_image_is_replaced_with_smaller_jpeg_preview() {
        with_isolated_home(|| {
            let original = png_data_url(512, 512, 7);
            let request = json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_image",
                        "image_url": original,
                        "detail": "original"
                    }]
                }]
            });
            let before = serde_json::to_vec(&request).unwrap().len();
            let normalized = normalize_request(request).unwrap();
            let after = serde_json::to_vec(&normalized.request).unwrap().len();
            let image_url = normalized.request["input"][0]["content"][0]["image_url"]
                .as_str()
                .unwrap();
            assert!(image_url.starts_with("data:image/jpeg;base64,"));
            assert!(after < before * 3 / 4, "before={before} after={after}");
            assert_eq!(normalized.stats.images_offloaded, 1);
            let records = media_registry::list(10).unwrap();
            assert_eq!(records.len(), 1);
            assert!(Path::new(&records[0].path).is_file());
        });
    }

    #[test]
    fn repeated_image_is_referenced_instead_of_repeating_base64() {
        with_isolated_home(|| {
            let url = png_data_url(64, 64, 3);
            let request = json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_image", "image_url": url.clone() },
                        { "type": "input_image", "image_url": url }
                    ]
                }]
            });
            let normalized = normalize_request(request).unwrap();
            let content = normalized.request["input"][0]["content"]
                .as_array()
                .unwrap();
            assert_eq!(normalized.stats.images_total, 2);
            assert_eq!(normalized.stats.images_replaced_with_reference, 1);
            assert!(content.iter().any(|part| part["type"] == "input_text"));
        });
    }
}
