//! AI capability execution envelope and route diagnostics.
//!
//! The bus keeps the public media payloads backward-compatible while adding one
//! stable envelope: selected route, attempts, request ids, usage and artefacts.
//! Provider-specific HTTP stays in the existing image/video/speech adapters.

use serde_json::{json, Map, Value};
use std::error::Error;

use crate::store::ai_services::{
    self, CAP_AUDIO_SPEECH, CAP_AUDIO_TRANSCRIBE, CAP_CHAT, CAP_IMAGE_EDIT, CAP_IMAGE_GENERATE,
    CAP_VIDEO_GENERATE,
};

const SCHEMA_VERSION: &str = "himind.ai-capability-result.v1";

fn push_invalid(checks: &mut Vec<Value>, id: &str, message: &str) {
    checks.push(json!({"id": id, "ok": false, "message": message}));
}

fn string_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn request_capability(input: &Value) -> Result<&str, Box<dyn Error>> {
    let capability = string_field(input, "capability").ok_or("capability 不能为空")?;
    if !ai_services::SUPPORTED_AI_CAPABILITIES.contains(&capability) {
        return Err(format!(
            "不支持的能力：{capability}；可选 {}",
            ai_services::SUPPORTED_AI_CAPABILITIES.join("、")
        )
        .into());
    }
    Ok(capability)
}

fn requested_service(input: &Value) -> &str {
    string_field(input, "service").unwrap_or_default()
}

/// Return the same explainable route model used by the settings UI.
pub(crate) fn route_plan(input: &Value) -> Result<Value, Box<dyn Error>> {
    let capability = request_capability(input)?;
    ai_services::capability_route_plan(capability, requested_service(input))
}

fn adapter_limits(adapter: &str) -> Value {
    ai_services::builtin_media_adapters()
        .into_iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(adapter))
        .and_then(|item| item.get("limits").cloned())
        .unwrap_or_else(|| json!({}))
}

fn limit_u64(limits: &Value, key: &str) -> Option<u64> {
    limits.get(key).and_then(Value::as_u64)
}

fn limit_range(limits: &Value, key: &str) -> Option<(f64, f64)> {
    let values = limits.get(key)?.as_array()?;
    Some((values.first()?.as_f64()?, values.get(1)?.as_f64()?))
}

