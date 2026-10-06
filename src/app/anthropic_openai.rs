//! 入口 Anthropic Messages ↔ 上游 OpenAI Chat 的互译（ADR 0113 第 3 节）。
//!
//! Claude Code 讲 Anthropic 线格式，而多数可用的上游服务是 OpenAI 兼容。
//! 网关既然站在中间，就必须把「客户端看到的协议」与「上游真实协议」解耦：
//! 客户端按自己的协议说话，绑定的服务按自己的协议应答。
//!
//! 只做这一对本轮用得到的互译；其它组合返回可区分的错误，不静默降级。
//! 图片内容块暂不支持，遇到直接报错而不是悄悄丢掉。

use serde_json::{json, Map, Value};

/// 把 Anthropic Messages 请求体翻成 OpenAI Chat Completions 请求体。
pub(crate) fn request_to_openai(body: &Value, default_model: &str) -> Result<Value, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "Anthropic 请求体不是对象".to_string())?;
    let requested_model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = requested_model.unwrap_or(default_model).to_string();
    if model.is_empty() {
        return Err("绑定没有默认模型，无法翻译请求".to_string());
    }

    let mut messages = Vec::<Value>::new();
    if let Some(system) = object.get("system") {
        let text = blocks_to_text(system)?;
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    let source_messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Anthropic 请求缺少 messages".to_string())?;
    for message in source_messages {
        translate_message(message, &mut messages)?;
    }

    let mut translated = Map::new();
    translated.insert("model".to_string(), json!(model));
    translated.insert("messages".to_string(), Value::Array(messages));
    for key in ["max_tokens", "temperature", "top_p"] {
        if let Some(value) = object.get(key) {
            translated.insert(key.to_string(), value.clone());
        }
    }
    if let Some(stop) = object.get("stop_sequences") {
        translated.insert("stop".to_string(), stop.clone());
    }
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        if !tools.is_empty() {
            translated.insert("tools".to_string(), Value::Array(translate_tools(tools)));
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
        // OpenAI 的流式响应默认不带用量，必须显式要求。
        translated.insert("stream_options".to_string(), json!({ "include_usage": true }));
    }
    Ok(Value::Object(translated))
}

fn translate_message(message: &Value, out: &mut Vec<Value>) -> Result<(), String> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_string();
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    match content {
        Value::String(text) => {
            out.push(json!({ "role": role, "content": text }));
            Ok(())
        }
        Value::Array(blocks) => {
            // assistant 的 tool_use 与 user 的 tool_result 都要落到 OpenAI 的
            // 工具调用/工具结果行，否则下一轮模型看不到自己调过什么。
            let mut text_parts = Vec::<String>::new();
            let mut tool_calls = Vec::<Value>::new();
            let mut tool_results = Vec::<Value>::new();
            for block in &blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        text_parts.push(
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                        let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                        let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": arguments.to_string() },
                        }));
                    }
                    Some("tool_result") => {
                        tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),
                            "content": tool_result_text(block),
                        }));
                    }
                    Some("image") => {
                        return Err("本网关暂不支持图片内容块".to_string());
                    }
                    _ => {}
                }
            }
            if !tool_calls.is_empty() {
                let mut assistant = Map::new();
                assistant.insert("role".to_string(), json!("assistant"));
                assistant.insert(
                    "content".to_string(),
                    if text_parts.is_empty() {
                        Value::Null
                    } else {
                        json!(text_parts.join(""))
                    },
                );
                assistant.insert("tool_calls".to_string(), Value::Array(tool_calls));
                out.push(Value::Object(assistant));
            } else if !text_parts.is_empty() {
                out.push(json!({ "role": role, "content": text_parts.join("") }));
            }
            out.extend(tool_results);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn blocks_to_text(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("image") {
                    return Err("本网关暂不支持图片内容块".to_string());
                }
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            parts.join("")
        }
        _ => String::new(),
    })
}

fn translate_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": tool.get("description").and_then(Value::as_str).unwrap_or_default(),
                    "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                }
            }))
        })
        .collect()
}

fn translate_tool_choice(value: Option<&Value>) -> Option<Value> {
    let choice = value?;
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|name| json!({ "type": "function", "function": { "name": name } })),
        _ => None,
    }
}

