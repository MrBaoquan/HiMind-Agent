//! 入口 OpenAI Chat Completions ↔ 上游 OpenAI Responses 的互译。
//!
//! `responses_chat.rs` 解决的是「Codex 讲 Responses、上游只讲 Chat」；这里补的是
//! 反方向：OpenCode 等通用客户端讲 Chat，而上游服务（DeepSeek、云渡 GPT 等）
//! 声明的是 `openai-responses`。网关站在中间，让客户端说自己的、上游答自己的。
//!
//! 实现路径与 `anthropic_responses.rs` 保持一致，参考 cc-switch（MIT）的
//! transform_responses / streaming_responses 做法，保留对 HiMind 必要且可验证的边界：
//! - system / developer → `instructions`，多条按空行合并；
//! - Chat message → Responses `input` item；user 图片转 `input_image`；
//! - assistant `tool_calls` 提升为顶层 `function_call` item，`tool` 消息转
//!   `function_call_output`；
//! - `max_tokens` / `max_completion_tokens` → `max_output_tokens`，小于 16 时抬到下限；
//! - Chat 嵌套 `tools[].function` → Responses 扁平 `tools[]`，`tool_choice` 同步改写；
//! - Responses 命名 SSE 事件 → Chat `chat.completion.chunk`，工具参数按 index 增量下发。
//!
//! 不在这里实现 reasoning / web search / `response_format` / `stop` / `n` 等
//! 两端语义不对等的能力：丢弃这些字段，而不是把不兼容结构原样发给上游。

use serde_json::{json, Map, Value};
use std::collections::HashMap;

const RESPONSES_MIN_MAX_OUTPUT_TOKENS: u64 = 16;

/// Chat Completions 请求体 → Responses 请求体。
pub(crate) fn request_to_responses(body: &Value, default_model: &str) -> Result<Value, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "Chat 请求体不是对象".to_string())?;
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

    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Chat 请求缺少 messages".to_string())?;
    let (instructions, input) = convert_messages(messages)?;

    let mut translated = Map::new();
    translated.insert("model".to_string(), json!(model));
    translated.insert("input".to_string(), Value::Array(input));
    // 网关是无状态转发：不把上游会话留在供应商侧。
    translated.insert("store".to_string(), json!(false));
    if !instructions.is_empty() {
        translated.insert("instructions".to_string(), json!(instructions));
    }

    let max_tokens = object
        .get("max_completion_tokens")
        .or_else(|| object.get("max_tokens"));
    if let Some(value) = max_tokens {
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
    if let Some(effort) = object
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        translated.insert("reasoning".to_string(), json!({ "effort": effort }));
    }
    if let Some(value) = object.get("parallel_tool_calls") {
        translated.insert("parallel_tool_calls".to_string(), value.clone());
    }
    let mut translated_tools = Vec::<Value>::new();
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        translated_tools = tools.iter().filter_map(chat_tool_to_responses).collect();
    }
    // 没有可用工具时不发 tools；`tool_choice` 也一并省掉，避免上游把它判成非法组合。
    if !translated_tools.is_empty() {
        translated.insert("tools".to_string(), Value::Array(translated_tools));
        if let Some(choice) = translate_tool_choice(object.get("tool_choice")) {
            translated.insert("tool_choice".to_string(), choice);
        }
    }
    translated.insert(
        "stream".to_string(),
        json!(object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false)),
    );
    Ok(Value::Object(translated))
}

fn convert_messages(messages: &[Value]) -> Result<(String, Vec<Value>), String> {
    let mut instructions = Vec::<String>::new();
    let mut input = Vec::<Value>::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match role {
            // Responses 用 instructions 承载系统指令；developer 是 OpenAI 新角色名。
            "system" | "developer" => {
                let text = content_text(message.get("content"));
                if !text.trim().is_empty() {
                    instructions.push(text);
                }
            }
            "tool" | "function" => {
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .or_else(|| message.get("name").and_then(Value::as_str))
                    .unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content_text(message.get("content")),
                }));
            }
            _ => {
                let assistant = role == "assistant";
                if let Some(content) = message.get("content") {
                    if !content.is_null() {
                        let parts = chat_content_parts(content, assistant);
                        if !parts.is_empty() {
                            input.push(json!({
                                "type": "message",
                                "role": role,
                                "content": parts,
                            }));
                        }
                    }
                }
                if assistant {
                    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                        for call in calls {
                            let function =
                                call.get("function").cloned().unwrap_or_else(|| json!({}));
                            input.push(json!({
                                "type": "function_call",
                                "call_id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                                "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                                "arguments": arguments_text(function.get("arguments")),
                            }));
                        }
                    }
                }
            }
        }
    }
    Ok((instructions.join("\n\n"), input))
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "input_text" | "output_text") | None
                )
                .then(|| part.get("text").and_then(Value::as_str).unwrap_or_default())
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) if !value.is_null() => value.to_string(),
        _ => String::new(),
    }
}

