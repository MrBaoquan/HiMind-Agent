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
    Arc, RwLock,
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
// The browser entry keeps one loopback port per DSH home. DSH remembers the
// rail view — which Workspace groups are open, and per-account ordering — in
// browser storage keyed by page origin, so an entry that moves to a fresh
// ephemeral port on every launch resets that memory: the user's Session
// records come back folded away and look missing. A launch-stable port keeps
// one origin per home; a port that is already taken falls back to an ephemeral
// one, which the first-paint preset below repairs.
const BROWSER_PORT_BASE: u16 = 21_600;
const BROWSER_PORT_SPAN: u16 = 1_200;
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
  // The runtime opens its Remote mux over `ws://` while the page itself is
  // served over `http://`, so a plain `origin` comparison treats the socket
  // that carries every Session record as cross-site and leaves it unauthorised.
  // Compare the site instead, and keep the socket scheme untouched.
  const sameSite = (url) => {
    const scheme = url.protocol === "ws:" ? "http:" : url.protocol === "wss:" ? "https:" : url.protocol;
    return scheme + "//" + url.host === location.origin;
  };
  const withSession = (value) => {
    try {
      const url = value instanceof URL ? new URL(value.href) : new URL(String(value), location.href);
      if (sameSite(url) && !url.searchParams.has("himind_session")) {
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

/// Build the first-paint Workspace rail preset.
///
/// The rail is the only place DSH lists Sessions, and it remembers which
/// Workspace groups are open per browser origin. HiMind Agent serves the
/// runtime from a fresh loopback port on every launch, so that memory is
/// always empty and every group starts collapsed; existing Sessions then look
/// like missing records. Opening the known groups before the runtime scripts
/// boot keeps real records visible on the first paint.
///
/// A stored choice is never overwritten: the preset only fills in groups the
/// browser has not decided about yet, and every other field of the stored
/// view — including fields written by a newer runtime — is carried over
/// untouched.
///
/// The stored view is not optional: the runtime reads `groupBy` and `orderBy`
/// out of it, and a view object that lacks them leaves the rail unpainted
/// instead of falling back to defaults. The defaults below are the exact
/// values the runtime itself persists for a fresh origin.
///
/// `session_id` is the Session this launch adopted for the entry's own
/// Workspace. It is seeded into the runtime's current-Session slot only when
/// that slot is still empty, so the entry opens on its own project instead of
/// on whichever Workspace happens to have been touched last.
fn rail_view_preset_script(workspace_ids: &[String], session_id: Option<&str>) -> String {
    let keys = serde_json::to_string(workspace_ids).unwrap_or_else(|_| "[]".to_string());
    let session = serde_json::to_string(&session_id).unwrap_or_else(|_| "null".to_string());
    format!(
        r#"<script data-himind-rail-view>
(() => {{
  const PREFIX = "dsh.workspace.view.v";
  const KNOWN_KEY = "dsh.workspace.view.v5";
  const CURRENT_KEY = "dsh.sessions.current";
  const KEYS = {keys};
  const SESSION_ID = {session};
  const isObject = (value) => value !== null && typeof value === "object";
  const asObject = (value) => isObject(value) ? value : {{}};
  const opened = (previous) => {{
    const expansion = asObject(previous.groupExpansion);
    for (const key of KEYS) {{
      if (!Object.prototype.hasOwnProperty.call(expansion, key)) expansion[key] = true;
    }}
    return expansion;
  }};
  // An entry opened for a project must land in that project. The runtime only
  // guesses a Workspace (the most recently updated one) when the browser has
  // not recorded a Session yet, and that guess is shared by every entry, so
  // two entries opened for two projects would race for the same one. Seeding
  // the Session this launch adopted pins the entry to its own Workspace.
  // A browser that already chose is never overridden: the user's pick, however
  // it was made, outranks the preset.
  try {{
    if (SESSION_ID !== null && localStorage.getItem(CURRENT_KEY) === null) {{
      localStorage.setItem(CURRENT_KEY, JSON.stringify({{ sessionId: SESSION_ID }}));
    }}
  }} catch {{
    // Storage can be blocked. The runtime then keeps its own defaults.
  }}
  try {{
    const viewKeys = new Set();
    for (let index = 0; index < localStorage.length; index += 1) {{
      const name = localStorage.key(index);
      if (typeof name === "string" && name.indexOf(PREFIX) === 0) viewKeys.add(name);
    }}
    viewKeys.add(KNOWN_KEY);
    for (const viewKey of viewKeys) {{
      const raw = localStorage.getItem(viewKey);
      // Only the schema this build was verified against may be created from
      // scratch. An unknown newer store is left alone unless it already
      // exists, so the preset can never hand the runtime a shape it rejects.
      if (raw === null && viewKey !== KNOWN_KEY) continue;
      const previous = raw === null ? {{}} : asObject(JSON.parse(raw));
      const next = Object.assign({{}}, previous);
      if (typeof next.groupBy !== "string") next.groupBy = "workspace";
      if (typeof next.orderBy !== "string") next.orderBy = "updated";
      if (!isObject(next.sessionOrderByAccount)) next.sessionOrderByAccount = {{}};
      if (!isObject(next.sessionUpdatedAtByAccount)) next.sessionUpdatedAtByAccount = {{}};
      next.groupExpansion = opened(previous);
      localStorage.setItem(viewKey, JSON.stringify(next));
    }}
  }} catch {{
    // Storage can be blocked or hold a foreign value. The runtime then keeps
    // its own defaults, so there is nothing to repair here.
  }}
}})();
</script>"#
    )
}

/// Collect every Workspace id the runtime has already recorded in one home.
///
/// The rail groups by Workspace membership, so these are exactly the group
/// keys the browser must open for recorded Sessions to be visible.
fn rail_workspace_ids(home: &Path) -> Vec<String> {
    let path = home.join("storages").join("workspace.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    value
        .get("global")
        .and_then(|global| global.get("workspaceIds"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) type EventObserver = Arc<dyn Fn(Value) + Send + Sync + 'static>;

/// The loopback port this home's browser entry prefers, derived from the DSH
/// home path so the same home keeps the same origin across launches and
/// different homes on one machine stay apart.
fn preferred_browser_port(origin_key: Option<&str>) -> Option<u16> {
    let key = origin_key?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(BROWSER_PORT_BASE + (hash % u64::from(BROWSER_PORT_SPAN)) as u16)
}

/// Bind the browser entry, preferring the home's stable port and degrading to
/// an ephemeral one when that port is already in use.
fn bind_browser_listener(origin_key: Option<&str>) -> Result<TcpListener, String> {
    if let Some(port) = preferred_browser_port(origin_key) {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok(listener);
        }
    }
    TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("无法创建 HiMind AI 本机入口：{error}"))
}

pub(crate) struct BuiltinAiProxy {
    url: String,
    shutdown: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    rail_view_preset: Arc<RwLock<Option<String>>>,
}

#[derive(Clone, Debug)]
struct UpstreamSession {
    authority: String,
    cookie: String,
}

#[derive(Clone)]
pub(crate) struct BuiltinAiProxyControl {
    url: String,
    rail_view_preset: Arc<RwLock<Option<String>>>,
}

impl BuiltinAiProxy {
    pub(crate) fn start(
        upstream_url: &str,
        observer: Option<EventObserver>,
        origin_key: Option<&str>,
    ) -> Result<Self, String> {
        let (upstream, upstream_session) = prepare_upstream(upstream_url)?;
        let listener = bind_browser_listener(origin_key)?;
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
        let rail_view_preset = Arc::new(RwLock::new(None::<String>));
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_token = token.clone();
        let listener_active_connections = Arc::clone(&active_connections);
        let listener_rail_view_preset = Arc::clone(&rail_view_preset);
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
                            let connection_rail_view_preset =
                                Arc::clone(&listener_rail_view_preset);
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
                                        connection_rail_view_preset,
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
            rail_view_preset,
        })
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn control(&self) -> BuiltinAiProxyControl {
        BuiltinAiProxyControl {
            url: self.url.clone(),
            rail_view_preset: Arc::clone(&self.rail_view_preset),
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

    /// Open the recorded Workspace groups before the runtime boots, and claim a
    /// Session row when this entry belongs to a project directory.
    ///
    /// The preset is published for every entry: it is the only thing that makes
    /// recorded Sessions visible on the first paint, because the rail remembers
    /// expansion per browser origin and DSH serves from a fresh loopback port
    /// on every launch.
    ///
    /// Registering the directory as a named Workspace — and giving that group a
    /// Session — is for the entries that were opened *for* a project, where the
    /// user expects to land in that project. The plain HiMind AI entry leaves
    /// the registry alone: its directory is only a launch default, and turning
    /// it into a group would grow the rail with a row that holds no record.
    ///
    /// Grouping is presentation only — the Session itself already runs in the
    /// requested directory — so a failure is reported as a degraded rail rather
    /// than taken as a reason to refuse the entry.
    pub(crate) fn prepare_rail(
        &self,
        home: &Path,
        workspace: &Path,
        adopt_session: bool,
    ) -> Result<Vec<String>, String> {
        let registered = match adopt_session {
            true => self.register_workspace(workspace).map(Some),
            false => Ok(None),
        };
        let mut opened = Vec::new();
        if let Ok(Some(workspace_id)) = &registered {
            opened.push(workspace_id.clone());
        }
        let adopted = match &registered {
            Ok(Some(workspace_id)) => self.adopt_rail_session(workspace_id, workspace).map(Some),
            _ => Ok(None),
        };
        self.set_rail_view_preset(
            home,
            &opened,
            adopted.as_ref().ok().and_then(Option::as_deref),
        );
        match (registered, adopted) {
            (Err(error), _) => Err(format!("HiMind AI 工作目录未进入分组：{error}")),
            (_, Err(error)) => Err(format!("HiMind AI 项目会话未就绪：{error}")),
            (Ok(_), Ok(_)) => Ok(opened),
        }
    }

    /// Give one registered Workspace a current Session row.
    ///
    /// Reuse a recorded unused Session when the runtime reports one: adopting
    /// it keeps the group current without growing the rail by one abandoned row
    /// per launch. Any rejection (a Session that is live elsewhere, for
    /// example) falls back to a fresh row instead of failing the launch.
    ///
    /// The adopted Session id is returned so the entry can also be pinned to
    /// that row on its first paint.
    fn adopt_rail_session(&self, workspace_id: &str, workspace: &Path) -> Result<String, String> {
        if let Some(session_id) = self.idle_rail_session(workspace).unwrap_or_default() {
            if let Ok(adopted) = self.create_workspace_session(workspace_id, Some(&session_id)) {
                return Ok(adopted);
            }
        }
        self.create_workspace_session(workspace_id, None)
    }

    /// Publish the Workspace groups that must be open on the next page load.
    ///
    /// Every recorded Workspace in `home` is included, plus `workspace_ids`
    /// for groups this launch just created, plus the runtime's Ungrouped
    /// bucket — Sessions whose directory was never registered only live there.
    /// The rail itself remembers expansion per browser origin, and this
    /// embedded runtime serves from a fresh loopback port on every launch, so
    /// without this preset a returning user is greeted by closed groups and
    /// their recorded Sessions look missing.
    ///
    /// `session_id` pins this entry to the Session adopted for its own
    /// Workspace, which is what keeps concurrent entries from landing in each
    /// other's project.
    pub(crate) fn set_rail_view_preset(
        &self,
        home: &Path,
        workspace_ids: &[String],
        session_id: Option<&str>,
    ) {
        let mut keys: Vec<String> = vec![String::new()];
        for workspace_id in rail_workspace_ids(home).iter().chain(workspace_ids.iter()) {
            if !workspace_id.is_empty() && !keys.iter().any(|key| key == workspace_id) {
                keys.push(workspace_id.clone());
            }
        }
        if let Ok(mut preset) = self.rail_view_preset.write() {
            *preset = Some(rail_view_preset_script(&keys, session_id));
        }
    }

    /// Register (idempotently) one directory as a native DSH Workspace.
    ///
    /// `workspace/create` is the only way an external client can put a Session
    /// into a named Workspace group: DSH groups the browser rail by Workspace
    /// membership and files everything else under its collapsed Ungrouped
    /// bucket, which is why an unregistered Session looks like a missing record.
    pub(crate) fn register_workspace(&self, workspace: &Path) -> Result<String, String> {
        let workspace_path = crate::extension_workspace::display_path(workspace);
        let response = self.call_runtime_api(
            "workspace/create",
            json!({ "request": { "path": workspace_path } }),
        )?;
        let workspace_value = runtime_result_value(&response, "注册 DSH 工作区")?;
        workspace_value
            .get("workspace")
            .and_then(|workspace| workspace.get("workspaceId"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| "DSH 工作区响应缺少 workspaceId".to_string())
    }

    /// Create — or idempotently adopt — a Session inside a Workspace so DSH
    /// selects that Workspace group on load instead of leaving the Session in
    /// Ungrouped.
    pub(crate) fn create_workspace_session(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
    ) -> Result<String, String> {
        let mut request = json!({ "workspaceId": workspace_id });
        if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
            request["sessionId"] = json!(session_id);
        }
        let response = self.call_runtime_api("session/create", json!({ "request": request }))?;
        let session_value = runtime_result_value(&response, "创建 DSH 项目会话")?;
        session_value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| "DSH 项目会话响应缺少 sessionId".to_string())
    }

    /// Find the Session the rail should reuse for one directory: the newest
    /// unused ("blank") Session DSH already recorded there, if any.
    ///
    /// Adopting it keeps the group current without growing the rail by one
    /// abandoned row per launch.
    fn idle_rail_session(&self, workspace: &Path) -> Result<Option<String>, String> {
        let directory = crate::extension_workspace::display_path(workspace);
        let response = self.call_runtime_api("session/list", json!({ "_request": {} }))?;
        let value = runtime_result_value(&response, "读取 DSH 会话列表")?;
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut newest: Option<(u64, String)> = None;
        for item in items {
            if item.get("blank").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let matches_directory = item
                .get("cwd")
                .and_then(Value::as_str)
                .is_some_and(|cwd| cwd.eq_ignore_ascii_case(&directory));
            if !matches_directory {
                continue;
            }
            let Some(session_id) = item.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            let updated_at = item.get("updatedAt").and_then(Value::as_u64).unwrap_or(0);
            if newest.as_ref().is_none_or(|(best, _)| updated_at > *best) {
                newest = Some((updated_at, session_id.to_string()));
            }
        }
        Ok(newest.map(|(_, session_id)| session_id))
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
        let described = self.call_api(&client, "settings/describe", json!({}))?;
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

    /// Call one DSH Typert Remote method over the authenticated local carrier.
    ///
    /// DSH publishes a Remote method as the literal `/api/<namespace>/<method>`
    /// endpoint and requires the wire payload to hold exactly one plain-object
    /// `args` field whose keys match that method's descriptor. Callers therefore
    /// pass the endpoint verbatim together with that method's named arguments.
    fn call_api(&self, client: &Client, endpoint: &str, args: Value) -> Result<Value, String> {
        let mut url =
            url::Url::parse(&self.url).map_err(|_| "HiMind AI 本机地址无效".to_string())?;
        let session = url
            .query_pairs()
            .find(|(name, _)| name == SESSION_QUERY)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "HiMind AI 本机会话令牌不可用".to_string())?;
        url.set_path(&format!("/api/{endpoint}"));
        url.set_query(None);
        let request = json!({
            "type": "client-request",
            "rpcId": next_rpc_id("himind-sync"),
            "method": endpoint,
            "payload": { "args": args },
        });
        let response = client
            .post(url)
            // WebView2 accepts the Secure localhost cookie. Reqwest follows
            // standard HTTP cookie rules, so carry the short-lived local
            // session explicitly for the Agent-to-proxy control request.
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}={session}"),
            )
            .json(&request)
            .send()
            .map_err(|error| format!("DSH {endpoint} 请求失败: {error}"))?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .map_err(|error| format!("DSH {endpoint} 响应无效: {error}"))?;
        if !status.is_success() {
            return Err(format!("DSH {endpoint} 返回 HTTP {status}"));
        }
        Ok(body)
    }

    /// Send a control request through the authenticated local DSH carrier.
    /// Runtime endpoint names are intentionally kept at the gateway boundary;
    /// this method only owns transport/session-cookie handling.
    pub(crate) fn call_runtime_api(&self, endpoint: &str, args: Value) -> Result<Value, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| format!("创建 DSH 控制客户端失败: {error}"))?;
        self.call_api(&client, endpoint, args)
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
    pub(crate) fn probe_runtime_api(&self, endpoint: &str, args: Value) -> Result<Value, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| format!("创建 DSH 能力探测客户端失败: {error}"))?;
        self.call_api(&client, endpoint, args)
    }

    fn mutate_settings(
        &self,
        client: &Client,
        namespace: &str,
        ops: Vec<Value>,
        revision: Option<i64>,
    ) -> Result<(), String> {
        let mut args = json!({ "ns": namespace, "ops": ops });
        if let Some(revision) = revision {
            args["expectedRevision"] = json!(revision);
        }
        let response = self.call_api(client, "settings/mutate", args)?;
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

pub(crate) fn runtime_result_value<'a>(
    response: &'a Value,
    operation: &str,
) -> Result<&'a Value, String> {
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

