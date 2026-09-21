use base64::Engine;
use rand::RngCore;
use reqwest::blocking::Client;
use serde_json::json;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const HEADER_LIMIT: usize = 64 * 1024;
const HTML_RESPONSE_LIMIT: usize = 1024 * 1024;
const OBSERVED_FRAME_LIMIT: u64 = 4 * 1024 * 1024;
const MAX_PROXY_CONNECTIONS: usize = 64;
const SESSION_QUERY: &str = "himind_session";
const SESSION_COOKIE: &str = "himind_ai_session";
const RUNTIME_TOKEN_QUERY: &str = "token";
// Keep the iframe on a browser-visible loopback hostname. Authentication is
// carried by the session token bridge rather than a cross-site cookie.
const BROWSER_HOST: &str = "localhost";
const RUNTIME_REFERRER_POLICY: &str = r#"<meta name="referrer" content="same-origin">"#;
// DSH's model selector prefers the optional display name over the provider
// and model id. Keep the user-facing label tied to the real catalog id so a
// managed HiMind provider cannot turn `deepseek-v4-flash` into `HiMind-v4`.
fn model_profile_entry(model: &str) -> Value {
    let model = model.trim();
    json!({ "id": model, "name": model })
}

const RUNTIME_BRAND_BRIDGE: &str = r#"<style data-himind-runtime-brand>
button:has(> svg[viewBox="0 0 182 24"]) > svg {
  display: none !important;
}
button:has(> svg[viewBox="0 0 182 24"])::before {
  content: 'HiMind AI';
  color: currentColor;
  font: 600 18px/24px system-ui, sans-serif;
  white-space: nowrap;
}
</style>
<script>
(() => {
  const replacements = [
    [/DeepSeek Harness/gi, 'HiMind AI'],
    [/\bHARNESS\b/g, 'AI'],
  ];
  const replace = (value) => replacements.reduce(
    (current, [pattern, replacement]) => current.replace(pattern, replacement),
    value,
  );
  const apply = () => {
    document.title = 'HiMind AI';
    if (!document.body) return;
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      const next = replace(node.nodeValue || '');
      if (next !== node.nodeValue) node.nodeValue = next;
    }
    document.querySelectorAll('[aria-label], [title]').forEach((element) => {
      for (const attribute of ['aria-label', 'title']) {
        const value = element.getAttribute(attribute);
        if (value) element.setAttribute(attribute, replace(value));
      }
    });
  };
  let scheduled = false;
  const schedule = () => {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      apply();
    });
  };
  new MutationObserver(schedule).observe(document.documentElement, {
    childList: true,
    subtree: true,
    characterData: true,
    attributes: true,
    attributeFilter: ['aria-label', 'title'],
  });
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', schedule, { once: true });
  } else {
    schedule();
  }
})();
</script>"#;

const RUNTIME_AUTH_BRIDGE: &str = r#"<script data-himind-runtime-auth>
(() => {
  const session = new URLSearchParams(location.search).get("himind_session");
  if (!session) return;
  const withSession = (value) => {
    try {
      const url = value instanceof URL ? new URL(value.href) : new URL(String(value), location.href);
      if (url.origin === location.origin && !url.searchParams.has("himind_session")) {
        url.searchParams.set("himind_session", session);
      }
      return url.toString();
    } catch {
      return value;
    }
  };
  const sessionHeaders = (headers) => {
    const next = new Headers(headers || {});
    next.set("X-HiMind-Session", session);
    return next;
  };
  const nativeFetch = window.fetch;
  if (typeof nativeFetch === "function") {
    window.fetch = (input, init = {}) => {
      if (input instanceof Request) {
        const request = new Request(withSession(input.url), input);
        return nativeFetch(request, { ...init, headers: sessionHeaders(init.headers || request.headers) });
      }
      return nativeFetch(withSession(input), { ...init, headers: sessionHeaders(init.headers) });
    };
  }
  const nativeOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function(method, url, ...rest) {
    const opened = nativeOpen.call(this, method, withSession(url), ...rest);
    this.setRequestHeader("X-HiMind-Session", session);
    return opened;
  };
  if (typeof EventSource === "function") {
    const NativeEventSource = EventSource;
    window.EventSource = new Proxy(NativeEventSource, {
      construct(target, args) {
        args[0] = withSession(args[0]);
        return Reflect.construct(target, args);
      },
    });
  }
  if (typeof WebSocket === "function") {
    const NativeWebSocket = WebSocket;
    const HimindWebSocket = function(url, protocols) {
      return new NativeWebSocket(withSession(url), protocols);
    };
    HimindWebSocket.prototype = NativeWebSocket.prototype;
    for (const key of ["CONNECTING", "OPEN", "CLOSING", "CLOSED"]) {
      Object.defineProperty(HimindWebSocket, key, { value: NativeWebSocket[key] });
    }
    window.WebSocket = HimindWebSocket;
  }
})();
</script>"#;

pub(crate) type EventObserver = Arc<dyn Fn(Value) + Send + Sync + 'static>;

