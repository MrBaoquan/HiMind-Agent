//! 入口 Anthropic Messages ↔ 上游 OpenAI Responses 的互译。
//!
//! 实现路径参考 cc-switch（MIT）的 transform_responses.rs 与
//! streaming_responses.rs，保留对 HiMind 必要且可验证的边界：
//! - system → instructions；
//! - message content 中的 tool_use / tool_result 提升为顶层 Responses item；
//! - Anthropic 图片转为 input_image；
//! - Responses 的函数调用参数必须是 JSON object，缺失/非法参数按完成状态处理；
//! - Anthropic 的 max_tokens 小于 16 时抬到 Responses 下限；
//! - Responses 命名 SSE 事件转换为 Anthropic message/content_block 生命周期。
//!
//! 不在这里实现 web search、扩展思考签名或 Anthropic 缓存等供应商专属能力；
//! 遇到这些数据时保留文本/工具主路径，不把不兼容字段原样发给上游。

use serde_json::{json, Map, Value};
use std::collections::HashMap;

const RESPONSES_MIN_MAX_OUTPUT_TOKENS: u64 = 16;
const TOOL_RESULT_ERROR_MARKER: &str = "[himind:tool-error]";

/// Anthropic Messages 请求 → OpenAI Responses 请求。
pub(crate) fn request_to_responses(body: &Value, default_model: &str) -> Result<Value, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "Anthropic 请求体不是对象".to_string())?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_model)
        .to_string();
    if model.is_empty() {
        return Err("绑定没有默认模型，无法翻译请求".to_string());
    }

    let mut translated = Map::new();
    translated.insert("model".to_string(), json!(model));

    if let Some(system) = object.get("system") {
        let instructions = system_instructions(system);
        if !instructions.is_empty() {
            translated.insert("instructions".to_string(), json!(instructions));
        }
    }

    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Anthropic 请求缺少 messages".to_string())?;
    translated.insert(
        "input".to_string(),
        Value::Array(convert_messages_to_input(messages)?),
    );

    if let Some(value) = object.get("max_tokens") {
        translated.insert(
            "max_output_tokens".to_string(),
            match value.as_u64() {
                Some(value) if (1..RESPONSES_MIN_MAX_OUTPUT_TOKENS).contains(&value) => {
                    json!(RESPONSES_MIN_MAX_OUTPUT_TOKENS)
                }
                _ => value.clone(),
            },
        );
    }
    for key in ["temperature", "top_p"] {
        if let Some(value) = object.get(key) {
            translated.insert(key.to_string(), value.clone());
        }
    }
    translated.insert(
        "stream".to_string(),
        json!(object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false)),
    );
    translated.insert("store".to_string(), json!(false));

    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter(|tool| tool.get("type").and_then(Value::as_str) != Some("BatchTool"))
            .map(|tool| {
                let mut translated = json!({
                    "type": "function",
                    "strict": false,
                    "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                });
                if let Some(description) = tool.get("description").filter(|value| !value.is_null())
                {
                    translated["description"] = description.clone();
                }
                translated["parameters"] = clean_schema(
                    tool.get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                );
                translated
            })
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            translated.insert("tools".to_string(), Value::Array(tools));
        }
    }
    if let Some(choice) = object.get("tool_choice") {
        translated.insert("tool_choice".to_string(), translate_tool_choice(choice));
        if let Some(disable_parallel) = choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
        {
            translated.insert("parallel_tool_calls".to_string(), json!(!disable_parallel));
        }
    }
    Ok(Value::Object(translated))
}