pub(crate) fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// DSH correlates each client-request by `rpcId`; two control requests issued
/// in the same millisecond must not share one id.
fn next_rpc_id(prefix: &str) -> String {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{sequence}", unix_millis())
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
    rail_view_preset: Arc<RwLock<Option<String>>>,
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
        let view_preset = rail_view_preset
            .read()
            .ok()
            .and_then(|preset| preset.clone());
        return proxy_customized_runtime_entry(
            &mut server,
            &mut client,
            token,
            view_preset.as_deref(),
        );
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
    token: &str,
    rail_view_preset: Option<&str>,
) -> io::Result<()> {
    server.set_read_timeout(Some(Duration::from_secs(5)))?;
    let response = read_complete_http_response(server)?;
    let Some(customized) = customize_runtime_html_response(&response, token, rail_view_preset)?
    else {
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

fn customize_runtime_html_response(
    response: &[u8],
    token: &str,
    rail_view_preset: Option<&str>,
) -> io::Result<Option<Vec<u8>>> {
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
    let customized = customize_runtime_html(&html, rail_view_preset);
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
    // Hand the browser the same short-lived carrier every later request needs.
    // The injected bridge covers script-owned traffic; the cookie also covers
    // what the bridge cannot reach, so the runtime stays authenticated even if
    // a live view is created outside the page's own realm.
    output.push_str(&format!(
        "Set-Cookie: {SESSION_COOKIE}={token}; Path=/; SameSite=Strict; HttpOnly\r\n"
    ));
    output.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
    let mut bytes = output.into_bytes();
    bytes.extend_from_slice(customized.as_bytes());
    Ok(Some(bytes))
}

fn customize_runtime_html(html: &str, rail_view_preset: Option<&str>) -> String {
    let mut html = html.replace(
        "<title>DeepSeek Harness</title>",
        "<title>HiMind AI</title>",
    );
    if let Some(preset) = rail_view_preset.filter(|preset| !preset.is_empty()) {
        if !html.contains("data-himind-rail-view") {
            inject_head_snippet(&mut html, preset);
        }
    }
    if !html.contains("data-himind-runtime-auth") {
        let bridge = format!("{RUNTIME_REFERRER_POLICY}{RUNTIME_AUTH_BRIDGE}");
        inject_head_snippet(&mut html, &bridge);
    }
    if !html.contains("data-himind-runtime-brand") {
        html = html.replacen("</head>", &format!("{RUNTIME_BRAND_BRIDGE}\n</head>"), 1);
    }
    html
}

/// Insert a bridge snippet ahead of the first runtime script so it runs before
/// any module code, falling back to the start of `<head>` and then of the page.
fn inject_head_snippet(html: &mut String, snippet: &str) {
    let lowercase = html.to_ascii_lowercase();
    let head_end = lowercase.find("</head>");
    let insertion = lowercase
        .find("<script")
        .filter(|script| head_end.is_none_or(|head_end| *script < head_end))
        .or(head_end)
        .or_else(|| lowercase.find("<head>").map(|index| index + "<head>".len()));
    match insertion {
        Some(index) => html.insert_str(index, snippet),
        None => html.insert_str(0, snippet),
    }
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
        // The runtime's own cookie stays between the proxy and the runtime; the
        // browser only ever receives the proxy's short-lived carrier.
        assert!(!exchange.contains("dsh-auth-test"));
        assert!(exchange.contains(&format!("Set-Cookie: {SESSION_COOKIE}={proxy_token}")));
        proxy.stop();
        server.join().unwrap();
    }

    #[test]
    fn rail_view_preset_reaches_the_browser_entry_before_the_runtime_boots() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = upstream_listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut auth, _) = upstream_listener.accept().unwrap();
            let _ = read_http_header(&mut auth).unwrap();
            auth.write_all(
                b"HTTP/1.1 303 See Other\r\nSet-Cookie: dsh-auth-test=runtime-secret; Path=/; HttpOnly\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            for _ in 0..2 {
                let (mut entry, _) = upstream_listener.accept().unwrap();
                let _ = read_http_header(&mut entry).unwrap();
                let body = "<html><head><script>window.__ModuleLoader__={}</script></head><body>ready</body></html>";
                entry
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });

        // The preset has to come from both sources: the Workspaces the runtime
        // already recorded, and the one this launch just registered.
        let home = std::env::temp_dir().join(format!("himind-rail-entry-{}", std::process::id()));
        std::fs::create_dir_all(home.join("storages")).unwrap();
        std::fs::write(
            home.join("storages").join("workspace.json"),
            r#"{"global":{"workspaceIds":["recorded-alpha"]},"tables":{}}"#,
        )
        .unwrap();

        let mut proxy = BuiltinAiProxy::start(
            &format!("http://127.0.0.1:{}/?token=test-token", upstream.port()),
            None,
            None,
        )
        .unwrap();
        proxy
            .control()
            .set_rail_view_preset(&home, &["launched-beta".to_string()], None);
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
        assert!(exchange.contains("data-himind-rail-view"));
        assert!(exchange.contains("recorded-alpha"));
        assert!(exchange.contains("launched-beta"));
        // The Ungrouped bucket is an empty group key and must survive encoding.
        assert!(exchange.contains("\"\""));
        assert!(
            exchange.find("data-himind-rail-view").unwrap()
                < exchange.find("window.__ModuleLoader__").unwrap(),
            "the rail preset must be in place before the runtime reads its view store"
        );

        proxy.stop();
        server.join().unwrap();
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn runtime_auth_bridge_is_injected_before_runtime_scripts() {
        let html =
            "<html><head><script>window.__ModuleLoader__={}</script></head><body></body></html>";
        let customized = customize_runtime_html(html, None);

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
    fn runtime_auth_bridge_authorises_the_socket_the_page_already_owns() {
        // The Remote mux — the stream that carries every Session record — is
        // opened as `ws://` from an `http://` page. Comparing bare origins
        // classifies that socket as cross-site and leaves it unauthorised, so
        // the rail stays on "reconnecting" with no records behind it.
        assert!(RUNTIME_AUTH_BRIDGE.contains(r#"url.protocol === "ws:" ? "http:""#));
        assert!(RUNTIME_AUTH_BRIDGE
            .contains("sameSite(url) && !url.searchParams.has(\"himind_session\")"));
    }

    #[test]
    fn runtime_entry_html_is_rebranded_without_changing_runtime_assets() {
        let html = "<html><head><title>DeepSeek Harness</title></head><body></body></html>";
        let customized = customize_runtime_html(html, None);

        assert!(customized.contains("<title>HiMind AI</title>"));
        assert!(customized.contains("data-himind-runtime-brand"));
        assert!(customized.contains("svg[viewBox=\"0 0 182 24\"]"));
        assert!(customized.contains("MutationObserver"));
    }

    #[test]
    fn the_browser_entry_keeps_one_origin_per_home() {
        // DSH remembers the rail view per page origin, so the same home must
        // land on the same loopback port on every launch; an entry that drifted
        // to a fresh port each time would fold every recorded Session away.
        let port = preferred_browser_port(Some("home-a")).expect("preferred port");
        assert_eq!(preferred_browser_port(Some("home-a")), Some(port));
        assert!(preferred_browser_port(None).is_none());
        assert!((BROWSER_PORT_BASE..BROWSER_PORT_BASE + BROWSER_PORT_SPAN).contains(&port));

        // A port already in use degrades to an ephemeral one instead of
        // failing the entry.
        let first = bind_browser_listener(Some("home-a")).unwrap();
        let second = bind_browser_listener(Some("home-a")).unwrap();
        assert_ne!(
            first.local_addr().unwrap().port(),
            second.local_addr().unwrap().port()
        );
    }

    #[test]
    fn rail_view_preset_opens_recorded_groups_before_runtime_scripts() {
        let html =
            "<html><head><script>window.__ModuleLoader__={}</script></head><body></body></html>";
        let workspace_ids = [
            String::new(),
            "17d2afb0-14d1-484a-a150-89d69ea2ed06".to_string(),
        ];
        let preset = rail_view_preset_script(&workspace_ids, None);
        let customized = customize_runtime_html(html, Some(&preset));

        assert!(customized.contains("data-himind-rail-view"));
        assert!(customized.contains("dsh.workspace.view.v5"));
        assert!(
            customized.find("data-himind-rail-view").unwrap()
                < customized.find("window.__ModuleLoader__").unwrap(),
            "the rail preset must run before the runtime boots"
        );
        // The Ungrouped bucket is an empty key, and it must survive encoding.
        assert!(customized.contains("\"\""));
        assert!(customized.contains("17d2afb0-14d1-484a-a150-89d69ea2ed06"));
    }

    #[test]
    fn rail_preset_keeps_the_groups_a_user_already_decided_about() {
        let keys = vec![
            String::new(),
            "workspace-a".to_string(),
            "workspace-b".to_string(),
        ];
        let preset = rail_view_preset_script(&keys, None);

        // Filling in only missing keys is what makes a stored collapse choice
        // survive the next launch, so the preset must not assign unconditionally.
        assert!(preset.contains("hasOwnProperty.call(expansion, key)"));
    }

    #[test]
    fn rail_preset_carries_the_stored_view_forward_untouched() {
        let preset = rail_view_preset_script(&[String::new(), "workspace-a".to_string()], None);

        // The runtime needs groupBy/orderBy present, so the preset must supply
        // the same defaults the runtime itself writes; everything else in a
        // stored view — including a field a newer runtime added — is copied
        // over rather than replaced.
        assert!(preset.contains("Object.assign({}, previous)"));
        assert!(preset.contains("next.orderBy = \"updated\""));
        assert!(preset.contains("next.groupExpansion = opened(previous)"));
        // A stored view from a newer runtime is repaired too, not only the
        // version this build was verified against.
        assert!(preset.contains("localStorage.key(index)"));
        assert!(preset.contains("if (raw === null && viewKey !== KNOWN_KEY) continue"));
    }

    #[test]
    fn rail_preset_pins_a_fresh_entry_to_its_own_session() {
        let preset = rail_view_preset_script(
            &[String::new(), "workspace-a".to_string()],
            Some("session-own"),
        );

        // The runtime picks the most recently updated Workspace when the
        // browser has no current Session, and every entry opened in a fresh
        // origin would then race for the same one. Seeding this entry's own
        // Session is what makes concurrent entries land in their own project.
        assert!(preset.contains("const CURRENT_KEY = \"dsh.sessions.current\""));
        assert!(preset.contains("const SESSION_ID = \"session-own\""));
        assert!(preset.contains("localStorage.getItem(CURRENT_KEY) === null"));
        assert!(preset.contains("JSON.stringify({ sessionId: SESSION_ID })"));
    }

    #[test]
    fn rail_preset_leaves_a_browser_that_already_chose_alone() {
        // No adopted Session (the plain HiMind AI entry) must not write the
        // slot at all, and the guard has to be on the stored value rather than
        // an unconditional write — a user who switched Sessions in this origin
        // keeps that choice on the next launch.
        let preset = rail_view_preset_script(&[String::new()], None);

        assert!(preset.contains("const SESSION_ID = null"));
        assert!(preset
            .contains("if (SESSION_ID !== null && localStorage.getItem(CURRENT_KEY) === null)"));
    }

    #[test]
    fn rail_workspace_ids_read_every_recorded_workspace() {
        let home = std::env::temp_dir().join(format!("himind-rail-home-{}", std::process::id()));
        let storages = home.join("storages");
        std::fs::create_dir_all(&storages).unwrap();
        std::fs::write(
            storages.join("workspace.json"),
            r#"{"global":{"workspaceIds":["alpha","beta"]},"tables":{}}"#,
        )
        .unwrap();

        assert_eq!(
            rail_workspace_ids(&home),
            vec!["alpha".to_string(), "beta".to_string()]
        );

        std::fs::write(storages.join("workspace.json"), "not json").unwrap();
        assert!(rail_workspace_ids(&home).is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Serve one real browser entry and hold it open so a browser can be
    /// pointed at it.
    ///
    /// Real runtime, real proxy, real first-paint preset — the only thing the
    /// caller supplies is the upstream DeepSeek Harness the Agent itself would
    /// have started. Not part of the suite: it exists so the fix can be checked
    /// against an installed runtime instead of a stand-in.
    #[test]
    #[ignore = "manual browser verification"]
    fn serve_rail_for_browser_verification() {
        let upstream = std::env::var("HIMIND_RAIL_UPSTREAM")
            .expect("set HIMIND_RAIL_UPSTREAM to the runtime URL including its launch token");
        let home = std::env::var("HIMIND_RAIL_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|_| crate::runtime::builtin::interactive_home_path())
            .expect("resolve the DSH home");
        let workspace = std::env::var("HIMIND_RAIL_WORKSPACE")
            .map(std::path::PathBuf::from)
            .expect("set HIMIND_RAIL_WORKSPACE");
        let hold = std::env::var("HIMIND_RAIL_HOLD_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(120);

        let origin_key = home.to_string_lossy().to_string();
        let proxy = BuiltinAiProxy::start(&upstream, None, Some(origin_key.as_str())).unwrap();
        println!("workspace-rail-url={}", proxy.url());
        println!(
            "workspace-rail-prepare={:?}",
            proxy.control().prepare_rail(&home, &workspace, false)
        );
        for second in 0..hold {
            if second % 10 == 0 {
                println!("workspace-rail-hold={second}");
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Print the exact first-paint preset this build injects so a real browser
    /// can be pointed at the real runtime with the real artifact instead of a
    /// hand-written approximation.
    ///
    /// Not part of the suite: it only exists for manual verification against an
    /// installed DeepSeek Harness.
    #[test]
    #[ignore = "manual browser verification"]
    fn dump_rail_view_preset_for_browser_verification() {
        let home = std::env::var("HIMIND_RAIL_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(
                    r"C:\Users\Administrator\AppData\Local\HiMindAgent\runtimes\deepseek-harness\homes\interactive",
                )
            });
        let mut keys: Vec<String> = vec![String::new()];
        for workspace_id in rail_workspace_ids(&home) {
            if !workspace_id.is_empty() && !keys.iter().any(|key| *key == workspace_id) {
                keys.push(workspace_id);
            }
        }
        let session_id = std::env::var("HIMIND_RAIL_SESSION_ID").ok();
        println!("{}", rail_view_preset_script(&keys, session_id.as_deref()));
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
        let rewritten = customize_runtime_html_response(response.as_bytes(), "test-token", None)
            .unwrap()
            .expect("HTML response should be customized");
        let rewritten = String::from_utf8(rewritten).unwrap();

        assert!(rewritten.contains("Content-Length:"));
        assert!(!rewritten.to_ascii_lowercase().contains("transfer-encoding"));
        assert!(rewritten.contains("data-himind-runtime-brand"));
        assert!(rewritten.contains("Set-Cookie: himind_ai_session=test-token"));
    }
}