pub(crate) struct BuiltinAiProxy {
    url: String,
    shutdown: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
struct UpstreamSession {
    authority: String,
    cookie: String,
}

#[derive(Clone)]
pub(crate) struct BuiltinAiProxyControl {
    url: String,
}

impl BuiltinAiProxy {
    pub(crate) fn start(
        upstream_url: &str,
        observer: Option<EventObserver>,
    ) -> Result<Self, String> {
        let (upstream, upstream_session) = prepare_upstream(upstream_url)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("无法创建 HiMind AI 本机入口：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法配置 HiMind AI 本机入口：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("无法读取 HiMind AI 本机入口：{error}"))?
            .port();
        let token = random_token();
        let url = format!("http://{BROWSER_HOST}:{port}/?{SESSION_QUERY}={token}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_token = token.clone();
        let listener_active_connections = Arc::clone(&active_connections);
        let listener_thread = thread::Builder::new()
            .name("himind-ai-proxy-listener".to_string())
            .spawn(move || {
                while !listener_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if listener_active_connections
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                                    (current < MAX_PROXY_CONNECTIONS).then_some(current + 1)
                                })
                                .is_err()
                            {
                                let mut stream = stream;
                                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                                let _ = write_proxy_busy(&mut stream);
                                continue;
                            }
                            let connection_shutdown = Arc::clone(&listener_shutdown);
                            let connection_token = listener_token.clone();
                            let connection_upstream_session = upstream_session.clone();
                            let connection_observer = observer.clone();
                            let connection_active_connections =
                                Arc::clone(&listener_active_connections);
                            let spawn_result = thread::Builder::new()
                                .name("himind-ai-proxy-connection".to_string())
                                .spawn(move || {
                                    if let Err(error) = handle_connection(
                                        stream,
                                        upstream,
                                        &connection_token,
                                        &connection_upstream_session,
                                        connection_shutdown,
                                        connection_observer,
                                    ) {
                                        if error.kind() != io::ErrorKind::ConnectionReset
                                            && error.kind() != io::ErrorKind::BrokenPipe
                                        {
                                            eprintln!("HiMind AI 本机入口连接已关闭：{error}");
                                        }
                                    }
                                    connection_active_connections.fetch_sub(1, Ordering::AcqRel);
                                });
                            if let Err(error) = spawn_result {
                                listener_active_connections.fetch_sub(1, Ordering::AcqRel);
                                eprintln!("HiMind AI 本机入口连接线程创建失败：{error}");
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(error) => {
                            eprintln!("HiMind AI 本机入口已停止：{error}");
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("无法创建 HiMind AI 本机入口线程：{error}"))?;
        Ok(Self {
            url,
            shutdown,
            listener: Some(listener_thread),
        })
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn control(&self) -> BuiltinAiProxyControl {
        BuiltinAiProxyControl {
            url: self.url.clone(),
        }
    }

    pub(crate) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

impl BuiltinAiProxyControl {
    pub(crate) fn verify_browser_entry(&self) -> Result<(), String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("创建 HiMind AI 页面验证客户端失败: {error}"))?;
        let mut endpoint =
            url::Url::parse(&self.url).map_err(|_| "HiMind AI 本机地址无效".to_string())?;
        let session = endpoint
            .query_pairs()
            .find(|(name, _)| name == SESSION_QUERY)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "HiMind AI 本机会话令牌不可用".to_string())?;
        endpoint.set_path("/");
        endpoint.set_query(None);
        endpoint
            .set_host(Some("127.0.0.1"))
            .map_err(|_| "HiMind AI 本机地址无效".to_string())?;
        let response = client
            .get(endpoint)
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}={session}"),
            )
            .send()
            .map_err(|error| format!("请求 HiMind AI 页面失败: {error}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = response
            .text()
            .map_err(|error| format!("读取 HiMind AI 页面失败: {error}"))?;
        if !status.is_success() {
            return Err(format!("HiMind AI 页面返回 HTTP {status}"));
        }
        if !content_type.contains("text/html") || !body.contains("__ModuleLoader__") {
            return Err("HiMind AI 页面没有返回可启动的 Web 入口".to_string());
        }
        Ok(())
    }

    /// Register an Agent-selected project as a native DSH Workspace and create
    /// its blank session before the browser loads. DSH then applies its own
    /// recent-Workspace selection policy and opens the intended project.
    pub(crate) fn start_workspace_session(&self, workspace: &Path) -> Result<String, String> {
        let workspace_path = crate::extension_workspace::display_path(workspace);
        let response =
            self.call_runtime_api("workspace.create", json!({ "path": workspace_path }))?;
        let workspace_value = runtime_result_value(&response, "注册 DSH 工作区")?;
        let workspace_id = workspace_value
            .get("workspace")
            .and_then(|workspace| workspace.get("workspaceId"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "DSH 工作区响应缺少 workspaceId".to_string())?;

        let response =
            self.call_runtime_api("session.create", json!({ "workspaceId": workspace_id }))?;
        let session_value = runtime_result_value(&response, "创建 DSH 项目会话")?;
        session_value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| "DSH 项目会话响应缺少 sessionId".to_string())
    }

    /// Synchronize the Agent-owned provider through DSH's public API carrier.
    /// The initial browser handshake is important: DSH keeps its own session
    /// cookie in addition to the Agent proxy cookie, so calling the upstream
    /// port directly is intentionally avoided.
    pub(crate) fn sync_model_catalog(
        &self,
        default_model: &str,
        base_url: &str,
        models: &[String],
    ) -> Result<(), String> {
        let default_model = default_model.trim();
        let base_url = base_url.trim();
        if default_model.is_empty() || base_url.is_empty() || models.is_empty() {
            return Err("HiMind AI 模型目录为空".to_string());
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|error| format!("创建 DSH 模型同步客户端失败: {error}"))?;
        let described = self.call_api(&client, "settings.describe", json!({}))?;
        let namespaces = described
            .get("result")
            .and_then(|result| result.get("ok").and_then(Value::as_bool).filter(|ok| *ok))
            .and_then(|_| {
                described
                    .get("result")
                    .and_then(|result| result.get("value"))
            })
            .and_then(|value| value.get("namespaces"))
            .and_then(Value::as_array)
            .ok_or_else(|| "DSH 设置目录不可用".to_string())?;

        let provider_profile = json!({
            "displayName": "HiMind AI",
            "apiKeyEnv": "DEEPSEEK_API_KEY",
            "api": "openai-completions",
            "baseURL": base_url,
            "models": models
                .iter()
                .map(|model| model_profile_entry(model))
                .filter(|model| model.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()))
                .collect::<Vec<_>>(),
        });
        let llm_revision = namespace_revision(namespaces, "llm-pi-ai");
        self.mutate_settings(
            &client,
            "llm-pi-ai",
            vec![json!({
                "op": "set",
                "path": ["providers", "himind-proxy"],
                "value": provider_profile,
            })],
            llm_revision,
        )?;

        // A user-selected provider remains untouched. The built-in DeepSeek
        // default is migrated to the managed route, while an existing HiMind
        // model remains user-owned. Only an empty HiMind model is initialized
        // from the current service default.
        let default_namespace = namespaces
            .iter()
            .find(|item| item.get("ns").and_then(Value::as_str) == Some("agent-default-model"));
        let current_provider = default_namespace
            .and_then(|item| item.get("user"))
            .and_then(|value| value.get("provider"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let current_model = default_namespace
            .and_then(|item| item.get("user"))
            .and_then(|value| value.get("model"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if should_initialize_managed_model(current_provider, current_model) {
            let revision = default_namespace
                .and_then(|item| item.get("revision"))
                .and_then(Value::as_i64);
            self.mutate_settings(
                &client,
                "agent-default-model",
                vec![
                    json!({ "op": "set", "path": ["provider"], "value": "himind-proxy" }),
                    json!({ "op": "set", "path": ["model"], "value": default_model }),
                ],
                revision,
            )?;
        }
        Ok(())
    }

    fn call_api(&self, client: &Client, method: &str, payload: Value) -> Result<Value, String> {
        let mut endpoint =
            url::Url::parse(&self.url).map_err(|_| "HiMind AI 本机地址无效".to_string())?;
        let session = endpoint
            .query_pairs()
            .find(|(name, _)| name == SESSION_QUERY)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "HiMind AI 本机会话令牌不可用".to_string())?;
        endpoint.set_path(&format!("/api/{method}"));
        endpoint.set_query(None);
        let request = json!({
            "type": "client-request",
            "rpcId": format!("himind-sync-{}", unix_millis()),
            "method": method,
            "payload": payload,
        });
        let response = client
            .post(endpoint)
            // WebView2 accepts the Secure localhost cookie. Reqwest follows
            // standard HTTP cookie rules, so carry the short-lived local
            // session explicitly for the Agent-to-proxy control request.
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}={session}"),
            )
            .json(&request)
            .send()
            .map_err(|error| format!("DSH {method} 请求失败: {error}"))?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .map_err(|error| format!("DSH {method} 响应无效: {error}"))?;
        if !status.is_success() {
            return Err(format!("DSH {method} 返回 HTTP {status}"));
        }
        Ok(body)
    }

    /// Send a control request through the authenticated local DSH carrier.
    /// Runtime command names are intentionally kept at the gateway boundary;
    /// this method only owns transport/session-cookie handling.
    pub(crate) fn call_runtime_api(&self, method: &str, payload: Value) -> Result<Value, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| format!("创建 DSH 控制客户端失败: {error}"))?;
        self.call_api(&client, method, payload)
    }

