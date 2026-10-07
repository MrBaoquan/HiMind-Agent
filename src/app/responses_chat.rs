//! 入口 OpenAI Responses ↔ 上游 OpenAI Chat 的互译（ADR 0113 第 3 节）。
//!
//! Codex 0.150 起只讲 Responses（`wire_api = "chat"` 被移除），而多数可用的
//! 上游服务是 Chat 兼容。网关站在中间，就把这对协议补齐，让客户端说自己的、
//! 上游答自己的。
//!
//! 支持的 Responses 输入项：`message`（input_text/output_text）、`function_call`、
//! `function_call_output`。`reasoning` 等无法映射的项直接丢弃——Chat 上游没有
//! 对应概念，保留只会让请求非法。

use serde_json::{json, Map, Value};

/// Responses 请求体 → Chat Completions 请求体。
pub(crate) fn request_to_chat(body: &Value, default_model: &str) -> Result<Value, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "Responses 请求体不是对象".to_string())?;
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

    let mut messages = Vec::<Value>::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str) {
        if !instructions.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }
    match object.get("input") {
        Some(Value::String(text)) => messages.push(json!({ "role": "user", "content": text })),
        Some(Value::Array(items)) => {
            for item in items {
                translate_input_item(item, &mut messages)?;
            }
        }
        _ => {}
    }

    let mut translated = Map::new();
    translated.insert("model".to_string(), json!(model));
    translated.insert("messages".to_string(), Value::Array(messages));
    for key in ["temperature", "top_p"] {
        if let Some(value) = object.get(key) {
            translated.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("max_output_tokens") {
        translated.insert("max_tokens".to_string(), value.clone());
    }
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").and_then(Value::as_str).unwrap_or_default(),
                        "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
                    }
                }))
            })
            .collect::<Vec<Value>>();
        if !tools.is_empty() {
            translated.insert("tools".to_string(), Value::Array(tools));
        }
    }
    if let Some(choice) = translate_tool_choice(object.get("tool_choice")) {
        translated.insert("tool_choice".to_string(), choice);
    }
    let streaming = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    translated.insert("stream".to_string(), json!(streaming));
    if streaming {
        translated.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }
    Ok(Value::Object(translated))
}

fn translate_input_item(item: &Value, out: &mut Vec<Value>) -> Result<(), String> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            // Responses 用 `developer` 表示指令通道；Chat 里只有 `system`。
            // 原样透传会被上游判为非法角色（DeepSeek 直接 422）。
            let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
                "developer" => "system",
                other => other,
            }
            .to_string();
            out.push(json!({ "role": role, "content": content_text(item.get("content"))? }));
            Ok(())
        }
        Some("function_call") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            out.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{ "id": call_id, "type": "function",
                                 "function": { "name": name, "arguments": arguments } }],
            }));
            Ok(())
        }
        Some("function_call_output") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let output = match item.get("output") {
                Some(Value::String(text)) => text.clone(),
                Some(value) => value.to_string(),
                None => String::new(),
            };
            out.push(json!({ "role": "tool", "tool_call_id": call_id, "content": output }));
            Ok(())
        }
        // reasoning / 其它 Responses 专有项在 Chat 里没有对应概念。
        _ => Ok(()),
    }
}

fn content_text(content: Option<&Value>) -> Result<String, String> {
    Ok(match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push(text.to_string());
                }
            }
            out.join("")
        }
        _ => String::new(),
    })
}

fn translate_tool_choice(value: Option<&Value>) -> Option<Value> {
    let choice = value?;
    match choice {
        Value::String(text) => Some(json!(text)),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("function") {
                return object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| json!({ "type": "function", "function": { "name": name } }));
            }
            None
        }
        _ => None,
    }
}

/// 非流式 Chat 响应 → Responses 响应体。
pub(crate) fn response_to_responses(chat: &Value, model: &str) -> Value {
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut output = Vec::<Value>::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            output.push(message_item(text));
        }
    }
    for (index, call) in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let function = call.get("function").cloned().unwrap_or_else(|| json!({}));
        output.push(json!({
            "type": "function_call",
            "id": format!("fc_{index}"),
            "call_id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
            "arguments": function.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
            "status": "completed",
        }));
    }
    response_envelope(chat, model, output, "completed")
}

fn message_item(text: &str) -> Value {
    json!({
        "type": "message",
        "id": "msg_himind_gateway",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
    })
}

fn response_envelope(chat: &Value, model: &str, output: Vec<Value>, status: &str) -> Value {
    let usage = chat.get("usage").cloned().unwrap_or_else(|| json!({}));
    let input = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "id": chat.get("id").and_then(Value::as_str).unwrap_or("resp_himind_gateway"),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": status,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "usage": {
            "input_tokens": input,
            "input_tokens_details": { "cached_tokens": cached },
            "output_tokens": output_tokens,
            "output_tokens_details": { "reasoning_tokens": reasoning },
            "total_tokens": input.saturating_add(output_tokens),
        },
    })
}