fn system_instructions(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                (part.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| part.get("text").and_then(Value::as_str).unwrap_or_default())
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn convert_messages_to_input(messages: &[Value]) -> Result<Vec<Value>, String> {
    let mut input = Vec::<Value>::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match message.get("content") {
            Some(Value::String(text)) => {
                input.push(json!({
                    "type": "message",
                    "role": role,
                    "content": [{
                        "type": if role == "assistant" { "output_text" } else { "input_text" },
                        "text": text,
                    }],
                }));
            }
            Some(Value::Array(blocks)) => {
                let mut content = Vec::<Value>::new();
                for block in blocks {
                    match block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                content.push(json!({
                                    "type": if role == "assistant" { "output_text" } else { "input_text" },
                                    "text": text,
                                }));
                            }
                        }
                        "image" => {
                            if let Some(part) = anthropic_image_to_responses_part(block) {
                                content.push(part);
                            } else {
                                return Err("Anthropic 图片块缺少有效 source".to_string());
                            }
                        }
                        "document" => {
                            if let Some(part) = anthropic_document_to_responses_part(block) {
                                content.push(part);
                            }
                        }
                        "tool_use" => {
                            flush_content(&mut input, role, &mut content);
                            let arguments =
                                block.get("input").cloned().unwrap_or_else(|| json!({}));
                            input.push(json!({
                                "type": "function_call",
                                "call_id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                                "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                "arguments": serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
                            }));
                        }
                        "tool_result" => {
                            flush_content(&mut input, role, &mut content);
                            let is_error = block
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let output = tool_result_output(block.get("content"), is_error);
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                                "output": output,
                            }));
                        }
                        "thinking"
                        | "redacted_thinking"
                        | "server_tool_use"
                        | "web_search_tool_result" => {}
                        _ => {}
                    }
                }
                flush_content(&mut input, role, &mut content);
            }
            _ => {}
        }
    }
    Ok(input)
}

fn flush_content(input: &mut Vec<Value>, role: &str, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    input.push(json!({ "type": "message", "role": role, "content": std::mem::take(content) }));
}

fn anthropic_image_to_responses_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .map(|url| json!({ "type": "input_image", "image_url": url })),
        Some("base64") | None => {
            let data = source.get("data").and_then(Value::as_str)?;
            if data.is_empty() {
                return None;
            }
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            Some(json!({
                "type": "input_image",
                "image_url": format!("data:{media_type};base64,{data}"),
            }))
        }
        _ => None,
    }
}

fn anthropic_document_to_responses_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let filename = block
        .get("title")
        .or_else(|| block.get("filename"))
        .and_then(Value::as_str)
        .unwrap_or("document.pdf");
    match source.get("type").and_then(Value::as_str) {
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .map(|url| json!({ "type": "input_file", "file_url": url, "filename": filename })),
        Some("base64") => {
            let data = source.get("data").and_then(Value::as_str)?;
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("application/pdf");
            Some(json!({
                "type": "input_file",
                "filename": filename,
                "file_data": format!("data:{media_type};base64,{data}"),
            }))
        }
        _ => None,
    }
}

fn tool_result_output(content: Option<&Value>, is_error: bool) -> Value {
    let mut text = match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) if !value.is_null() => value.to_string(),
        _ => String::new(),
    };
    if is_error {
        text = format!("{TOOL_RESULT_ERROR_MARKER} {text}");
    }
    json!(text)
}

fn clean_schema(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            object.remove("cache_control");
            object.remove("$schema");
            for child in object.values_mut() {
                *child = clean_schema(std::mem::take(child));
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = clean_schema(std::mem::take(item));
            }
        }
        _ => {}
    }
    value
}

fn translate_tool_choice(choice: &Value) -> Value {
    match choice {
        Value::String(value) => match value.as_str() {
            "auto" => json!("auto"),
            "any" => json!("required"),
            "none" => json!("none"),
            _ => json!("auto"),
        },
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("tool") => object
                .get("name")
                .and_then(Value::as_str)
                .map(|name| json!({ "type": "function", "name": name }))
                .unwrap_or_else(|| json!("auto")),
            Some("any") => json!("required"),
            Some("none") => json!("none"),
            _ => json!("auto"),
        },
        _ => json!("auto"),
    }
}

/// OpenAI Responses 响应 → Anthropic Messages 响应。
pub(crate) fn response_to_anthropic(response: &Value, model: &str) -> Result<Value, String> {
    validate_terminal_status(response)?;
    let completed = response.get("status").and_then(Value::as_str) != Some("incomplete");
    let mut content = Vec::<Value>::new();
    let mut has_tool_use = false;
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| {
                                matches!(
                                    part.get("type").and_then(Value::as_str),
                                    Some("output_text" | "text")
                                )
                                .then(|| {
                                    part.get("text").and_then(Value::as_str).unwrap_or_default()
                                })
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("function_call") => {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let input = function_arguments(item, completed)?;
                content.push(json!({
                    "type": "tool_use",
                    "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                    "name": name,
                    "input": input,
                }));
                has_tool_use = true;
            }
            _ => {}
        }
    }
    let incomplete_reason = response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str);
    let stop_reason = if has_tool_use {
        "tool_use"
    } else if matches!(incomplete_reason, Some("max_output_tokens" | "max_tokens")) {
        "max_tokens"
    } else {
        "end_turn"
    };
    Ok(json!({
        "id": response.get("id").and_then(Value::as_str).unwrap_or(""),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": anthropic_usage(response.get("usage")),
    }))
}