    /// Answer a DSH server-request. Unlike ordinary runtime calls this is a
    /// client-response envelope and therefore is intentionally not routed
    /// through the client-request method dispatcher.
    pub(crate) fn respond_runtime_request(
        &self,
        rpc_id: &str,
        result_value: Value,
    ) -> Result<Value, String> {
        let rpc_id = rpc_id.trim();
        if rpc_id.is_empty() {
            return Err("DSH client-response requires rpcId".to_string());
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| format!("创建 DSH 响应客户端失败: {error}"))?;
        let mut endpoint =
            url::Url::parse(&self.url).map_err(|_| "HiMind AI 本机地址无效".to_string())?;
        let session = endpoint
            .query_pairs()
            .find(|(name, _)| name == SESSION_QUERY)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "HiMind AI 本机会话令牌不可用".to_string())?;
        endpoint.set_path("/api/respond");
        endpoint.set_query(None);
        let response = client
            .post(endpoint)
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}={session}"),
            )
            .json(&json!({
                "type": "client-response",
                "rpcId": rpc_id,
                "result": {"ok": true, "value": result_value},
            }))
            .send()
            .map_err(|error| format!("DSH client-response 请求失败: {error}"))?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .map_err(|error| format!("DSH client-response 响应无效: {error}"))?;
        if !status.is_success() {
            return Err(format!("DSH client-response 返回 HTTP {status}"));
        }
        Ok(body)
    }

    /// Probe only the shape/availability of a DSH RPC. Probes use a shorter
    /// timeout so a degraded local runtime cannot delay Agent startup or the
    /// command claim loop.
    pub(crate) fn probe_runtime_api(&self, method: &str, payload: Value) -> Result<Value, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| format!("创建 DSH 能力探测客户端失败: {error}"))?;
        self.call_api(&client, method, payload)
    }

    fn mutate_settings(
        &self,
        client: &Client,
        namespace: &str,
        ops: Vec<Value>,
        revision: Option<i64>,
    ) -> Result<(), String> {
        let mut payload = json!({ "ns": namespace, "ops": ops });
        if let Some(revision) = revision {
            payload["expectedRevision"] = json!(revision);
        }
        let response = self.call_api(client, "settings.mutate", payload)?;
        let result = response
            .get("result")
            .ok_or_else(|| "DSH 设置同步响应缺少结果".to_string())?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let message = result
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("DSH 设置同步被拒绝");
        Err(message.to_string())
    }
}

fn runtime_result_value<'a>(response: &'a Value, operation: &str) -> Result<&'a Value, String> {
    let result = response
        .get("result")
        .ok_or_else(|| format!("{operation}响应缺少结果"))?;
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return result
            .get("value")
            .ok_or_else(|| format!("{operation}响应缺少值"));
    }
    let code = result
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = result
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("DSH 拒绝了请求");
    Err(format!("{operation}失败（{code}）：{message}"))
}

fn namespace_revision(namespaces: &[Value], namespace: &str) -> Option<i64> {
    namespaces
        .iter()
        .find(|item| item.get("ns").and_then(Value::as_str) == Some(namespace))
        .and_then(|item| item.get("revision"))
        .and_then(Value::as_i64)
}