/// Chat 流式分片 → Responses SSE 事件。
pub(crate) struct ResponsesStream {
    model: String,
    created: bool,
    message_started: bool,
    message_text: String,
    tool_items: Vec<ToolItem>,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
}

struct ToolItem {
    source_index: u64,
    output_index: u64,
    id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl ResponsesStream {
    pub(crate) fn new(model: String) -> Self {
        Self {
            model,
            created: false,
            message_started: false,
            message_text: String::new(),
            tool_items: Vec::new(),
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    pub(crate) fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    pub(crate) fn cached_tokens(&self) -> u64 {
        self.cached_tokens
    }

    pub(crate) fn reasoning_tokens(&self) -> u64 {
        self.reasoning_tokens
    }

    pub(crate) fn push_line(&mut self, line: &str) -> Vec<String> {
        let payload = match line.trim().strip_prefix("data:") {
            Some(payload) => payload.trim(),
            None => return Vec::new(),
        };
        if payload.is_empty() || payload == "[DONE]" {
            return Vec::new();
        }
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(chunk) => chunk,
            Err(_) => return Vec::new(),
        };
        let mut events = Vec::new();
        if !self.created {
            self.created = true;
            events.push(event(
                "response.created",
                json!({ "type": "response.created",
                        "response": response_envelope(&json!({}), &self.model, Vec::new(), "in_progress") }),
            ));
            events.push(event(
                "response.in_progress",
                json!({ "type": "response.in_progress",
                        "response": response_envelope(&json!({}), &self.model, Vec::new(), "in_progress") }),
            ));
        }
        if let Some(usage) = chunk.get("usage") {
            if let Some(value) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = value;
            }
            if let Some(value) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = value;
            }
            if let Some(value) = usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
            {
                self.cached_tokens = value;
            }
            if let Some(value) = usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_u64)
            {
                self.reasoning_tokens = value;
            }
        }
        let choice = match chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            Some(choice) => choice.clone(),
            None => return events,
        };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                if !self.message_started {
                    self.message_started = true;
                    events.push(event(
                        "response.output_item.added",
                        json!({ "type": "response.output_item.added", "output_index": self.message_output_index(),
                                "item": { "type": "message", "id": "msg_himind_gateway", "status": "in_progress",
                                          "role": "assistant", "content": [] } }),
                    ));
                    events.push(event(
                        "response.content_part.added",
                        json!({ "type": "response.content_part.added", "item_id": "msg_himind_gateway",
                                "output_index": self.message_output_index(), "content_index": 0,
                                "part": { "type": "output_text", "text": "", "annotations": [] } }),
                    ));
                }
                self.message_text.push_str(text);
                events.push(event(
                    "response.output_text.delta",
                    json!({ "type": "response.output_text.delta", "item_id": "msg_himind_gateway",
                            "output_index": self.message_output_index(), "content_index": 0, "delta": text }),
                ));
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let source_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let position = self
                    .tool_items
                    .iter()
                    .position(|item| item.source_index == source_index);
                let position = match position {
                    Some(position) => position,
                    None => {
                        let output_index = self.next_output_index();
                        let function = call.get("function").cloned().unwrap_or_else(|| json!({}));
                        let item = ToolItem {
                            source_index,
                            output_index,
                            id: format!("fc_{output_index}"),
                            call_id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call_himind_gateway")
                                .to_string(),
                            name: function
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            arguments: String::new(),
                        };
                        events.push(event(
                            "response.output_item.added",
                            json!({ "type": "response.output_item.added", "output_index": item.output_index,
                                    "item": { "type": "function_call", "id": item.id, "call_id": item.call_id,
                                              "name": item.name, "arguments": "", "status": "in_progress" } }),
                        ));
                        self.tool_items.push(item);
                        self.tool_items.len() - 1
                    }
                };
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    if !arguments.is_empty() {
                        let item = &mut self.tool_items[position];
                        item.arguments.push_str(arguments);
                        let (id, output_index) = (item.id.clone(), item.output_index);
                        events.push(event(
                            "response.function_call_arguments.delta",
                            json!({ "type": "response.function_call_arguments.delta", "item_id": id,
                                    "output_index": output_index, "delta": arguments }),
                        ));
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(reason.to_string());
        }
        events
    }

    /// 上游流结束：补齐 done 事件与 response.completed。
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if !self.created {
            return Vec::new();
        }
        let mut events = Vec::new();
        let mut output = Vec::<Value>::new();
        if self.message_started {
            events.push(event(
                "response.output_text.done",
                json!({ "type": "response.output_text.done", "item_id": "msg_himind_gateway",
                        "output_index": self.message_output_index(), "content_index": 0, "text": self.message_text }),
            ));
            events.push(event(
                "response.content_part.done",
                json!({ "type": "response.content_part.done", "item_id": "msg_himind_gateway",
                        "output_index": self.message_output_index(), "content_index": 0,
                        "part": { "type": "output_text", "text": self.message_text, "annotations": [] } }),
            ));
            let item = message_item(&self.message_text);
            events.push(event(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": self.message_output_index(), "item": item }),
            ));
            output.push(item);
        }
        for item in &self.tool_items {
            let done = json!({ "type": "function_call", "id": item.id, "call_id": item.call_id,
                               "name": item.name, "arguments": item.arguments, "status": "completed" });
            events.push(event(
                "response.function_call_arguments.done",
                json!({ "type": "response.function_call_arguments.done", "item_id": item.id,
                        "output_index": item.output_index, "arguments": item.arguments }),
            ));
            events.push(event(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": item.output_index, "item": done }),
            ));
            output.push(done);
        }
        let usage = json!({ "usage": {
            "prompt_tokens": self.input_tokens,
            "completion_tokens": self.output_tokens,
            "prompt_tokens_details": { "cached_tokens": self.cached_tokens },
            "completion_tokens_details": { "reasoning_tokens": self.reasoning_tokens },
        }});
        let response = response_envelope(&usage, &self.model, output, "completed");
        events.push(event(
            "response.completed",
            json!({ "type": "response.completed", "response": response }),
        ));
        events
    }

    fn message_output_index(&self) -> u64 {
        0
    }

    fn next_output_index(&self) -> u64 {
        if self.message_started {
            1 + self.tool_items.len() as u64
        } else {
            self.tool_items.len() as u64
        }
    }
}

