//! Local speech-provider primitives.
//!
//! This module invokes a configured OpenAI-compatible speech endpoint and
//! returns a normalised audio artefact. Producer workflows (beat segmentation,
//! voice licensing, mixing, and QA) remain owned by the audio-production
//! extension.

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::ai_services::{
    self, AIServiceProtocol, CustomAIService, MediaExecutionMode, CAP_AUDIO_SPEECH,
    CAP_AUDIO_TRANSCRIBE,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 180;
const FISH_ASR_TIMEOUT_SECONDS: u64 = 900;
const MAX_TEXT_CHARS: usize = 4096;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_VOICE: &str = "alloy";
const DEFAULT_FORMAT: &str = "mp3";
const ADAPTER_OPENAI_SPEECH: &str = "openai-speech";
const ADAPTER_OPENAI_AUDIO: &str = "openai-audio";
const ADAPTER_FISH_SPEECH: &str = "fish-audio-tts";
const ADAPTER_FISH_TRANSCRIBE: &str = "fish-audio-asr";

#[derive(Debug)]
struct SpeechRequest {
    text: String,
    service: String,
    model: Option<String>,
    voice: Option<String>,
    format: String,
    speed: Option<f64>,
    output_dir: Option<String>,
    filename: Option<String>,
    provider_options: Option<Map<String, Value>>,
}

impl SpeechRequest {
    fn parse(input: &Value) -> Result<Self, Box<dyn Error>> {
        let object = input.as_object().ok_or("语音合成参数必须是 JSON 对象")?;
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("text 不能为空")?
            .to_string();
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!("text 过长（最多 {MAX_TEXT_CHARS} 个字符）").into());
        }
        let voice = optional_string(input, "voice", 120)?;
        let format = optional_string(input, "format", 16)?
            .unwrap_or_else(|| DEFAULT_FORMAT.to_string())
            .to_ascii_lowercase();
        if !matches!(
            format.as_str(),
            "mp3" | "opus" | "aac" | "flac" | "wav" | "pcm"
        ) {
            return Err("format 只支持 mp3、opus、aac、flac、wav 或 pcm".into());
        }
        let speed = match input.get("speed") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|value| (0.25..=4.0).contains(value))
                    .ok_or("speed 必须是 0.25-4.0 的数字")?,
            ),
        };
        let provider_options = match object.get("provider_options") {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(map.clone()),
            Some(_) => return Err("provider_options 必须是对象".into()),
        };
        Ok(Self {
            text,
            service: optional_string(input, "service", 80)?.unwrap_or_default(),
            model: optional_string(input, "model", 160)?,
            voice,
            format,
            speed,
            output_dir: optional_string(input, "output_dir", 2000)?,
            filename: optional_string(input, "filename", 120)?,
            provider_options,
        })
    }
}

