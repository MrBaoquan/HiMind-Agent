//! 本机推理网关（ADR 0113）。
//!
//! 一个环回监听，服务多个绑定：客户端配置里只有 `http://127.0.0.1:<port>`
//! 与一枚本机令牌，真实凭据留在 Agent。网关按上游协议直通转发，并在响应里
//! 提取用量写入本机台账（`store::local_usage`）。
//!
//! P1 只做直通：入口协议与上游协议一致时原样转发。协议互译属 P2（见 0113）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::store::local_usage::{self, LocalUsageRecord};

const HEADER_LIMIT: usize = 64 * 1024;
const BODY_LIMIT: usize = 32 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const SSE_LINE_LIMIT: usize = 1024 * 1024;
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// 固定端口：客户端配置里写死的地址必须跨重启稳定，否则每次启动都要重写配置。
pub(crate) const DEFAULT_GATEWAY_PORT: u16 = 18_150;

/// 已启动网关的地址。客户端注入与状态查询都从这里取，避免各处各自猜端口。
/// 进程级单例：网关与 Agent 同生命周期。持有状态而不是裸实例，是因为
/// 「为什么没起来」和「端口是不是被占」都要如实报给界面。
static GATEWAY: OnceLock<Mutex<GatewayState>> = OnceLock::new();

struct GatewayState {
    gateway: Option<InferenceGateway>,
    /// 启动失败的硬原因（端口被占用且已有绑定、绑定端口不可用等）。
    last_error: String,
    /// 不足以失败、但用户该知道的事实（例如优先端口被占用后临时换端口）。
    notice: String,
}

fn gateway_state() -> &'static Mutex<GatewayState> {
    GATEWAY.get_or_init(|| {
        Mutex::new(GatewayState {
            gateway: None,
            last_error: String::new(),
            notice: String::new(),
        })
    })
}

pub(crate) fn url() -> Option<String> {
    gateway_state()
        .lock()
        .ok()
        .and_then(|state| state.gateway.as_ref().map(|gateway| gateway.url.clone()))
}

pub(crate) fn port() -> Option<u16> {
    gateway_state()
        .lock()
        .ok()
        .and_then(|state| state.gateway.as_ref().map(|gateway| gateway.port))
}

pub(crate) fn running() -> bool {
    gateway_state()
        .lock()
        .map(|state| state.gateway.is_some())
        .unwrap_or(false)
}

/// 状态快照：地址、端口、失败原因与提示。
pub(crate) fn status() -> (bool, String, u16, String, String) {
    let state = match gateway_state().lock() {
        Ok(state) => state,
        Err(_) => return (false, String::new(), 0, String::new(), String::new()),
    };
    match state.gateway.as_ref() {
        Some(gateway) => (
            true,
            gateway.url.clone(),
            gateway.port,
            String::new(),
            state.notice.clone(),
        ),
        None => (
            false,
            String::new(),
            0,
            state.last_error.clone(),
            state.notice.clone(),
        ),
    }
}

pub(crate) fn ensure_started(
    preferred_port: Option<u16>,
    resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync>,
) -> Result<(), String> {
    let mut state = gateway_state()
        .lock()
        .map_err(|_| "本机推理网关状态不可用".to_string())?;
    if state.gateway.is_some() {
        return Ok(());
    }
    start_locked(&mut state, preferred_port, resolver)
}

/// 重启：先停监听再起。用于端口被释放、异常退出后的恢复。
pub(crate) fn restart(
    preferred_port: Option<u16>,
    resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync>,
) -> Result<(), String> {
    let mut state = gateway_state()
        .lock()
        .map_err(|_| "本机推理网关状态不可用".to_string())?;
    if let Some(mut gateway) = state.gateway.take() {
        gateway.stop();
    }
    start_locked(&mut state, preferred_port, resolver)
}

/// 停止监听。返回此前是否在运行。绑定数据不动，由调用方决定要不要一并解除。
pub(crate) fn stop() -> bool {
    let Ok(mut state) = gateway_state().lock() else {
        return false;
    };
    match state.gateway.take() {
        Some(mut gateway) => {
            gateway.stop();
            true
        }
        None => false,
    }
}

fn start_locked(
    state: &mut GatewayState,
    preferred_port: Option<u16>,
    resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync>,
) -> Result<(), String> {
    let has_bindings = !resolver().is_empty();
    match InferenceGateway::start(preferred_port, resolver, has_bindings) {
        Ok(gateway) => {
            state.notice = gateway.notice.clone();
            state.last_error.clear();
            state.gateway = Some(gateway);
            Ok(())
        }
        Err(error) => {
            state.last_error = error.clone();
            state.notice.clear();
            Err(error)
        }
    }
}