/// 非流式 Responses 响应补齐为完整 Anthropic SSE 生命周期。
/// 上游偶尔忽略 stream=true 并直接返回 JSON；客户端仍期待 SSE 时使用。
pub(crate) fn response_to_anthropic_sse(response: &Value, model: &str) -> Result<String, String> {
    let message = response_to_anthropic(response, model)?;
    let mut output = String::new();
    output.push_str(&sse(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message.get("id").cloned().unwrap_or_else(|| json!("msg_himind_gateway")),
                "type": "message",
                "role": "assistant",
                "model": message.get("model").cloned().unwrap_or_else(|| json!(model)),
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 },
            },
        }),
    ));
    for (index, block) in message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let index = index as u64;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                output.push_str(&sse(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": { "type": "text", "text": "" },
                    }),
                ));
                output.push_str(&sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": text },
                    }),
                ));
            }
            Some("tool_use") => {
                output.push_str(&sse(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                            "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                            "input": {},
                        },
                    }),
                ));
                let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                output.push_str(&sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
                        },
                    }),
                ));
            }
            _ => continue,
        }
        output.push_str(&sse(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        ));
    }
    output.push_str(&sse(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": message.get("stop_reason").cloned().unwrap_or_else(|| json!("end_turn")),
                "stop_sequence": null,
            },
            "usage": message.get("usage").cloned().unwrap_or_else(|| json!({
                "input_tokens": 0,
                "output_tokens": 0,
            })),
        }),
    ));
    output.push_str(&sse("message_stop", json!({ "type": "message_stop" })));
    Ok(output)
}

fn function_arguments(item: &Value, completed: bool) -> Result<Value, String> {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let raw = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let input: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) if !completed => return Ok(json!({})),
        Err(error) => {
            return Err(format!(
                "Responses 函数调用 {name} 的 arguments 不是合法 JSON：{error}"
            ))
        }
    };
    if input.is_object() {
        Ok(input)
    } else if completed {
        Err(format!(
            "Responses 函数调用 {name} 的 arguments 必须是 JSON object"
        ))
    } else {
        Ok(json!({}))
    }
}

fn validate_terminal_status(response: &Value) -> Result<(), String> {
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return Err(format!(
            "Responses 上游返回错误：{}",
            response_error_message(response, "unknown upstream error")
        ));
    }
    match response.get("status").and_then(Value::as_str) {
        Some("failed") => Err(format!(
            "Responses 上游失败：{}",
            response_error_message(response, "response generation failed")
        )),
        Some("cancelled") => Err("Responses 上游取消了生成".to_string()),
        _ => Ok(()),
    }
}

fn response_error_message(response: &Value, fallback: &str) -> String {
    response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| response.get("message").and_then(Value::as_str))
        .or_else(|| response.get("error").and_then(Value::as_str))
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn anthropic_usage(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let number = |path: &[&str]| -> u64 {
        let mut current = usage;
        for key in path {
            let Some(next) = current.get(*key) else {
                return 0;
            };
            current = next;
        }
        current.as_u64().unwrap_or(0)
    };
    let input = number(&["input_tokens"]).max(number(&["prompt_tokens"]));
    let output = number(&["output_tokens"]).max(number(&["completion_tokens"]));
    let cached = number(&["cache_read_input_tokens"])
        .max(number(&["input_tokens_details", "cached_tokens"]));
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_input_tokens": cached,
    })
}

/// Responses SSE → Anthropic SSE。只处理命名事件中的文本和函数调用主路径；
/// 不支持的 reasoning/web-search 事件不会破坏后续文本与工具事件。
pub(crate) struct AnthropicResponsesStream {
    response_id: String,
    model: String,
    message_started: bool,
    finished: bool,
    next_index: u64,
    text_index: Option<u64>,
    tools: HashMap<String, ToolBlock>,
    tools_by_output: HashMap<u64, String>,
    has_tool_use: bool,
    terminal_stop_reason: Option<String>,
    usage: Value,
}

