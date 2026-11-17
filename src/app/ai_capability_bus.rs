//! AI capability execution envelope and route diagnostics.
//!
//! The bus keeps the public media payloads backward-compatible while adding one
//! stable envelope: selected route, attempts, request ids, usage and artefacts.
//! Provider-specific HTTP stays in the existing image/video/speech adapters.

use serde_json::{json, Map, Value};
use std::error::Error;

use crate::store::ai_services::{
    self, CAP_AUDIO_SPEECH, CAP_AUDIO_TRANSCRIBE, CAP_CHAT, CAP_IMAGE_EDIT, CAP_IMAGE_GENERATE,
    CAP_VIDEO_GENERATE, CAP_VISION,
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
    ai_services::normalize_ai_capability(capability)
}

/// Local plan and optional payload preflight; no upstream requests.
pub(crate) fn route_plan(input: &Value) -> Result<Value, Box<dyn Error>> {
    let capability = request_capability(input)?;
    let mut plan = ai_services::capability_selection_plan(capability, input)?;
    if let Some(payload) = input.get("payload") {
        if !payload.is_object() {
            return Err("payload 必须是对象".into());
        }
        let checks = plan["attempts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|attempt| {
                preflight(
                    capability,
                    attempt["service_id"].as_str().unwrap_or_default(),
                    attempt["model"].as_str().unwrap_or_default(),
                    payload,
                )
            })
            .collect::<Vec<_>>();
        plan["preflight"] = json!(checks);
    }
    Ok(plan)
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
///
/// `model` 是本次尝试真正要用的模型（来自路由计划）。为空时回落到服务在该
/// 能力上的默认绑定，与 [`ai_services::capability_models`] 的取值一致。
pub(crate) fn preflight(capability: &str, service_id: &str, model: &str, payload: &Value) -> Value {
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
    let model = model.trim();
    let mut limits = json!({});
    if capability == CAP_CHAT || capability == CAP_VISION {
        let resolved = if model.is_empty() {
            ai_services::capability_models(&service, capability)
                .into_iter()
                .next()
                .unwrap_or_default()
        } else {
            model.to_string()
        };
        checks.push(json!({"id": "model", "ok": !resolved.is_empty(), "message": if resolved.is_empty() { "未配置对话模型" } else { resolved.as_str() }}));
    } else {
        match ai_services::binding_details_for_model(
            &service,
            capability,
            (!model.is_empty()).then_some(model),
        ) {
            Some(binding) => {
                limits = adapter_limits(&binding.adapter);
                checks.push(json!({"id": "binding", "ok": true, "message": binding.adapter}));
                checks.push(json!({"id": "model", "ok": !binding.model.is_empty(), "message": binding.model}));
                let modes = json!([binding.execution_mode]);
                let supported = ai_services::binding_executable(
                    capability,
                    &binding.adapter,
                    &json!(binding.execution_mode),
                );
                checks.push(json!({"id": "execution_mode", "ok": supported, "message": modes}));
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

/// 把一次路由尝试写成对下游 adapter 的请求：服务与模型都显式写入。
///
/// `model` 为空时删掉请求体里的 `model` 键，交回服务的能力绑定去解析
/// （跟随服务默认模型，ADR 0023）。这让「主选 A/x 失败 → 降到 B」时 B
/// 收到的是自己的模型，而不是主选的 x。
fn with_attempt(input: &Value, service_id: &str, model: &str) -> Value {
    let mut object = input.as_object().cloned().unwrap_or_default();
    object.insert("service".to_string(), json!(format!("custom:{service_id}")));
    object.remove("capability");
    object.remove("selection");
    object.remove("selection_source");
    object.remove("fallback");
    if model.trim().is_empty() {
        object.remove("model");
    } else {
        object.insert("model".to_string(), json!(model.trim()));
    }
    Value::Object(object)
}

fn invoke(capability: &str, payload: &Value) -> Result<Value, Box<dyn Error>> {
    match capability {
        CAP_IMAGE_GENERATE => crate::app::local_image::generate(payload),
        CAP_IMAGE_EDIT => crate::app::local_image::edit(payload),
        CAP_VIDEO_GENERATE => crate::app::local_video::generate(payload),
        CAP_AUDIO_SPEECH => crate::app::local_speech::speech(payload),
        CAP_AUDIO_TRANSCRIBE => crate::app::local_speech::transcribe(payload),
        CAP_VISION => crate::app::vision_describe::describe_attempt(payload),
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
    model: &str,
    request_id: &str,
    attempts: &[Value],
    fallback_used: bool,
    value: Value,
) -> Value {
    let payload = value.get("structuredContent").unwrap_or(&value);
    let provider_request_id = payload
        .get("provider_request_id")
        .or_else(|| payload.get("request_id"))
        .or_else(|| payload.get("job_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let usage = payload
        .get("usage")
        .cloned()
        .unwrap_or_else(|| json!({"state": "unknown", "source": "provider_not_reported"}));
    let mut object = value.as_object().cloned().unwrap_or_else(|| {
        let mut map = Map::new();
        map.insert("result".to_string(), value.clone());
        map
    });
    if let Some(payload) = payload.as_object() {
        for (key, value) in payload {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    object.insert("schema_version".to_string(), json!(SCHEMA_VERSION));
    object.insert("request_id".to_string(), json!(request_id));
    object.insert("capability".to_string(), json!(capability));
    object.insert("service_id".to_string(), json!(service_id));
    object.insert("model".to_string(), json!(model));
    object.insert("provider_request_id".to_string(), provider_request_id);
    object.insert("usage".to_string(), usage);
    object.insert("artifacts".to_string(), json!(artifacts(payload)));
    object.insert("attempts".to_string(), json!(attempts));
    object.insert("fallback_used".to_string(), json!(fallback_used));
    object.insert("warnings".to_string(), json!([]));
    Value::Object(object)
}

/// Execute one media capability with an explicit, same-capability fallback path.
pub(crate) fn execute_media(capability: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    let plan = ai_services::capability_selection_plan(capability, input)?;
    execute_plan(capability, input, &plan)
}

fn execute_plan(capability: &str, input: &Value, plan: &Value) -> Result<Value, Box<dyn Error>> {
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
        let model = attempt
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let preflight_result = preflight(capability, service_id, model, input);
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
        attempt_log.push(json!({
            "service_id": service_id,
            "model": model,
            "state": "running",
        }));
        match invoke(capability, &with_attempt(input, service_id, model)) {
            Ok(result) => {
                if let Some(last) = attempt_log.last_mut() {
                    last["state"] = json!("succeeded");
                }
                let mut result = decorate(
                    capability,
                    service_id,
                    model,
                    &request_id,
                    &attempt_log,
                    attempt["role"] == "fallback",
                    result,
                );
                result["requested"] = plan["requested"].clone();
                result["selection_source"] = plan["selection_source"].clone();
                result["fallback_policy"] = plan["fallback_policy"].clone();
                result["selected"] = plan["selected"].clone();
                result["actual"] = attempt.clone();
                // Inline MCP results must expose the same envelope to SDK consumers.
                if result.get("structuredContent").is_some() {
                    let mut structured = result.as_object().cloned().unwrap_or_default();
                    structured.remove("content");
                    structured.remove("structuredContent");
                    let structured = Value::Object(structured);
                    if let Some(block) = result["content"]
                        .as_array_mut()
                        .and_then(|items| items.iter_mut().find(|block| block["type"] == "text"))
                    {
                        block["text"] = json!(serde_json::to_string(&structured)?);
                    }
                    result["structuredContent"] = structured;
                }
                return Ok(result);
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
    let plan = ai_services::capability_selection_plan(capability, input)?;
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
    let model = selected
        .get("model")
        .and_then(Value::as_str)
        .ok_or("所选模型不可用")?;
    let confirm_cost = input
        .get("confirm_cost")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if capability != CAP_CHAT && capability != CAP_VISION && !confirm_cost {
        return Ok(json!({
            "state": "confirmation_required",
            "capability": capability,
            "service_id": service_id,
            "message": "该检查可能产生 Provider 费用；确认后才会发起真实调用。",
            "route": plan,
        }));
    }
    if capability == CAP_CHAT || capability == CAP_VISION {
        let models = ai_services::list_models(service_id)?;
        return Ok(json!({
            "state": "healthy",
            "capability": capability,
            "service_id": service_id,
            "message": format!("连接可用 · {} 个模型", models.len()),
            "models": models,
            "route": plan,
        }));
    }
    let payload = input
        .get("payload")
        .cloned()
        .unwrap_or_else(|| input.clone());
    let preflight_result = preflight(capability, service_id, model, &payload);
    if preflight_result.get("state").and_then(Value::as_str) != Some("ready") {
        return Ok(
            json!({"state": "needs_input", "capability": capability, "service_id": service_id, "preflight": preflight_result, "route": plan}),
        );
    }
    let result = execute_plan(capability, &payload, &plan)?;
    Ok(
        json!({"state": "accepted", "capability": capability, "service_id": service_id, "model": model, "result": result, "route": plan}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ai_services::{
        AIServiceProtocol, CapabilityRouteEntry, CustomAIServiceInput, MediaModelBinding,
        CAP_IMAGE_GENERATE, CAP_VIDEO_GENERATE,
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
                models: Vec::new(),
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
            endpoints: Vec::new(),
        })
        .unwrap();
    }

    #[test]
    fn route_plan_explains_same_capability_fallbacks() {
        with_isolated_home(|| {
            video_service("seedance", "ark-video", "doubao-seedance");
            video_service("wan", "wan-video", "wan2.6-i2v-flash");
            ai_services::set_capability_routes(
                CAP_VIDEO_GENERATE,
                vec![
                    ai_services::CapabilityRouteEntry::new("seedance", ""),
                    ai_services::CapabilityRouteEntry::new("wan", ""),
                ],
            )
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
    fn vision_capability_binds_like_chat_and_reports_route() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_VIDEO_GENERATE.to_string(),
                MediaModelBinding {
                    adapter: "ark-video".to_string(),
                    model: "seedance".to_string(),
                    models: Vec::new(),
                    execution_mode: None,
                },
            );
            ai_services::upsert(CustomAIServiceInput {
                id: "vision-a".to_string(),
                display_name: "Vision A".to_string(),
                base_url: "https://vision.example".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "vision-model".to_string(),
                models: vec!["vision-model".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-vision-a".to_string(),
                endpoints: Vec::new(),
            })
            .unwrap();
            ai_services::set_media_service(CAP_VISION, "vision-a").unwrap();
            let plan = route_plan(&json!({"capability": CAP_VISION})).unwrap();
            assert_eq!(plan["state"], "resolved");
            assert_eq!(plan["selected"]["service_id"], "vision-a");
            // 视觉与对话一样按服务自身模型解析，不需要额外的 media 绑定。
            assert_eq!(plan["selected"]["model"], "vision-model");
            assert_eq!(plan["selected"]["adapter"], "openai-responses");
        });
    }

    #[test]
    fn route_plan_resolves_each_route_entry_its_own_model() {
        with_isolated_home(|| {
            // 两个服务，各有两个生图模型；主选用第二个模型，备用用自己第一个。
            for (id, first, second) in [
                ("ark", "doubao-seedream-first", "doubao-seedream-second"),
                ("relay", "flux-first", "flux-second"),
            ] {
                let mut media = std::collections::BTreeMap::new();
                media.insert(
                    CAP_IMAGE_GENERATE.to_string(),
                    MediaModelBinding {
                        adapter: "ark-images".to_string(),
                        model: first.to_string(),
                        models: vec![first.to_string(), second.to_string()],
                        execution_mode: None,
                    },
                );
                ai_services::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiChat,
                    model: "chat".to_string(),
                    models: vec!["chat".to_string()],
                    image_model: String::new(),
                    image_adapter: String::new(),
                    media,
                    api_key: format!("sk-{id}"),
                    endpoints: Vec::new(),
                })
                .unwrap();
            }
            ai_services::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![
                    CapabilityRouteEntry::new("ark", "doubao-seedream-second"),
                    CapabilityRouteEntry::new("relay", ""),
                ],
            )
            .unwrap();

            let plan = route_plan(&json!({"capability": CAP_IMAGE_GENERATE})).unwrap();
            assert_eq!(plan["schema_version"], "himind.ai-capability-route.v2");
            assert_eq!(plan["state"], "resolved");
            assert_eq!(plan["selected"]["service_id"], "ark");
            assert_eq!(plan["selected"]["model"], "doubao-seedream-second");
            assert_eq!(plan["selected"]["model_source"], "explicit");

            // 每个尝试带自己的模型：备用服务收到的是它自己的默认模型，不是主选的。
            let attempts = plan["attempts"].as_array().unwrap();
            assert_eq!(attempts.len(), 2);
            assert_eq!(attempts[0]["model"], "doubao-seedream-second");
            assert_eq!(attempts[1]["service_id"], "relay");
            assert_eq!(attempts[1]["model"], "flux-first");
            assert_eq!(attempts[1]["model_source"], "service_default");
            assert_eq!(plan["fallbacks"][0]["model"], "flux-first");

            // 候选按 (服务, 模型) 展开：2 个路由项 + 每个服务各 1 个剩余模型 = 4。
            let candidates = plan["candidates"].as_array().unwrap();
            assert_eq!(candidates.len(), 4);
            let relay_models = candidates
                .iter()
                .filter(|item| item["service_id"] == "relay")
                .map(|item| item["model"].as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            assert!(relay_models.contains(&"flux-first".to_string()));
            assert!(relay_models.contains(&"flux-second".to_string()));
            assert!(candidates
                .iter()
                .filter(|item| item["service_id"] == "relay" && item["model"] == "flux-first")
                .all(|item| item["eligibility"] == "fallback"));
        });
    }

    #[test]
    fn staged_route_model_that_disappeared_is_never_silently_replaced() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_IMAGE_GENERATE.to_string(),
                MediaModelBinding {
                    adapter: "ark-images".to_string(),
                    model: "current-model".to_string(),
                    models: Vec::new(),
                    execution_mode: None,
                },
            );
            ai_services::upsert(CustomAIServiceInput {
                id: "ark".to_string(),
                display_name: "方舟".to_string(),
                base_url: "https://ark.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-ark".to_string(),
                endpoints: Vec::new(),
            })
            .unwrap();

            // 直接写文件，绕开校验，模拟「模型被服务编辑删掉」的历史路由项。
            let selection = serde_json::json!({
                "schema_version": 2,
                "capability_routes": {
                    CAP_IMAGE_GENERATE: [{"service_id": "ark", "model": "retired-model"}]
                }
            });
            std::fs::write(
                crate::store::paths::agent_home().join("ai-service-selection.json"),
                serde_json::to_vec_pretty(&selection).unwrap(),
            )
            .unwrap();

            let plan = route_plan(&json!({"capability": CAP_IMAGE_GENERATE})).unwrap();
            assert_eq!(plan["state"], "missing");
            assert_eq!(plan["reason"], "model_unavailable");
            assert!(plan["attempts"].as_array().unwrap().is_empty());
            assert_eq!(plan["selected"]["model"], serde_json::Value::Null);
            let stale = plan["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["model"] == "retired-model")
                .expect("retired model should still be explained");
            assert_eq!(stale["eligible"], false);
            assert_eq!(stale["reason"], "model_unavailable");
            // 绝不静默改用 current-model 执行。
            assert!(!plan["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["model"] == "current-model"));
        });
    }

    #[test]
    fn each_attempt_writes_its_own_model_into_the_request() {
        let input = json!({
            "capability": CAP_IMAGE_GENERATE,
            "service": "custom:ark",
            "model": "doubao-seedream-second",
            "prompt": "a cat",
        });
        // 降级到备用服务时，模型换成备用自己的，主选的模型不能串过去。
        let fallback = with_attempt(&input, "relay", "flux-first");
        assert_eq!(fallback["service"], "custom:relay");
        assert_eq!(fallback["model"], "flux-first");
        assert_eq!(fallback["prompt"], "a cat");
        assert!(fallback.get("capability").is_none());

        // 跟随服务默认时删掉 model 键，交回能力绑定解析。
        let follows = with_attempt(&input, "relay", "");
        assert!(follows.get("model").is_none());
        assert_eq!(follows["service"], "custom:relay");
    }

    #[test]
    fn preflight_is_local_and_reports_missing_video_prompt() {
        with_isolated_home(|| {
            video_service("wan", "wan-video", "wan2.6-i2v-flash");
            let result = preflight(CAP_VIDEO_GENERATE, "wan", "", &json!({}));
            assert_eq!(result["state"], "invalid");
            assert!(result["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "prompt" && item["ok"] == false));
            let too_many_images = preflight(
                CAP_VIDEO_GENERATE,
                "wan",
                "",
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

    fn plugin_ai_service(id: &str, base_url: &str, capability: &str, adapter: &str) {
        let media = std::collections::BTreeMap::from([(
            capability.to_string(),
            MediaModelBinding {
                adapter: adapter.into(),
                model: "default-model".into(),
                models: vec!["selected-model".into()],
                execution_mode: None,
            },
        )]);
        ai_services::upsert(CustomAIServiceInput {
            id: id.into(),
            display_name: id.into(),
            base_url: base_url.into(),
            protocol: AIServiceProtocol::OpenaiChat,
            model: "chat".into(),
            models: vec!["chat".into()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: "test-key-never-leaves-localhost".into(),
            endpoints: vec![],
        })
        .unwrap();
    }

    #[test]
    fn plugin_ai_selection_modes_preserve_model_and_fallback_intent() {
        with_isolated_home(|| {
            plugin_ai_service(
                "a",
                "http://127.0.0.1:9/v1",
                CAP_IMAGE_GENERATE,
                "openai-images",
            );
            plugin_ai_service(
                "b",
                "http://127.0.0.1:9/v1",
                CAP_IMAGE_GENERATE,
                "openai-images",
            );
            ai_services::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![
                    CapabilityRouteEntry::new("a", "selected-model"),
                    CapabilityRouteEntry::new("b", ""),
                ],
            )
            .unwrap();
            let before = ai_services::capability_routes(CAP_IMAGE_GENERATE).unwrap();
            let plan = |extra: Value| {
                let mut value = extra;
                value["capability"] = json!("media.image.generate");
                route_plan(&value).unwrap()
            };
            assert_eq!(plan(json!({}))["selected"]["model"], "selected-model");
            assert_eq!(
                plan(json!({"service":"custom:a"}))["selected"]["model"],
                "default-model"
            );
            assert_eq!(
                plan(json!({"service":"custom:a"}))["attempts"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            let pinned = plan(json!({"service":"custom:a","model":"default-model"}));
            assert_eq!(pinned["selected"]["model"], "default-model");
            assert_eq!(pinned["fallback_policy"], "none");
            let opted = plan(
                json!({"selection":{"mode":"pinned","service_id":"a","model":"default-model"},"fallback":"configured","selection_source":"project"}),
            );
            assert_eq!(opted["attempts"].as_array().unwrap().len(), 2);
            assert_eq!(opted["attempts"][1]["model"], "default-model");
            assert_eq!(opted["selection_source"], "project");
            assert_eq!(
                plan(json!({"model":"default-model"}))["selected"]["model"],
                "default-model"
            );
            let stale = plan(json!({"service":"a","model":"deleted"}));
            assert_eq!(stale["state"], "missing");
            assert!(stale["attempts"].as_array().unwrap().is_empty());
            let preview = plan(json!({"payload":{"prompt":"cat"}}));
            assert_eq!(preview["billable"], false);
            assert_eq!(preview["preflight"][0]["state"], "ready");
            let catalog = ai_services::filtered_media_provider_catalog(&json!({"capability":"media.image.generate","execution_mode":"sync","required_features":["text_to_image"],"ready_only":true})).unwrap();
            assert_eq!(catalog["providers"].as_array().unwrap().len(), 2);
            assert_eq!(catalog["providers"][0]["health"]["state"], "unknown");
            assert!(catalog["providers"][0].get("base_url").is_none());
            assert!(!catalog.to_string().contains("test-key-never"));
            let stream = ai_services::filtered_media_provider_catalog(
                &json!({"capability":"image.generate","execution_mode":"stream"}),
            )
            .unwrap();
            assert!(stream["providers"].as_array().unwrap().is_empty());
            assert_eq!(
                before,
                ai_services::capability_routes(CAP_IMAGE_GENERATE).unwrap()
            );
        });
    }

    /// Real HTTP adapter requests against a bounded loopback provider, never a billable endpoint.
    #[test]
    fn plugin_ai_image_and_audio_execute_the_exact_preflighted_model() {
        use std::io::{Read, Write};
        with_isolated_home(|| {
            for (capability, adapter, content_type, response) in [
                (CAP_IMAGE_GENERATE, "openai-images", "application/json", br#"{"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="}]}"#.to_vec()),
                (CAP_AUDIO_SPEECH, "openai-speech", "audio/mpeg", b"ID3test-audio".to_vec()),
            ] {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let address = listener.local_addr().unwrap();
                let server = std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
                            Err(error) => panic!("mock provider: {error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut buf = [0; 4096];
                        let n = stream.read(&mut buf).unwrap();
                        assert!(n > 0);
                        bytes.extend_from_slice(&buf[..n]);
                        if let Some(header_end) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&bytes[..header_end]);
                            let length = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok())).unwrap();
                            if bytes.len() >= header_end + 4 + length { break; }
                        }
                    }
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                    stream.write_all(&response).unwrap();
                    String::from_utf8(bytes).unwrap()
                });
                plugin_ai_service("local", &format!("http://{address}/v1"), capability, adapter);
                ai_services::set_capability_routes(capability, vec![CapabilityRouteEntry::new("local", "default-model")]).unwrap();
                let output = crate::store::paths::agent_home().join(capability);
                let payload = json!({"service":"custom:local","model":"selected-model","prompt":"cat","text":"hello","output_dir":output,"inline":capability == CAP_IMAGE_GENERATE});
                let plan = ai_services::capability_selection_plan(capability, &payload).unwrap();
                let result = execute_media(capability, &payload).unwrap();
                assert_eq!(result["model"], "selected-model");
                assert_eq!(result["actual"]["model"], plan["selected"]["model"]);
                assert_eq!(result["requested"]["selection"]["mode"], "pinned");
                assert_eq!(result["fallback_used"], false);
                if capability == CAP_IMAGE_GENERATE {
                    assert_eq!(result["structuredContent"]["actual"], result["actual"]);
                    assert_eq!(result["structuredContent"]["artifacts"], result["artifacts"]);
                    assert!(result["content"].as_array().unwrap().iter().any(|block| block["type"] == "image"));
                }
                assert!(!result["artifacts"].as_array().unwrap().is_empty());
                let request = server.join().unwrap();
                let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(body["model"], "selected-model");
                assert!(body.get("selection").is_none());
                let invalid = execute_media(capability, &json!({"service":"local","model":"deleted","prompt":"cat","text":"hello"}));
                assert!(invalid.is_err());
            }
        });
    }

    fn plugin_ai_mock_provider(
        status: &str,
        body: Value,
    ) -> (String, std::thread::JoinHandle<Value>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let payload = loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let size = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + size {
                        break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + size])
                            .unwrap();
                    }
                }
            };
            let response = body.to_string();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            payload
        });
        (format!("http://{address}/v1"), server)
    }

    #[test]
    fn plugin_ai_cross_service_fallback_requires_opt_in_and_uses_its_own_model() {
        with_isolated_home(|| {
            let failure = json!({"error":{"message":"quota test-primary-failure"}});
            let (url, failed) =
                plugin_ai_mock_provider("500 Internal Server Error", failure.clone());
            plugin_ai_service("a", &url, CAP_IMAGE_GENERATE, "openai-images");
            plugin_ai_service(
                "b",
                "http://127.0.0.1:9/v1",
                CAP_IMAGE_GENERATE,
                "openai-images",
            );
            ai_services::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![
                    CapabilityRouteEntry::new("a", "default-model"),
                    CapabilityRouteEntry::new("b", "default-model"),
                ],
            )
            .unwrap();
            let payload = json!({"selection":{"mode":"pinned","service_id":"a","model":"selected-model"},"prompt":"cat","inline":false,"output_dir":crate::store::paths::agent_home().join("outputs")});
            let error = execute_media(CAP_IMAGE_GENERATE, &payload)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("test-primary-failure"),
                "must stop at explicit selection: {error}"
            );
            assert_eq!(failed.join().unwrap()["model"], "selected-model");
            let (url, failed) = plugin_ai_mock_provider("500 Internal Server Error", failure);
            plugin_ai_service("a", &url, CAP_IMAGE_GENERATE, "openai-images");
            let (url, succeeded) = plugin_ai_mock_provider(
                "200 OK",
                json!({"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="}]}),
            );
            plugin_ai_service("b", &url, CAP_IMAGE_GENERATE, "openai-images");
            let mut opted = payload;
            opted["fallback"] = json!("configured");
            let result = execute_media(CAP_IMAGE_GENERATE, &opted).unwrap();
            assert_eq!(result["actual"]["service_id"], "b");
            assert_eq!(result["actual"]["model"], "default-model");
            assert_eq!(result["fallback_used"], true);
            assert_eq!(result["attempts"].as_array().unwrap().len(), 2);
            assert_eq!(failed.join().unwrap()["model"], "selected-model");
            assert_eq!(succeeded.join().unwrap()["model"], "default-model");
        });
    }
}
