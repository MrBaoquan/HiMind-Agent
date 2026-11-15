from pathlib import Path
p=Path('src/app/inference_gateway.rs')
s=p.read_text(encoding='utf-8')
# Never echo provider controlled text, including apparently harmless message fields.
s=s.replace('let detail = String::from_utf8_lossy(&raw_error).to_string();','let detail = "上游拒绝请求；请凭 request_id 查询安全诊断。";')
s=s.replace('"error": { "type": "api_error", "message": detail }','"error": { "type": "api_error", "code": "himind_gateway_upstream_rejected", "request_id": current_request_id(), "message": detail }')
a=s.index('        write_buffered_head(\n            stream,\n            status.as_u16(),\n            &content_type,',s.index('if !status.is_success() {',s.index('fn forward(')))
b=s.index('        if kind != "models" {',a)
s=s[:a]+'''        write_json_error(stream, status.as_u16(), "himind_gateway_upstream_rejected", "上游拒绝请求；请凭 request_id 查询安全诊断。")
            .map_err(|_| GatewayError::Answered)?;
'''+s[b:]
# Distinguish body failures everywhere.
s=s.replace('Err(actual) => {\n            let message = format!(\n                "上游非流式响应超过本机网关上限（{actual} bytes > {RESPONSE_BODY_LIMIT} bytes），已拒绝转发。"\n            );','Err(failure) => {\n            let message = "上游响应读取未完成，已拒绝转发。";')
s=s.replace('"himind_gateway_response_too_large",\n                &message,','failure.code(),\n                message,')
a=s.index('fn read_success_body(');b=s.index('\nfn write_upstream_failure',a)
s=s[:a]+'''#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFailure { TooLarge, Timeout, Truncated, Io }
impl BodyFailure {
    fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "himind_gateway_response_too_large",
            Self::Timeout => "himind_gateway_upstream_timeout",
            Self::Truncated => "himind_gateway_response_truncated",
            Self::Io => "himind_gateway_upstream_io",
        }
    }
}
fn classify_read_error(error: &std::io::Error) -> BodyFailure {
    // reqwest wraps hyper errors in io::ErrorKind::Other. Inspect the private
    // chain only for classification; never publish or log its text/URL.
    let text = format!("{error:?}").to_ascii_lowercase();
    if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
        || text.contains("timeout") || text.contains("timedout") { BodyFailure::Timeout }
    else if error.kind() == std::io::ErrorKind::UnexpectedEof || text.contains("incomplete")
        || text.contains("unexpected eof") || text.contains("body length") { BodyFailure::Truncated }
    else { BodyFailure::Io }
}
fn read_success_body(response: reqwest::blocking::Response) -> Result<Vec<u8>, BodyFailure> {
    let mut raw = Vec::new();
    response.take(RESPONSE_BODY_LIMIT as u64 + 1).read_to_end(&mut raw)
        .map_err(|error| classify_read_error(&error))?;
    if raw.len() > RESPONSE_BODY_LIMIT { return Err(BodyFailure::TooLarge); }
    // A clean EOF is not proof of a complete JSON response.
    serde_json::from_slice::<Value>(&raw).map_err(|_| BodyFailure::Truncated)?;
    Ok(raw)
}
'''+s[b:]
s=s.replace('fn write_upstream_failure(stream: &mut TcpStream, detail: &str)', 'fn write_upstream_failure(stream: &mut TcpStream, _detail: &str)')
s=s.replace('request_id={request_id} {detail}', 'request_id={request_id} code=himind_gateway_upstream_failed')
s=s.replace('&format!("上游请求失败：{detail}（request_id={request_id}）")','"上游请求失败；请凭 request_id 查询安全诊断。"')
# All generic public errors have stable code and request id, no uncontrolled detail.
s=s.replace('let body = json!({ "error": { "code": code, "message": message } }).to_string();','''let safe_message = if code.contains("translation") || code.contains("upstream") {
        "上游请求或协议处理失败；请凭 request_id 查询安全诊断。"
    } else { message };
    let body = json!({ "type": "error", "error": { "type": "api_error", "code": code,
        "request_id": current_request_id(), "message": safe_message } }).to_string();''')