struct ToolBlock {
    index: u64,
    id: String,
    name: String,
    arguments: String,
    emitted_arguments: bool,
    closed: bool,
}

impl AnthropicResponsesStream {
    pub(crate) fn new(model: String) -> Self {
        Self {
            response_id: "msg_himind_gateway".to_string(),
            model,
            message_started: false,
            finished: false,
            next_index: 0,
            text_index: None,
            tools: HashMap::new(),
            tools_by_output: HashMap::new(),
            has_tool_use: false,
            terminal_stop_reason: None,
            usage: json!({}),
        }
    }

    pub(crate) fn push_line(&mut self, line: &str) -> Vec<String> {
        let Some(payload) = line.trim().strip_prefix("data:") else {
            return Vec::new();
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return Vec::new();
        };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.created" => {
                if let Some(response) = event.get("response") {
                    if let Some(id) = response.get("id").and_then(Value::as_str) {
                        self.response_id = if id.starts_with("msg_") {
                            id.to_string()
                        } else {
                            format!("msg_{id}")
                        };
                    }
                }
                self.message_start()
            }
            "response.content_part.added" => {
                if event.pointer("/part/type").and_then(Value::as_str) == Some("output_text") {
                    self.start_text_block()
                } else {
                    Vec::new()
                }
            }
            "response.output_text.delta" => {
                let Some(text) = event.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if text.is_empty() {
                    return Vec::new();
                }
                let mut events = self.message_start();
                let index = self.ensure_text_block();
                events.push(sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": text },
                    }),
                ));
                events
            }
            "response.output_text.done" => self.close_text_block(),
            "response.output_item.done" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("message") {
                    self.close_text_block()
                } else if event.pointer("/item/type").and_then(Value::as_str)
                    == Some("function_call")
                {
                    self.finish_tool_by_event(&event)
                } else {
                    Vec::new()
                }
            }
            "response.output_item.added" => self.handle_output_item_added(&event),
            "response.function_call_arguments.delta" => {
                self.handle_function_arguments(&event, false)
            }
            "response.function_call_arguments.done" => self.handle_function_arguments(&event, true),
            "response.completed" | "response.incomplete" => {
                if let Some(usage) = event.pointer("/response/usage") {
                    self.usage = usage.clone();
                }
                if kind == "response.incomplete"
                    && matches!(
                        event
                            .pointer("/response/incomplete_details/reason")
                            .and_then(Value::as_str),
                        Some("max_output_tokens" | "max_tokens")
                    )
                {
                    self.terminal_stop_reason = Some("max_tokens".to_string());
                }
                self.finish_lifecycle()
            }
            "response.failed" | "error" => {
                let message = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Responses 上游流式生成失败");
                let mut events = self.message_start();
                events.push(sse(
                    "error",
                    json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
                ));
                self.finished = true;
                events
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn finish(&mut self) -> Vec<String> {
        self.finish_lifecycle()
    }

    pub(crate) fn usage(&self) -> (u64, u64) {
        (
            self.usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            self.usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    }

    fn handle_output_item_added(&mut self, event: &Value) -> Vec<String> {
        let item = event.get("item").unwrap_or(&Value::Null);
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Vec::new();
        }
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "fc_himind_{}",
                    event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                )
            });
        let index = self.next_index;
        self.next_index += 1;
        let tool = ToolBlock {
            index,
            id: item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments: String::new(),
            emitted_arguments: false,
            closed: false,
        };
        if let Some(output_index) = event.get("output_index").and_then(Value::as_u64) {
            self.tools_by_output.insert(output_index, item_id.clone());
        }
        let mut events = self.message_start();
        events.push(sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool.id,
                    "name": tool.name,
                    "input": {},
                },
            }),
        ));
        self.tools.insert(item_id, tool);
        self.has_tool_use = true;
        if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
            if !arguments.is_empty() {
                events.extend(self.emit_tool_arguments_by_output(
                    event.get("output_index").and_then(Value::as_u64),
                    arguments,
                ));
            }
        }
        events
    }

    fn handle_function_arguments(&mut self, event: &Value, done: bool) -> Vec<String> {
        let Some(arguments) = event
            .get("delta")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        else {
            return if done {
                self.finish_tool_by_event(event)
            } else {
                Vec::new()
            };
        };
        let item_id = event
            .get("item_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| self.tools_by_output.get(&index).cloned())
            });
        let Some(item_id) = item_id else {
            return Vec::new();
        };
        let Some(tool) = self.tools.get_mut(&item_id) else {
            return Vec::new();
        };
        tool.arguments.push_str(arguments);
        tool.emitted_arguments = true;
        let index = tool.index;
        let mut events = vec![sse(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": arguments },
            }),
        )];
        if done {
            events.extend(self.close_tool(&item_id));
        }
        events
    }

    fn emit_tool_arguments_by_output(
        &mut self,
        output_index: Option<u64>,
        arguments: &str,
    ) -> Vec<String> {
        let Some(item_id) =
            output_index.and_then(|index| self.tools_by_output.get(&index).cloned())
        else {
            return Vec::new();
        };
        let Some(tool) = self.tools.get_mut(&item_id) else {
            return Vec::new();
        };
        tool.arguments.push_str(arguments);
        tool.emitted_arguments = true;
        let index = tool.index;
        vec![sse(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": arguments },
            }),
        )]
    }

    fn finish_tool_by_event(&mut self, event: &Value) -> Vec<String> {
        let item_id = event
            .get("item_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| self.tools_by_output.get(&index).cloned())
            });
        item_id
            .map(|item_id| {
                if let Some(tool) = self.tools.get_mut(&item_id) {
                    if tool.arguments.is_empty() {
                        let arguments = event
                            .get("item")
                            .and_then(|item| item.get("arguments"))
                            .or_else(|| event.get("arguments"))
                            .and_then(Value::as_str);
                        if let Some(arguments) = arguments {
                            tool.arguments.push_str(arguments);
                        }
                    }
                }
                self.close_tool(&item_id)
            })
            .unwrap_or_default()
    }

    fn close_tool(&mut self, item_id: &str) -> Vec<String> {
        let Some(tool) = self.tools.get_mut(item_id) else {
            return Vec::new();
        };
        if tool.closed {
            return Vec::new();
        }
        tool.closed = true;
        if !tool.emitted_arguments && !tool.arguments.is_empty() {
            tool.emitted_arguments = true;
            let index = tool.index;
            let arguments = std::mem::take(&mut tool.arguments);
            return vec![
                sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": arguments },
                    }),
                ),
                sse(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                ),
            ];
        }
        vec![sse(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": tool.index }),
        )]
    }

    fn start_text_block(&mut self) -> Vec<String> {
        let mut events = self.message_start();
        let index = self.ensure_text_block();
        events.push(sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" },
            }),
        ));
        events
    }

    fn ensure_text_block(&mut self) -> u64 {
        if let Some(index) = self.text_index {
            return index;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.text_index = Some(index);
        index
    }

    fn close_text_block(&mut self) -> Vec<String> {
        let Some(index) = self.text_index.take() else {
            return Vec::new();
        };
        vec![sse(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        )]
    }

    fn message_start(&mut self) -> Vec<String> {
        if self.message_started {
            return Vec::new();
        }
        self.message_started = true;
        vec![sse(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.response_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        )]
    }

    fn finish_lifecycle(&mut self) -> Vec<String> {
        if self.finished || !self.message_started {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.close_text_block();
        let mut tool_ids = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.closed)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        tool_ids.sort_by_key(|id| self.tools.get(id).map(|tool| tool.index).unwrap_or(0));
        for item_id in tool_ids {
            events.extend(self.close_tool(&item_id));
        }
        let stop_reason = if self.has_tool_use {
            "tool_use"
        } else {
            self.terminal_stop_reason.as_deref().unwrap_or("end_turn")
        };
        let usage = anthropic_usage(Some(&self.usage));
        events.push(sse(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": usage,
            }),
        ));
        events.push(sse("message_stop", json!({ "type": "message_stop" })));
        events
    }
}

