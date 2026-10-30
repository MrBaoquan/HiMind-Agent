//! 本机推理网关（ADR 0113）。
//!
//! 一个环回监听，服务多个绑定：客户端配置里只有 `http://localhost:<port>`
//! 与一枚本机令牌，真实凭据留在 Agent。网关按上游协议直通转发，并在响应里
//! 提取用量写入本机台账（`store::local_usage`）。
//!
//! P1 只做直通：入口协议与上游协议一致时原样转发。协议互译属 P2（见 0113）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::store::local_usage::{self, LocalUsageRecord};

const HEADER_LIMIT: usize = 64 * 1024;
/// 本机是受信任的规格化边界：入口允许容纳客户端已经生成的大历史，
/// 但出站请求会在媒体外置后严格压到远端网关限制以下。
///
/// 注意：这条上限必须比“客户端已积累的历史”更大，否则网关会在收到请求前
/// 就拒绝，根本来不及把图片外置。默认 192 MiB，可用环境变量覆盖。
const DEFAULT_INBOUND_BODY_MIB: usize = 192;
const MIN_INBOUND_BODY_MIB: usize = 32;
const MAX_INBOUND_BODY_MIB: usize = 512;
const INBOUND_BODY_MIB_ENV: &str = "HIMIND_AGENT_GATEWAY_INBOUND_MIB";
const OUTBOUND_BODY_LIMIT: usize = 24 * 1024 * 1024;
const RESPONSE_BODY_LIMIT: usize = 32 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;

fn inbound_body_limit_mib() -> usize {
    std::env::var(INBOUND_BODY_MIB_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_INBOUND_BODY_MIB)
        .clamp(MIN_INBOUND_BODY_MIB, MAX_INBOUND_BODY_MIB)
}

fn inbound_body_limit_bytes() -> usize {
    inbound_body_limit_mib() * 1024 * 1024
}
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const SSE_LINE_LIMIT: usize = 1024 * 1024;
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// 默认端口：客户端配置里写死的地址必须跨重启稳定，否则每次启动都要重写配置。
/// 它只是默认值——实际端口由 [`configured_port`] 解析，可被持久化设置或环境变量覆盖。
pub(crate) const DEFAULT_GATEWAY_PORT: u16 = 18_150;

/// 覆盖端口的操作员环境变量：同一台机器并行跑多个 Agent（例如 development profile）
/// 时，用它给后启动的实例换一个环回端口，不必停掉先启动的实例。
pub(crate) const GATEWAY_PORT_ENV: &str = "HIMIND_AGENT_GATEWAY_PORT";

/// 网关端口的持久化设置文件（与注入簿记同目录，随 profile 隔离）。
const GATEWAY_CONFIG_FILE: &str = "gateway.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct GatewayConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

fn gateway_config_path(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(GATEWAY_CONFIG_FILE)
}

/// 端口解析优先级：环境变量 > 持久化设置 > 默认 18150。`0` 与解析失败都视为未设置。
///
/// 抽成纯函数，便于单测不触碰环境变量与磁盘的分支。
fn resolve_gateway_port(env_override: Option<u16>, stored: Option<u16>) -> u16 {
    // 注意：不能写成 `env_override.or(stored).filter(>0)`——`Some(0).or(...)`
    // 会在 `Some` 上短路，把 `stored` 整个丢掉。两路都要各自先过滤。
    env_override
        .filter(|port| *port > 0)
        .or(stored.filter(|port| *port > 0))
        .unwrap_or(DEFAULT_GATEWAY_PORT)
}

/// 当前生效的网关端口。
pub(crate) fn configured_port(state_path: &Path) -> u16 {
    let env_override = std::env::var(GATEWAY_PORT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok());
    let stored = std::fs::read(gateway_config_path(state_path))
        .ok()
        .and_then(|content| serde_json::from_slice::<GatewayConfig>(&content).ok())
        .and_then(|config| config.port);
    resolve_gateway_port(env_override, stored)
}