# No provider message/skeleton is safe merely because known token patterns were removed.
a=s.index('    let value = serde_json::from_str::<Value>(upstream_text).ok();',s.index('fn summarize_upstream_error'))
b=s.index('\n}\n',a)
s=s[:a]+'''    format!("upstream_body_withheld bytes={}", upstream_text.len())'''+s[b:]
s=s.replace('let request_id = upstream_request_id(upstream_headers, upstream_text.as_ref());','let request_id = Some(gateway_request_id.clone());\n    let _ = upstream_headers;')
s=s.replace('"type": "invalid_request_error",\n                "message": safe_message,','"type": "invalid_request_error",\n                "code": "himind_gateway_compatibility_rejected",\n                "request_id": gateway_request_id,\n                "message": safe_message,')
s=s.replace('"code": "himind_gateway_compatibility_rejected",\n                "message": safe_message,','"code": "himind_gateway_compatibility_rejected",\n                "request_id": gateway_request_id,\n                "message": safe_message,')
s=s.replace('"code": "himind_gateway_upstream_not_found",','"code": "himind_gateway_upstream_not_found",\n                "request_id": current_request_id(),')
# Native stream forwarding uses same boundary as translators.
a=s.index('        write_stream_head(stream, status.as_u16(), &content_type,',s.index('// 流式：'))
b=s.index('        if content_sanitizer.removed()',a)
s=s[:a]+'''        let mut content_sanitizer = ReasoningTagSanitizer::default();
        let mut scanner = StreamScanner::new(response_binding.protocol.clone());
        let complete = relay_stream(stream, &mut response, &response_binding.id, &response_binding.protocol,
            |line| {
                scanner.push(format!("{line}\\n").as_bytes());
                format!("{}\\n", rewrite_sse_model_line(line, &display_model, &mut content_sanitizer))
            });
'''+s[b:]
a=s.index('        let usage = scanner.usage;',a);b=s.index('        return Ok(());',a)
part=s[a:b].replace('status.as_u16(),','if complete { status.as_u16() } else { 502 },').replace('usage.is_empty(),','!complete || usage.is_empty(),')
s=s[:a]+part+s[b:]
# Replace four duplicate read/translate loops. Never synthesize completion after a transport failure.
for name in ['crate::app::anthropic_openai::AnthropicStream','crate::app::anthropic_responses::AnthropicResponsesStream','crate::app::responses_chat::ResponsesStream','crate::app::chat_responses::ChatStream']:
    pos=s.index(name+'::new(display_model.clone())')
    a=s.rfind('        write_stream_head(',0,pos)
    b=s.index('        let (input, output) = translator.usage();',pos)
    protocol='openai-chat' if ('anthropic_openai' in name or 'responses_chat' in name) else 'openai-responses'
    s=s[:a]+f'''        let mut translator = {name}::new(display_model.clone());
        let complete = relay_stream(stream, &mut response, &binding.id, "{protocol}",
            |line| translator.push_line(line).concat());
        let mut failed = !complete || translator.failed();
        if !failed {{
            let tail = translator.finish().concat();
            failed = stream.write_all(tail.as_bytes()).and_then(|_| stream.flush()).is_err()
                || translator.failed();
        }}
'''+s[b:]
# only in stream usage blocks
s=s.replace('            input == 0 && output == 0,','            failed || (input == 0 && output == 0),')
# Common relay observes terminal protocol event, bounded lines, error events, IO, EOF.
pos=s.index('/// SSE 行扫描')
s=s[:pos]+'''/// No retry is permitted once an upstream 200 body is being consumed. A clean
/// socket EOF without a protocol terminal marker is still an interrupted stream.
fn relay_stream<F: FnMut(&str) -> String>(
    stream: &mut TcpStream, response: &mut reqwest::blocking::Response,
    binding_id: &str, protocol: &str, mut translate: F,
) -> bool {
    let mut pending = String::new();
    let mut chunk = [0u8; 8192];
    let mut written = false;
    let mut terminal = false;
    let mut total = 0usize;
    let failure = loop {
        let count = match response.read(&mut chunk) {
            Ok(0) => {
                if terminal && pending.trim().is_empty() { return true; }
                break BodyFailure::Truncated;
            },
            Ok(count) => count,
            Err(error) => break classify_read_error(&error),
        };
        total = total.saturating_add(count);
        if total > RESPONSE_BODY_LIMIT { break BodyFailure::TooLarge; }
        pending.push_str(&String::from_utf8_lossy(&chunk[..count]));
        if pending.len() > SSE_LINE_LIMIT && !pending.contains('\\n') { break BodyFailure::TooLarge; }
        let mut protocol_error = false;
        while let Some(position) = pending.find('\\n') {
            if position > SSE_LINE_LIMIT { protocol_error = true; break; }
            let line = pending[..position].trim_end_matches('\\r').to_string();
            pending.drain(..=position);
            if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                if data == "[DONE]" { terminal = protocol == "openai-chat"; }
                else if let Ok(value) = serde_json::from_str::<Value>(data) {
                    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
                    if value.get("error").is_some() || matches!(kind, "error" | "response.failed" | "response.incomplete") {
                        protocol_error = true; break;
                    }
                    terminal |= matches!((protocol, kind), ("openai-responses", "response.completed") | ("anthropic", "message_stop"));
                }
            }
            // Delay HTTP headers until at least one complete, non-error line.
            if !written {
                if write_stream_head(stream, 200, "text/event-stream", binding_id).is_err() { return false; }
                written = true;
            }
            let output = translate(&line);
            if stream.write_all(output.as_bytes()).and_then(|_| stream.flush()).is_err() {
                log_transport_outcome("himind_gateway_client_io", true); return false;
            }
        }
        if protocol_error { break BodyFailure::Truncated; }
    };
    log_transport_outcome(failure.code(), written);
    if !written {
        let _ = write_json_error(stream, 502, failure.code(), "上游响应未完成。");
    } else {
        let payload = json!({"type":"error","error":{"type":"api_error","code":failure.code(),
            "request_id":current_request_id(),"message":"上游流已中断；不会自动重放。"}});
        let _ = write!(stream, "event: error\\ndata: {payload}\\n\\n");
        let _ = stream.flush();
    }
    false
}
fn log_transport_outcome(code: &str, written: bool) {
    crate::approval::manager::append_event_log("warn", &format!(
        "gateway transport_outcome={code} request_id={} response_started={written} usage_complete=false replay=false", current_request_id()));
}

'''+s[pos:]
p.write_text(s,encoding='utf-8')