fn sse(event: &str, payload: Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&payload).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_request_maps_tools_images_and_min_budget() {
        let request = json!({
            "model": "alias/gpt",
            "max_tokens": 1,
            "system": [{ "type": "text", "text": "你是助手" }],
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "看图和文件" },
                    { "type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": "AAAA"
                    }},
                    { "type": "tool_result", "tool_use_id": "call_1", "content": "ok" }
                ]
            }],
            "tools": [{ "name": "read", "description": "读取", "input_schema": { "type": "object", "cache_control": {"type":"ephemeral"} } }],
            "tool_choice": { "type": "any" },
            "stream": true
        });
        let translated = request_to_responses(&request, "fallback").unwrap();
        assert_eq!(translated["instructions"], "你是助手");
        assert_eq!(translated["max_output_tokens"], 16);
        assert_eq!(translated["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(translated["input"][1]["type"], "function_call_output");
        assert_eq!(translated["tools"][0]["strict"], false);
        assert!(translated["tools"][0]["parameters"]
            .get("cache_control")
            .is_none());
        assert_eq!(translated["tool_choice"], "required");
        assert_eq!(translated["store"], false);
    }

    #[test]
    fn responses_response_maps_to_anthropic_tool_use() {
        let response = json!({
            "id": "resp_1",
            "model": "gpt",
            "status": "completed",
            "output": [
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "回答" }] },
                { "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a\"}" }
            ],
            "usage": { "input_tokens": 8, "output_tokens": 3 }
        });
        let translated = response_to_anthropic(&response, "alias/gpt").unwrap();
        assert_eq!(translated["model"], "alias/gpt");
        assert_eq!(translated["content"][0]["text"], "回答");
        assert_eq!(translated["content"][1]["type"], "tool_use");
        assert_eq!(translated["content"][1]["input"]["path"], "a");
        assert_eq!(translated["stop_reason"], "tool_use");
        assert_eq!(translated["usage"]["input_tokens"], 8);
    }

    #[test]
    fn incomplete_function_arguments_become_empty_object() {
        let response = json!({
            "id": "resp_1",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{ "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":" }],
            "usage": { "input_tokens": 2, "output_tokens": 1 }
        });
        let translated = response_to_anthropic(&response, "m").unwrap();
        assert_eq!(translated["content"][0]["input"], json!({}));
        assert_eq!(translated["stop_reason"], "tool_use");
    }

    #[test]
    fn responses_stream_uses_done_arguments_when_deltas_are_absent() {
        let mut stream = AnthropicResponsesStream::new("alias/gpt".to_string());
        let events = [
            json!({"type":"response.created","response":{"id":"resp_1"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read"}}),
            json!({"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"path\":\"a\"}"}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":2,"output_tokens":1}}}),
        ];
        let mut output = String::new();
        for event in events {
            output.push_str(&stream.push_line(&format!("data: {event}")).concat());
        }
        output.push_str(&stream.finish().concat());
        assert!(output.contains(r#""partial_json":"{\"path\":\"a\"}""#));
        assert!(output.contains(r#""stop_reason":"tool_use""#));
    }

    #[test]
    fn responses_stream_maps_incomplete_max_output_tokens() {
        let mut stream = AnthropicResponsesStream::new("alias/gpt".to_string());
        let events = [
            json!({"type":"response.created","response":{"id":"resp_1"}}),
            json!({"type":"response.output_text.delta","delta":"截断"}),
            json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":2,"output_tokens":1}}}),
        ];
        let mut output = String::new();
        for event in events {
            output.push_str(&stream.push_line(&format!("data: {event}")).concat());
        }
        assert!(output.contains(r#""stop_reason":"max_tokens""#));
    }

    #[test]
    fn responses_stream_emits_anthropic_text_tool_and_usage_events() {
        let mut stream = AnthropicResponsesStream::new("alias/gpt".to_string());
        let events = [
            json!({"type":"response.created","response":{"id":"resp_1","model":"gpt"}}),
            json!({"type":"response.output_text.delta","delta":"你"}),
            json!({"type":"response.output_text.delta","delta":"好"}),
            json!({"type":"response.output_item.done","item":{"type":"message"}}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read"}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"a\"}"}),
            json!({"type":"response.function_call_arguments.done","item_id":"fc_1"}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":2}}}),
        ];
        let mut output = String::new();
        for event in events {
            output.push_str(&stream.push_line(&format!("data: {event}")).concat());
        }
        output.push_str(&stream.finish().concat());
        assert!(output.contains("event: message_start"));
        assert!(output.contains(r#""text_delta""#));
        assert!(output.contains(r#""type":"tool_use""#));
        assert!(output.contains(r#""partial_json":"{\"path\":"#));
        assert!(output.contains(r#""stop_reason":"tool_use""#));
        assert!(output.contains("event: message_stop"));
        assert_eq!(stream.usage(), (5, 2));
    }
}