fn event(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_maps_instructions_and_message_items() {
        let request = json!({
            "model": "gpt-6.1-sol",
            "instructions": "你是助手",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "你好" }] },
                { "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "文件内容" }
            ],
            "tools": [{ "type": "function", "name": "read", "description": "读取", "parameters": { "type": "object" } }],
            "stream": true
        });
        let chat = request_to_chat(&request, "fallback").unwrap();
        assert_eq!(chat["model"], "gpt-6.1-sol");
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][1]["content"], "你好");
        assert_eq!(
            chat["messages"][2]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(chat["messages"][3]["role"], "tool");
        assert_eq!(chat["messages"][3]["content"], "文件内容");
        assert_eq!(chat["tools"][0]["function"]["name"], "read");
        assert_eq!(chat["stream_options"]["include_usage"], true);
    }

    #[test]
    fn request_drops_reasoning_items() {
        let request = json!({
            "input": [
                { "type": "reasoning", "summary": [] },
                { "type": "message", "role": "user", "content": "hi" }
            ]
        });
        let chat = request_to_chat(&request, "m").unwrap();
        assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn developer_role_maps_to_system() {
        let request = json!({
            "input": [
                { "type": "message", "role": "developer", "content": "项目规则" },
                { "type": "message", "role": "user", "content": "hi" }
            ]
        });
        let chat = request_to_chat(&request, "m").unwrap();
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], "项目规则");
        assert_eq!(chat["messages"][1]["role"], "user");
    }

    #[test]
    fn non_stream_response_maps_text_and_usage() {
        let chat = json!({
            "id": "chatcmpl_1",
            "choices": [{ "message": { "role": "assistant", "content": "回答" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 4,
                       "prompt_tokens_details": { "cached_tokens": 3 },
                       "completion_tokens_details": { "reasoning_tokens": 1 } }
        });
        let response = response_to_responses(&chat, "m");
        assert_eq!(response["object"], "response");
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["content"][0]["text"], "回答");
        assert_eq!(response["usage"]["input_tokens"], 9);
        assert_eq!(response["usage"]["output_tokens"], 4);
        assert_eq!(
            response["usage"]["input_tokens_details"]["cached_tokens"],
            3
        );
    }

    #[test]
    fn stream_emits_responses_event_order() {
        let mut stream = ResponsesStream::new("m".to_string());
        let mut out = String::new();
        for line in [
            r#"data: {"id":"c1","choices":[{"delta":{"role":"assistant","content":"收"}}]}"#,
            r#"data: {"id":"c1","choices":[{"delta":{"content":"到"}}]}"#,
            r#"data: {"id":"c1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"data: {"id":"c1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
        ] {
            out.push_str(&stream.push_line(line).concat());
        }
        out.push_str(&stream.finish().concat());
        assert!(out.contains("event: response.created"));
        assert!(out.contains("event: response.output_item.added"));
        assert!(out.contains("event: response.output_text.delta"));
        assert!(out.contains("event: response.output_text.done"));
        assert!(out.contains("event: response.completed"));
        let created = out.find("event: response.created").unwrap();
        let added = out.find("event: response.output_item.added").unwrap();
        let delta = out.find("event: response.output_text.delta").unwrap();
        let completed = out.find("event: response.completed").unwrap();
        assert!(created < added && added < delta && delta < completed);
        assert_eq!(stream.usage(), (5, 2));
    }

    #[test]
    fn stream_maps_tool_call_to_function_call_item() {
        let mut stream = ResponsesStream::new("m".to_string());
        let mut out = String::new();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"a\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ] {
            out.push_str(&stream.push_line(line).concat());
        }
        out.push_str(&stream.finish().concat());
        assert!(out.contains(r#""type":"function_call""#));
        assert!(out.contains("event: response.function_call_arguments.delta"));
        assert!(out.contains(r#""call_id":"call_1""#));
        assert!(out.contains(r#""name":"read""#));
    }
}