fn should_initialize_managed_model(provider: &str, model: &str) -> bool {
    provider.trim().is_empty()
        || provider == "deepseek-official"
        || (provider == "himind-proxy" && model.trim().is_empty())
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

impl Drop for BuiltinAiProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse_upstream(value: &str) -> Result<(SocketAddr, String, String), String> {
    let parsed = url::Url::parse(value).map_err(|_| "HiMind AI 地址无效".to_string())?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_none()
    {
        return Err("HiMind AI 地址不是本机安全地址".to_string());
    }
    let port = parsed.port().expect("validated port");
    let tokens = parsed
        .query_pairs()
        .filter(|(name, _)| name == RUNTIME_TOKEN_QUERY)
        .map(|(_, value)| value.into_owned())
        .collect::<Vec<_>>();
    if tokens.len() != 1 || tokens[0].is_empty() {
        return Err("HiMind AI 启动地址缺少唯一运行时令牌".to_string());
    }
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    Ok((
        SocketAddr::from(([127, 0, 0, 1], port)),
        format!("127.0.0.1:{port}"),
        target,
    ))
}

fn prepare_upstream(value: &str) -> Result<(SocketAddr, UpstreamSession), String> {
    let (upstream, authority, target) = parse_upstream(value)?;
    let cookie = establish_upstream_session(upstream, &authority, &target)
        .map_err(|error| format!("无法建立 HiMind AI 浏览器会话：{error}"))?;
    Ok((upstream, UpstreamSession { authority, cookie }))
}

fn establish_upstream_session(
    upstream: SocketAddr,
    authority: &str,
    target: &str,
) -> io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&upstream, Duration::from_secs(5))?;
    configure_stream(&stream)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {authority}\r\nAccept: text/html\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let response = read_complete_http_response(&mut stream)?;
    let header_end = find_header_end(&response).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "upstream authentication response is incomplete",
        )
    })?;
    let header = String::from_utf8_lossy(&response[..header_end]);
    let status_ok = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .is_some_and(|status| status == "303");
    if !status_ok {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "upstream rejected the launch token",
        ));
    }
    header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .find_map(|(_, value)| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "upstream authentication response did not issue a cookie",
            )
        })
}

fn cookie_name_from_pair(cookie: &str) -> &str {
    cookie
        .split_once('=')
        .map(|(name, _)| name.trim())
        .unwrap_or_default()
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn handle_connection(
    mut client: TcpStream,
    upstream: SocketAddr,
    token: &str,
    upstream_session: &UpstreamSession,
    shutdown: Arc<AtomicBool>,
    observer: Option<EventObserver>,
) -> io::Result<()> {
    configure_stream(&client)?;
    let initial = read_http_header(&mut client)?;
    let header_end = find_header_end(&initial)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "incomplete HTTP header"))?;
    let header = &initial[..header_end];
    let remainder = &initial[header_end..];
    let request = String::from_utf8_lossy(header);
    if !request_token_matches(&request, token) {
        return write_forbidden(&mut client);
    }

    let websocket = is_websocket_upgrade(&request);
    let runtime_entry_request = !websocket && is_runtime_entry_request(&request);
    let rewritten =
        rewrite_request_header(&request, websocket, runtime_entry_request, upstream_session);
    let mut server = TcpStream::connect_timeout(&upstream, Duration::from_secs(5))?;
    configure_stream(&server)?;
    server.write_all(rewritten.as_bytes())?;
    server.write_all(remainder)?;

    if runtime_entry_request {
        return proxy_customized_runtime_entry(&mut server, &mut client);
    }

    let mut client_reader = client.try_clone()?;
    let mut server_writer = server.try_clone()?;
    let upload_shutdown = Arc::clone(&shutdown);
    let upload = thread::Builder::new()
        .name("himind-ai-proxy-upload".to_string())
        .spawn(move || {
            copy_until_shutdown(
                &mut client_reader,
                &mut server_writer,
                &upload_shutdown,
                None,
            )
        })
        .map_err(|error| {
            io::Error::other(format!("proxy upload thread creation failed: {error}"))
        })?;

    let mut websocket_observer = websocket.then(|| WebSocketObserver::new(observer));
    let download = copy_until_shutdown(
        &mut server,
        &mut client,
        &shutdown,
        websocket_observer.as_mut(),
    );
    let _ = upload.join();
    download
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_nodelay(true)
}

fn read_http_header(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut data = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    while data.len() < HEADER_LIMIT {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                data.extend_from_slice(&chunk[..count]);
                if find_header_end(&data).is_some() {
                    return Ok(data);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTP header is missing or too large",
    ))
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn query_token_matches(request: &str, token: &str) -> bool {
    let Some(target) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        return false;
    };
    url::Url::parse(&format!("http://127.0.0.1{target}"))
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(name, _)| name == SESSION_QUERY)
                .map(|(_, value)| value == token)
        })
        .unwrap_or(false)
}

fn cookie_token_matches(request: &str, token: &str) -> bool {
    request.lines().skip(1).any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("cookie")
            && value.split(';').any(|item| {
                item.trim()
                    .split_once('=')
                    .is_some_and(|(name, value)| name == SESSION_COOKIE && value == token)
            })
    })
}

fn request_token_matches(request: &str, token: &str) -> bool {
    query_token_matches(request, token)
        || cookie_token_matches(request, token)
        || header_token_matches(request, "x-himind-session", token)
        || referer_token_matches(request, token)
}

fn header_token_matches(request: &str, header_name: &str, token: &str) -> bool {
    request.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case(header_name) && value.trim() == token
        })
    })
}

fn referer_token_matches(request: &str, token: &str) -> bool {
    request
        .lines()
        .skip(1)
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("referer").then_some(value.trim())
        })
        .is_some_and(|referer| {
            url::Url::parse(referer).ok().is_some_and(|url| {
                url.query_pairs()
                    .any(|(name, value)| name == SESSION_QUERY && value == token)
            })
        })
}

fn is_websocket_upgrade(request: &str) -> bool {
    request.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket")
        })
    })
}

