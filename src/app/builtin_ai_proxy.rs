use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const HEADER_LIMIT: usize = 64 * 1024;
const OBSERVED_FRAME_LIMIT: u64 = 4 * 1024 * 1024;
const SESSION_QUERY: &str = "himind_session";
const SESSION_COOKIE: &str = "himind_ai_session";

pub(crate) type EventObserver = Arc<dyn Fn(Value) + Send + Sync + 'static>;

pub(crate) struct BuiltinAiProxy {
    url: String,
    shutdown: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

impl BuiltinAiProxy {
    pub(crate) fn start(
        upstream_url: &str,
        observer: Option<EventObserver>,
    ) -> Result<Self, String> {
        let upstream = parse_upstream(upstream_url)?;
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
        let url = format!("http://127.0.0.1:{port}/?{SESSION_QUERY}={token}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_token = token.clone();
        let listener_thread = thread::spawn(move || {
            while !listener_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let connection_shutdown = Arc::clone(&listener_shutdown);
                        let connection_token = listener_token.clone();
                        let connection_observer = observer.clone();
                        thread::spawn(move || {
                            if let Err(error) = handle_connection(
                                stream,
                                upstream,
                                &connection_token,
                                connection_shutdown,
                                connection_observer,
                            ) {
                                if error.kind() != io::ErrorKind::ConnectionReset
                                    && error.kind() != io::ErrorKind::BrokenPipe
                                {
                                    eprintln!("HiMind AI 本机入口连接已关闭：{error}");
                                }
                            }
                        });
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
        });
        Ok(Self {
            url,
            shutdown,
            listener: Some(listener_thread),
        })
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

impl Drop for BuiltinAiProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse_upstream(value: &str) -> Result<SocketAddr, String> {
    let parsed = url::Url::parse(value).map_err(|_| "HiMind AI 地址无效".to_string())?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_none()
    {
        return Err("HiMind AI 地址不是本机安全地址".to_string());
    }
    Ok(SocketAddr::from((
        [127, 0, 0, 1],
        parsed.port().expect("validated port"),
    )))
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
    if query_token_matches(&request, token) {
        return establish_browser_session(&mut client, token);
    }
    if !cookie_token_matches(&request, token) {
        return write_forbidden(&mut client);
    }

    let websocket = is_websocket_upgrade(&request);
    let rewritten = rewrite_request_header(&request, websocket);
    let mut server = TcpStream::connect_timeout(&upstream, Duration::from_secs(5))?;
    configure_stream(&server)?;
    server.write_all(rewritten.as_bytes())?;
    server.write_all(remainder)?;

    let mut client_reader = client.try_clone()?;
    let mut server_writer = server.try_clone()?;
    let upload_shutdown = Arc::clone(&shutdown);
    let upload = thread::spawn(move || {
        copy_until_shutdown(
            &mut client_reader,
            &mut server_writer,
            &upload_shutdown,
            None,
        )
    });

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

fn is_websocket_upgrade(request: &str) -> bool {
    request.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket")
        })
    })
}

fn rewrite_request_header(request: &str, websocket: bool) -> String {
    let mut lines = request.lines();
    let mut output = String::new();
    if let Some(line) = lines.next() {
        output.push_str(line);
        output.push_str("\r\n");
    }
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("sec-websocket-extensions")
            || (!websocket && name.eq_ignore_ascii_case("connection"))
        {
            continue;
        }
        if name.eq_ignore_ascii_case("cookie") {
            let cookies = value
                .split(';')
                .map(str::trim)
                .filter(|item| {
                    item.split_once('=')
                        .is_none_or(|(cookie_name, _)| cookie_name != SESSION_COOKIE)
                })
                .collect::<Vec<_>>();
            if cookies.is_empty() {
                continue;
            }
            output.push_str("Cookie: ");
            output.push_str(&cookies.join("; "));
            output.push_str("\r\n");
            continue;
        }
        output.push_str(name);
        output.push(':');
        output.push_str(value);
        output.push_str("\r\n");
    }
    if !websocket {
        output.push_str("Connection: close\r\n");
    }
    output.push_str("\r\n");
    output
}

fn establish_browser_session(stream: &mut TcpStream, token: &str) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: /\r\nSet-Cookie: {SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes())
}

fn write_forbidden(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: 9\r\nConnection: close\r\n\r\nforbidden",
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
            parse_upstream("http://127.0.0.1:3080").unwrap().port(),
            3080
        );
        assert!(parse_upstream("http://localhost:3080").is_err());
        assert!(parse_upstream("https://127.0.0.1:3080").is_err());
        assert!(parse_upstream("http://127.0.0.1").is_err());
    }

    #[test]
    fn session_token_is_accepted_only_from_query_or_cookie() {
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
        let rewritten = rewrite_request_header(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: himind_ai_session=secret; theme=dark\r\nConnection: keep-alive\r\n\r\n",
            false,
        );
        assert!(!rewritten.contains("secret"));
        assert!(rewritten.contains("Cookie: theme=dark"));
        assert!(rewritten.contains("Connection: close"));
    }
}