fn chat_content_parts(content: &Value, assistant: bool) -> Vec<Value> {
    let text_type = if assistant {
        "output_text"
    } else {
        "input_text"
    };
    match content {
        Value::String(text) => vec![json!({ "type": text_type, "text": text })],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text" | "input_text" | "output_text") | None => part
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!({ "type": text_type, "text": text })),
                // 图片只有 user 通道合法；assistant 历史里的图片无法进入 Responses。
                Some("image_url") if !assistant => chat_image_to_input(part),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn chat_image_to_input(part: &Value) -> Option<Value> {
    let (url, detail) = match part.get("image_url") {
        Some(Value::String(url)) => (url.as_str(), None),
        Some(Value::Object(image)) => (
            image.get("url").and_then(Value::as_str)?,
            image.get("detail").and_then(Value::as_str),
        ),
        _ => return None,
    };
    let mut translated = json!({ "type": "input_image", "image_url": url });
    if let Some(detail) = detail {
        translated["detail"] = json!(detail);
    }
    Some(translated)
}

fn arguments_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "{}".to_string(),
    }
}

fn chat_tool_to_responses(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    let function = tool.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;
    let mut translated = json!({
        "type": "function",
        "name": name,
        "parameters": function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" })),
    });
    if let Some(description) = function.get("description").filter(|value| !value.is_null()) {
        translated["description"] = description.clone();
    }
    if let Some(strict) = function.get("strict").and_then(Value::as_bool) {
        translated["strict"] = json!(strict);
    }
    Some(translated)
}

fn translate_tool_choice(value: Option<&Value>) -> Option<Value> {
    match value? {
        Value::String(text) => match text.as_str() {
            "auto" | "none" | "required" => Some(json!(text)),
            _ => None,
        },
        Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("function") => {
            object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(|name| json!({ "type": "function", "name": name }))
        }
        _ => None,
    }
}

/// 非流式 Responses 响应 → Chat Completions 响应。
pub(crate) fn response_to_chat(response: &Value, model: &str) -> Result<Value, String> {
    if response.get("status").and_then(Value::as_str) == Some("failed") {
        return Err(response_error_message(response, "Responses 上游生成失败"));
    }
    let mut text = String::new();
    let mut tool_calls = Vec::<Value>::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            if let Some(value) = part.get("text").and_then(Value::as_str) {
                                text.push_str(value);
                            }
                        }
                        Some("refusal") => {
                            if let Some(value) = part.get("refusal").and_then(Value::as_str) {
                                text.push_str(value);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                tool_calls.push(json!({
                    "id": item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("call_himind_gateway"),
                    "type": "function",
                    "function": {
                        "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "arguments": arguments_text(item.get("arguments")),
                    },
                }));
            }
            _ => {}
        }
    }
    let incomplete = response.get("status").and_then(Value::as_str) == Some("incomplete");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if incomplete
        && matches!(
            response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str),
            Some("max_output_tokens" | "max_tokens")
        )
    {
        "length"
    } else {
        "stop"
    };
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    Ok(json!({
        "id": chat_id(response),
        "object": "chat.completion",
        "created": response
            .get("created_at")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp()),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": chat_usage(response.get("usage")),
    }))
}