fn rewrite_request_header(
    request: &str,
    websocket: bool,
    runtime_entry_request: bool,
    upstream_session: &UpstreamSession,
) -> String {
    let mut lines = request.lines();
    let mut output = String::new();
    let mut saw_cookie = false;
    if let Some(line) = lines.next() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some(method), Some(target), Some(version)) => {
                output.push_str(method);
                output.push(' ');
                output.push_str(&strip_session_query(target));
                output.push(' ');
                output.push_str(version);
                output.push_str("\r\n");
            }
            _ => {
                output.push_str(line);
                output.push_str("\r\n");
            }
        }
    }
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("sec-websocket-extensions")
            || name.eq_ignore_ascii_case("x-himind-session")
            || (runtime_entry_request && name.eq_ignore_ascii_case("accept-encoding"))
            || (!websocket && name.eq_ignore_ascii_case("connection"))
        {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            output.push_str("Host: ");
            output.push_str(&upstream_session.authority);
            output.push_str("\r\n");
            continue;
        }
        if name.eq_ignore_ascii_case("origin") {
            output.push_str("Origin: http://");
            output.push_str(&upstream_session.authority);
            output.push_str("\r\n");
            continue;
        }
        if name.eq_ignore_ascii_case("cookie") {
            saw_cookie = true;
            let cookies = value
                .split(';')
                .map(str::trim)
                .filter(|item| {
                    let item = item.trim();
                    item.split_once('=').is_none_or(|(cookie_name, _)| {
                        cookie_name != SESSION_COOKIE
                            && item != upstream_session.cookie.as_str()
                            && cookie_name != cookie_name_from_pair(&upstream_session.cookie)
                    })
                })
                .collect::<Vec<_>>();
            output.push_str("Cookie: ");
            if !cookies.is_empty() {
                output.push_str(&cookies.join("; "));
                output.push_str("; ");
            }
            output.push_str(&upstream_session.cookie);
            output.push_str("\r\n");
            continue;
        }
        output.push_str(name);
        output.push(':');
        output.push_str(value);
        output.push_str("\r\n");
    }
    if !saw_cookie {
        output.push_str("Cookie: ");
        output.push_str(&upstream_session.cookie);
        output.push_str("\r\n");
    }
    if !websocket {
        if runtime_entry_request {
            output.push_str("Accept-Encoding: identity\r\n");
        }
        output.push_str("Connection: close\r\n");
    }
    output.push_str("\r\n");
    output
}

fn strip_session_query(target: &str) -> String {
    let Some((path, query)) = target.split_once('?') else {
        return target.to_string();
    };
    let filtered = query
        .split('&')
        .filter(|part| part.split_once('=').map(|(name, _)| name).unwrap_or(part) != SESSION_QUERY)
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", filtered.join("&"))
    }
}

fn is_runtime_entry_request(request: &str) -> bool {
    request
        .lines()
        .next()
        .and_then(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?, parts.next()?))
        })
        .is_some_and(|(method, target)| {
            method == "GET"
                && url::Url::parse(&format!("http://127.0.0.1{target}"))
                    .is_ok_and(|url| url.path() == "/")
        })
}

fn proxy_customized_runtime_entry(
    server: &mut TcpStream,
    client: &mut TcpStream,
) -> io::Result<()> {
    server.set_read_timeout(Some(Duration::from_secs(5)))?;
    let response = read_complete_http_response(server)?;
    let Some(customized) = customize_runtime_html_response(&response)? else {
        return client.write_all(&response);
    };
    client.write_all(&customized)
}

fn read_complete_http_response(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut response = Vec::with_capacity(16 * 1024);
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(response),
            Ok(count) => {
                response.extend_from_slice(&chunk[..count]);
                if response.len() > HTML_RESPONSE_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "HiMind AI entry response is too large",
                    ));
                }
                if http_response_complete(&response)? {
                    return Ok(response);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HiMind AI entry response timed out",
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn http_response_complete(response: &[u8]) -> io::Result<bool> {
    let Some(header_end) = find_header_end(response) else {
        return Ok(false);
    };
    let header = String::from_utf8_lossy(&response[..header_end]);
    let body = &response[header_end..];
    if header_has_token(&header, "transfer-encoding", "chunked") {
        return decode_chunked_body(body).map(|body| body.is_some());
    }
    if let Some(length) = response_content_length(&header)? {
        return Ok(body.len() >= length);
    }
    Ok(false)
}

fn customize_runtime_html_response(response: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let Some(header_end) = find_header_end(response) else {
        return Ok(None);
    };
    let header = String::from_utf8_lossy(&response[..header_end]);
    if !header_has_token(&header, "content-type", "text/html")
        || header.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("content-encoding")
                    && !value.trim().eq_ignore_ascii_case("identity")
            })
        })
    {
        return Ok(None);
    }
    let raw_body = &response[header_end..];
    let body = if header_has_token(&header, "transfer-encoding", "chunked") {
        decode_chunked_body(raw_body)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete chunked HTML response",
            )
        })?
    } else if let Some(length) = response_content_length(&header)? {
        raw_body
            .get(..length)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete HTML response")
            })?
            .to_vec()
    } else {
        raw_body.to_vec()
    };
    let html = String::from_utf8(body)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HTML response is not UTF-8"))?;
    let customized = customize_runtime_html(&html);
    let mut output = String::new();
    for (index, line) in header.lines().enumerate() {
        if index > 0
            && line.split_once(':').is_some_and(|(name, _)| {
                name.eq_ignore_ascii_case("content-length")
                    || name.eq_ignore_ascii_case("transfer-encoding")
                    || name.eq_ignore_ascii_case("connection")
                    || name.eq_ignore_ascii_case("keep-alive")
            })
        {
            continue;
        }
        if !line.is_empty() {
            output.push_str(line);
            output.push_str("\r\n");
        }
    }
    output.push_str(&format!("Content-Length: {}\r\n", customized.len()));
    output.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
    let mut bytes = output.into_bytes();
    bytes.extend_from_slice(customized.as_bytes());
    Ok(Some(bytes))
}

fn customize_runtime_html(html: &str) -> String {
    let mut html = html.replace(
        "<title>DeepSeek Harness</title>",
        "<title>HiMind AI</title>",
    );
    if !html.contains("data-himind-runtime-auth") {
        let bridge = format!("{RUNTIME_REFERRER_POLICY}{RUNTIME_AUTH_BRIDGE}");
        let lowercase = html.to_ascii_lowercase();
        let head_end = lowercase.find("</head>");
        let insertion = lowercase
            .find("<script")
            .filter(|script| head_end.is_none_or(|head_end| *script < head_end))
            .or(head_end)
            .or_else(|| lowercase.find("<head>").map(|index| index + "<head>".len()));
        if let Some(index) = insertion {
            html.insert_str(index, &bridge);
        } else {
            html.insert_str(0, &bridge);
        }
    }
    if !html.contains("data-himind-runtime-brand") {
        html = html.replacen("</head>", &format!("{RUNTIME_BRAND_BRIDGE}\n</head>"), 1);
    }
    html
}