/// 非流式的 OpenAI 响应 → Anthropic Messages 响应。
pub(crate) fn response_to_anthropic(openai: &Value, model: &str) -> Result<Value, String> {
    let choice = openai
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| "上游响应没有 choices".to_string())?;
    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut content = Vec::<Value>::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
    }
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for call in &tool_calls {
        let function = call.get("function").cloned().unwrap_or_else(|| json!({}));
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        content.push(json!({
            "type": "tool_use",
            "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
            "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
        }));
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }
    let stop_reason = if !tool_calls.is_empty() {
        "tool_use".to_string()
    } else {
        match choice.get("finish_reason").and_then(Value::as_str) {
            Some("length") => "max_tokens".to_string(),
            Some("tool_calls") => "tool_use".to_string(),
            _ => "end_turn".to_string(),
        }
    };
    Ok(json!({
        "id": openai.get("id").and_then(Value::as_str).unwrap_or("msg_himind_gateway"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage_value(openai, "prompt_tokens"),
            "output_tokens": usage_value(openai, "completion_tokens"),
        }
    }))
}

fn usage_value(openai: &Value, key: &str) -> u64 {
    openai
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// OpenAI 流式分片 → Anthropic SSE 事件。
///
/// 客户端（Claude Code）按 Anthropic 的事件序消费：`message_start` →
/// 若干 `content_block_*` → `message_delta` → `message_stop`。OpenAI 的
/// 文本与工具调用分片在这里被折叠成同一套事件序。
pub(crate) struct AnthropicStream {
    model: String,
    started: bool,
    text_block: Option<u64>,
    tool_blocks: Vec<(u64, u64)>,
    next_index: u64,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    finished: bool,
}

impl AnthropicStream {
    pub(crate) fn new(model: String) -> Self {
        Self {
            model,
            started: false,
            text_block: None,
            tool_blocks: Vec::new(),
            next_index: 0,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            finished: false,
        }
    }

    pub(crate) fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    /// 消费一行 OpenAI SSE，返回需要写给客户端的完整 Anthropic 事件块。
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
        if !self.started {
            self.started = true;
            events.push(event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": chunk.get("id").and_then(Value::as_str).unwrap_or("msg_himind_gateway"),
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": Value::Null,
                        "stop_sequence": Value::Null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 },
                    }
                }),
            ));
        }
        if let Some(usage) = chunk.get("usage") {
            if let Some(input) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = input;
            }
            if let Some(output) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = output;
            }
        }
        let choice = match chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            Some(choice) => choice,
            None => return events,
        };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                let index = match self.text_block {
                    Some(index) => index,
                    None => {
                        let index = self.next_index;
                        self.next_index += 1;
                        self.text_block = Some(index);
                        events.push(event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": { "type": "text", "text": "" },
                            }),
                        ));
                        index
                    }
                };
                events.push(event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": text },
                    }),
                ));
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let source_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let existing = self
                    .tool_blocks
                    .iter()
                    .find(|(source, _)| *source == source_index)
                    .map(|(_, index)| *index);
                let index = match existing {
                    Some(index) => index,
                    None => {
                        let index = self.next_index;
                        self.next_index += 1;
                        self.tool_blocks.push((source_index, index));
                        let function = call.get("function").cloned().unwrap_or_else(|| json!({}));
                        events.push(event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": call.get("id").and_then(Value::as_str).unwrap_or("toolu_himind_gateway"),
                                    "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                                    "input": {},
                                },
                            }),
                        ));
                        index
                    }
                };
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    if !arguments.is_empty() {
                        events.push(event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": index,
                                "delta": { "type": "input_json_delta", "partial_json": arguments },
                            }),
                        ));
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(match reason {
                "length" => "max_tokens".to_string(),
                "tool_calls" => "tool_use".to_string(),
                _ => {
                    if self.tool_blocks.is_empty() {
                        "end_turn".to_string()
                    } else {
                        "tool_use".to_string()
                    }
                }
            });
        }
        events
    }

    /// 上游流结束：补齐块结束事件与 message_delta/message_stop。
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        if !self.started {
            return events;
        }
        let mut open = Vec::new();
        if let Some(index) = self.text_block {
            open.push(index);
        }
        open.extend(self.tool_blocks.iter().map(|(_, index)| *index));
        open.sort_unstable();
        for index in open {
            events.push(event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
        }
        let stop_reason = self
            .stop_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string());
        events.push(event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": Value::Null },
                "usage": { "output_tokens": self.output_tokens },
            }),
        ));
        events.push(event("message_stop", json!({ "type": "message_stop" })));
        events
    }
}