/// `media.audio.speech`: one synchronous provider invocation.
pub(crate) fn speech(input: &Value) -> Result<Value, Box<dyn Error>> {
    let request = SpeechRequest::parse(input)?;
    let (service, api_key) =
        ai_services::resolve_media_service(CAP_AUDIO_SPEECH, &request.service)?;
    let binding = resolve_binding(&service, CAP_AUDIO_SPEECH, request.model.as_deref())?;
    if !matches!(
        binding.adapter.as_str(),
        ADAPTER_OPENAI_SPEECH | ADAPTER_FISH_SPEECH
    ) {
        return Err(format!(
            "适配器 {} 暂不支持同步语音合成；当前支持 {ADAPTER_OPENAI_SPEECH}、{ADAPTER_FISH_SPEECH}",
            binding.adapter
        )
        .into());
    }
    if binding.execution_mode != MediaExecutionMode::Sync {
        return Err(
            "当前 media.audio.speech 入口只执行同步 Provider；异步/流式需要对应适配器入口".into(),
        );
    }
    let output_dir = crate::app::local_image::resolve_output_dir(request.output_dir.as_deref())?;
    let mut client_builder =
        Client::builder().timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS));
    if is_loopback_url(&service.base_url) {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;

    let voice_for_response: String;
    let response = if binding.adapter == ADAPTER_OPENAI_SPEECH {
        let voice = request
            .voice
            .clone()
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());
        voice_for_response = voice.clone();
        let mut body = json!({
            "model": binding.model,
            "input": request.text,
            "voice": voice,
            "response_format": request.format,
        });
        if let Some(speed) = request.speed {
            body["speed"] = json!(speed);
        }
        if let Some(options) = request.provider_options {
            for (key, value) in options {
                if !matches!(
                    key.as_str(),
                    "model" | "input" | "voice" | "response_format" | "speed"
                ) {
                    body[key] = value;
                }
            }
        }
        client
            .post(speech_endpoint(&service.base_url))
            .bearer_auth(&api_key)
            .json(&body)
            .send()?
    } else {
        if !matches!(request.format.as_str(), "mp3" | "wav" | "pcm" | "opus") {
            return Err("Fish Audio 的 format 只支持 mp3、wav、pcm 或 opus".into());
        }
        if request
            .speed
            .is_some_and(|speed| !(0.5..=2.0).contains(&speed))
        {
            return Err("Fish Audio 的 speed 必须在 0.5-2.0 之间".into());
        }
        voice_for_response = request.voice.clone().unwrap_or_default();
        let mut body = json!({
            "text": request.text,
            "format": request.format,
        });
        if let Some(reference_id) = request.voice.as_deref() {
            body["reference_id"] = json!(reference_id);
        }
        if let Some(speed) = request.speed {
            body["speed"] = json!(speed);
        }
        if let Some(options) = request.provider_options {
            for (key, value) in options {
                if !matches!(
                    key.as_str(),
                    "text" | "reference_id" | "format" | "speed" | "model"
                ) {
                    body[key] = value;
                }
            }
        }
        client
            .post(fish_tts_endpoint(&service.base_url))
            .header("model", binding.model.clone())
            .bearer_auth(&api_key)
            .json(&body)
            .send()?
    };

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        return Err(normalized_provider_error(Some(status.as_u16()), &text).into());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    if !content_type.starts_with("audio/") && content_type != "application/octet-stream" {
        return Err(format!("语音合成 Provider 返回了非音频内容：{content_type}").into());
    }
    let bytes = response.bytes()?;
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!("语音产物大小异常：{} bytes", bytes.len()).into());
    }
    fs::create_dir_all(&output_dir)?;
    let path = output_dir.join(output_filename(
        request.filename.as_deref(),
        &request.format,
        &bytes,
    ));
    crate::store::atomic_file::atomic_write(&path, &bytes)?;
    Ok(json!({
        "state": "ready",
        "capability": CAP_AUDIO_SPEECH,
        "provider": service.id,
        "adapter": binding.adapter,
        "model": binding.model,
        "execution_mode": "sync",
        "voice": voice_for_response,
        "format": request.format,
        "path": path.to_string_lossy(),
        "mime": content_type,
        "bytes": bytes.len(),
        "sha256": hex_sha256(&bytes),
    }))
}
#[derive(Debug, Clone)]
struct ResolvedBinding {
    adapter: String,
    model: String,
    execution_mode: MediaExecutionMode,
}

fn resolve_binding(
    service: &CustomAIService,
    capability: &str,
    requested_model: Option<&str>,
) -> Result<ResolvedBinding, Box<dyn Error>> {
    if matches!(service.protocol, AIServiceProtocol::Anthropic) {
        return Err(format!(
            "服务「{}」使用 Anthropic 协议，不支持 OpenAI 兼容音频能力",
            service.display_name
        )
        .into());
    }
    let requested = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (
        ai_services::binding_details_for(service, capability),
        requested,
    ) {
        (Some(binding), Some(model)) => Ok(ResolvedBinding {
            adapter: binding.adapter,
            model: model.to_string(),
            execution_mode: binding.execution_mode,
        }),
        (Some(binding), None) => Ok(ResolvedBinding {
            adapter: binding.adapter,
            model: binding.model,
            execution_mode: binding.execution_mode,
        }),
        (None, Some(model)) => Ok(ResolvedBinding {
            adapter: ai_services::default_media_adapter(capability).to_string(),
            model: model.to_string(),
            execution_mode: ai_services::default_execution_mode(capability),
        }),
        (None, None) => Err(format!(
            "服务「{}」尚未配置 {capability} 媒体能力；请先填写对应模型",
            service.display_name
        )
        .into()),
    }
}