/// Local, non-billable eligibility checks. This never calls the provider.
pub(crate) fn preflight(capability: &str, service_id: &str, payload: &Value) -> Value {
    let mut checks = Vec::new();
    let service = match ai_services::load_secret(service_id) {
        Ok((service, api_key)) => {
            checks.push(json!({"id": "credential", "ok": !api_key.trim().is_empty(), "message": if api_key.trim().is_empty() { "缺少 API Key" } else { "凭据已保存" }}));
            service
        }
        Err(error) => {
            return json!({
                "state": "invalid",
                "checks": [{"id": "service", "ok": false, "message": error.to_string()}],
            });
        }
    };
    checks.push(json!({"id": "service", "ok": true, "message": service.display_name}));
    let mut limits = json!({});
    if capability == CAP_CHAT {
        let model = string_field(payload, "model").unwrap_or(service.model.trim());
        checks.push(json!({"id": "model", "ok": !model.is_empty(), "message": if model.is_empty() { "未配置对话模型" } else { model }}));
    } else {
        match ai_services::binding_details_for(&service, capability) {
            Some(binding) => {
                limits = adapter_limits(&binding.adapter);
                checks.push(json!({"id": "binding", "ok": true, "message": binding.adapter}));
                checks.push(json!({"id": "model", "ok": !binding.model.is_empty(), "message": binding.model}));
                let modes = json!([binding.execution_mode]);
                checks.push(json!({"id": "execution_mode", "ok": true, "message": modes}));
                checks
                    .push(json!({"id": "limits_source", "ok": true, "message": "adapter_catalog"}));
            }
            None => checks.push(json!({
                "id": "binding",
                "ok": false,
                "message": format!("服务「{}」未绑定能力 {capability}", service.display_name),
            })),
        }
    }

    let object = payload.as_object();
    match capability {
        CAP_IMAGE_GENERATE | CAP_IMAGE_EDIT => {
            let prompt = string_field(payload, "prompt").unwrap_or_default();
            if prompt.is_empty() {
                push_invalid(&mut checks, "prompt", "prompt 不能为空");
            } else if prompt.chars().count() as u64
                > limit_u64(&limits, "prompt_chars").unwrap_or(8_000)
            {
                push_invalid(
                    &mut checks,
                    "prompt",
                    &format!(
                        "prompt 超过 {} 字符",
                        limit_u64(&limits, "prompt_chars").unwrap_or(8_000)
                    ),
                );
            } else {
                checks.push(json!({"id": "prompt", "ok": true, "message": format!("{} 字符", prompt.chars().count())}));
            }
            if capability == CAP_IMAGE_GENERATE {
                let n = payload.get("n").and_then(Value::as_u64).unwrap_or(1);
                if let Some((min, max)) = limit_range(&limits, "n") {
                    if (n as f64) < min || (n as f64) > max {
                        push_invalid(&mut checks, "n", &format!("图片数量必须在 {min}-{max}"));
                    }
                }
            }
            if capability == CAP_IMAGE_EDIT {
                let count = object
                    .and_then(|value| value.get("input_images"))
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                let max = limit_u64(&limits, "input_images").unwrap_or(4) as usize;
                if count == 0 || count > max {
                    push_invalid(
                        &mut checks,
                        "input_images",
                        &format!("改图需要 1-{max} 张参考图"),
                    );
                } else {
                    checks.push(json!({"id": "input_images", "ok": true, "message": count}));
                }
            }
        }
        CAP_VIDEO_GENERATE => {
            let prompt = string_field(payload, "prompt").unwrap_or_default();
            if prompt.is_empty() {
                push_invalid(&mut checks, "prompt", "prompt 不能为空");
            } else if prompt.chars().count() as u64
                > limit_u64(&limits, "prompt_chars").unwrap_or(8_000)
            {
                push_invalid(
                    &mut checks,
                    "prompt",
                    &format!(
                        "prompt 超过 {} 字符",
                        limit_u64(&limits, "prompt_chars").unwrap_or(8_000)
                    ),
                );
            } else {
                checks.push(json!({"id": "prompt", "ok": true, "message": format!("{} 字符", prompt.chars().count())}));
            }
            let count = object
                .and_then(|value| value.get("input_images"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let max_images = limit_u64(&limits, "input_images").unwrap_or(2) as usize;
            if count > max_images {
                push_invalid(
                    &mut checks,
                    "input_images",
                    &format!("视频参考图最多 {max_images} 张"),
                );
            }
            if let Some(duration) = payload.get("duration_seconds").and_then(Value::as_u64) {
                if let Some((min, max)) = limit_range(&limits, "duration_seconds") {
                    if (duration as f64) < min || (duration as f64) > max {
                        push_invalid(
                            &mut checks,
                            "duration_seconds",
                            &format!("视频时长必须在 {min}-{max} 秒"),
                        );
                    }
                }
            }
        }
        CAP_AUDIO_SPEECH => {
            let text = string_field(payload, "text").unwrap_or_default();
            if text.is_empty() {
                push_invalid(&mut checks, "text", "text 不能为空");
            } else if text.chars().count() as u64
                > limit_u64(&limits, "text_chars").unwrap_or(4_096)
            {
                push_invalid(
                    &mut checks,
                    "text",
                    &format!(
                        "text 超过 {} 字符",
                        limit_u64(&limits, "text_chars").unwrap_or(4_096)
                    ),
                );
            } else {
                checks.push(json!({"id": "text", "ok": true, "message": format!("{} 字符", text.chars().count())}));
            }
            if let Some(speed) = payload.get("speed").and_then(Value::as_f64) {
                if let Some((min, max)) = limit_range(&limits, "speed") {
                    if speed < min || speed > max {
                        push_invalid(&mut checks, "speed", &format!("speed 必须在 {min}-{max}"));
                    }
                }
            }
        }
        CAP_AUDIO_TRANSCRIBE => {
            if let Some(prompt) = string_field(payload, "prompt") {
                let max = limit_u64(&limits, "prompt_chars").unwrap_or(2_000);
                if prompt.chars().count() as u64 > max {
                    push_invalid(&mut checks, "prompt", &format!("prompt 超过 {max} 字符"));
                }
            }
            let file = string_field(payload, "file").unwrap_or_default();
            let path = std::path::Path::new(file);
            if file.is_empty() {
                push_invalid(&mut checks, "file", "转写需要音频文件");
            } else if !path.is_absolute() || !path.is_file() {
                push_invalid(&mut checks, "file", "音频文件必须是存在的绝对路径");
            } else {
                checks.push(json!({"id": "file", "ok": true, "message": file}));
            }
        }
        _ => {}
    }
    let ready = checks
        .iter()
        .all(|item| item.get("ok").and_then(Value::as_bool).unwrap_or(false));
    json!({
        "state": if ready { "ready" } else { "invalid" },
        "service_id": service_id,
        "checks": checks,
    })
}

fn error_kind(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("media_provider_auth") || lower.contains("401") || lower.contains("403") {
        "authentication"
    } else if lower.contains("media_provider_quota")
        || lower.contains("quota")
        || lower.contains("insufficient")
    {
        "quota"
    } else if lower.contains("media_provider_rate_limit")
        || lower.contains("429")
        || lower.contains("rate limit")
    {
        "rate_limit"
    } else if lower.contains("media_provider_content_policy")
        || lower.contains("content policy")
        || lower.contains("safety")
        || lower.contains("moderation")
    {
        "content_policy"
    } else if lower.contains("timeout") || lower.contains("timed out") || lower.contains("connect")
    {
        "transport"
    } else if lower.contains("invalid") || lower.contains("不能为空") || lower.contains("必须是")
    {
        "invalid_request"
    } else {
        "provider_error"
    }
}

fn retryable(kind: &str) -> bool {
    matches!(
        kind,
        "quota" | "rate_limit" | "transport" | "provider_error"
    )
}

fn local_request_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    format!("cap-{now:x}")
}

fn with_service(input: &Value, service_id: &str) -> Value {
    let mut object = input.as_object().cloned().unwrap_or_default();
    object.insert("service".to_string(), json!(format!("custom:{service_id}")));
    object.remove("capability");
    Value::Object(object)
}

fn invoke(capability: &str, payload: &Value) -> Result<Value, Box<dyn Error>> {
    match capability {
        CAP_IMAGE_GENERATE => crate::app::local_image::generate(payload),
        CAP_IMAGE_EDIT => crate::app::local_image::edit(payload),
        CAP_VIDEO_GENERATE => crate::app::local_video::generate(payload),
        CAP_AUDIO_SPEECH => crate::app::local_speech::speech(payload),
        CAP_AUDIO_TRANSCRIBE => crate::app::local_speech::transcribe(payload),
        _ => Err(format!("能力 {capability} 当前没有可执行 adapter").into()),
    }
}

fn artifacts(value: &Value) -> Vec<Value> {
    if let Some(items) = value.get("outputs").and_then(Value::as_array) {
        return items.clone();
    }
    if let Some(items) = value.get("files").and_then(Value::as_array) {
        return items.clone();
    }
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        return vec![
            json!({"path": path, "mime": value.get("mime").cloned(), "bytes": value.get("bytes").cloned(), "sha256": value.get("sha256").cloned()}),
        ];
    }
    Vec::new()
}

