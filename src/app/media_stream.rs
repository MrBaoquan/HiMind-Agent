//! 插件媒体流：Agent 统一媒体端点。
//!
//! 插件进程是“每次能力调用一个短生命周期进程”，不能在插件内常驻回环服务。
//! 因此由 Agent 提供只读 Range 端点，插件用每台 Agent 独立的密钥签发短时票据：
//!
//!   ticket = base64url(payload) + "." + base64url(HMAC-SHA256(secret, payload))
//!   payload = {"p":"<绝对路径>","e":<过期秒>,"id":"<插件 ID>"}
//!
//! 端点只监听本机回环、只读、支持 GET/HEAD + Range，用于 <video>/<audio> 拖动。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOCAL_PORT: AtomicU16 = AtomicU16::new(0);
static SECRET: OnceLock<String> = OnceLock::new();

pub(crate) fn set_local_port(port: u16) {
    LOCAL_PORT.store(port, Ordering::Release);
}

pub(crate) fn base_url() -> Option<String> {
    let port = LOCAL_PORT.load(Ordering::Acquire);
    if port == 0 {
        None
    } else {
        Some(format!("http://127.0.0.1:{port}"))
    }
}

pub(crate) fn secret() -> &'static str {
    SECRET.get_or_init(|| {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    })
}

#[derive(Debug, Deserialize)]
pub(crate) struct Ticket {
    pub p: String,
    pub e: i64,
    #[serde(default)]
    pub id: String,
}

pub(crate) fn verify_ticket(token: &str) -> Result<Ticket, String> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| "invalid media ticket".to_string())?;
    if payload.is_empty() || signature.is_empty() {
        return Err("invalid media ticket".into());
    }
    let expected = URL_SAFE_NO_PAD.encode(hmac_sha256(secret().as_bytes(), payload.as_bytes()));
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err("invalid media ticket signature".into());
    }
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "invalid media ticket payload".to_string())?;
    let ticket: Ticket =
        serde_json::from_slice(&raw).map_err(|_| "invalid media ticket payload".to_string())?;
    if ticket.p.trim().is_empty() {
        return Err("media ticket path is empty".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    if ticket.e <= now {
        return Err("media ticket expired".into());
    }
    Ok(ticket)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let digest = outer.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&digest);
    result
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

enum RangeSpec {
    Full,
    Partial(u64, u64),
    Unsatisfiable,
}

// 单次响应最多发 4 MiB。播放器经常用 `bytes=0-` 探路，若照原样回整段
// 3GB，Chromium 会把整个 mdat 读完才拿到尾部 moov，首帧要等数秒；
// 截断后它会立刻改请求尾部 moov，起播稳定在亚秒级。
const MAX_RANGE_BYTES: u64 = 4 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 256 * 1024;

// 一段合成文件区间：客户端看到的偏移 vs 磁盘上的原始偏移。
// `data` 有值时（被改写的 moov）直接从这里发，不再读磁盘。
#[derive(Debug, Clone)]
struct Segment {
    syn_start: u64,
    len: u64,
    orig_start: u64,
}

struct FaststartLayout {
    segments: Vec<Segment>,
    moov: Vec<u8>,
    moov_syn_start: u64,
}

/// moov 里的 stco/co64 记录的是绝对文件偏移；把 moov 挪到 mdat 前面后，
/// mdat 整体后移了 moov 的长度，必须同步修正这些偏移，否则播放会跳帧/跳到结尾。
fn patch_chunk_offsets(buf: &mut [u8], delta: u64) {
    const CONTAINERS: [&[u8]; 8] = [
        b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta", b"dinf",
    ];
    let mut offset = 0usize;
    while offset + 8 <= buf.len() {
        let size = u32::from_be_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]) as usize;
        if size < 8 || offset + size > buf.len() {
            break;
        }
        let name = [
            buf[offset + 4],
            buf[offset + 5],
            buf[offset + 6],
            buf[offset + 7],
        ];
        if name == *b"stco" && size >= 16 {
            let count = u32::from_be_bytes([
                buf[offset + 12],
                buf[offset + 13],
                buf[offset + 14],
                buf[offset + 15],
            ]) as usize;
            for index in 0..count {
                let at = offset + 16 + index * 4;
                if at + 4 > offset + size {
                    break;
                }
                let value = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
                let shifted = (value as u64).saturating_add(delta).min(u32::MAX as u64) as u32;
                buf[at..at + 4].copy_from_slice(&shifted.to_be_bytes());
            }
        } else if name == *b"co64" && size >= 16 {
            let count = u32::from_be_bytes([
                buf[offset + 12],
                buf[offset + 13],
                buf[offset + 14],
                buf[offset + 15],
            ]) as usize;
            for index in 0..count {
                let at = offset + 16 + index * 8;
                if at + 8 > offset + size {
                    break;
                }
                let value = u64::from_be_bytes([
                    buf[at],
                    buf[at + 1],
                    buf[at + 2],
                    buf[at + 3],
                    buf[at + 4],
                    buf[at + 5],
                    buf[at + 6],
                    buf[at + 7],
                ]);
                buf[at..at + 8].copy_from_slice(&value.saturating_add(delta).to_be_bytes());
            }
        } else if CONTAINERS.contains(&&name[..]) {
            patch_chunk_offsets(&mut buf[offset + 8..offset + size], delta);
        }
        offset += size;
    }
}

