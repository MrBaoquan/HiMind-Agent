use serde_json::{json, Value};
use std::cell::RefCell;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

thread_local! {
    static RESPONSE_ORIGIN: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn set_response_origin(origin: Option<&str>) {
    RESPONSE_ORIGIN.with(|current| {
        *current.borrow_mut() = origin.map(str::to_string);
    });
}

pub(crate) fn write_local_response(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
    content_type: &str,
) -> Result<(), Box<dyn Error>> {
    let status_text = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        400 => "Bad Request",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let cors_headers = RESPONSE_ORIGIN.with(|current| {
        current
            .borrow()
            .as_deref()
            .map(|origin| {
                format!(
                    "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, X-Upload-Id, X-File-Name, X-Upload-Final, X-HiMind-Local-Ticket\r\n"
                )
            })
            .unwrap_or_default()
    });
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n{cors_headers}Content-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

pub(crate) fn split_target(target: &str) -> (String, String) {
    let mut parts = target.splitn(2, '?');
    (
        parts.next().unwrap_or("/").to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

pub(crate) fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let mut kv = part.splitn(2, '=');
        let key_value = percent_decode(kv.next().unwrap_or_default());
        let value = percent_decode(kv.next().unwrap_or_default());
        if key_value == key {
            Some(value)
        } else {
            None
        }
    })
}

pub(crate) fn local_tree_json(path: &str, depth: usize) -> Result<Value, Box<dyn Error>> {
    if path.trim().is_empty() {
        let roots: Vec<Value> = ('A'..='Z')
            .filter_map(|letter| {
                let path = format!("{letter}:\\");
                if Path::new(&path).exists() {
                    Some(json!({ "name": format!("{letter}:"), "path": path, "is_dir": true }))
                } else {
                    None
                }
            })
            .collect();
        return Ok(json!({ "name": "此电脑", "path": "", "is_dir": true, "children": roots }));
    }
    let root = PathBuf::from(path);
    let name = root
        .file_name()
        .map(|item| item.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let children = if depth == 0 {
        Vec::new()
    } else {
        local_tree_children(&root, depth)?
    };
    Ok(
        json!({ "name": name, "path": root.to_string_lossy(), "is_dir": true, "children": children }),
    )
}

pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(hex);
                index += 3;
                continue;
            }
        }
        out.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn local_tree_children(root: &Path, depth: usize) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        let path = entry.path();
        let is_dir = path.is_dir();
        let name = entry.file_name().to_string_lossy().to_string();
        let nested = if is_dir && depth > 1 {
            local_tree_children(&path, depth - 1)?
        } else {
            Vec::new()
        };
        children.push(json!({ "name": name, "path": path.to_string_lossy(), "is_dir": is_dir, "children": nested }));
        if children.len() >= 200 {
            break;
        }
    }
    children.sort_by(|a, b| {
        b["is_dir"]
            .as_bool()
            .cmp(&a["is_dir"].as_bool())
            .then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["name"].as_str().unwrap_or_default())
            })
    });
    Ok(children)
}