#[derive(Debug)]
struct TranscriptionRequest {
    file: PathBuf,
    service: String,
    model: Option<String>,
    language: Option<String>,
    prompt: Option<String>,
    format: String,
    provider_options: Option<Map<String, Value>>,
}

impl TranscriptionRequest {
    fn parse(input: &Value) -> Result<Self, Box<dyn Error>> {
        let object = input.as_object().ok_or("语音转写参数必须是 JSON 对象")?;
        let file = input
            .get("file")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("file 不能为空")?;
        let file = PathBuf::from(file);
        if !file.is_absolute() {
            return Err("file 必须是绝对路径".into());
        }
        if !file.is_file() {
            return Err(format!("音频文件不存在：{}", file.display()).into());
        }
        let format = optional_string(input, "format", 24)?
            .unwrap_or_else(|| "json".to_string())
            .to_ascii_lowercase();
        if !matches!(
            format.as_str(),
            "json" | "text" | "verbose_json" | "srt" | "vtt"
        ) {
            return Err("format 只支持 json、text、verbose_json、srt 或 vtt".into());
        }
        let provider_options = match object.get("provider_options") {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(map.clone()),
            Some(_) => return Err("provider_options 必须是对象".into()),
        };
        Ok(Self {
            file,
            service: optional_string(input, "service", 80)?.unwrap_or_default(),
            model: optional_string(input, "model", 160)?,
            language: optional_string(input, "language", 32)?,
            prompt: optional_string(input, "prompt", 2000)?,
            format,
            provider_options,
        })
    }
}