#[derive(Debug, Clone, Copy)]
struct Atom {
    name: [u8; 4],
    offset: u64,
    size: u64,
}

fn read_atoms(file: &mut File, total: u64) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut offset = 0u64;
    while offset + 8 <= total {
        if file.seek(SeekFrom::Start(offset)).is_err() {
            break;
        }
        let mut header = [0u8; 8];
        if file.read_exact(&mut header).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let name = [header[4], header[5], header[6], header[7]];
        if size == 1 {
            let mut large = [0u8; 8];
            if file.read_exact(&mut large).is_err() {
                break;
            }
            size = u64::from_be_bytes(large);
        } else if size == 0 {
            size = total - offset;
        }
        if size < 8 || offset + size > total {
            break;
        }
        atoms.push(Atom { name, offset, size });
        offset += size;
        if atoms.len() > 32 {
            break;
        }
    }
    atoms
}

/// moov 在文件尾部（DJI 等相机默认）时，客户端必须读完整个 mdat 才能拿到索引，
/// 首帧会等满一个文件的 I/O。这里不改文件、不转码，只在读取时把 moov 挪到
/// mdat 前面，客户端看到的是「已 faststart」的同一份数据。
fn faststart_segments(file: &mut File, total: u64) -> Option<FaststartLayout> {
    let atoms = read_atoms(file, total);
    if atoms.is_empty() {
        return None;
    }
    let moov = atoms.iter().find(|atom| &atom.name == b"moov")?;
    let mdat = atoms.iter().find(|atom| &atom.name == b"mdat")?;
    if moov.offset < mdat.offset {
        return None;
    }
    let mut moov_bytes = vec![0u8; moov.size as usize];
    file.seek(SeekFrom::Start(moov.offset)).ok()?;
    file.read_exact(&mut moov_bytes).ok()?;
    patch_chunk_offsets(&mut moov_bytes, moov.size);
    let mut segments = Vec::new();
    let mut syn = 0u64;
    let mut moov_syn_start = 0u64;
    let mut push = |orig_start: u64, len: u64, segments: &mut Vec<Segment>, syn: &mut u64| -> u64 {
        if len == 0 {
            return *syn;
        }
        let start = *syn;
        segments.push(Segment {
            syn_start: start,
            len,
            orig_start,
        });
        *syn += len;
        start
    };
    // 前缀：ftyp/free/wide 等，保持原顺序。
    for atom in atoms.iter().filter(|atom| atom.offset < mdat.offset) {
        push(atom.offset, atom.size, &mut segments, &mut syn);
    }
    // 索引提前（内容已按新偏移改写）。
    moov_syn_start = push(moov.offset, moov.size, &mut segments, &mut syn);
    // 再按原顺序放 mdat 及其后的其它原子（moov 已经提前，跳过）。
    for atom in atoms
        .iter()
        .filter(|atom| atom.offset >= mdat.offset && &atom.name != b"moov")
    {
        push(atom.offset, atom.size, &mut segments, &mut syn);
    }
    if syn != total {
        return None;
    }
    Some(FaststartLayout {
        segments,
        moov: moov_bytes,
        moov_syn_start,
    })
}