/// 持久化网关端口。`0` 不合法。
///
/// 调用方必须保证改端口不会留下指向死端口的客户端配置——见
/// `commands::set_inference_gateway_port` 的前置检查。
pub(crate) fn set_configured_port(state_path: &Path, port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("端口必须在 1-65535 之间。".to_string());
    }
    let path = gateway_config_path(state_path);
    let _lock = crate::store::atomic_file::lock(&path).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&GatewayConfig { port: Some(port) })
        .map_err(|error| error.to_string())?;
    crate::store::atomic_file::atomic_write(&path, &bytes).map_err(|error| error.to_string())
}

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
    /// 聚合入口 ID；旧单服务绑定为空。
    pub profile_id: String,
    /// 客户端看到的入口协议；状态面板与兼容检查使用。
    pub entry_protocol: String,
    /// 客户端看到的稳定模型别名。
    pub exposed_model: String,
    /// 该服务声明的模型目录，用于没有令牌时的模型名兜底归属。
    pub models: Vec<String>,
    /// 绑定的默认模型：翻译请求时作为模型名的兜底。
    pub default_model: String,
    /// 实际发送给上游的模型 ID。
    pub upstream_model: String,
    /// 该路由是否为 Profile 的默认模型。
    pub is_default: bool,
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
    UnknownModel,
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
            Self::UnknownModel => "himind_gateway_unknown_model",
            Self::ProtocolMismatch => "himind_gateway_protocol_mismatch",
            Self::UnsupportedPath => "himind_gateway_unsupported_path",
            Self::Answered => "himind_gateway_answered",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::UnknownToken => "本机令牌无效，请在 AI 连接页重新注入",
            Self::AmbiguousModel => "请求没有携带可识别的本机令牌，且模型名对应多个绑定",
            Self::UnknownModel => "请求的模型不在当前聚合入口中",
            Self::ProtocolMismatch => "请求协议与绑定的服务协议不一致，本机网关暂不互译",
            Self::UnsupportedPath => {
                "本机网关只转发 /v1/chat/completions、/v1/responses、/v1/messages 与 /v1/models"
            }
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
                    notice = format!("端口 {port} 被占用，已临时改用 {actual}。");
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
                                let _ = write_json_error(
                                    &mut stream,
                                    503,
                                    "himind_gateway_busy",
                                    "本机推理网关连接数已满",
                                );
                                continue;
                            }
                            let connection_active = Arc::clone(&worker_active);
                            let connection_resolver = Arc::clone(&resolver);
                            let spawn = thread::Builder::new()
                                .name("himind-inference-gateway-connection".to_string())
                                .spawn(move || {
                                    if let Err(error) =
                                        handle_connection(stream, &connection_resolver)
                                    {
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
            // 客户端侧统一使用 localhost：Windows 系统代理通常会绕过本地主机名，
            // 但对裸 127.0.0.1 不保证绕过，可能把本机请求送进代理后得到 502。
            url: format!("http://localhost:{port}"),
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

enum RequestRead {
    Request(RequestHead, Vec<u8>),
    Empty,
    TooLarge { actual: usize, limit: usize },
}

fn handle_connection(stream: TcpStream, resolver: &BindingResolver) -> std::io::Result<()> {
    let mut stream = stream;
    // Windows 上 accept 出来的套接字会继承 listener 的非阻塞标志；本模块的
    // 读写是阻塞语义，所以必须显式切回阻塞，否则会拿到 WSAEWOULDBLOCK。
    stream.set_nonblocking(false)?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    let (head, body) = match read_request(&mut stream)? {
        RequestRead::Empty => return Ok(()),
        RequestRead::TooLarge { actual, limit } => {
            let message = format!(
                "请求体约 {:.1} MiB，超过本机网关 {} MiB 入站上限。请新建会话或减少图片/附件后重试。",
                actual as f64 / (1024.0 * 1024.0),
                limit / (1024 * 1024),
            );
            return write_json_error(
                &mut stream,
                413,
                "himind_gateway_request_too_large",
                &message,
            );
        }
        RequestRead::Request(head, body) => (head, body),
    };
    match route(&mut stream, resolver, &head, &body) {
        Ok(()) => Ok(()),
        Err(GatewayError::Answered) => Ok(()),
        Err(GatewayError::UnsupportedPath) => write_json_error(
            &mut stream,
            404,
            GatewayError::UnsupportedPath.code(),
            GatewayError::UnsupportedPath.message(),
        ),
        Err(GatewayError::UnknownToken) => write_json_error(
            &mut stream,
            401,
            GatewayError::UnknownToken.code(),
            GatewayError::UnknownToken.message(),
        ),
        Err(error) => write_json_error(&mut stream, 400, error.code(), error.message()),
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<RequestRead> {
    let mut buffer = Vec::<u8>::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            break position;
        }
        if buffer.len() > HEADER_LIMIT {
            return Ok(RequestRead::Empty);
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(RequestRead::Empty);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head_text = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let head =
        parse_head(&head_text).ok_or_else(|| std::io::Error::other("malformed request head"))?;
    let content_length = head
        .headers
        .get("content-length")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // 超限请求在发送 100 Continue 之前直接拒绝，避免客户端继续上传几十 MiB。
    let inbound_limit = inbound_body_limit_bytes();
    if content_length > inbound_limit {
        return Ok(RequestRead::TooLarge {
            actual: content_length,
            limit: inbound_limit,
        });
    }
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
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(RequestRead::Request(head, body))
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
    Some(RequestHead {
        method,
        path,
        headers,
    })
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

/// 令牌优先定位客户端/Profile，再按请求模型选择具体路由。
///
/// 同一 Profile 的多个模型共享同一个令牌，因此不能再像单服务绑定那样
/// “令牌命中第一个 binding 就返回”。没有令牌时只保留旧的模型名兜底，
/// 且仍然要求全局唯一。
fn authorize(
    bindings: &[GatewayBinding],
    headers: &HashMap<String, String>,
    body: &[u8],
    _required_protocol: &str,
) -> Result<GatewayBinding, GatewayError> {
    let model = model_from_body(body);
    let token = token_from_headers(headers);
    let scoped = if let Some(token) = token.as_deref() {
        let matched = bindings
            .iter()
            .filter(|binding| binding.token == token)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            return Err(GatewayError::UnknownToken);
        }
        matched
    } else {
        let model = model.as_deref().ok_or(GatewayError::UnknownToken)?;
        let matched = bindings
            .iter()
            .filter(|binding| binding_has_model(binding, model))
            .collect::<Vec<_>>();
        if matched.is_empty() {
            return Err(GatewayError::UnknownToken);
        }
        matched
    };

    if let Some(model) = model.as_deref() {
        let exact = scoped
            .iter()
            .filter(|binding| binding_exposed_model(binding) == model)
            .collect::<Vec<_>>();
        if token.is_some() {
            return exact
                .first()
                .map(|binding| (**binding).clone())
                .ok_or(GatewayError::UnknownModel);
        }
        if exact.len() == 1 {
            return Ok((*exact[0]).clone());
        }
        if exact.len() > 1 {
            return Err(GatewayError::AmbiguousModel);
        }
    }
    if scoped.len() == 1 {
        return Ok(scoped[0].clone());
    }
    if let Some(default) = scoped.iter().find(|binding| binding.is_default) {
        return Ok((*default).clone());
    }
    Err(GatewayError::AmbiguousModel)
}

fn binding_exposed_model(binding: &GatewayBinding) -> &str {
    if binding.exposed_model.is_empty() {
        binding
            .models
            .first()
            .map(String::as_str)
            .unwrap_or(binding.default_model.as_str())
    } else {
        binding.exposed_model.as_str()
    }
}

/// 绑定的模型目录来自服务定义；旧绑定用模型列表判定，新绑定使用 exposed_model。
fn binding_has_model(binding: &GatewayBinding, model: &str) -> bool {
    binding_exposed_model(binding) == model
        || binding
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

fn summarize_responses_request(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return "invalid-json".to_string();
    };
    let model = value.get("model").and_then(Value::as_str).unwrap_or("");
    let stream = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let input_count = value
        .get("input")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    let tool_types = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let previous = value.get("previous_response_id").is_some();
    let store = value.get("store").and_then(Value::as_bool);
    format!(
        "model={model} stream={stream} input_items={input_count} tools={tool_types} previous_response_id={previous} store={store:?} bytes={}",
        body.len()
    )
}
fn upstream_client(base_url: &str) -> Result<reqwest::blocking::Client, reqwest::Error> {
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    let loopback = url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"));
    if loopback {
        builder = builder.no_proxy();
    }
    builder.build()
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
    let options = object.entry("stream_options").or_insert_with(|| json!({}));
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
            reasoning: number(
                value,
                &["usage", "completion_tokens_details", "reasoning_tokens"],
            ),
        },
        "openai-responses" => Usage {
            input: number(value, &["usage", "input_tokens"]),
            output: number(value, &["usage", "output_tokens"]),
            cached: number(value, &["usage", "input_tokens_details", "cached_tokens"]),
            reasoning: number(
                value,
                &["usage", "output_tokens_details", "reasoning_tokens"],
            ),
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
    let started = Instant::now();
    let bindings = resolver();
    // 聚合模型目录由本机生成，不依赖任一上游 `/v1/models` 是否可用。
    if head.method == "GET" && normalize_api_path(&head.path) == "/models" {
        let token = token_from_headers(&head.headers).ok_or(GatewayError::UnknownToken)?;
        let scoped = bindings
            .iter()
            .filter(|binding| binding.token == token)
            .collect::<Vec<_>>();
        if scoped.is_empty() {
            return Err(GatewayError::UnknownToken);
        }
        return write_models_response(stream, &scoped);
    }
    let required_protocol = path_protocol(&head.path).ok_or(GatewayError::UnsupportedPath)?;
    let binding = authorize(&bindings, &head.headers, body, required_protocol)?;
    // 入口协议与上游协议不一致时走互译：
    // Anthropic → Chat/Responses、Responses → Chat。
    let translatable = matches!(
        (required_protocol, binding.protocol.as_str()),
        ("anthropic", "openai-chat" | "openai-responses") | ("openai-responses", "openai-chat")
    );
    if binding.protocol != required_protocol && !translatable {
        return Err(GatewayError::ProtocolMismatch);
    }
    forward(stream, &binding, head, body, required_protocol, started)
}

fn models_payload(bindings: &[&GatewayBinding]) -> String {
    let mut rows = bindings
        .iter()
        .map(|binding| {
            let id = binding_exposed_model(binding).to_string();
            let owner = if binding.profile_id.is_empty() {
                binding.service.clone()
            } else {
                binding.profile_id.clone()
            };
            (id, owner)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows.dedup_by(|left, right| left.0 == right.0);
    let data = rows
        .into_iter()
        .map(|(id, owned_by)| {
            json!({
                "id": id,
                "object": "model",
                "owned_by": owned_by,
            })
        })
        .collect::<Vec<_>>();
    json!({ "object": "list", "data": data }).to_string()
}

fn write_models_response(
    stream: &mut TcpStream,
    bindings: &[&GatewayBinding],
) -> Result<(), GatewayError> {
    let payload = models_payload(bindings);
    let binding_id = bindings
        .first()
        .map(|binding| binding.id.as_str())
        .unwrap_or("models");
    let head = format!(
        "{}content-length: {}\r\n\r\n",
        gateway_response_head(200, "application/json", binding_id),
        payload.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(payload.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|_| GatewayError::Answered)
}
fn rewrite_request_model(body: &[u8], upstream_model: &str) -> Vec<u8> {
    if body.is_empty() || upstream_model.trim().is_empty() {
        return body.to_vec();
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };
    if !object.contains_key("model") {
        return body.to_vec();
    }
    object.insert("model".to_string(), json!(upstream_model));
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

fn rewrite_json_model(raw: &[u8], exposed_model: &str) -> Vec<u8> {
    if raw.is_empty() || exposed_model.trim().is_empty() {
        return raw.to_vec();
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(raw) else {
        return raw.to_vec();
    };
    if value.get("model").is_none() {
        return raw.to_vec();
    }
    value["model"] = json!(exposed_model);
    serde_json::to_vec(&value).unwrap_or_else(|_| raw.to_vec())
}

fn rewrite_sse_model_line(line: &str, exposed_model: &str) -> String {
    let trimmed = line.trim_end_matches('\r');
    let Some(payload) = trimmed.strip_prefix("data:") else {
        return line.to_string();
    };
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return line.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
        return line.to_string();
    };
    if value.get("model").is_none() {
        return line.to_string();
    }
    value["model"] = json!(exposed_model);
    format!("data: {value}")
}
fn forward(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    head: &RequestHead,
    body: &[u8],
    kind: &str,
    started: Instant,
) -> Result<(), GatewayError> {
    // 客户端请求使用 exposed_model，上游只能看到真实 upstream_model。
    let rewritten_body = rewrite_request_model(body, &binding.upstream_model);
    let body = rewritten_body.as_slice();
    let display_model = binding_exposed_model(binding).to_string();
    // Claude Code / Claude Desktop 讲 Anthropic：按上游协议选择互译实现。
    if kind == "anthropic" {
        if binding.protocol == "openai-chat" {
            return forward_translated(stream, binding, body, started);
        }
        if binding.protocol == "openai-responses" {
            return forward_anthropic_responses_translated(stream, binding, body, started);
        }
    }
    let request_summary = if kind == "openai-responses" {
        summarize_responses_request(body)
    } else {
        String::new()
    };
    let normalized_responses = if kind == "openai-responses" {
        let request: Value = serde_json::from_slice(body)
            .map_err(|_| translate_error(stream, "Responses 请求体不是合法 JSON"))?;
        let normalized = crate::app::responses_media::normalize_request(request)
            .map_err(|error| media_normalize_error(stream, &error))?;
        if normalized.stats.images_total > 0 {
            let message = format!(
                "本机网关媒体规格化：images={} offloaded={} referenced={} inline={}->{} bytes request={}->{} bytes",
                normalized.stats.images_total,
                normalized.stats.images_offloaded,
                normalized.stats.images_replaced_with_reference,
                normalized.stats.inline_bytes_before,
                normalized.stats.inline_bytes_after,
                normalized.stats.request_bytes_before,
                normalized.stats.request_bytes_after,
            );
            crate::approval::manager::append_event_log("info", &message);
        }
        for warning in normalized.warnings {
            crate::approval::manager::append_event_log(
                "warn",
                &format!("本机网关媒体规格化警告：{warning}"),
            );
        }
        Some(normalized.request)
    } else {
        None
    };
    // Codex 只讲 Responses，同样落在 Chat 上游上。
    if kind == "openai-responses" && binding.protocol == "openai-chat" {
        return forward_responses_translated(
            stream,
            binding,
            normalized_responses
                .as_ref()
                .expect("Responses request was normalized above"),
            started,
        );
    }
    let outgoing_body = match normalized_responses {
        Some(request) => serde_json::to_vec(&request)
            .map_err(|error| media_normalize_error(stream, &error.to_string()))?,
        None if kind == "openai-chat" => inject_stream_usage_flag(body),
        None => body.to_vec(),
    };
    if kind != "models" {
        enforce_outbound_body_limit(stream, outgoing_body.len())?;
    }
    let upstream_url = join_upstream(&binding.base_url, &head.path);
    let client = upstream_client(&upstream_url).map_err(|_| GatewayError::Answered)?;
    let method =
        reqwest::Method::from_bytes(head.method.as_bytes()).unwrap_or(reqwest::Method::POST);
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

    // 上游失败时先完整收下错误体：既便于本机诊断，也避免把空 502
    // 当成正常的流式响应交给客户端，导致真正原因被吞掉。
    if !status.is_success() {
        let mut raw = Vec::new();
        let _ = response.read_to_end(&mut raw);
        let excerpt = String::from_utf8_lossy(&raw[..raw.len().min(1200)]).replace('\n', " ");
        let header_names = head.headers.keys().cloned().collect::<Vec<_>>().join(",");
        crate::approval::manager::append_event_log(
            "warn",
            &format!(
                "本机网关上游返回失败：status={} path={} bytes={} headers={} request={} body={excerpt}",
                status.as_u16(),
                head.path,
                raw.len(),
                header_names,
                request_summary,
            ),
        );
        write_buffered_head(
            stream,
            status.as_u16(),
            &content_type,
            &binding.id,
            raw.len(),
        )
        .map_err(|_| GatewayError::Answered)?;
        let _ = stream.write_all(&raw);
        let _ = stream.flush();
        if kind != "models" {
            record_usage(
                binding,
                &display_model,
                streaming,
                status.as_u16(),
                Usage::default(),
                true,
                started.elapsed().as_millis() as u64,
            );
        }
        return Ok(());
    }

    // 流式：长度不可知，用 `Connection: close` 结束，客户端按 EOF 收尾。
    if streaming {
        write_stream_head(stream, status.as_u16(), &content_type, &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut scanner = StreamScanner::new(binding.protocol.clone());
        let mut pending = String::new();
        let mut chunk = [0_u8; 8192];
        let mut client_gone = false;
        loop {
            let read = match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            scanner.push(&chunk[..read]);
            pending.push_str(&String::from_utf8_lossy(&chunk[..read]));
            while let Some(position) = pending.find('\n') {
                let line = pending[..position].trim_end_matches('\r').to_string();
                pending.drain(..=position);
                let rewritten = rewrite_sse_model_line(&line, &display_model);
                if stream.write_all(rewritten.as_bytes()).is_err()
                    || stream.write_all(b"\n").is_err()
                {
                    client_gone = true;
                    break;
                }
            }
            if client_gone {
                // 客户端断开：已看到的用量仍然记账，再结束这次转发。
                break;
            }
        }
        if !client_gone && !pending.is_empty() {
            let rewritten = rewrite_sse_model_line(&pending, &display_model);
            let _ = stream.write_all(rewritten.as_bytes());
        }
        let _ = stream.flush();
        let usage = scanner.usage;
        if kind != "models" {
            record_usage(
                binding,
                &display_model,
                true,
                status.as_u16(),
                usage,
                usage.is_empty(),
                started.elapsed().as_millis() as u64,
            );
        }
        return Ok(());
    }

    // 非流式：先收完再写，才能给出准确的 content-length。
    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    if raw.len() > RESPONSE_BODY_LIMIT {
        raw.truncate(RESPONSE_BODY_LIMIT);
    }
    let output_raw = rewrite_json_model(&raw, &display_model);
    write_buffered_head(
        stream,
        status.as_u16(),
        &content_type,
        &binding.id,
        output_raw.len(),
    )
    .map_err(|_| GatewayError::Answered)?;
    let _ = stream.write_all(&output_raw);
    let _ = stream.flush();
    if kind != "models" {
        let usage = serde_json::from_slice::<Value>(&raw)
            .map(|value| extract_usage(&binding.protocol, &value))
            .unwrap_or_default();
        record_usage(
            binding,
            &display_model,
            false,
            status.as_u16(),
            usage,
            usage.is_empty(),
            started.elapsed().as_millis() as u64,
        );
    }
    Ok(())
}

/// Anthropic 入口 → OpenAI 上游：请求翻译、响应翻译，用量照记。
fn forward_translated(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    body: &[u8],
    started: Instant,
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
    let display_model = binding_exposed_model(binding).to_string();
    let openai_body =
        match crate::app::anthropic_openai::request_to_openai(&request, &binding.default_model) {
            Ok(openai_body) => openai_body,
            Err(error) => return Err(translate_error(stream, &error)),
        };
    let openai_payload = openai_body.to_string();
    enforce_outbound_body_limit(stream, openai_payload.len())?;
    let client = upstream_client(&binding.base_url).map_err(|_| GatewayError::Answered)?;
    let mut response = match client
        .post(join_upstream(&binding.base_url, "/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", binding.api_key))
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(openai_payload)
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
        record_usage(
            binding,
            &display_model,
            streaming,
            status.as_u16(),
            Usage::default(),
            true,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    if streaming {
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut translator =
            crate::app::anthropic_openai::AnthropicStream::new(display_model.clone());
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
            &display_model,
            true,
            status.as_u16(),
            Usage {
                input,
                output,
                cached: 0,
                reasoning: 0,
            },
            input == 0 && output == 0,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    let openai: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    let anthropic =
        match crate::app::anthropic_openai::response_to_anthropic(&openai, &display_model) {
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
    record_usage(
        binding,
        &display_model,
        false,
        status.as_u16(),
        usage,
        usage.is_empty(),
        started.elapsed().as_millis() as u64,
    );
    Ok(())
}

/// Anthropic 入口 → Responses 上游：参考 cc-switch 的直接转换边界，
/// 把命名事件 SSE 转换为 Anthropic message/content_block 生命周期。
fn forward_anthropic_responses_translated(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    body: &[u8],
    started: Instant,
) -> Result<(), GatewayError> {
    let request: Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return Err(translate_error(stream, "Anthropic 请求体不是合法 JSON")),
    };
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let display_model = binding_exposed_model(binding).to_string();
    let responses_body = match crate::app::anthropic_responses::request_to_responses(
        &request,
        &binding.default_model,
    ) {
        Ok(body) => body,
        Err(error) => return Err(translate_error(stream, &error)),
    };
    let payload = responses_body.to_string();
    enforce_outbound_body_limit(stream, payload.len())?;
    let client = upstream_client(&binding.base_url).map_err(|_| GatewayError::Answered)?;
    let mut response = match client
        .post(join_upstream(&binding.base_url, "/responses"))
        .header("authorization", format!("Bearer {}", binding.api_key))
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(payload)
        .send()
    {
        Ok(response) => response,
        Err(error) => return Err(write_upstream_failure(stream, &error.to_string())),
    };
    let status = response.status();
    if !status.is_success() {
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
        record_usage(
            binding,
            &display_model,
            streaming,
            status.as_u16(),
            Usage::default(),
            true,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if streaming && content_type.contains("text/event-stream") {
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut translator =
            crate::app::anthropic_responses::AnthropicResponsesStream::new(display_model.clone());
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
            &display_model,
            true,
            status.as_u16(),
            Usage {
                input,
                output,
                cached: 0,
                reasoning: 0,
            },
            input == 0 && output == 0,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    let upstream: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    let usage = extract_usage("openai-responses", &upstream);
    if streaming {
        let payload = match crate::app::anthropic_responses::response_to_anthropic_sse(
            &upstream,
            &display_model,
        ) {
            Ok(payload) => payload,
            Err(error) => return Err(translate_error(stream, &error)),
        };
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
    } else {
        let payload =
            match crate::app::anthropic_responses::response_to_anthropic(&upstream, &display_model)
            {
                Ok(payload) => payload.to_string(),
                Err(error) => return Err(translate_error(stream, &error)),
            };
        let head = format!(
            "{}content-length: {}\r\n\r\n",
            gateway_response_head(status.as_u16(), "application/json", &binding.id),
            payload.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
    }
    record_usage(
        binding,
        &display_model,
        streaming,
        status.as_u16(),
        usage,
        usage.is_empty(),
        started.elapsed().as_millis() as u64,
    );
    Ok(())
}

/// Responses 入口 → Chat 上游：请求翻译、响应翻译，用量照记。
///
/// Codex 0.150 起只讲 Responses（`wire_api = "chat"` 已被移除），所以这条路
/// 是它接任何 Chat 类上游的唯一入口。
fn forward_responses_translated(
    stream: &mut TcpStream,
    binding: &GatewayBinding,
    request: &Value,
    started: Instant,
) -> Result<(), GatewayError> {
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let display_model = binding_exposed_model(binding).to_string();
    let chat_body =
        match crate::app::responses_chat::request_to_chat(request, &binding.default_model) {
            Ok(chat_body) => chat_body,
            Err(error) => return Err(translate_error(stream, &error)),
        };
    let chat_payload = chat_body.to_string();
    enforce_outbound_body_limit(stream, chat_payload.len())?;
    let client = upstream_client(&binding.base_url).map_err(|_| GatewayError::Answered)?;
    let mut response = match client
        .post(join_upstream(&binding.base_url, "/chat/completions"))
        .header("authorization", format!("Bearer {}", binding.api_key))
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(chat_payload)
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
        record_usage(
            binding,
            &display_model,
            streaming,
            status.as_u16(),
            Usage::default(),
            true,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    if streaming {
        write_stream_head(stream, status.as_u16(), "text/event-stream", &binding.id)
            .map_err(|_| GatewayError::Answered)?;
        let mut translator =
            crate::app::responses_chat::ResponsesStream::new(display_model.clone());
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
            &display_model,
            true,
            status.as_u16(),
            Usage {
                input,
                output,
                cached: translator.cached_tokens(),
                reasoning: translator.reasoning_tokens(),
            },
            input == 0 && output == 0,
            started.elapsed().as_millis() as u64,
        );
        return Ok(());
    }

    let mut raw = Vec::new();
    let _ = response.read_to_end(&mut raw);
    let chat: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    let payload =
        crate::app::responses_chat::response_to_responses(&chat, &display_model).to_string();
    let head = format!(
        "{}content-length: {}\r\n\r\n",
        gateway_response_head(status.as_u16(), "application/json", &binding.id),
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
    let usage = extract_usage("openai-chat", &chat);
    record_usage(
        binding,
        &display_model,
        false,
        status.as_u16(),
        usage,
        usage.is_empty(),
        started.elapsed().as_millis() as u64,
    );
    Ok(())
}

fn translate_error(stream: &mut TcpStream, detail: &str) -> GatewayError {
    let _ = write_json_error(stream, 400, "himind_gateway_translation_failed", detail);
    GatewayError::Answered
}

fn media_normalize_error(stream: &mut TcpStream, detail: &str) -> GatewayError {
    let _ = write_json_error(
        stream,
        400,
        "himind_gateway_media_normalize_failed",
        &format!("媒体请求规格化失败：{detail}"),
    );
    GatewayError::Answered
}

fn enforce_outbound_body_limit(stream: &mut TcpStream, actual: usize) -> Result<(), GatewayError> {
    if actual <= OUTBOUND_BODY_LIMIT {
        return Ok(());
    }
    let message = format!(
        "媒体外置后请求体仍为 {:.1} MiB，超过本机网关 {} MiB 出站预算。请新建会话或减少历史工具输出后重试。",
        actual as f64 / (1024.0 * 1024.0),
        OUTBOUND_BODY_LIMIT / (1024 * 1024),
    );
    let _ = write_json_error(
        stream,
        413,
        "himind_gateway_request_budget_exceeded",
        &message,
    );
    Err(GatewayError::Answered)
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
        413 => "Payload Too Large",
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
    let head = format!(
        "{}\r\n",
        gateway_response_head(status, content_type, binding_id)
    );
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
    crate::approval::manager::append_event_log("warn", &format!("本机网关上游请求失败：{detail}"));
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
    latency_ms: u64,
) {
    let record = LocalUsageRecord {
        occurred_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        binding_id: binding.id.clone(),
        client: binding.client.clone(),
        service: binding.service.clone(),
        profile_id: binding.profile_id.clone(),
        model: model.to_string(),
        upstream_model: binding.upstream_model.clone(),
        protocol: binding.protocol.clone(),
        stream: streaming,
        input_tokens: usage.input,
        output_tokens: usage.output,
        cached_tokens: usage.cached,
        reasoning_tokens: usage.reasoning,
        status: if status < 400 { "success" } else { "failure" }.to_string(),
        usage_unreported: unreported,
        latency_ms,
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
            profile_id: String::new(),
            entry_protocol: protocol.to_string(),
            exposed_model: "deepseek-v4-flash".to_string(),
            models: vec!["deepseek-v4-flash".to_string()],
            default_model: "deepseek-v4-flash".to_string(),
            upstream_model: "deepseek-v4-flash".to_string(),
            is_default: true,
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
        assert_eq!(
            head.headers.get("content-type").unwrap(),
            "application/json"
        );
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
        assert_eq!(
            token_from_headers(&headers(&[("authorization", "Basic zz")])),
            None
        );
    }

    #[test]
    fn authorize_prefers_token_and_rejects_unknown_tokens() {
        let bindings = vec![binding("codex", "openai-chat", "tok-1")];
        let ok = authorize(
            &bindings,
            &headers(&[("authorization", "Bearer tok-1")]),
            b"",
            "openai-chat",
        );
        assert_eq!(ok.unwrap().client, "codex");
        let unknown = authorize(
            &bindings,
            &headers(&[("authorization", "Bearer nope")]),
            b"",
            "openai-chat",
        );
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
    fn token_selects_route_by_exposed_model() {
        let mut first = binding("codex", "openai-chat", "tok-1");
        first.id = "codex:profile:alias-a".to_string();
        first.profile_id = "profile".to_string();
        first.exposed_model = "service-a/model-a".to_string();
        first.models = vec![first.exposed_model.clone()];
        first.upstream_model = "model-a".to_string();
        first.is_default = true;
        let mut second = first.clone();
        second.id = "codex:profile:alias-b".to_string();
        second.exposed_model = "service-b/model-b".to_string();
        second.models = vec![second.exposed_model.clone()];
        second.upstream_model = "model-b".to_string();
        second.is_default = false;
        let bindings = vec![first, second];

        let selected = authorize(
            &bindings,
            &headers(&[("authorization", "Bearer tok-1")]),
            br#"{"model":"service-b/model-b"}"#,
            "openai-chat",
        )
        .unwrap();
        assert_eq!(selected.upstream_model, "model-b");

        assert_eq!(
            authorize(
                &bindings,
                &headers(&[("authorization", "Bearer tok-1")]),
                br#"{"model":"missing"}"#,
                "openai-chat",
            )
            .unwrap_err(),
            GatewayError::UnknownModel
        );
    }

    #[test]
    fn aggregated_models_payload_uses_exposed_aliases() {
        let mut first = binding("codex", "openai-chat", "tok-1");
        first.profile_id = "profile".to_string();
        first.exposed_model = "service-a/model-a".to_string();
        first.models = vec![first.exposed_model.clone()];
        let mut second = binding("codex", "openai-chat", "tok-1");
        second.profile_id = "profile".to_string();
        second.exposed_model = "service-b/model-b".to_string();
        second.models = vec![second.exposed_model.clone()];
        let refs = vec![&first, &second];
        let payload: Value = serde_json::from_str(&models_payload(&refs)).unwrap();
        let ids = payload["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["service-a/model-a", "service-b/model-b"]);
    }

    #[test]
    fn request_and_response_models_are_mapped_to_aliases() {
        let request =
            rewrite_request_model(br#"{"model":"service-a/model-a","input":"hi"}"#, "model-a");
        let request: Value = serde_json::from_slice(&request).unwrap();
        assert_eq!(request["model"], "model-a");

        let response =
            rewrite_json_model(br#"{"model":"model-a","output":"ok"}"#, "service-a/model-a");
        let response: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["model"], "service-a/model-a");

        let line = rewrite_sse_model_line(
            r#"data: {"model":"model-a","delta":"ok"}"#,
            "service-a/model-a",
        );
        assert!(line.contains(r#""model":"service-a/model-a""#));
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

        let untouched = inject_stream_usage_flag(
            br#"{"model":"m","stream":true,"stream_options":{"include_usage":true}}"#,
        );
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
            Usage {
                input: 100,
                output: 20,
                cached: 64,
                reasoning: 7
            }
        );

        let responses: Value = serde_json::from_str(
            r#"{"usage":{"input_tokens":11,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}"#,
        )
        .unwrap();
        assert_eq!(
            extract_usage("openai-responses", &responses),
            Usage {
                input: 11,
                output: 2,
                cached: 3,
                reasoning: 0
            }
        );

        let anthropic: Value = serde_json::from_str(
            r#"{"usage":{"input_tokens":5,"output_tokens":6,"cache_read_input_tokens":7,"cache_creation_input_tokens":8}}"#,
        )
        .unwrap();
        assert_eq!(
            extract_usage("anthropic", &anthropic),
            Usage {
                input: 5,
                output: 6,
                cached: 7,
                reasoning: 0
            }
        );
    }

    #[test]
    fn sse_scanner_merges_split_anthropic_usage() {
        let mut scanner = StreamScanner::new("anthropic".to_string());
        scanner.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":30}}}\n\n");
        scanner.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\n");
        assert_eq!(
            scanner.usage,
            Usage {
                input: 12,
                output: 9,
                cached: 30,
                reasoning: 0
            }
        );
    }

    #[test]
    fn sse_scanner_reads_responses_completed_usage() {
        let mut scanner = StreamScanner::new("openai-responses".to_string());
        scanner.push(b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":4}}}\n\n");
        assert_eq!(
            scanner.usage,
            Usage {
                input: 8,
                output: 4,
                cached: 0,
                reasoning: 0
            }
        );
    }

    #[test]
    fn sse_scanner_ignores_done_and_partial_lines() {
        let mut scanner = StreamScanner::new("openai-chat".to_string());
        scanner.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n");
        scanner.push(b"data: {\"usa");
        assert!(scanner.usage.is_empty());
        scanner.push(b"ge\":{\"prompt_tokens\":4,\"completion_tokens\":1}}\n\n");
        assert_eq!(
            scanner.usage,
            Usage {
                input: 4,
                output: 1,
                cached: 0,
                reasoning: 0
            }
        );
    }

    #[test]
    fn oversized_declared_body_returns_structured_413() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let resolver: BindingResolver = Arc::new(Vec::new);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, &resolver).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        let head = format!(
            "POST /v1/responses HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            inbound_body_limit_bytes() + 1
        );
        client.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 413 Payload Too Large"));
        assert!(response.contains("himind_gateway_request_too_large"));
        assert!(response.contains("192 MiB"));
    }
    #[test]
    fn outbound_budget_exceeded_returns_structured_413() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let error =
                enforce_outbound_body_limit(&mut stream, OUTBOUND_BODY_LIMIT + 1).unwrap_err();
            assert_eq!(error, GatewayError::Answered);
        });
        let mut client = TcpStream::connect(address).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 413 Payload Too Large"));
        assert!(response.contains("himind_gateway_request_budget_exceeded"));
        assert!(response.contains("24 MiB"));
    }
    #[test]
    fn gateway_routes_alias_and_rewrites_upstream_model() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_thread = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let (_, body) = match read_request(&mut stream).unwrap() {
                RequestRead::Request(head, body) => (head, body),
                _ => panic!("expected request"),
            };
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["model"], "model-a");
            let response = br#"{"id":"chatcmpl-test","model":"model-a","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(response).unwrap();
            stream.flush().unwrap();
        });

        let token = "alias-token".to_string();
        let resolver_token = token.clone();
        let resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync> = Box::new(move || {
            vec![GatewayBinding {
                id: "codex:profile:service/model-a".to_string(),
                client: "codex".to_string(),
                service: "custom:service".to_string(),
                profile_id: "profile".to_string(),
                entry_protocol: "openai-chat".to_string(),
                exposed_model: "service/model-a".to_string(),
                models: vec!["service/model-a".to_string()],
                default_model: "model-a".to_string(),
                upstream_model: "model-a".to_string(),
                is_default: true,
                protocol: "openai-chat".to_string(),
                base_url: format!("http://{upstream_address}"),
                api_key: "upstream-key".to_string(),
                token: resolver_token.clone(),
                platform_metered: false,
            }]
        });
        let mut gateway = InferenceGateway::start(Some(0), resolver, false).unwrap();
        let request_body =
            br#"{"model":"service/model-a","messages":[{"role":"user","content":"hi"}]}"#;
        let mut client = TcpStream::connect(("127.0.0.1", gateway.port())).unwrap();
        let head = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            request_body.len()
        );
        client.write_all(head.as_bytes()).unwrap();
        client.write_all(request_body).unwrap();
        client.flush().unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        gateway.stop();
        upstream_thread.join().unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "response={response}"
        );
        assert!(
            response.contains(r#""model":"service/model-a""#),
            "response={response}"
        );
    }
    #[test]
    fn responses_request_through_gateway_offloads_large_images_before_upstream() {
        use base64::Engine as _;
        use image::{ImageBuffer, Rgb};

        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-gateway-media-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);

        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_thread = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let (head, body) = match read_request(&mut stream).unwrap() {
                RequestRead::Request(head, body) => (head, body),
                _ => panic!("mock upstream expected a request"),
            };
            assert_eq!(head.path, "/v1/responses");
            let response = br#"{"id":"resp_test","usage":{"input_tokens":1,"output_tokens":1}}"#;
            let response_head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(response_head.as_bytes()).unwrap();
            stream.write_all(response).unwrap();
            stream.flush().unwrap();
            body
        });

        let token = "local-test-token".to_string();
        let resolver_token = token.clone();
        let resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync> = Box::new(move || {
            vec![GatewayBinding {
                id: "codex:test".to_string(),
                client: "codex".to_string(),
                service: "test".to_string(),
                profile_id: String::new(),
                entry_protocol: "openai-responses".to_string(),
                exposed_model: "test-model".to_string(),
                models: vec!["test-model".to_string()],
                default_model: "test-model".to_string(),
                upstream_model: "test-model".to_string(),
                is_default: true,
                protocol: "openai-responses".to_string(),
                base_url: format!("http://{upstream_address}"),
                api_key: "upstream-key".to_string(),
                token: resolver_token.clone(),
                platform_metered: false,
            }]
        });
        let mut gateway = InferenceGateway::start(Some(0), resolver, false).unwrap();

        let image: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(512, 512, |x, y| {
            let value = (x.wrapping_mul(31) ^ y.wrapping_mul(17)) as u8;
            Rgb([value, value.wrapping_add(41), value.wrapping_add(83)])
        });
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        let request = json!({
            "model": "test-model",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": data_url,
                    "detail": "original"
                }]
            }],
            "stream": false
        });
        let request_body = serde_json::to_vec(&request).unwrap();
        let input_size = request_body.len();
        let mut client = TcpStream::connect(("127.0.0.1", gateway.port())).unwrap();
        let request_head = format!(
            "POST /v1/responses HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            request_body.len()
        );
        client.write_all(request_head.as_bytes()).unwrap();
        client.write_all(&request_body).unwrap();
        client.flush().unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        gateway.stop();
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "gateway response={response}"
        );
        let forwarded = upstream_thread.join().unwrap();
        let forwarded_json: Value = serde_json::from_slice(&forwarded).unwrap();
        let forwarded_url = forwarded_json["input"][0]["content"][0]["image_url"]
            .as_str()
            .unwrap();
        assert!(forwarded_url.starts_with("data:image/jpeg;base64,"));
        assert!(forwarded.len() < input_size * 3 / 4);
        assert!(forwarded_json["input"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "input_text"
                && part["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("media_id=")));

        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    /// 已有客户端走网关时，优先端口被占用必须失败：换端口会让那些客户端
    /// 配置里的地址立刻失效，而且界面上看不出来。
    #[test]
    fn occupied_preferred_port_fails_when_clients_are_bound() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let result = InferenceGateway::start(Some(port), Box::new(Vec::new), true);
        let error = result.err().expect("端口被占用时不应静默换端口");
        assert!(
            error.contains("不换端口"),
            "错误应说明为什么不换端口：{error}"
        );
    }

    /// 还没有任何绑定时允许退到临时端口，但必须把这件事记成提示。
    #[test]
    fn occupied_preferred_port_falls_back_only_without_bindings() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let gateway = InferenceGateway::start(Some(port), Box::new(Vec::new), false).unwrap();
        assert_ne!(gateway.port(), port);
        assert!(
            gateway.url().starts_with("http://localhost:"),
            "客户端侧地址必须使用 localhost，避免 Windows 系统代理拦截裸 127.0.0.1"
        );
        assert!(!gateway.notice.is_empty(), "换端口必须留下提示");
    }

    /// 端口解析优先级：环境变量 > 持久化设置 > 默认值；`0` 视为未设置。
    #[test]
    fn gateway_port_resolution_prefers_env_then_setting_then_default() {
        assert_eq!(resolve_gateway_port(Some(19_001), Some(19_002)), 19_001);
        assert_eq!(resolve_gateway_port(None, Some(19_002)), 19_002);
        assert_eq!(resolve_gateway_port(None, None), DEFAULT_GATEWAY_PORT);
        assert_eq!(resolve_gateway_port(Some(0), Some(19_002)), 19_002);
        assert_eq!(resolve_gateway_port(Some(0), Some(0)), DEFAULT_GATEWAY_PORT);
    }

    /// 持久化端口写到 `gateway.json`，`configured_port` 能原样读回。
    #[test]
    fn gateway_port_round_trips_through_settings_file() {
        std::env::remove_var(GATEWAY_PORT_ENV);
        let dir = std::env::temp_dir().join(format!("himind-gateway-port-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let state_path = dir.join("data").join("state.json");

        assert_eq!(configured_port(&state_path), DEFAULT_GATEWAY_PORT);
        set_configured_port(&state_path, 19_250).unwrap();
        assert_eq!(configured_port(&state_path), 19_250);
        assert!(dir.join("data").join(GATEWAY_CONFIG_FILE).is_file());
        assert!(
            set_configured_port(&state_path, 0).is_err(),
            "0 不是合法端口"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn anthropic_entry_translates_to_responses_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_thread = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let (head, body) = match read_request(&mut stream).unwrap() {
                RequestRead::Request(head, body) => (head, body),
                _ => panic!("expected request"),
            };
            assert_eq!(head.path, "/v1/responses");
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "model-a");
            assert_eq!(request["instructions"], "系统提示");
            assert_eq!(request["input"][0]["content"][0]["text"], "你好");
            assert_eq!(request["tools"][0]["strict"], false);
            let response = br#"{"id":"resp_test","object":"response","model":"model-a","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":3,"output_tokens":1}}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(response);
            let _ = stream.flush();
        });

        let token = "anthropic-responses-token".to_string();
        let resolver_token = token.clone();
        let resolver: Box<dyn Fn() -> Vec<GatewayBinding> + Send + Sync> = Box::new(move || {
            vec![GatewayBinding {
                id: "claude-desktop:profile:service/model-a".to_string(),
                client: "claude-desktop".to_string(),
                service: "custom:service".to_string(),
                profile_id: "profile".to_string(),
                entry_protocol: "anthropic".to_string(),
                exposed_model: "service/model-a".to_string(),
                models: vec!["service/model-a".to_string()],
                default_model: "model-a".to_string(),
                upstream_model: "model-a".to_string(),
                is_default: true,
                protocol: "openai-responses".to_string(),
                base_url: format!("http://{upstream_address}"),
                api_key: "upstream-key".to_string(),
                token: resolver_token.clone(),
                platform_metered: false,
            }]
        });
        let mut gateway = InferenceGateway::start(Some(0), resolver, false).unwrap();
        let request_body = r#"{"model":"service/model-a","max_tokens":8,"system":"系统提示","messages":[{"role":"user","content":"你好"}],"tools":[{"name":"read","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}]}"#.as_bytes();
        let mut client = TcpStream::connect(("127.0.0.1", gateway.port())).unwrap();
        let head = format!(
            "POST /v1/messages HTTP/1.1\r\nhost: 127.0.0.1\r\nx-api-key: {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            request_body.len()
        );
        let _ = client.write_all(head.as_bytes());
        let _ = client.write_all(request_body);
        let _ = client.flush();
        let mut response = String::new();
        let _ = client.read_to_string(&mut response);
        gateway.stop();
        upstream_thread.join().unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "response={response}"
        );
        assert!(
            response.contains(r#""type":"message""#),
            "response={response}"
        );
        assert!(response.contains(r#""text":"ok""#), "response={response}");
        assert!(
            response.contains(r#""model":"service/model-a""#),
            "response={response}"
        );
    }
}