/// `media.audio.transcribe`: one synchronous provider invocation.
pub(crate) fn transcribe(input: &Value) -> Result<Value, Box<dyn Error>> {
    let request = TranscriptionRequest::parse(input)?;
    let (service, api_key) =
        ai_services::resolve_media_service(CAP_AUDIO_TRANSCRIBE, &request.service)?;
    let binding = resolve_binding(&service, CAP_AUDIO_TRANSCRIBE, request.model.as_deref())?;
    if !matches!(
        binding.adapter.as_str(),
        ADAPTER_OPENAI_AUDIO | ADAPTER_FISH_TRANSCRIBE
    ) {
        return Err(format!(
            "适配器 {} 暂不支持同步语音转写；当前支持 {ADAPTER_OPENAI_AUDIO}、{ADAPTER_FISH_TRANSCRIBE}",
            binding.adapter
        )
        .into());
    }
    if binding.execution_mode != MediaExecutionMode::Sync {
        return Err("当前 media.audio.transcribe 入口只执行同步 Provider".into());
    }
    if binding.adapter == ADAPTER_FISH_TRANSCRIBE
        && !matches!(request.format.as_str(), "json" | "text" | "verbose_json")
    {
        return Err(
            "Fish Audio ASR 只返回 JSON 结果；format 仅支持 json、text 或 verbose_json".into(),
        );
    }
    let metadata = fs::metadata(&request.file)?;
    if metadata.len() == 0 {
        return Err("音频文件为空".into());
    }
    let bytes = fs::read(&request.file)?;
    let file_name = request
        .file
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio.bin")
        .to_string();
    let part = Part::bytes(bytes).file_name(file_name);
    let timeout = if binding.adapter == ADAPTER_FISH_TRANSCRIBE {
        Duration::from_secs(FISH_ASR_TIMEOUT_SECONDS)
    } else {
        Duration::from_secs(DEFAULT_TIMEOUT_SECONDS)
    };
    let mut client_builder = Client::builder().timeout(timeout);
    if is_loopback_url(&service.base_url) {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;

    let response = if binding.adapter == ADAPTER_OPENAI_AUDIO {
        let mut form = Form::new()
            .part("file", part)
            .text("model", binding.model.clone())
            .text("response_format", request.format.clone());
        if let Some(language) = request.language.clone() {
            form = form.text("language", language);
        }
        if let Some(prompt) = request.prompt.clone() {
            form = form.text("prompt", prompt);
        }
        if let Some(options) = request.provider_options.clone() {
            form = append_multipart_options(
                form,
                options,
                &["file", "model", "response_format", "language", "prompt"],
            );
        }
        client
            .post(transcription_endpoint(&service.base_url))
            .bearer_auth(&api_key)
            .multipart(form)
            .send()?
    } else {
        let mut form = Form::new().part("audio", part);
        if let Some(language) = request.language.clone() {
            form = form.text("language", language);
        }
        if let Some(options) = request.provider_options.clone() {
            form = append_multipart_options(
                form,
                options,
                &[
                    "audio",
                    "file",
                    "model",
                    "response_format",
                    "language",
                    "prompt",
                ],
            );
        }
        client
            .post(fish_asr_endpoint(&service.base_url))
            .header("model", binding.model.clone())
            .bearer_auth(&api_key)
            .multipart(form)
            .send()?
    };

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        return Err(normalized_provider_error(Some(status.as_u16()), &text).into());
    }
    let raw = response.text()?;
    let parsed = serde_json::from_str::<Value>(&raw).ok();
    let text = parsed
        .as_ref()
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| raw.clone());
    let duration_seconds = parsed
        .as_ref()
        .and_then(|value| value.get("duration"))
        .and_then(Value::as_f64);
    let segments = parsed
        .as_ref()
        .and_then(|value| value.get("segments"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let speaker_turns = parsed
        .as_ref()
        .and_then(|value| value.get("speaker_turns"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let language = parsed
        .as_ref()
        .and_then(|value| value.get("language").or_else(|| value.get("language_code")))
        .cloned()
        .unwrap_or(Value::Null);
    let request_id = parsed
        .as_ref()
        .and_then(|value| value.get("request_id"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "state": "ready",
        "capability": CAP_AUDIO_TRANSCRIBE,
        "provider": service.id,
        "adapter": binding.adapter,
        "model": binding.model,
        "execution_mode": "sync",
        "format": request.format,
        "text": text,
        "duration_seconds": duration_seconds,
        "segments": segments,
        "speaker_turns": speaker_turns,
        "language": language,
        "provider_request_id": request_id,
        "result": parsed,
    }))
}

fn append_multipart_options(
    mut form: Form,
    options: Map<String, Value>,
    reserved: &[&str],
) -> Form {
    for (key, value) in options {
        if reserved.contains(&key.as_str()) {
            continue;
        }
        if let Some(value) = value.as_str() {
            form = form.text(key, value.to_string());
        } else if let Some(value) = value.as_bool() {
            form = form.text(key, value.to_string());
        } else if let Some(value) = value.as_i64() {
            form = form.text(key, value.to_string());
        } else if let Some(value) = value.as_f64() {
            form = form.text(key, value.to_string());
        }
    }
    form
}
fn is_loopback_url(base_url: &str) -> bool {
    url::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
}

fn fish_asr_endpoint(base_url: &str) -> String {
    provider_endpoint(base_url, "asr")
}
fn fish_tts_endpoint(base_url: &str) -> String {
    provider_endpoint(base_url, "tts")
}
fn speech_endpoint(base_url: &str) -> String {
    provider_endpoint(base_url, "audio/speech")
}

fn transcription_endpoint(base_url: &str) -> String {
    provider_endpoint(base_url, "audio/transcriptions")
}

fn provider_endpoint(base_url: &str, suffix: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/{suffix}")
    } else {
        format!("{base}/v1/{suffix}")
    }
}

fn normalized_provider_error(status: Option<u16>, message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("content policy")
        || lower.contains("safety")
        || lower.contains("moderation")
    {
        "content_policy"
    } else if lower.contains("quota") || lower.contains("insufficient") {
        "quota"
    } else {
        match status {
            Some(401) | Some(403) => "auth",
            Some(400) | Some(413) | Some(422) => "invalid_input",
            Some(408) | Some(504) => "timeout",
            Some(429) => "rate_limit",
            Some(500..=599) => "provider",
            _ => "provider",
        }
    };
    format!("media_provider_{kind}: {}", truncate(message.trim(), 500))
}

fn output_filename(prefix: Option<&str>, format: &str, bytes: &[u8]) -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let stem = prefix
        .map(sanitize_stem)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "speech".to_string());
    let digest = hex_sha256(bytes);
    format!("{stem}-{stamp}-{}.{}", &digest[..12], format)
}