/// 一个「服务 × 客户端」绑定。令牌只证明请求来自已绑定的客户端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayBinding {
    pub id: String,
    pub client: String,
    pub service: String,
    /// 该服务声明的模型目录，用于没有令牌时的模型名兜底归属。
    pub models: Vec<String>,
    /// 绑定的默认模型：翻译请求时作为模型名的兜底。
    pub default_model: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key: String,
    pub token: String,
    /// 上游是平台托管服务：以平台口径为准，本机合计里排除。
    pub platform_metered: bool,
}

type BindingResolver = Arc<dyn Fn() -> Vec<GatewayBinding> + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
enum GatewayError {
    UnknownToken,
    AmbiguousModel,
    ProtocolMismatch,
    UnsupportedPath,
    /// 响应已经写过（例如上游失败时已回 502），调用方不要再补一个响应。
    Answered,
}

impl GatewayError {
    fn code(&self) -> &'static str {
        match self {
            Self::UnknownToken => "himind_gateway_unknown_token",
            Self::AmbiguousModel => "himind_gateway_ambiguous_binding",
            Self::ProtocolMismatch => "himind_gateway_protocol_mismatch",
            Self::UnsupportedPath => "himind_gateway_unsupported_path",
            Self::Answered => "himind_gateway_answered",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::UnknownToken => "本机令牌无效，请在 AI 连接页重新注入",
            Self::AmbiguousModel => "请求没有携带可识别的本机令牌，且模型名对应多个绑定",
            Self::ProtocolMismatch => "请求协议与绑定的服务协议不一致，本机网关暂不互译",
            Self::UnsupportedPath => "本机网关只转发 /v1/chat/completions、/v1/responses、/v1/messages 与 /v1/models",
            Self::Answered => "",
        }
    }
}