fn event(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_maps_system_messages_and_stream_usage() {
        let request = json!({
            "model": "deepseek-chat",
            "max_tokens": 64,
            "system": "你是助手",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "你好" }] }],
            "stream": true
        });
        let translated = request_to_openai(&request, "fallback").unwrap();
        assert_eq!(translated["model"], "deepseek-chat");
        assert_eq!(translated["messages"][0]["role"], "system");
        assert_eq!(translated["messages"][0]["content"], "你是助手");
        assert_eq!(translated["messages"][1]["content"], "你好");
        assert_eq!(translated["stream_options"]["include_usage"], true);
    }

    #[test]
    fn request_maps_tools_and_tool_results() {
        let request = json!({
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "text", "text": "我来查一下" },
                    { "type": "tool_use", "id": "toolu_1", "name": "read", "input": { "path": "a.txt" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": [{ "type": "text", "text": "内容" }] }
                ]}
            ],
            "tools": [{ "name": "read", "description": "读取文件", "input_schema": { "type": "object" } }],
            "tool_choice": { "type": "auto" }
        });
        let translated = request_to_openai(&request, "fallback").unwrap();
        assert_eq!(translated["model"], "fallback");
        assert_eq!(translated["messages"][0]["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(translated["messages"][1]["role"], "tool");
        assert_eq!(translated["messages"][1]["content"], "内容");
        assert_eq!(translated["tools"][0]["function"]["parameters"]["type"], "object");
        assert_eq!(translated["tool_choice"], "auto");
    }

    #[test]
    fn image_blocks_fail_loudly() {
        let request = json!({
            "messages": [{ "role": "user", "content": [{ "type": "image", "source": {} }] }]
        });
        assert!(request_to_openai(&request, "m").unwrap_err().contains("图片"));
    }

    #[test]
    fn response_maps_text_and_tool_calls() {
        let openai = json!({
            "id": "chatcmpl_1",
            "choices": [{ "message": { "role": "assistant", "content": "回答", "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }
            ]}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 3 }
        });
        let anthropic = response_to_anthropic(&openai, "deepseek-chat").unwrap();
        assert_eq!(anthropic["content"][0]["type"], "text");
        assert_eq!(anthropic["content"][1]["type"], "tool_use");
        assert_eq!(anthropic["content"][1]["input"]["path"], "a");
        assert_eq!(anthropic["stop_reason"], "tool_use");
        assert_eq!(anthropic["usage"]["input_tokens"], 12);
        assert_eq!(anthropic["usage"]["output_tokens"], 3);
    }

    #[test]
    fn stream_emits_anthropic_event_order() {
        let mut stream = AnthropicStream::new("deepseek-chat".to_string());
        let mut out = String::new();
        for line in [
            r#"data: {"id":"chatcmpl_1","choices":[{"delta":{"role":"assistant","content":"你"}}]}"#,
            r#"data: {"id":"chatcmpl_1","choices":[{"delta":{"content":"好"}}]}"#,
            r#"data: {"id":"chatcmpl_1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"data: {"id":"chatcmpl_1","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#,
            "data: [DONE]",
        ] {
            out.push_str(&stream.push_line(line).join(""));
        }
        out.push_str(&stream.finish().join(""));
        assert!(out.contains("event: message_start"));
        assert!(out.contains("event: content_block_start"));
        assert!(out.contains(r#""text_delta""#));
        assert!(out.contains("event: content_block_stop"));
        assert!(out.contains(r#""stop_reason":"end_turn""#));
        assert!(out.contains("event: message_stop"));
        let start = out.find("event: message_start").unwrap();
        let block = out.find("event: content_block_start").unwrap();
        let stop = out.find("event: content_block_stop").unwrap();
        let delta = out.find("event: message_delta").unwrap();
        assert!(start < block && block < stop && stop < delta, "事件顺序必须是 Anthropic 的规范序");
        assert_eq!(stream.usage(), (7, 2));
    }

    #[test]
    fn stream_maps_tool_call_fragments_to_input_json_delta() {
        let mut stream = AnthropicStream::new("m".to_string());
        let mut out = String::new();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ] {
            out.push_str(&stream.push_line(line).join(""));
        }
        out.push_str(&stream.finish().join(""));
        assert!(out.contains(r#""type":"tool_use""#));
        assert!(out.contains(r#""name":"read""#));
        assert!(out.contains(r#""partial_json":"{\"path\":"#));
        assert!(out.contains(r#""stop_reason":"tool_use""#));
    }
}