fn sanitize_stem(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    value.trim_matches('-').to_string()
}

fn optional_string(input: &Value, key: &str, max: usize) -> Result<Option<String>, Box<dyn Error>> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| format!("{key} 必须是字符串"))?
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(format!("{key} 过长（最多 {max} 个字符）").into());
    }
    Ok(Some(value.to_string()))
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

fn truncate(value: &str, limit: usize) -> String {
    let mut text: String = value.chars().take(limit).collect();
    if value.chars().count() > limit {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;

    fn fake_openai_speech() -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buffer = [0u8; 8192];
            let mut text = String::new();
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                    Err(_) => break,
                }
            }
            *sink.lock().unwrap() = text;
            let audio: &[u8] = b"ID3-test-audio";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                audio.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(audio);
            let _ = stream.flush();
        });
        (format!("http://{address}"), captured)
    }

    fn fake_fish_transcription() -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buffer = [0u8; 8192];
            let mut text = String::new();
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                    Err(_) => break,
                }
            }
            *sink.lock().unwrap() = text;
            let body = "{\"text\":\"你好，Fish Audio\",\"duration\":12.5,\"language\":\"zh\",\"segments\":[{\"text\":\"你好\",\"start\":0.0,\"end\":0.5}],\"speaker_turns\":[{\"speaker\":\"S1\",\"start\":0.0,\"end\":2.0}],\"request_id\":\"req-fish-1\"}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (format!("http://{address}"), captured)
    }
    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "himind-local-speech-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fake_openai_transcription() -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buffer = [0u8; 8192];
            let mut text = String::new();
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => text.push_str(&String::from_utf8_lossy(&buffer[..read])),
                    Err(_) => break,
                }
            }
            *sink.lock().unwrap() = text;
            let body = "{\"text\":\"你好世界\",\"language\":\"zh\"}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (format!("http://{address}"), captured)
    }

    #[test]
    fn transcription_posts_multipart_and_returns_text() {
        let (base_url, captured) = fake_openai_transcription();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("transcribe-home");
        let source = temp_dir("transcribe-src").join("sample.mp3");
        fs::write(&source, b"ID3-test-audio").unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let service_id = "voice-transcribe";
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_AUDIO_TRANSCRIBE.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_OPENAI_AUDIO.to_string(),
                model: "whisper-test".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        ai_services::upsert(ai_services::CustomAIServiceInput {
            id: service_id.to_string(),
            display_name: "Transcribe".to_string(),
            base_url,
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: "sk-transcribe-test".to_string(),
        })
        .unwrap();

        let result = transcribe(&json!({
            "file": source,
            "service": format!("custom:{service_id}"),
            "format": "json"
        }))
        .expect("transcription should succeed");
        assert_eq!(result["text"], "你好世界");
        assert!(!result.to_string().contains("sk-transcribe-test"));
        let request = captured.lock().unwrap().clone();
        assert!(request.contains("multipart/form-data"));
        assert!(request.contains("whisper-test"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn fish_audio_asr_maps_model_header_and_structured_result() {
        let (base_url, captured) = fake_fish_transcription();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("fish-asr-home");
        let source = temp_dir("fish-asr-src").join("sample.mp3");
        fs::write(&source, b"ID3-test-audio").unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let service_id = "fish-transcribe";
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_AUDIO_TRANSCRIBE.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_FISH_TRANSCRIBE.to_string(),
                model: "transcribe-1-pro".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        ai_services::upsert(ai_services::CustomAIServiceInput {
            id: service_id.to_string(),
            display_name: "Fish Transcribe".to_string(),
            base_url: base_url.clone(),
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: "sk-fish-asr-test".to_string(),
        })
        .unwrap();

        let result = transcribe(&json!({
            "file": source,
            "service": format!("custom:{service_id}"),
            "format": "verbose_json",
            "language": "zh",
            "provider_options": {
                "ignore_timestamps": false,
                "diarize": true,
                "tag_audio_events": false,
                "num_speakers": 2
            }
        }))
        .expect("Fish transcription should succeed");
        assert_eq!(result["text"], "你好，Fish Audio");
        assert_eq!(result["duration_seconds"], 12.5);
        assert_eq!(result["segments"].as_array().unwrap().len(), 1);
        assert_eq!(result["speaker_turns"].as_array().unwrap().len(), 1);
        assert_eq!(result["provider_request_id"], "req-fish-1");
        assert_eq!(result["adapter"], ADAPTER_FISH_TRANSCRIBE);
        let request = captured.lock().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post /v1/asr "), "{request}");
        assert!(request.contains("authorization: bearer sk-fish-asr-test"));
        assert!(request.contains("model: transcribe-1-pro"));
        assert!(request.contains("name=\"audio\""));
        assert!(request.contains("name=\"ignore_timestamps\""));
        assert!(request.contains("false"));
        assert!(request.contains("name=\"diarize\""));
        assert!(!result.to_string().contains("sk-fish-asr-test"));
        let _ = fs::remove_dir_all(&home);
    }
    #[test]
    fn fish_audio_tts_maps_model_header_voice_and_format() {
        let (base_url, captured) = fake_openai_speech();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("fish-tts-home");
        let out_dir = temp_dir("fish-tts-out");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let service_id = "fish-voice";
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_AUDIO_SPEECH.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_FISH_SPEECH.to_string(),
                model: "s2.1-pro-free".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        ai_services::upsert(ai_services::CustomAIServiceInput {
            id: service_id.to_string(),
            display_name: "Fish Voice".to_string(),
            base_url: base_url.clone(),
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: "sk-fish-test".to_string(),
        })
        .unwrap();

        let result = speech(&json!({
            "text": "你好，Fish Audio",
            "service": format!("custom:{service_id}"),
            "voice": "voice-reference-1",
            "format": "mp3",
            "speed": 1.2,
            "output_dir": out_dir,
            "filename": "fish-voice"
        }))
        .expect("Fish speech should succeed");
        let path = result.get("path").and_then(Value::as_str).unwrap();
        assert!(Path::new(path).is_file());
        assert_eq!(result["adapter"], ADAPTER_FISH_SPEECH);
        assert_eq!(result["voice"], "voice-reference-1");
        let request = captured.lock().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post /v1/tts "), "{request}");
        assert!(request.contains("authorization: bearer sk-fish-test"));
        assert!(request.contains("model: s2.1-pro-free"));
        assert!(request.contains("\"text\":\"你好，fish audio\""));
        assert!(request.contains("\"reference_id\":\"voice-reference-1\""));
        assert!(request.contains("\"format\":\"mp3\""));
        assert!(request.contains("\"speed\":1.2"));
        assert!(!result.to_string().contains("sk-fish-test"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&out_dir);
    }
    #[test]
    #[ignore = "requires HIMIND_FISH_LIVE_API_KEY and network/proxy access"]
    fn live_fish_audio_tts_and_asr_round_trip() {
        let api_key = std::env::var("HIMIND_FISH_LIVE_API_KEY")
            .expect("HIMIND_FISH_LIVE_API_KEY must be set");
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("fish-live-home");
        let out_dir = temp_dir("fish-live-out");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let service_id = "fish-live";
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_AUDIO_SPEECH.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_FISH_SPEECH.to_string(),
                model: "s2.1-pro-free".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        media.insert(
            CAP_AUDIO_TRANSCRIBE.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_FISH_TRANSCRIBE.to_string(),
                model: "transcribe-1-pro".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        ai_services::upsert(ai_services::CustomAIServiceInput {
            id: service_id.to_string(),
            display_name: "Fish Live".to_string(),
            base_url: "https://api.fish.audio".to_string(),
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key,
        })
        .unwrap();

        let speech_result = speech(&json!({
            "text": "HiMind Agent Fish Audio live integration test.",
            "service": format!("custom:{service_id}"),
            "format": "mp3",
            "output_dir": out_dir,
            "filename": "live-fish"
        }))
        .expect("live Fish TTS should succeed");
        let path = speech_result["path"].as_str().unwrap().to_string();
        assert!(Path::new(&path).is_file());
        assert!(fs::metadata(&path).unwrap().len() > 1_000);
        let asr_result = transcribe(&json!({
            "file": path,
            "service": format!("custom:{service_id}"),
            "format": "verbose_json",
            "provider_options": {
                "ignore_timestamps": false,
                "diarize": false
            }
        }))
        .expect("live Fish ASR should succeed");
        assert!(!asr_result["text"].as_str().unwrap_or_default().is_empty());
        assert!(!asr_result["segments"].as_array().unwrap().is_empty());
        assert_eq!(asr_result["adapter"], ADAPTER_FISH_TRANSCRIBE);
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&out_dir);
    }
    #[test]
    fn provider_errors_are_normalized_to_stable_codes() {
        assert!(normalized_provider_error(Some(401), "bad key").starts_with("media_provider_auth"));
        assert!(normalized_provider_error(Some(400), "bad input")
            .starts_with("media_provider_invalid_input"));
        assert!(normalized_provider_error(Some(429), "slow down")
            .starts_with("media_provider_rate_limit"));
        assert!(normalized_provider_error(Some(500), "upstream failed")
            .starts_with("media_provider_provider"));
        assert!(normalized_provider_error(Some(429), "quota exhausted")
            .starts_with("media_provider_quota"));
    }

    #[test]
    fn speech_posts_openai_compatible_payload_and_saves_audio() {
        let (base_url, captured) = fake_openai_speech();
        let _guard = crate::store::paths::test_env_lock();
        let home = temp_dir("home");
        let out_dir = temp_dir("out");
        std::env::set_var("HIMIND_AGENT_HOME", &home);
        let service_id = "voice";
        let mut media = std::collections::BTreeMap::new();
        media.insert(
            CAP_AUDIO_SPEECH.to_string(),
            ai_services::MediaModelBinding {
                adapter: ADAPTER_OPENAI_SPEECH.to_string(),
                model: "tts-test".to_string(),
                execution_mode: Some(MediaExecutionMode::Sync),
            },
        );
        ai_services::upsert(ai_services::CustomAIServiceInput {
            id: service_id.to_string(),
            display_name: "Voice".to_string(),
            base_url: base_url.clone(),
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "chat".to_string(),
            models: vec!["chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media,
            api_key: "sk-speech-test".to_string(),
        })
        .unwrap();

        let result = speech(&json!({
            "text": "你好，语音测试",
            "service": format!("custom:{service_id}"),
            "voice": "alloy",
            "output_dir": out_dir,
            "filename": "voice-test"
        }))
        .expect("speech should succeed");
        let path = result.get("path").and_then(Value::as_str).unwrap();
        assert!(Path::new(path).is_file());
        let request = captured.lock().unwrap().clone();
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-speech-test"));
        assert!(request.contains("\"model\":\"tts-test\""));
        assert!(request.contains("\"input\":\"你好，语音测试\""));
        assert!(!result.to_string().contains("sk-speech-test"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&out_dir);
    }
}