fn response_content_length(header: &str) -> io::Result<Option<usize>> {
    header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("content-length") {
                return None;
            }
            Some(
                value.trim().parse::<usize>().map(Some).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid content length")
                }),
            )
        })
        .transpose()
        .map(Option::flatten)
}

fn header_has_token(header: &str, expected_name: &str, expected_value: &str) -> bool {
    header.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case(expected_name)
                && value
                    .split(';')
                    .flat_map(|part| part.split(','))
                    .any(|part| part.trim().eq_ignore_ascii_case(expected_value))
        })
    })
}

fn decode_chunked_body(body: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut cursor = 0_usize;
    let mut decoded = Vec::new();
    loop {
        let Some(line_end) = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|index| cursor + index)
        else {
            return Ok(None);
        };
        let size_text = std::str::from_utf8(&body[cursor..line_end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size"))?;
        let size =
            usize::from_str_radix(size_text.split(';').next().unwrap_or_default().trim(), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size"))?;
        cursor = line_end + 2;
        if size == 0 {
            return Ok(Some(decoded));
        }
        let Some(chunk_end) = cursor.checked_add(size) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk is too large",
            ));
        };
        if body.len() < chunk_end + 2 {
            return Ok(None);
        }
        if &body[chunk_end..chunk_end + 2] != b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk terminator is missing",
            ));
        }
        decoded.extend_from_slice(&body[cursor..chunk_end]);
        cursor = chunk_end + 2;
    }
}

fn write_forbidden(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: 9\r\nConnection: close\r\n\r\nforbidden",
    )
}

fn write_proxy_busy(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy",
    )
}