fn decorate(
    capability: &str,
    service_id: &str,
    request_id: &str,
    attempts: &[Value],
    fallback_used: bool,
    value: Value,
) -> Value {
    let provider_request_id = value
        .get("provider_request_id")
        .or_else(|| value.get("request_id"))
        .or_else(|| value.get("job_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let usage = value
        .get("usage")
        .cloned()
        .unwrap_or_else(|| json!({"state": "unknown", "source": "provider_not_reported"}));
    let mut object = value.as_object().cloned().unwrap_or_else(|| {
        let mut map = Map::new();
        map.insert("result".to_string(), value.clone());
        map
    });
    object.insert("schema_version".to_string(), json!(SCHEMA_VERSION));
    object.insert("request_id".to_string(), json!(request_id));
    object.insert("capability".to_string(), json!(capability));
    object.insert("service_id".to_string(), json!(service_id));
    object.insert("provider_request_id".to_string(), provider_request_id);
    object.insert("usage".to_string(), usage);
    object.insert("artifacts".to_string(), json!(artifacts(&value)));
    object.insert("attempts".to_string(), json!(attempts));
    object.insert("fallback_used".to_string(), json!(fallback_used));
    object.insert("warnings".to_string(), json!([]));
    Value::Object(object)
}

/// Execute one media capability with an explicit, same-capability fallback path.
pub(crate) fn execute_media(capability: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    let plan = ai_services::capability_route_plan(capability, requested_service(input))?;
    let attempts = plan
        .get("attempts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if attempts.is_empty() {
        return Err(format!(
            "能力 {capability} 没有可执行的候选服务；路由状态：{}",
            plan.get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )
        .into());
    }
    let request_id = local_request_id();
    let mut attempt_log = Vec::new();
    let mut last_error = String::new();
    for (index, attempt) in attempts.iter().enumerate() {
        let service_id = attempt
            .get("service_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let preflight_result = preflight(capability, service_id, input);
        if preflight_result.get("state").and_then(Value::as_str) != Some("ready") {
            let messages = preflight_result
                .get("checks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|check| check.get("ok").and_then(Value::as_bool) == Some(false))
                .filter_map(|check| check.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("；");
            return Err(format!("能力预检失败：{messages}").into());
        }
        attempt_log.push(json!({"service_id": service_id, "state": "running"}));
        match invoke(capability, &with_service(input, service_id)) {
            Ok(result) => {
                if let Some(last) = attempt_log.last_mut() {
                    last["state"] = json!("succeeded");
                }
                return Ok(decorate(
                    capability,
                    service_id,
                    &request_id,
                    &attempt_log,
                    index > 0,
                    result,
                ));
            }
            Err(error) => {
                last_error = error.to_string();
                let kind = error_kind(&last_error);
                let can_retry = retryable(kind) && index + 1 < attempts.len();
                if let Some(last) = attempt_log.last_mut() {
                    last["state"] = json!(if can_retry {
                        "failed_retryable"
                    } else {
                        "failed"
                    });
                    last["error_kind"] = json!(kind);
                    last["error"] = json!(last_error);
                }
                if !can_retry {
                    return Err(last_error.into());
                }
            }
        }
    }
    Err(last_error.into())
}

/// Capability-level self check. Billable media calls require explicit consent.
pub(crate) fn test(input: &Value) -> Result<Value, Box<dyn Error>> {
    let capability = request_capability(input)?;
    let plan = ai_services::capability_route_plan(capability, requested_service(input))?;
    let selected = plan
        .get("selected")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!(
                "能力 {capability} 尚未解析到服务；当前状态 {}",
                plan.get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )
        })?;
    let service_id = selected
        .get("service_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let confirm_cost = input
        .get("confirm_cost")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if capability != CAP_CHAT && !confirm_cost {
        return Ok(json!({
            "state": "confirmation_required",
            "capability": capability,
            "service_id": service_id,
            "message": "该检查可能产生 Provider 费用；确认后才会发起真实调用。",
            "route": plan,
        }));
    }
    if capability == CAP_CHAT {
        let models = ai_services::list_models(service_id)?;
        return Ok(json!({
            "state": "healthy",
            "capability": capability,
            "service_id": service_id,
            "message": format!("模型接口可用 · {} 个模型", models.len()),
            "models": models,
            "route": plan,
        }));
    }
    let payload = input
        .get("payload")
        .cloned()
        .unwrap_or_else(|| input.clone());
    let preflight_result = preflight(capability, service_id, &payload);
    if preflight_result.get("state").and_then(Value::as_str) != Some("ready") {
        return Ok(
            json!({"state": "needs_input", "capability": capability, "service_id": service_id, "preflight": preflight_result, "route": plan}),
        );
    }
    let result = execute_media(capability, &with_service(&payload, service_id))?;
    Ok(
        json!({"state": "accepted", "capability": capability, "service_id": service_id, "result": result, "route": plan}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ai_services::{
        AIServiceProtocol, CustomAIServiceInput, MediaModelBinding, CAP_VIDEO_GENERATE,
    };
    use serde_json::json;

    fn with_isolated_home<T>(run: impl FnOnce() -> T) -> T {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-capability-bus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
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

    fn video_service(id: &str, adapter: &str, model: &str) {
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_VIDEO_GENERATE.to_string(),
            MediaModelBinding {
                adapter: adapter.to_string(),
                model: model.to_string(),
                execution_mode: None,
            },
        );
        ai_services::upsert(CustomAIServiceInput {
            id: id.to_string(),
            display_name: id.to_string(),
            base_url: "https://video.example".to_string(),
            protocol: AIServiceProtocol::OpenaiChat,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: format!("sk-{id}"),
        })
        .unwrap();
    }

    #[test]
    fn route_plan_explains_same_capability_fallbacks() {
        with_isolated_home(|| {
            video_service("seedance", "ark-video", "doubao-seedance");
            video_service("wan", "wan-video", "wan2.6-i2v-flash");
            ai_services::set_media_service(CAP_VIDEO_GENERATE, "seedance").unwrap();
            ai_services::set_capability_fallbacks(CAP_VIDEO_GENERATE, vec!["wan".to_string()])
                .unwrap();
            let plan = route_plan(&json!({"capability": CAP_VIDEO_GENERATE})).unwrap();
            assert_eq!(plan["state"], "resolved");
            assert_eq!(plan["selected"]["service_id"], "seedance");
            assert_eq!(plan["fallbacks"][0]["service_id"], "wan");
            assert_eq!(plan["attempts"].as_array().unwrap().len(), 2);
            let wan = plan["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["service_id"] == "wan")
                .unwrap();
            assert_eq!(wan["eligibility"], "fallback");
            assert_eq!(wan["adapter"], "wan-video");
        });
    }

    #[test]
    fn preflight_is_local_and_reports_missing_video_prompt() {
        with_isolated_home(|| {
            video_service("wan", "wan-video", "wan2.6-i2v-flash");
            let result = preflight(CAP_VIDEO_GENERATE, "wan", &json!({}));
            assert_eq!(result["state"], "invalid");
            assert!(result["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "prompt" && item["ok"] == false));
            let too_many_images = preflight(
                CAP_VIDEO_GENERATE,
                "wan",
                &json!({"prompt": "test", "input_images": ["a.png", "b.png"]}),
            );
            assert!(too_many_images["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "input_images" && item["ok"] == false));
            assert!(too_many_images["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "limits_source" && item["message"] == "adapter_catalog"));
        });
    }

    #[test]
    fn billable_self_check_requires_confirmation_before_network() {
        with_isolated_home(|| {
            video_service("wan", "wan-video", "wan2.6-i2v-flash");
            ai_services::set_media_service(CAP_VIDEO_GENERATE, "wan").unwrap();
            let result = test(&json!({
                "capability": CAP_VIDEO_GENERATE,
                "confirm_cost": false,
            }))
            .unwrap();
            assert_eq!(result["state"], "confirmation_required");
            assert_eq!(result["service_id"], "wan");
        });
    }
}