fn write_range(
    stream: &mut TcpStream,
    file: &mut File,
    layout: Option<&FaststartLayout>,
    total: u64,
    start: u64,
    length: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let end = start.saturating_add(length).min(total);
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut written = 0u64;
    match layout {
        Some(layout) => {
            for segment in &layout.segments {
                let segment_end = segment.syn_start + segment.len;
                let from = start.max(segment.syn_start);
                let to = end.min(segment_end);
                if from >= to {
                    continue;
                }
                let mut remaining = to - from;
                let local_start = from - segment.syn_start;
                if segment.syn_start == layout.moov_syn_start {
                    let mut offset = local_start as usize;
                    while remaining > 0 {
                        let want = remaining.min(buffer.len() as u64) as usize;
                        let available = offset + want;
                        if available > layout.moov.len() {
                            break;
                        }
                        if stream.write_all(&layout.moov[offset..available]).is_err() {
                            return Ok(());
                        }
                        offset = available;
                        remaining -= want as u64;
                        written += want as u64;
                    }
                } else {
                    let offset = segment.orig_start + local_start;
                    if file.seek(SeekFrom::Start(offset)).is_err() {
                        break;
                    }
                    while remaining > 0 {
                        let want = remaining.min(buffer.len() as u64) as usize;
                        let read = match file.read(&mut buffer[..want]) {
                            Ok(0) => break,
                            Ok(read) => read,
                            Err(_) => break,
                        };
                        if stream.write_all(&buffer[..read]).is_err() {
                            return Ok(());
                        }
                        remaining -= read as u64;
                        written += read as u64;
                    }
                }
                if written >= length {
                    break;
                }
            }
        }
        None => {
            file.seek(SeekFrom::Start(start))?;
            let mut limited = file.take(length);
            loop {
                let read = limited.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                if stream.write_all(&buffer[..read]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = stream.flush();
    Ok(())
}

fn parse_range(value: Option<&str>, total: u64) -> RangeSpec {
    let Some(value) = value else {
        return RangeSpec::Full;
    };
    let value = value.trim();
    let Some(spec) = value.strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    // 只处理单段范围；多段请求回退到完整响应，浏览器可以接受。
    let first = spec.split(',').next().unwrap_or_default().trim();
    let Some((start, end)) = first.split_once('-') else {
        return RangeSpec::Full;
    };
    if start.is_empty() {
        let suffix: u64 = match end.parse() {
            Ok(value) => value,
            Err(_) => return RangeSpec::Unsatisfiable,
        };
        if suffix == 0 || total == 0 {
            return RangeSpec::Unsatisfiable;
        }
        let length = suffix.min(total);
        return RangeSpec::Partial(total - length, total - 1);
    }
    let start: u64 = match start.parse() {
        Ok(value) => value,
        Err(_) => return RangeSpec::Unsatisfiable,
    };
    if start >= total {
        return RangeSpec::Unsatisfiable;
    }
    let end: u64 = if end.is_empty() {
        total - 1
    } else {
        match end.parse::<u64>() {
            Ok(value) => value.min(total - 1),
            Err(_) => return RangeSpec::Unsatisfiable,
        }
    };
    if end < start {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Partial(start, end)
}

fn cap_range(start: u64, end: u64) -> (u64, u64) {
    (start, end.min(start.saturating_add(MAX_RANGE_BYTES - 1)))
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        // MOV 与 MP4 同为 ISO BMFF；按 video/mp4 返回可让 Chromium 走同一套
        // 解复用与硬件解码路径，起播更快，也不会被识别成需要外部播放器的类型。
        "mov" | "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub(crate) fn serve(
    stream: &mut TcpStream,
    method: &str,
    request: &str,
    query: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if method != "GET" && method != "HEAD" {
        return write_status(stream, 405, "Method Not Allowed", "method not allowed");
    }
    let token = crate::app::http::query_param(query, "ticket").unwrap_or_default();
    let ticket = match verify_ticket(&token) {
        Ok(ticket) => ticket,
        Err(message) => return write_status(stream, 403, "Forbidden", &message),
    };
    let path = PathBuf::from(&ticket.p);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return write_status(stream, 404, "Not Found", "media not found"),
    };
    let info = match file.metadata() {
        Ok(info) => info,
        Err(_) => return write_status(stream, 404, "Not Found", "media not found"),
    };
    if !info.is_file() {
        return write_status(stream, 404, "Not Found", "media not found");
    }
    let total = info.len();
    let layout = faststart_segments(&mut file, total);
    let modified = info
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{total}-{modified}\"");
    let range_header = crate::app::security::header_value(request, "range");
    let (status, start, length, content_range) = match parse_range(range_header, total) {
        RangeSpec::Full => (200, 0, total, None),
        RangeSpec::Partial(start, end) => {
            let (start, capped_end) = cap_range(start, end);
            (
                206,
                start,
                capped_end - start + 1,
                Some(format!("bytes {start}-{capped_end}/{total}")),
            )
        }
        RangeSpec::Unsatisfiable => {
            let response = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes())?;
            return Ok(());
        }
    };
    let status_text = if status == 206 {
        "Partial Content"
    } else {
        "OK"
    };
    let mut headers = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nCache-Control: private, max-age=300\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        content_type_for(&path)
    );
    if let Some(value) = content_range {
        headers.push_str(&format!("Content-Range: {value}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    if method == "HEAD" {
        return Ok(());
    }
    write_range(stream, &mut file, layout.as_ref(), total, start, length)
}

fn write_status(
    stream: &mut TcpStream,
    status: u16,
    text: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = message.as_bytes();
    let response = format!(
        "HTTP/1.1 {status} {text}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket_for(path: &str, expires_in: i64) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let payload =
            serde_json::json!({ "p": path, "e": now + expires_in, "id": "com.example.test" });
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signature =
            URL_SAFE_NO_PAD.encode(hmac_sha256(secret().as_bytes(), encoded.as_bytes()));
        format!("{encoded}.{signature}")
    }

    #[test]
    fn verifies_valid_ticket() {
        let token = ticket_for("C:\\media\\clip.mp4", 60);
        let ticket = verify_ticket(&token).expect("valid ticket");
        assert_eq!(ticket.p, "C:\\media\\clip.mp4");
    }

    #[test]
    fn rejects_tampered_or_expired_ticket() {
        let token = ticket_for("C:\\media\\clip.mp4", 60);
        let mut tampered = token.clone();
        tampered.push('x');
        assert!(verify_ticket(&tampered).is_err());
        let expired = ticket_for("C:\\media\\clip.mp4", -10);
        assert!(verify_ticket(&expired).is_err());
    }

    #[test]
    fn caps_open_ended_ranges() {
        // 播放器用 `bytes=0-` 探路时不能回整段文件，否则要读完整段 mdat 才知道 moov 位置。
        let total = 3_000_000_000u64;
        match parse_range(Some("bytes=0-"), total) {
            RangeSpec::Partial(start, end) => {
                let (start, end) = cap_range(start, end);
                assert_eq!(start, 0);
                assert_eq!(end + 1, MAX_RANGE_BYTES);
            }
            _ => panic!("expected partial"),
        }
        match parse_range(Some("bytes=900-"), total) {
            RangeSpec::Partial(start, end) => {
                let (start, end) = cap_range(start, end);
                assert_eq!(start, 900);
                assert!(end - start + 1 <= MAX_RANGE_BYTES);
            }
            _ => panic!("expected partial"),
        }
        // 小范围请求保持原样，不做多余拆分。
        match parse_range(Some("bytes=0-1023"), total) {
            RangeSpec::Partial(start, end) => {
                let (start, end) = cap_range(start, end);
                assert_eq!((start, end), (0, 1023));
            }
            _ => panic!("expected partial"),
        }
    }

    #[test]
    fn serves_mov_as_mp4() {
        assert_eq!(content_type_for(Path::new("clip.MOV")), "video/mp4");
        assert_eq!(content_type_for(Path::new("clip.mp4")), "video/mp4");
        assert_eq!(content_type_for(Path::new("clip.mkv")), "video/x-matroska");
    }

    #[test]
    fn reorders_moov_before_mdat() {
        // ftyp(32) + mdat(100) + moov(40)：moov 在尾部，读取时应被提前。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend(std::iter::repeat(0u8).take(24));
        bytes.extend_from_slice(&100u32.to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend(std::iter::repeat(7u8).take(92));
        bytes.extend_from_slice(&40u32.to_be_bytes());
        bytes.extend_from_slice(b"moov");
        bytes.extend(std::iter::repeat(9u8).take(32));
        let path =
            std::env::temp_dir().join(format!("himind-media-layout-{}.bin", rand::random::<u32>()));
        std::fs::write(&path, &bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        let total = bytes.len() as u64;
        let layout = faststart_segments(&mut file, total).expect("layout");
        let segments = &layout.segments;
        // 前 32 字节仍是 ftyp，然后 40 字节 moov，最后 100 字节 mdat。
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].syn_start, 0);
        assert_eq!(segments[0].orig_start, 0);
        assert_eq!(segments[0].len, 32);
        assert_eq!(segments[1].syn_start, 32);
        assert_eq!(segments[1].orig_start, 132);
        assert_eq!(segments[1].len, 40);
        assert_eq!(segments[2].syn_start, 72);
        assert_eq!(segments[2].orig_start, 32);
        assert_eq!(segments[2].len, 100);
        assert_eq!(layout.moov_syn_start, 32);
        assert_eq!(layout.moov.len(), 40);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn patches_stco_offsets_when_moving_moov() {
        // moov 里放一个 stco：header 8 + version/flags 4 + count 4 + 1 个偏移 4 = 20 字节。
        let mut stco = Vec::new();
        stco.extend_from_slice(&20u32.to_be_bytes());
        stco.extend_from_slice(b"stco");
        stco.extend_from_slice(&0u32.to_be_bytes());
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&4096u32.to_be_bytes());
        let mut moov = Vec::new();
        moov.extend_from_slice(&(8u32 + stco.len() as u32).to_be_bytes());
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&stco);
        let mut buffer = moov.clone();
        patch_chunk_offsets(&mut buffer, 1000);
        let patched = u32::from_be_bytes([
            buffer[buffer.len() - 4],
            buffer[buffer.len() - 3],
            buffer[buffer.len() - 2],
            buffer[buffer.len() - 1],
        ]);
        assert_eq!(patched, 5096);
    }

    #[test]
    fn leaves_faststart_files_alone() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend(std::iter::repeat(0u8).take(24));
        bytes.extend_from_slice(&40u32.to_be_bytes());
        bytes.extend_from_slice(b"moov");
        bytes.extend(std::iter::repeat(9u8).take(32));
        bytes.extend_from_slice(&100u32.to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend(std::iter::repeat(7u8).take(92));
        let path =
            std::env::temp_dir().join(format!("himind-media-layout-{}.bin", rand::random::<u32>()));
        std::fs::write(&path, &bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        let total = bytes.len() as u64;
        assert!(faststart_segments(&mut file, total).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parses_single_ranges() {
        assert!(matches!(parse_range(None, 1000), RangeSpec::Full));
        match parse_range(Some("bytes=0-99"), 1000) {
            RangeSpec::Partial(start, end) => {
                assert_eq!((start, end), (0, 99));
            }
            _ => panic!("expected partial"),
        }
        match parse_range(Some("bytes=900-"), 1000) {
            RangeSpec::Partial(start, end) => {
                assert_eq!((start, end), (900, 999));
            }
            _ => panic!("expected partial"),
        }
        match parse_range(Some("bytes=-100"), 1000) {
            RangeSpec::Partial(start, end) => {
                assert_eq!((start, end), (900, 999));
            }
            _ => panic!("expected partial"),
        }
        assert!(matches!(
            parse_range(Some("bytes=2000-"), 1000),
            RangeSpec::Unsatisfiable
        ));
    }
}