fn copy_until_shutdown(
    reader: &mut TcpStream,
    writer: &mut TcpStream,
    shutdown: &AtomicBool,
    mut observer: Option<&mut WebSocketObserver>,
) -> io::Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    while !shutdown.load(Ordering::Acquire) {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if let Some(observer) = observer.as_deref_mut() {
                    observer.feed(&buffer[..count]);
                }
                writer.write_all(&buffer[..count])?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct WebSocketObserver {
    handshake_complete: bool,
    buffer: Vec<u8>,
    skip_payload: u64,
    fragmented: Vec<u8>,
    fragmented_text: bool,
    observer: Option<EventObserver>,
}

impl WebSocketObserver {
    fn new(observer: Option<EventObserver>) -> Self {
        Self {
            handshake_complete: false,
            buffer: Vec::new(),
            skip_payload: 0,
            fragmented: Vec::new(),
            fragmented_text: false,
            observer,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        if self.observer.is_none() {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        if !self.handshake_complete {
            let Some(end) = find_header_end(&self.buffer) else {
                if self.buffer.len() > HEADER_LIMIT {
                    self.observer = None;
                }
                return;
            };
            self.buffer.drain(..end);
            self.handshake_complete = true;
        }
        self.parse_frames();
    }

    fn parse_frames(&mut self) {
        loop {
            if self.skip_payload > 0 {
                let consumed = self.buffer.len().min(self.skip_payload as usize);
                self.buffer.drain(..consumed);
                self.skip_payload -= consumed as u64;
                if self.skip_payload > 0 {
                    return;
                }
            }
            if self.buffer.len() < 2 {
                return;
            }
            let first = self.buffer[0];
            let second = self.buffer[1];
            let fin = first & 0x80 != 0;
            let opcode = first & 0x0f;
            let masked = second & 0x80 != 0;
            let mut header_len = 2_usize;
            let mut payload_len = u64::from(second & 0x7f);
            if payload_len == 126 {
                if self.buffer.len() < 4 {
                    return;
                }
                payload_len = u64::from(u16::from_be_bytes([self.buffer[2], self.buffer[3]]));
                header_len += 2;
            } else if payload_len == 127 {
                if self.buffer.len() < 10 {
                    return;
                }
                payload_len = u64::from_be_bytes(self.buffer[2..10].try_into().unwrap());
                header_len += 8;
            }
            let mask = if masked {
                if self.buffer.len() < header_len + 4 {
                    return;
                }
                let value: [u8; 4] = self.buffer[header_len..header_len + 4].try_into().unwrap();
                header_len += 4;
                Some(value)
            } else {
                None
            };
            if payload_len > OBSERVED_FRAME_LIMIT {
                self.buffer.drain(..header_len);
                self.skip_payload = payload_len;
                self.fragmented.clear();
                self.fragmented_text = false;
                continue;
            }
            let total = header_len.saturating_add(payload_len as usize);
            if self.buffer.len() < total {
                return;
            }
            let mut payload = self.buffer[header_len..total].to_vec();
            self.buffer.drain(..total);
            if let Some(mask) = mask {
                for (index, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[index % 4];
                }
            }
            match opcode {
                0x1 if fin => self.observe_text(&payload),
                0x1 => {
                    self.fragmented = payload;
                    self.fragmented_text = true;
                }
                0x0 if self.fragmented_text => {
                    if self.fragmented.len().saturating_add(payload.len())
                        > OBSERVED_FRAME_LIMIT as usize
                    {
                        self.fragmented.clear();
                        self.fragmented_text = false;
                        continue;
                    }
                    self.fragmented.extend_from_slice(&payload);
                    if fin {
                        let completed = std::mem::take(&mut self.fragmented);
                        self.fragmented_text = false;
                        self.observe_text(&completed);
                    }
                }
                _ => {}
            }
        }
    }

    fn observe_text(&self, payload: &[u8]) {
        let Some(observer) = self.observer.as_ref() else {
            return;
        };
        if let Ok(value) = serde_json::from_slice::<Value>(payload) {
            observer(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn upstream_must_be_a_numbered_loopback_http_url() {
        assert_eq!(
            parse_upstream("http://127.0.0.1:3080/?token=test-token")
                .unwrap()
                .0
                .port(),
            3080
        );
        assert!(parse_upstream("http://127.0.0.1:3080/?token=test-token").is_ok());
        assert!(parse_upstream("http://127.0.0.1:3080").is_err());
        assert!(parse_upstream("http://localhost:3080/?token=test-token").is_err());
        assert!(parse_upstream("https://127.0.0.1:3080/?token=test-token").is_err());
        assert!(parse_upstream("http://127.0.0.1").is_err());
    }

    #[test]
    fn session_token_is_accepted_from_the_browser_bridge() {
        let token = "test-token";
        assert!(query_token_matches(
            "GET /?himind_session=test-token HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            token
        ));
        assert!(cookie_token_matches(
            "GET /api/events.mux HTTP/1.1\r\nCookie: other=1; himind_ai_session=test-token\r\n\r\n",
            token
        ));
        assert!(!cookie_token_matches(
            "GET / HTTP/1.1\r\nCookie: himind_ai_session=wrong\r\n\r\n",
            token
        ));
        assert!(header_token_matches(
            "GET /api/events.mux HTTP/1.1\r\nX-HiMind-Session: test-token\r\n\r\n",
            "x-himind-session",
            token
        ));
        assert!(request_token_matches(
            "GET /api/events.mux HTTP/1.1\r\nReferer: http://localhost:4567/?himind_session=test-token\r\n\r\n",
            token
        ));
    }

    #[test]
    fn websocket_observer_reads_split_json_frames() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let callback: EventObserver = Arc::new(move |value| sink.lock().unwrap().push(value));
        let mut observer = WebSocketObserver::new(Some(callback));
        observer.feed(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n");
        let payload = br#"{"type":"server-request","payload":{"type":"session/event"}}"#;
        let mut frame = vec![0x81, payload.len() as u8];
        frame.extend_from_slice(payload);
        observer.feed(&frame[..5]);
        observer.feed(&frame[5..]);
        assert_eq!(observed.lock().unwrap().len(), 1);
    }

    #[test]
    fn proxy_cookie_is_not_forwarded_to_the_runtime() {
        let upstream_session = UpstreamSession {
            authority: "127.0.0.1:3080".to_string(),
            cookie: "dsh-auth-test=runtime-secret".to_string(),
        };
        let rewritten = rewrite_request_header(
            "GET /?himind_session=secret HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: himind_ai_session=secret; theme=dark\r\nX-HiMind-Session: secret\r\nConnection: keep-alive\r\n\r\n",
            false,
            true,
            &upstream_session,
        );
        assert!(rewritten.starts_with("GET / HTTP/1.1"));
        assert!(!rewritten.contains("himind_ai_session=secret"));
        assert!(!rewritten.contains("X-HiMind-Session"));
        assert!(rewritten.contains("Host: 127.0.0.1:3080"));
        assert!(rewritten.contains("Cookie: theme=dark; dsh-auth-test=runtime-secret"));
        assert!(rewritten.contains("Connection: close"));
    }

    #[test]
    fn upstream_authority_and_cookie_are_rewritten_for_websocket_requests() {
        let upstream_session = UpstreamSession {
            authority: "127.0.0.1:3080".to_string(),
            cookie: "dsh-auth-test=runtime-secret".to_string(),
        };
        let rewritten = rewrite_request_header(
            "GET /api/events.mux HTTP/1.1\r\nHost: localhost:4567\r\nOrigin: http://localhost:4567\r\nCookie: himind_ai_session=secret\r\nUpgrade: websocket\r\n\r\n",
            true,
            false,
            &upstream_session,
        );

        assert!(rewritten.contains("Host: 127.0.0.1:3080"));
        assert!(rewritten.contains("Origin: http://127.0.0.1:3080"));
        assert!(rewritten.contains("Cookie: dsh-auth-test=runtime-secret"));
        assert!(!rewritten.contains("himind_ai_session"));
    }

    #[test]
    fn non_entry_assets_keep_browser_compression_negotiation() {
        let upstream_session = UpstreamSession {
            authority: "127.0.0.1:3080".to_string(),
            cookie: "dsh-auth-test=runtime-secret".to_string(),
        };
        let rewritten = rewrite_request_header(
            "GET /plugins/??client.js HTTP/1.1\r\nHost: localhost:4567\r\nAccept-Encoding: gzip, br\r\nConnection: keep-alive\r\n\r\n",
            false,
            false,
            &upstream_session,
        );

        assert!(rewritten.contains("Accept-Encoding: gzip, br"));
        assert!(rewritten.contains("Connection: close"));
    }

    #[test]
    fn internal_session_query_is_removed_without_rewriting_other_queries() {
        assert_eq!(
            strip_session_query("/api/events.mux?after=42&himind_session=secret"),
            "/api/events.mux?after=42"
        );
        assert_eq!(
            strip_session_query("/plugins/??client.js"),
            "/plugins/??client.js"
        );
        assert_eq!(strip_session_query("/?himind_session=secret"), "/");
    }

    #[test]
    fn launch_token_exchange_returns_the_upstream_runtime_cookie() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_header(&mut stream).unwrap();
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("GET /?token=test-token HTTP/1.1"));
            assert!(request.contains(&format!("Host: 127.0.0.1:{}", address.port())));
            stream
                .write_all(
                    b"HTTP/1.1 303 See Other\r\nSet-Cookie: dsh-auth-test=runtime-secret; Path=/; HttpOnly\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let cookie = establish_upstream_session(
            address,
            &format!("127.0.0.1:{}", address.port()),
            "/?token=test-token",
        )
        .unwrap();
        assert_eq!(cookie, "dsh-auth-test=runtime-secret");
        server.join().unwrap();
    }

    #[test]
    fn proxy_handshakes_with_the_runtime_before_serving_the_browser() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = upstream_listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut auth, _) = upstream_listener.accept().unwrap();
            let request =
                String::from_utf8_lossy(&read_http_header(&mut auth).unwrap()).to_string();
            assert!(request.starts_with("GET /?token=test-token HTTP/1.1"));
            auth.write_all(
                b"HTTP/1.1 303 See Other\r\nSet-Cookie: dsh-auth-test=runtime-secret; Path=/; HttpOnly\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();

            for _ in 0..2 {
                let (mut entry, _) = upstream_listener.accept().unwrap();
                let request =
                    String::from_utf8_lossy(&read_http_header(&mut entry).unwrap()).to_string();
                assert!(request.starts_with("GET / HTTP/1.1"));
                assert!(!request.contains(SESSION_QUERY));
                assert!(!request.to_ascii_lowercase().contains("x-himind-session"));
                assert!(request.contains(&format!("Host: 127.0.0.1:{}", upstream.port())));
                assert!(request.contains("Cookie: dsh-auth-test=runtime-secret"));
                let body =
                    "<html><head><script>window.__ModuleLoader__={}</script><title>DeepSeek Harness</title></head><body>ready</body></html>";
                let (first, second) = body.split_at(body.len() / 2);
                entry
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
                            first.len(),
                            second.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });

        let mut proxy = BuiltinAiProxy::start(
            &format!("http://127.0.0.1:{}/?token=test-token", upstream.port()),
            None,
        )
        .unwrap();
        proxy.control().verify_browser_entry().unwrap();
        let proxy_url = url::Url::parse(proxy.url()).unwrap();
        let proxy_address = format!("127.0.0.1:{}", proxy_url.port().expect("proxy URL port"));
        let proxy_token = proxy_url
            .query_pairs()
            .find(|(name, _)| name == SESSION_QUERY)
            .map(|(_, value)| value.into_owned())
            .unwrap();

        let mut client = TcpStream::connect(&proxy_address).unwrap();
        client
            .write_all(
                format!(
                    "GET /?{SESSION_QUERY}={proxy_token} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
                    proxy_url.port().unwrap()
                )
                .as_bytes(),
            )
            .unwrap();
        let exchange =
            String::from_utf8_lossy(&read_complete_http_response(&mut client).unwrap()).to_string();
        assert!(exchange.starts_with("HTTP/1.1 200 OK"));
        assert!(exchange.contains("<title>HiMind AI</title>"));
        assert!(exchange.contains("data-himind-runtime-auth"));
        assert!(exchange.contains("name=\"referrer\" content=\"same-origin\""));
        assert!(!exchange.to_ascii_lowercase().contains("set-cookie"));
        proxy.stop();
        server.join().unwrap();
    }

    #[test]
    fn runtime_auth_bridge_is_injected_before_runtime_scripts() {
        let html =
            "<html><head><script>window.__ModuleLoader__={}</script></head><body></body></html>";
        let customized = customize_runtime_html(html);

        assert!(customized.contains("data-himind-runtime-auth"));
        assert!(customized.contains(RUNTIME_REFERRER_POLICY));
        assert!(
            customized.find("data-himind-runtime-auth").unwrap()
                < customized.find("window.__ModuleLoader__").unwrap()
        );
        assert!(customized.contains("X-HiMind-Session"));
        assert!(customized.contains("NativeWebSocket"));
    }

    #[test]
    fn runtime_entry_html_is_rebranded_without_changing_runtime_assets() {
        let html = "<html><head><title>DeepSeek Harness</title></head><body></body></html>";
        let customized = customize_runtime_html(html);

        assert!(customized.contains("<title>HiMind AI</title>"));
        assert!(customized.contains("data-himind-runtime-brand"));
        assert!(customized.contains("svg[viewBox=\"0 0 182 24\"]"));
        assert!(customized.contains("MutationObserver"));
    }

    #[test]
    fn runtime_brand_bridge_does_not_rewrite_model_names() {
        assert!(RUNTIME_BRAND_BRIDGE.contains("[/DeepSeek Harness/gi, 'HiMind AI']"));
        assert!(!RUNTIME_BRAND_BRIDGE.contains("[/\\bdeepseek\\b/gi, 'HiMind']"));
        assert!(!RUNTIME_BRAND_BRIDGE.contains("deepseek-v4-flash"));
    }

    #[test]
    fn model_profile_entry_preserves_the_real_id_as_display_name() {
        let entry = model_profile_entry(" deepseek-v4-flash ");
        assert_eq!(
            entry.get("id").and_then(Value::as_str),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            entry.get("name").and_then(Value::as_str),
            Some("deepseek-v4-flash")
        );
        assert!(!entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap()
            .to_ascii_lowercase()
            .contains("himind"));
    }

    #[test]
    fn runtime_result_value_accepts_success_and_preserves_payload() {
        let response = json!({
            "result": {
                "ok": true,
                "value": {"workspace": {"workspaceId": "workspace-1"}}
            }
        });

        let value = runtime_result_value(&response, "注册 DSH 工作区").unwrap();
        assert_eq!(value["workspace"]["workspaceId"], "workspace-1");
    }

    #[test]
    fn runtime_result_value_reports_dsh_business_error() {
        let response = json!({
            "result": {
                "ok": false,
                "error": {"code": "workspace-invalid-path", "message": "path is invalid"}
            }
        });

        let error = runtime_result_value(&response, "注册 DSH 工作区").unwrap_err();
        assert!(error.contains("workspace-invalid-path"));
        assert!(error.contains("path is invalid"));
    }

    #[test]
    fn existing_himind_model_is_not_replaced_by_service_default() {
        assert!(!should_initialize_managed_model(
            "himind-proxy",
            "deepseek-v4-flash"
        ));
        assert!(should_initialize_managed_model("himind-proxy", ""));
        assert!(should_initialize_managed_model("", ""));
        assert!(should_initialize_managed_model(
            "deepseek-official",
            "deepseek-chat"
        ));
        assert!(!should_initialize_managed_model(
            "personal-deepseek",
            "deepseek-chat"
        ));
    }

    #[test]
    fn chunked_html_response_is_decoded_and_rewritten_with_content_length() {
        let body = "<html><head><title>DeepSeek Harness</title></head></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        );
        let rewritten = customize_runtime_html_response(response.as_bytes())
            .unwrap()
            .expect("HTML response should be customized");
        let rewritten = String::from_utf8(rewritten).unwrap();

        assert!(rewritten.contains("Content-Length:"));
        assert!(!rewritten.to_ascii_lowercase().contains("transfer-encoding"));
        assert!(rewritten.contains("data-himind-runtime-brand"));
    }
}