fn response_error_message(response: &Value, fallback: &str) -> String {
    response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn chat_id(response: &Value) -> String {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("resp_himind_gateway");
    if id.starts_with("chatcmpl-") {
        id.to_string()
    } else {
        format!("chatcmpl-{id}")
    }
}

fn chat_usage(usage: Option<&Value>) -> Value {
    let input = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| input.saturating_add(output));
    let cached = usage
        .and_then(|usage| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage
        .and_then(|usage| usage.get("output_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": total,
        "prompt_tokens_details": { "cached_tokens": cached },
        "completion_tokens_details": { "reasoning_tokens": reasoning },
    })
}

/// Responses SSE → Chat Completions SSE。
///
/// 只处理文本与函数调用主路径；reasoning、web-search 等 Responses 专有事件
/// 不产生 Chat 分片，也不会打断后续文本/工具事件。
pub(crate) struct ChatStream {
    id: String,
    created: i64,
    model: String,
    role_sent: bool,
    tools: Vec<StreamTool>,
    tools_by_item: HashMap<String, usize>,
    finish_reason: Option<String>,
    usage: Value,
    done: bool,
    failed: bool,
}

struct StreamTool {
    index: u64,
    arguments: String,
    emitted_arguments: bool,
}

impl ChatStream {
    pub(crate) fn new(model: String) -> Self {
        Self {
            id: "chatcmpl-himind-gateway".to_string(),
            created: chrono::Utc::now().timestamp(),
            model,
            role_sent: false,
            tools: Vec::new(),
            tools_by_item: HashMap::new(),
            finish_reason: None,
            usage: json!({}),
            done: false,
            failed: false,
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed
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

    pub(crate) fn cached_tokens(&self) -> u64 {
        self.usage
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    pub(crate) fn reasoning_tokens(&self) -> u64 {
        self.usage
            .get("output_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
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
                    if response.get("id").and_then(Value::as_str).is_some() {
                        self.id = chat_id(response);
                    }
                    if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
                        self.created = created;
                    }
                }
                self.role_chunk()
            }
            "response.output_text.delta" => {
                let Some(text) = event.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if text.is_empty() {
                    return Vec::new();
                }
                let mut events = self.role_chunk();
                events.push(self.chunk(json!({ "content": text }), None, None));
                events
            }
            "response.output_item.added" => self.handle_output_item_added(&event),
            "response.function_call_arguments.delta" => self.handle_arguments_delta(&event),
            "response.output_item.done" => self.handle_output_item_done(&event),
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
                    self.finish_reason = Some("length".to_string());
                }
                self.finish_chunks()
            }
            "response.failed" | "error" => {
                let message = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Responses 上游流式生成失败");
                self.done = true;
                self.failed = true;
                vec![format!(
                    "data: {}\n\n",
                    json!({ "error": { "type": "api_error", "message": message } })
                )]
            }
            _ => Vec::new(),
        }
    }

    /// 上游流结束：补齐 finish_reason、用量与 `[DONE]`。
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if self.done {
            return Vec::new();
        }
        self.finish_chunks()
    }

    fn handle_output_item_added(&mut self, event: &Value) -> Vec<String> {
        let item = event.get("item").unwrap_or(&Value::Null);
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Vec::new();
        }
        let mut events = self.role_chunk();
        let index = self.tools.len() as u64;
        let id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_himind_{index}"));
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        events.push(self.chunk(
            json!({ "tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": "" },
            }] }),
            None,
            None,
        ));
        let position = self.tools.len();
        if let Some(item_id) = item.get("id").and_then(Value::as_str) {
            self.tools_by_item.insert(item_id.to_string(), position);
        }
        if let Some(output_index) = event.get("output_index").and_then(Value::as_u64) {
            self.tools_by_item
                .insert(format!("output:{output_index}"), position);
        }
        self.tools.push(StreamTool {
            index,
            arguments: String::new(),
            emitted_arguments: false,
        });
        events
    }

    fn handle_arguments_delta(&mut self, event: &Value) -> Vec<String> {
        let Some(position) = self.tool_position(event) else {
            return Vec::new();
        };
        let Some(delta) = event.get("delta").and_then(Value::as_str) else {
            return Vec::new();
        };
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = self.role_chunk();
        let tool = &mut self.tools[position];
        tool.arguments.push_str(delta);
        tool.emitted_arguments = true;
        let index = tool.index;
        events.push(self.chunk(
            json!({ "tool_calls": [{ "index": index, "function": { "arguments": delta } }] }),
            None,
            None,
        ));
        events
    }

    fn handle_output_item_done(&mut self, event: &Value) -> Vec<String> {
        let item = event.get("item").unwrap_or(&Value::Null);
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Vec::new();
        }
        let Some(position) = self.tool_position(event) else {
            return Vec::new();
        };
        let tool = &mut self.tools[position];
        if tool.emitted_arguments {
            return Vec::new();
        }
        let arguments = arguments_text(item.get("arguments"));
        if arguments.is_empty() || arguments == "{}" {
            return Vec::new();
        }
        tool.arguments = arguments.clone();
        tool.emitted_arguments = true;
        let index = tool.index;
        let mut events = self.role_chunk();
        events.push(self.chunk(
            json!({ "tool_calls": [{ "index": index, "function": { "arguments": arguments } }] }),
            None,
            None,
        ));
        events
    }

    fn tool_position(&self, event: &Value) -> Option<usize> {
        if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
            if let Some(position) = self.tools_by_item.get(item_id) {
                return Some(*position);
            }
        }
        if let Some(output_index) = event.get("output_index").and_then(Value::as_u64) {
            if let Some(position) = self.tools_by_item.get(&format!("output:{output_index}")) {
                return Some(*position);
            }
        }
        None
    }

    fn role_chunk(&mut self) -> Vec<String> {
        if self.role_sent {
            return Vec::new();
        }
        self.role_sent = true;
        vec![self.chunk(json!({ "role": "assistant", "content": "" }), None, None)]
    }

    fn finish_chunks(&mut self) -> Vec<String> {
        if self.done {
            return Vec::new();
        }
        let mut events = self.role_chunk();
        let reason = self.finish_reason.clone().unwrap_or_else(|| {
            if self.tools.is_empty() {
                "stop".to_string()
            } else {
                "tool_calls".to_string()
            }
        });
        events.push(self.chunk(
            json!({}),
            Some(reason.as_str()),
            Some(chat_usage(Some(&self.usage))),
        ));
        events.push("data: [DONE]\n\n".to_string());
        self.done = true;
        events
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>, usage: Option<Value>) -> String {
        let mut value = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        });
        if let Some(usage) = usage {
            value["usage"] = usage;
        }
        format!("data: {value}\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_drops_tool_choice_when_no_tool_survives_translation() {
        // 反方向同理：一批工具都翻译不出来时，`tool_choice` 也不能单独发。
        let request = json!({
            "model": "service/model",
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": "required",
            "tools": [{ "type": "web_search_preview" }]
        });
        let responses = request_to_responses(&request, "fallback").unwrap();
        assert!(responses.get("tools").is_none(), "responses={responses}");
        assert!(
            responses.get("tool_choice").is_none(),
            "responses={responses}"
        );

        let request = json!({
            "model": "service/model",
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": "required",
            "tools": [{ "type": "function", "function": { "name": "read", "parameters": { "type": "object" } } }]
        });
        let responses = request_to_responses(&request, "fallback").unwrap();
        assert_eq!(responses["tools"][0]["name"], "read");
        assert_eq!(responses["tool_choice"], "required");
    }

    #[test]
    fn request_maps_messages_tools_and_limits() {
        let request = json!({
            "model": "deepseek-flash",
            "messages": [
                { "role": "system", "content": "你是助手" },
                { "role": "user", "content": [
                    { "type": "text", "text": "看图" },
                    { "type": "image_url", "image_url": { "url": "https://example.com/a.png", "detail": "high" } }
                ]},
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "文件内容" }
            ],
            "tools": [{ "type": "function", "function": {
                "name": "read", "description": "读取", "parameters": { "type": "object" }
            }}],
            "tool_choice": { "type": "function", "function": { "name": "read" } },
            "max_tokens": 8,
            "stream": true
        });
        let translated = request_to_responses(&request, "fallback").unwrap();
        assert_eq!(translated["model"], "deepseek-flash");
        assert_eq!(translated["instructions"], "你是助手");
        assert_eq!(translated["max_output_tokens"], 16);
        assert_eq!(translated["store"], false);
        assert_eq!(translated["stream"], true);
        assert_eq!(translated["input"][0]["type"], "message");
        assert_eq!(translated["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(translated["input"][0]["content"][1]["detail"], "high");
        assert_eq!(translated["input"][1]["type"], "function_call");
        assert_eq!(translated["input"][1]["call_id"], "call_1");
        assert_eq!(translated["input"][2]["type"], "function_call_output");
        assert_eq!(translated["input"][2]["output"], "文件内容");
        assert_eq!(translated["tools"][0]["name"], "read");
        assert_eq!(
            translated["tool_choice"],
            json!({ "type": "function", "name": "read" })
        );
    }

    #[test]
    fn response_maps_text_and_tool_calls_to_chat() {
        let response = json!({
            "id": "resp_1",
            "status": "completed",
            "created_at": 1700000000,
            "output": [
                { "type": "message", "role": "assistant", "content": [
                    { "type": "output_text", "text": "好的" }
                ]},
                { "type": "function_call", "call_id": "call_9", "name": "read",
                  "arguments": "{\"path\":\"b\"}" }
            ],
            "usage": {
                "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                "input_tokens_details": { "cached_tokens": 3 },
                "output_tokens_details": { "reasoning_tokens": 2 }
            }
        });
        let chat = response_to_chat(&response, "deepseek/deepseek-flash").unwrap();
        assert_eq!(chat["object"], "chat.completion");
        assert_eq!(chat["id"], "chatcmpl-resp_1");
        assert_eq!(chat["model"], "deepseek/deepseek-flash");
        assert_eq!(chat["choices"][0]["message"]["content"], "好的");
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_9"
        );
        assert_eq!(chat["usage"]["prompt_tokens"], 10);
        assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
        assert_eq!(
            chat["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );
    }

    #[test]
    fn response_maps_incomplete_to_length_finish_reason() {
        let response = json!({
            "id": "resp_2",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{ "type": "message", "content": [
                { "type": "output_text", "text": "截断" }
            ]}],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        });
        let chat = response_to_chat(&response, "m").unwrap();
        assert_eq!(chat["choices"][0]["finish_reason"], "length");
    }

    fn chunk_values(output: &[String]) -> Vec<Value> {
        output
            .iter()
            .filter_map(|event| {
                let payload = event.trim().strip_prefix("data:")?.trim();
                if payload == "[DONE]" {
                    None
                } else {
                    serde_json::from_str(payload).ok()
                }
            })
            .collect()
    }

    #[test]
    fn stream_maps_text_and_tool_events_to_chat_chunks() {
        let mut stream = ChatStream::new("service/model".to_string());
        let mut output = Vec::new();
        for line in [
            r#"data: {"type":"response.created","response":{"id":"resp_3","created_at":1700000001,"model":"upstream-model"}}"#,
            r#"data: {"type":"response.output_text.delta","delta":"你好"}"#,
            r#"data: {"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read"}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"a\"}"}"#,
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":4}}}"#,
        ] {
            output.extend(stream.push_line(line));
        }
        output.extend(stream.finish());
        let chunks = chunk_values(&output);
        assert_eq!(chunks[0]["object"], "chat.completion.chunk");
        // 客户端只认它请求的模型名，不能看到上游真实模型。
        assert_eq!(chunks[0]["model"], "service/model");
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "你好");
        let tool_start = chunks
            .iter()
            .find(|chunk| chunk.pointer("/choices/0/delta/tool_calls/0/id").is_some())
            .expect("tool start chunk");
        assert_eq!(
            tool_start.pointer("/choices/0/delta/tool_calls/0/function/name"),
            Some(&json!("read"))
        );
        let arguments = chunks
            .iter()
            .filter_map(|chunk| {
                chunk
                    .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                    .and_then(Value::as_str)
            })
            .collect::<String>();
        assert_eq!(arguments, "{\"path\":\"a\"}");
        let last = chunks.last().expect("finish chunk");
        assert_eq!(
            last.pointer("/choices/0/finish_reason"),
            Some(&json!("tool_calls"))
        );
        assert_eq!(last.pointer("/usage/prompt_tokens"), Some(&json!(7)));
        assert_eq!(last.pointer("/usage/completion_tokens"), Some(&json!(4)));
        assert!(output.last().unwrap().ends_with("data: [DONE]\n\n"));
        assert_eq!(stream.usage(), (7, 4));
    }

    #[test]
    fn stream_emits_finish_and_done_when_upstream_ends_early() {
        let mut stream = ChatStream::new("service/model".to_string());
        let _ = stream.push_line(r#"data: {"type":"response.created","response":{"id":"resp_4"}}"#);
        let output = stream.finish();
        let chunks = chunk_values(&output);
        let last = chunks.last().expect("finish chunk");
        assert_eq!(
            last.pointer("/choices/0/finish_reason"),
            Some(&json!("stop"))
        );
        assert!(output.concat().ends_with("data: [DONE]\n\n"));
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn stream_failure_marks_failed_and_suppresses_done() {
        let mut stream = ChatStream::new("service/model".to_string());
        let output = stream
            .push_line(
                r#"data: {"type":"response.failed","response":{"error":{"message":"上游失败"}}}"#,
            )
            .concat();
        assert!(stream.failed());
        assert!(output.contains("\"error\""), "output={output}");
        assert!(stream.finish().is_empty());
    }
}