pub(crate) struct InferenceGateway {
    url: String,
    port: u16,
    notice: String,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl InferenceGateway {
    /// 启动网关。
    ///
    /// 端口策略取决于是否已有客户端指向它：**已有网关模式绑定时不换端口**——
    /// 换端口意味着那些客户端配置里的地址立刻失效，而且界面上看不出来。
    /// 只有在还没有任何绑定时才允许退到临时端口，并把这件事记成提示。
    pub(crate) fn start(
        preferred_port: Option<u16>,
        resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync>,
        require_preferred_port: bool,
    ) -> Result<Self, String> {
        let mut notice = String::new();
        let listener = match preferred_port {
            Some(port) if port > 0 => match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => listener,
                Err(error) if require_preferred_port => {
                    return Err(format!(
                        "端口 {port} 被占用（{error}）；已有工具走网关，不换端口。"
                    ));
                }
                Err(_) => {
                    let listener = TcpListener::bind("127.0.0.1:0")
                        .map_err(|error| format!("无法启动本机推理网关：{error}"))?;
                    let actual = listener
                        .local_addr()
                        .map(|address| address.port())
                        .unwrap_or(0);
                    notice = format!(
                        "端口 {port} 被占用，已临时改用 {actual}。"
                    );
                    listener
                }
            },
            _ => TcpListener::bind("127.0.0.1:0")
                .map_err(|error| format!("无法启动本机推理网关：{error}"))?,
        };
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法配置本机推理网关：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("无法读取本机推理网关地址：{error}"))?
            .port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_active = Arc::clone(&active);
        let resolver: BindingResolver = Arc::from(resolver);
        let worker = thread::Builder::new()
            .name("himind-inference-gateway".to_string())
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if worker_active
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                                    (current < MAX_CONNECTIONS).then_some(current + 1)
                                })
                                .is_err()
                            {
                                let mut stream = stream;
                                let _ = write_json_error(&mut stream, 503, "himind_gateway_busy", "本机推理网关连接数已满");
                                continue;
                            }
                            let connection_active = Arc::clone(&worker_active);
                            let connection_resolver = Arc::clone(&resolver);
                            let spawn = thread::Builder::new()
                                .name("himind-inference-gateway-connection".to_string())
                                .spawn(move || {
                                    if let Err(error) = handle_connection(stream, &connection_resolver) {
                                        let kind = error.kind();
                                        if kind != std::io::ErrorKind::ConnectionReset
                                            && kind != std::io::ErrorKind::BrokenPipe
                                        {
                                            eprintln!("本机推理网关连接已关闭：{error}");
                                        }
                                    }
                                    connection_active.fetch_sub(1, Ordering::AcqRel);
                                });
                            if spawn.is_err() {
                                worker_active.fetch_sub(1, Ordering::AcqRel);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(error) => {
                            eprintln!("本机推理网关已停止：{error}");
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("无法创建本机推理网关线程：{error}"))?;
        let gateway = Self {
            url: format!("http://127.0.0.1:{port}"),
            port,
            notice,
            shutdown,
            worker: Some(worker),
        };
        Ok(gateway)
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for InferenceGateway {
    fn drop(&mut self) {
        self.stop();
    }
}

struct RequestHead {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn handle_connection(stream: TcpStream, resolver: &BindingResolver) -> std::io::Result<()> {
    let mut stream = stream;
    // Windows 上 accept 出来的套接字会继承 listener 的非阻塞标志；本模块的
    // 读写是阻塞语义，所以必须显式切回阻塞，否则会拿到 WSAEWOULDBLOCK。
    stream.set_nonblocking(false)?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    let Some((head, body)) = read_request(&mut stream)? else {
        return Ok(());
    };
    match route(&mut stream, resolver, &head, &body) {
        Ok(()) => Ok(()),
        Err(GatewayError::Answered) => Ok(()),
        Err(GatewayError::UnsupportedPath) => {
            write_json_error(&mut stream, 404, GatewayError::UnsupportedPath.code(), GatewayError::UnsupportedPath.message())
        }
        Err(GatewayError::UnknownToken) => write_json_error(&mut stream, 401, GatewayError::UnknownToken.code(), GatewayError::UnknownToken.message()),
        Err(error) => write_json_error(&mut stream, 400, error.code(), error.message()),
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<(RequestHead, Vec<u8>)>> {
    let mut buffer = Vec::<u8>::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            break position;
        }
        if buffer.len() > HEADER_LIMIT {
            return Ok(None);
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head_text = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let head = parse_head(&head_text).ok_or_else(|| std::io::Error::other("malformed request head"))?;
    // .NET 系客户端（HttpClient 默认）会先发 `Expect: 100-continue` 再等应答；
    // 不回 100 就会被判定为连接错误，请求根本到不了转发逻辑。
    if head
        .headers
        .get("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        stream.flush()?;
    }
    let content_length = head
        .headers
        .get("content-length")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > BODY_LIMIT {
        return Ok(None);
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(Some((head, body)))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_head(text: &str) -> Option<RequestHead> {
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let target = parts.next()?;
    let path = target.split('?').next().unwrap_or(target).to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Some(RequestHead { method, path, headers })
}

/// 入口路径 → 该路径要求的协议族。互译属 P2，因此这里只接受同族请求。
///
/// `/v1` 前缀可有可无：不同客户端的 baseURL 约定不同（Codex 带 `/v1`，
/// OpenCode 的 openai-compatible provider 直接拼 `/chat/completions`）。
fn normalize_api_path(path: &str) -> &str {
    path.strip_prefix("/v1").unwrap_or(path)
}

fn path_protocol(path: &str) -> Option<&'static str> {
    match normalize_api_path(path) {
        "/chat/completions" => Some("openai-chat"),
        "/responses" => Some("openai-responses"),
        "/messages" => Some("anthropic"),
        _ => None,
    }
}

fn token_from_headers(headers: &HashMap<String, String>) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let value = value.trim();
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    headers
        .get("x-api-key")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn model_from_body(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 令牌优先；没有令牌时按模型名兜底，只接受唯一匹配，歧义一律拒绝。
fn authorize(
    bindings: &[GatewayBinding],
    headers: &HashMap<String, String>,
    body: &[u8],
    required_protocol: &str,
) -> Result<GatewayBinding, GatewayError> {
    if let Some(token) = token_from_headers(headers) {
        return bindings
            .iter()
            .find(|binding| binding.token == token)
            .cloned()
            .ok_or(GatewayError::UnknownToken);
    }
    let model = model_from_body(body).ok_or(GatewayError::UnknownToken)?;
    let mut matches = bindings
        .iter()
        .filter(|binding| binding.protocol == required_protocol && binding_has_model(binding, &model));
    let first = matches.next().cloned().ok_or(GatewayError::UnknownToken)?;
    if matches.next().is_some() {
        return Err(GatewayError::AmbiguousModel);
    }
    Ok(first)
}

/// 绑定的模型目录来自服务定义；P1 用「模型名出现在服务模型列表」判定。
fn binding_has_model(binding: &GatewayBinding, model: &str) -> bool {
    binding
        .models
        .iter()
        .any(|candidate| candidate.trim() == model)
}

/// 上游地址拼接：base 通常已含 `/v1`，避免拼成 `/v1/v1/...`。
fn join_upstream(base_url: &str, path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let path = if path.starts_with("/v1/") && base.ends_with("/v1") {
        // 两边都带 `/v1`：去掉路径里的那份，避免拼成 `/v1/v1/...`。
        &path[3..]
    } else {
        path
    };
    if !path.starts_with("/v1/") && !base.ends_with("/v1") {
        // 两边都没有 `/v1`：上游是 OpenAI 兼容服务，补上标准前缀。
        return format!("{base}/v1{path}");
    }
    format!("{base}{path}")
}

/// OpenAI Chat 的流式响应默认不带用量，必须显式要求上游在末尾补一个 usage 块。
fn inject_stream_usage_flag(body: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let streaming = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !streaming {
        return body.to_vec();
    }
    let already_requested = value
        .get("stream_options")
        .and_then(|options| options.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if already_requested {
        return body.to_vec();
    }
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };
    let options = object
        .entry("stream_options")
        .or_insert_with(|| json!({}));
    if let Some(options) = options.as_object_mut() {
        options.insert("include_usage".to_string(), json!(true));
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    input: u64,
    output: u64,
    cached: u64,
    reasoning: u64,
}

impl Usage {
    fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cached == 0 && self.reasoning == 0
    }
}

fn number(value: &Value, path: &[&str]) -> u64 {
    let mut current = value;
    for key in path {
        match current.get(*key) {
            Some(next) => current = next,
            None => return 0,
        }
    }
    current.as_u64().unwrap_or(0)
}

/// 各协议的用量字段互不相同；缺失即视为「上游未报用量」。
fn extract_usage(protocol: &str, value: &Value) -> Usage {
    match protocol {
        "openai-chat" => Usage {
            input: number(value, &["usage", "prompt_tokens"]),
            output: number(value, &["usage", "completion_tokens"]),
            cached: number(value, &["usage", "prompt_tokens_details", "cached_tokens"]),
            reasoning: number(value, &["usage", "completion_tokens_details", "reasoning_tokens"]),
        },
        "openai-responses" => Usage {
            input: number(value, &["usage", "input_tokens"]),
            output: number(value, &["usage", "output_tokens"]),
            cached: number(value, &["usage", "input_tokens_details", "cached_tokens"]),
            reasoning: number(value, &["usage", "output_tokens_details", "reasoning_tokens"]),
        },
        "anthropic" => Usage {
            input: number(value, &["usage", "input_tokens"]),
            output: number(value, &["usage", "output_tokens"]),
            cached: number(value, &["usage", "cache_read_input_tokens"]),
            reasoning: 0,
        },
        _ => Usage::default(),
    }
}

/// SSE 里同一份用量可能分片出现（Anthropic 的 input 在 message_start、
/// output 在 message_delta），因此逐事件取最大值合并，而不是覆盖。
fn merge_usage(current: Usage, next: Usage) -> Usage {
    Usage {
        input: current.input.max(next.input),
        output: current.output.max(next.output),
        cached: current.cached.max(next.cached),
        reasoning: current.reasoning.max(next.reasoning),
    }
}

/// 从一行 SSE 数据里取用量。Responses 的用量挂在 `response.usage` 下。
fn usage_from_sse_line(protocol: &str, line: &str) -> Option<Usage> {
    let payload = line.trim().strip_prefix("data:")?.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let value: Value = serde_json::from_str(payload).ok()?;
    let usage = match protocol {
        // Responses 的用量挂在 `response.completed` 的 `response` 下。
        "openai-responses" => {
            let nested = value.get("response").unwrap_or(&value);
            extract_usage(protocol, nested)
        }
        // Anthropic 的 `message_start` 把用量放在 `message` 里，
        // `message_delta` 放在顶层——两处都要看。
        "anthropic" => {
            if value.get("usage").is_some() {
                extract_usage(protocol, &value)
            } else if let Some(message) = value.get("message") {
                extract_usage(protocol, message)
            } else {
                Usage::default()
            }
        }
        _ => extract_usage(protocol, &value),
    };
    (!usage.is_empty()).then_some(usage)
}

fn route(
    stream: &mut TcpStream,
    resolver: &BindingResolver,
    head: &RequestHead,
    body: &[u8],
) -> Result<(), GatewayError> {
    let bindings = resolver();
    // `/v1/models` 不携带模型名，必须有令牌才能定位绑定。
    if head.method == "GET" && normalize_api_path(&head.path) == "/models" {
        let binding = bindings
            .iter()
            .find(|binding| token_from_headers(&head.headers).as_deref() == Some(binding.token.as_str()))
            .cloned()
            .ok_or(GatewayError::UnknownToken)?;
        return forward(stream, &binding, head, body, "models");
    }
    let required_protocol = path_protocol(&head.path).ok_or(GatewayError::UnsupportedPath)?;
    let binding = authorize(&bindings, &head.headers, body, required_protocol)?;
    // 入口协议与上游协议不一致时走互译。已实现的两对：
    // Anthropic 入口 → Chat 上游、Responses 入口 → Chat 上游。
    let translatable = binding.protocol == "openai-chat"
        && matches!(required_protocol, "anthropic" | "openai-responses");
    if binding.protocol != required_protocol && !translatable {
        return Err(GatewayError::ProtocolMismatch);
    }
    forward(stream, &binding, head, body, required_protocol)
}

fn forward(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    head: &RequestHead,
    body: &[u8],
    kind: &str,
) -> Result<(), GatewayError> {
    // Claude Code 讲 Anthropic、多数上游讲 OpenAI：这一条走互译而不是直通。
    if kind == "anthropic" && binding.protocol == "openai-chat" {
        return forward_translated(stream, binding, body);
    }
    // Codex 只讲 Responses，同样落在 Chat 上游上。
    if kind == "openai-responses" && binding.protocol == "openai-chat" {
        return forward_responses_translated(stream, binding, body);
    }
    let outgoing_body = if kind == "openai-chat" {
        inject_stream_usage_flag(body)
    } else {
        body.to_vec()
    };
    let upstream_url = join_upstream(&binding.base_url, &head.path);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| GatewayError::Answered)?;
    let method = reqwest::Method::from_bytes(head.method.as_bytes())
        .unwrap_or(reqwest::Method::POST);
    let mut request = client
        .request(method, &upstream_url)
        .header("accept-encoding", "identity");
    for (name, value) in &head.headers {
        if is_forwarded_request_header(name) {
            request = request.header(name.as_str(), value.as_str());
        }
    }
    // Anthropic 只用 `x-api-key`；同时发 Authorization 会被部分上游判为无效凭据。
    request = if binding.protocol == "anthropic" {
        request
            .header("x-api-key", binding.api_key.as_str())
            .header("anthropic-version", ANTHROPIC_VERSION)
    } else {
        request.header("authorization", format!("Bearer {}", binding.api_key))
    };
    if kind != "models" {
        request = request.body(outgoing_body);
    }
    let mut response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            return Err(write_upstream_failure(stream, &error.to_string()));
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let streaming = content_type.contains("text/event-stream");
    let model = model_from_body(body).unwrap_or_default();

    // 流式：长度不可知，用 `Connection: close` 结束，客户端按 EOF 收尾。
    if streaming {
        write_stream_head(stream, status.as_u16(), &content_type, &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut scanner = StreamScanner::new(binding.protocol.clone());
        let mut chunk = [0_u8; 8192];
        loop {
            let read = match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            if stream.write_all(&chunk[..read]).is_err() {
                // 客户端断开：已看到的用量仍然记账，再结束这次转发。
                break;
            }
            scanner.push(&chunk[..read]);
        }
        let _ = stream.flush();
        let usage = scanner.usage;
        if kind != "models" {
            record_usage(binding, &model, true, status.as_u16(), usage, usage.is_empty());
        }
        return Ok(());
    }

    // 非流式：先收完再写，才能给出准确的 content-length。
    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    if raw.len() > BODY_LIMIT {
        raw.truncate(BODY_LIMIT);
    }
    write_buffered_head(stream, status.as_u16(), &content_type, &binding.id, raw.len())
        .map_err(|_| GatewayError::Answered)?;
    let _ = stream.write_all(&raw);
    let _ = stream.flush();
    if kind != "models" {
        let usage = serde_json::from_slice::<Value>(&raw)
            .map(|value| extract_usage(&binding.protocol, &value))
            .unwrap_or_default();
        record_usage(binding, &model, false, status.as_u16(), usage, usage.is_empty());
    }
    Ok(())
}

/// Anthropic 入口 → OpenAI 上游：请求翻译、响应翻译，用量照记。
fn forward_translated(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    body: &[u8],
) -> Result<(), GatewayError> {
    let request: Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            return Err(translate_error(stream, "Anthropic 请求体不是合法 JSON"));
        }
    };
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(binding.default_model.as_str())
        .to_string();
    let openai_body = match crate::app::anthropic_openai::request_to_openai(
        &request,
        &binding.default_model,
    ) {
        Ok(openai_body) => openai_body,
        Err(error) => return Err(translate_error(stream, &error)),
    };
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| GatewayError::Answered)?;
    let mut response = match client
        .post(join_upstream(&binding.base_url, "/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", binding.api_key))
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(openai_body.to_string())
        .send()
    {
        Ok(response) => response,
        Err(error) => return Err(write_upstream_failure(stream, &error.to_string())),
    };
    let status = response.status();
    if !status.is_success() {
        // 上游错误原样翻成 Anthropic 的错误体，客户端才能显示真实原因。
        let detail = response.text().unwrap_or_default();
        let payload = json!({
            "type": "error",
            "error": { "type": "api_error", "message": detail },
        })
        .to_string();
        let head = format!(
            "{}content-length: {}\r\n\r\n",
            gateway_response_head(status.as_u16(), "application/json", &binding.id),
            payload.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
        record_usage(binding, &model, streaming, status.as_u16(), Usage::default(), true);
        return Ok(());
    }

    if streaming {
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut translator = crate::app::anthropic_openai::AnthropicStream::new(model.clone());
        let mut buffer = String::new();
        let mut chunk = [0_u8; 8192];
        let mut client_gone = false;
        loop {
            let read = match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
            while let Some(position) = buffer.find('\n') {
                let line = buffer[..position].trim_end_matches('\r').to_string();
                buffer.drain(..=position);
                let events = translator.push_line(&line);
                if !events.is_empty() && stream.write_all(events.concat().as_bytes()).is_err() {
                    client_gone = true;
                    break;
                }
            }
            if client_gone {
                break;
            }
        }
        let tail = translator.finish().concat();
        let _ = stream.write_all(tail.as_bytes());
        let _ = stream.flush();
        let (input, output) = translator.usage();
        record_usage(
            binding,
            &model,
            true,
            status.as_u16(),
            Usage { input, output, cached: 0, reasoning: 0 },
            input == 0 && output == 0,
        );
        return Ok(());
    }

    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    let openai: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    let anthropic = match crate::app::anthropic_openai::response_to_anthropic(&openai, &model) {
        Ok(anthropic) => anthropic,
        Err(error) => return Err(translate_error(stream, &error)),
    };
    let usage = extract_usage("openai-chat", &openai);
    let payload = anthropic.to_string();
    let head = format!(
        "{}content-length: {}\r\n\r\n",
        gateway_response_head(status.as_u16(), "application/json", &binding.id),
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
    record_usage(binding, &model, false, status.as_u16(), usage, usage.is_empty());
    Ok(())
}

/// Responses 入口 → Chat 上游：请求翻译、响应翻译，用量照记。
///
/// Codex 0.150 起只讲 Responses（`wire_api = "chat"` 已被移除），所以这条路
/// 是它接任何 Chat 类上游的唯一入口。
fn forward_responses_translated(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    body: &[u8],
) -> Result<(), GatewayError> {
    let request: Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return Err(translate_error(stream, "Responses 请求体不是合法 JSON")),
    };
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(binding.default_model.as_str())
        .to_string();
    let chat_body = match crate::app::responses_chat::request_to_chat(&request, &binding.default_model)
    {
        Ok(chat_body) => chat_body,
        Err(error) => return Err(translate_error(stream, &error)),
    };
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| GatewayError::Answered)?;
    let mut response = match client
        .post(join_upstream(&binding.base_url, "/chat/completions"))
        .header("authorization", format!("Bearer {}", binding.api_key))
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(chat_body.to_string())
        .send()
    {
        Ok(response) => response,
        Err(error) => return Err(write_upstream_failure(stream, &error.to_string())),
    };
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        let payload = json!({ "error": { "type": "api_error", "message": detail } }).to_string();
        let head = format!(
            "{}content-length: {}\r\n\r\n",
            gateway_response_head(status.as_u16(), "application/json", &binding.id),
            payload.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
        record_usage(binding, &model, streaming, status.as_u16(), Usage::default(), true);
        return Ok(());
    }

    if streaming {
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut translator = crate::app::responses_chat::ResponsesStream::new(model.clone());
        let mut buffer = String::new();
        let mut chunk = [0_u8; 8192];
        let mut client_gone = false;
        loop {
            let read = match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
            while let Some(position) = buffer.find('\n') {
                let line = buffer[..position].trim_end_matches('\r').to_string();
                buffer.drain(..=position);
                let events = translator.push_line(&line);
                if !events.is_empty() && stream.write_all(events.concat().as_bytes()).is_err() {
                    client_gone = true;
                    break;
                }
            }
            if client_gone {
                break;
            }
        }
        let tail = translator.finish().concat();
        let _ = stream.write_all(tail.as_bytes());
        let _ = stream.flush();
        let (input, output) = translator.usage();
        record_usage(
            binding,
            &model,
            true,
            status.as_u16(),
            Usage {
                input,
                output,
                cached: translator.cached_tokens(),
                reasoning: translator.reasoning_tokens(),
            },
            input == 0 && output == 0,
        );
        return Ok(());
    }

    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    let chat: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    let payload = crate::app::responses_chat::response_to_responses(&chat, &model).to_string();
    let head = format!(
        "{}content-length: {}\r\n\r\n",
        gateway_response_head(status.as_u16(), "application/json", &binding.id),
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
    let usage = extract_usage("openai-chat", &chat);
    record_usage(binding, &model, false, status.as_u16(), usage, usage.is_empty());
    Ok(())
}

fn translate_error(stream: &mut TcpStream, detail: &str) -> GatewayError {
    let _ = write_json_error(stream, 400, "himind_gateway_translation_failed", detail);
    GatewayError::Answered
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

fn gateway_response_head(status: u16, content_type: &str, binding_id: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncache-control: no-store\r\nconnection: close\r\nx-himind-gateway: {binding_id}\r\n",
        reason = reason_phrase(status),
    )
}

fn write_stream_head(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    binding_id: &str,
) -> std::io::Result<()> {
    let head = format!("{}\r\n", gateway_response_head(status, content_type, binding_id));
    stream.write_all(head.as_bytes())
}

fn write_buffered_head(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    binding_id: &str,
    length: usize,
) -> std::io::Result<()> {
    let head = format!(
        "{}content-length: {length}\r\n\r\n",
        gateway_response_head(status, content_type, binding_id)
    );
    stream.write_all(head.as_bytes())
}

/// 只转发与推理相关的请求头：本机令牌、长度、连接管理都不透传。
fn is_forwarded_request_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "accept"
            | "anthropic-version"
            | "anthropic-beta"
            | "openai-beta"
            | "user-agent"
            | "x-stainless-arch"
    )
}

fn write_upstream_failure(stream: &mut TcpStream, detail: &str) -> GatewayError {
    let _ = write_json_error(
        stream,
        502,
        "himind_gateway_upstream_failed",
        &format!("上游请求失败：{detail}"),
    );
    GatewayError::Answered
}

/// SSE 行扫描：只在行边界解析，避免把半个 JSON 当成用量。
struct StreamScanner {
    protocol: String,
    pending: String,
    usage: Usage,
}

impl StreamScanner {
    fn new(protocol: String) -> Self {
        Self {
            protocol,
            pending: String::new(),
            usage: Usage::default(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.push_str(&String::from_utf8_lossy(bytes));
        while let Some(position) = self.pending.find('\n') {
            let line = self.pending[..position].trim_end_matches('\r').to_string();
            self.pending.drain(..=position);
            if let Some(usage) = usage_from_sse_line(&self.protocol, &line) {
                self.usage = merge_usage(self.usage, usage);
            }
        }
        if self.pending.len() > SSE_LINE_LIMIT {
            self.pending.clear();
        }
    }
}

fn record_usage(
    binding: &GatewayBinding,
    model: &str,
    streaming: bool,
    status: u16,
    usage: Usage,
    unreported: bool,
) {
    let record = LocalUsageRecord {
        occurred_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        binding_id: binding.id.clone(),
        client: binding.client.clone(),
        service: binding.service.clone(),
        model: model.to_string(),
        protocol: binding.protocol.clone(),
        stream: streaming,
        input_tokens: usage.input,
        output_tokens: usage.output,
        cached_tokens: usage.cached,
        reasoning_tokens: usage.reasoning,
        status: if status < 400 { "success" } else { "failure" }.to_string(),
        usage_unreported: unreported,
        platform_metered: binding.platform_metered,
    };
    if let Err(error) = local_usage::append(&record) {
        eprintln!("本机用量台账写入失败：{error}");
    }
}

fn write_json_error(
    stream: &mut TcpStream,
    status: u16,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
    let body = json!({ "error": { "code": code, "message": message } }).to_string();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json; charset=utf-8\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n{body}",
        reason = reason_phrase(status),
        length = body.len(),
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(client: &str, protocol: &str, token: &str) -> GatewayBinding {
        GatewayBinding {
            id: format!("{client}:svc"),
            client: client.to_string(),
            service: "svc".to_string(),
            models: vec!["deepseek-v4-flash".to_string()],
            default_model: "deepseek-v4-flash".to_string(),
            protocol: protocol.to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "real-secret".to_string(),
            token: token.to_string(),
            platform_metered: false,
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn head_parsing_keeps_method_path_and_lowercase_headers() {
        let head = parse_head("POST /v1/chat/completions?x=1 HTTP/1.1\r\nAuthorization: Bearer abc\r\nContent-Type: application/json")
            .unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/v1/chat/completions");
        assert_eq!(head.headers.get("authorization").unwrap(), "Bearer abc");
        assert_eq!(head.headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn token_is_read_from_either_authorization_or_api_key() {
        assert_eq!(
            token_from_headers(&headers(&[("authorization", "Bearer tok-1")])),
            Some("tok-1".to_string())
        );
        assert_eq!(
            token_from_headers(&headers(&[("x-api-key", "tok-2")])),
            Some("tok-2".to_string())
        );
        assert_eq!(token_from_headers(&headers(&[("authorization", "Basic zz")])), None);
    }

    #[test]
    fn authorize_prefers_token_and_rejects_unknown_tokens() {
        let bindings = vec![binding("codex", "openai-chat", "tok-1")];
        let ok = authorize(&bindings, &headers(&[("authorization", "Bearer tok-1")]), b"", "openai-chat");
        assert_eq!(ok.unwrap().client, "codex");
        let unknown = authorize(&bindings, &headers(&[("authorization", "Bearer nope")]), b"", "openai-chat");
        assert_eq!(unknown.unwrap_err(), GatewayError::UnknownToken);
    }

    #[test]
    fn authorize_falls_back_to_model_only_when_unambiguous() {
        let bindings = vec![binding("codex", "openai-chat", "tok-1")];
        let body = br#"{"model":"deepseek-v4-flash"}"#;
        assert!(authorize(&bindings, &HashMap::new(), body, "openai-chat").is_ok());

        let ambiguous = vec![
            binding("codex", "openai-chat", "tok-1"),
            binding("claude", "openai-chat", "tok-2"),
        ];
        assert_eq!(
            authorize(&ambiguous, &HashMap::new(), body, "openai-chat").unwrap_err(),
            GatewayError::AmbiguousModel
        );
    }

    #[test]
    fn upstream_url_does_not_duplicate_v1() {
        assert_eq!(
            join_upstream("https://api.example.com/v1", "/v1/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream("https://api.example.com", "/v1/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream("https://api.example.com/v1/", "/v1/models"),
            "https://api.example.com/v1/models"
        );
        // 客户端不带 `/v1`（OpenCode 的 openai-compatible provider 就是这样）。
        assert_eq!(
            join_upstream("https://api.example.com/v1", "/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream("https://api.example.com", "/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn api_path_accepts_both_v1_and_bare_forms() {
        assert_eq!(path_protocol("/v1/chat/completions"), Some("openai-chat"));
        assert_eq!(path_protocol("/chat/completions"), Some("openai-chat"));
        assert_eq!(path_protocol("/v1/responses"), Some("openai-responses"));
        assert_eq!(path_protocol("/responses"), Some("openai-responses"));
        assert_eq!(path_protocol("/v1/messages"), Some("anthropic"));
        assert_eq!(path_protocol("/messages"), Some("anthropic"));
        assert_eq!(path_protocol("/v1/embeddings"), None);
    }

    #[test]
    fn stream_usage_flag_is_injected_only_when_missing() {
        let injected = inject_stream_usage_flag(br#"{"model":"m","stream":true}"#);
        let value: Value = serde_json::from_slice(&injected).unwrap();
        assert_eq!(value["stream_options"]["include_usage"], true);

        let untouched = inject_stream_usage_flag(br#"{"model":"m","stream":true,"stream_options":{"include_usage":true}}"#);
        let value: Value = serde_json::from_slice(&untouched).unwrap();
        assert_eq!(value["stream_options"]["include_usage"], true);

        let non_stream = inject_stream_usage_flag(br#"{"model":"m"}"#);
        let value: Value = serde_json::from_slice(&non_stream).unwrap();
        assert!(value.get("stream_options").is_none());
    }

    #[test]
    fn usage_extraction_matches_each_protocol() {
        let chat: Value = serde_json::from_str(
            r#"{"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":64},"completion_tokens_details":{"reasoning_tokens":7}}}"#,
        )
        .unwrap();
        assert_eq!(
            extract_usage("openai-chat", &chat),
            Usage { input: 100, output: 20, cached: 64, reasoning: 7 }
        );

        let responses: Value = serde_json::from_str(
            r#"{"usage":{"input_tokens":11,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}"#,
        )
        .unwrap();
        assert_eq!(
            extract_usage("openai-responses", &responses),
            Usage { input: 11, output: 2, cached: 3, reasoning: 0 }
        );

        let anthropic: Value = serde_json::from_str(
            r#"{"usage":{"input_tokens":5,"output_tokens":6,"cache_read_input_tokens":7,"cache_creation_input_tokens":8}}"#,
        )
        .unwrap();
        assert_eq!(
            extract_usage("anthropic", &anthropic),
            Usage { input: 5, output: 6, cached: 7, reasoning: 0 }
        );
    }

    #[test]
    fn sse_scanner_merges_split_anthropic_usage() {
        let mut scanner = StreamScanner::new("anthropic".to_string());
        scanner.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":30}}}\n\n");
        scanner.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\n");
        assert_eq!(
            scanner.usage,
            Usage { input: 12, output: 9, cached: 30, reasoning: 0 }
        );
    }

    #[test]
    fn sse_scanner_reads_responses_completed_usage() {
        let mut scanner = StreamScanner::new("openai-responses".to_string());
        scanner.push(b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":4}}}\n\n");
        assert_eq!(scanner.usage, Usage { input: 8, output: 4, cached: 0, reasoning: 0 });
    }

    #[test]
    fn sse_scanner_ignores_done_and_partial_lines() {
        let mut scanner = StreamScanner::new("openai-chat".to_string());
        scanner.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n");
        scanner.push(b"data: {\"usa");
        assert!(scanner.usage.is_empty());
        scanner.push(b"ge\":{\"prompt_tokens\":4,\"completion_tokens\":1}}\n\n");
        assert_eq!(scanner.usage, Usage { input: 4, output: 1, cached: 0, reasoning: 0 });
    }

    /// 已有客户端走网关时，优先端口被占用必须失败：换端口会让那些客户端
    /// 配置里的地址立刻失效，而且界面上看不出来。
    #[test]
    fn occupied_preferred_port_fails_when_clients_are_bound() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let result = InferenceGateway::start(Some(port), Box::new(Vec::new), true);
        let error = result.err().expect("端口被占用时不应静默换端口");
        assert!(error.contains("不换端口"), "错误应说明为什么不换端口：{error}");
    }

    /// 还没有任何绑定时允许退到临时端口，但必须把这件事记成提示。
    #[test]
    fn occupied_preferred_port_falls_back_only_without_bindings() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let gateway = InferenceGateway::start(Some(port), Box::new(Vec::new), false).unwrap();
        assert_ne!(gateway.port(), port);
        assert!(!gateway.notice.is_empty(), "换端口必须留下提示");
    }
}
