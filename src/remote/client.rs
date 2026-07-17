use reqwest::blocking::Client;
use std::env;
use std::error::Error;

use crate::store::credentials::load_inner_admin_credentials;

pub(crate) fn inner_admin_base() -> String {
    env::var("INNER_ADMIN_BASE_URL")
        .unwrap_or_else(|_| "http://172.16.0.197:8086".to_string())
        .trim_end_matches('/')
        .to_string()
}

pub(crate) fn inner_admin_client() -> Result<Client, Box<dyn Error>> {
    Ok(Client::builder()
        .cookie_store(true)
        .user_agent("himind-agent/0.1")
        .build()?)
}

pub(crate) fn inner_admin_login(
    client: &Client,
    base: &str,
    allow_env_fallback: bool,
) -> Result<(), Box<dyn Error>> {
    let (username, password) = load_inner_admin_credentials(allow_env_fallback)?;
    let login_url = format!("{}/admin/auth/login", base);
    let login_html = client.get(&login_url).send()?.error_for_status()?.text()?;
    let token = extract_input_value(&login_html, "_token");
    let mut form = vec![
        ("username".to_string(), username),
        ("password".to_string(), password),
        ("remember".to_string(), "1".to_string()),
    ];
    if let Some(token) = token {
        form.push(("_token".to_string(), token));
    }
    let text = client
        .post(&login_url)
        .form(&form)
        .send()?
        .error_for_status()?
        .text()?;
    if is_login_page(&text) {
        return Err("inner admin login failed".into());
    }
    Ok(())
}

pub(crate) fn is_login_page(html: &str) -> bool {
    html.contains("/admin/auth/login") || (html.contains("登录") && html.contains("password"))
}

pub(crate) fn extract_table_cells(row: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for part in row.split("<td").skip(1) {
        if let Some(after) = part.split_once('>').map(|(_, right)| right) {
            if let Some((cell, _)) = after.split_once("</td>") {
                cells.push(html_unescape(&strip_tags(cell)).trim().to_string());
            }
        }
    }
    cells
}

pub(crate) fn extract_table_header_cells(html: &str) -> Vec<String> {
    for row in html.split("<tr").skip(1) {
        if !row.contains("<th") {
            continue;
        }
        let mut cells = Vec::new();
        for part in row.split("<th").skip(1) {
            if let Some(after) = part.split_once('>').map(|(_, right)| right) {
                if let Some((cell, _)) = after.split_once("</th>") {
                    cells.push(html_unescape(&strip_tags(cell)).trim().to_string());
                }
            }
        }
        if !cells.is_empty() {
            return cells;
        }
    }
    Vec::new()
}

pub(crate) fn extract_input_value(html: &str, name: &str) -> Option<String> {
    for input in html.split("<input").skip(1) {
        if input.contains(&format!("name=\"{}\"", name))
            || input.contains(&format!("name='{}'", name))
        {
            if let Some(value) = extract_attr(input, "value") {
                return Some(html_unescape(&value));
            }
        }
    }
    None
}

pub(crate) fn extract_meta_content(html: &str, name: &str) -> Option<String> {
    for meta in html.split("<meta").skip(1) {
        if meta.contains(&format!("name=\"{}\"", name))
            || meta.contains(&format!("name='{}'", name))
        {
            if let Some(content) = extract_attr(meta, "content") {
                return Some(html_unescape(&content));
            }
        }
    }
    None
}

pub(crate) fn extract_between(text: &str, start: &str, end: &str) -> Option<String> {
    let from = text.find(start)? + start.len();
    let rest = &text[from..];
    let to = rest.find(end)?;
    Some(rest[..to].to_string())
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut value: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        value.push_str("...");
    }
    value
}

pub(crate) fn html_unescape(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#039;", "'")
        .replace("&ldquo;", "\"")
        .replace("&rdquo;", "\"")
        .replace("&lsquo;", "'")
        .replace("&rsquo;", "'")
        .replace("&mdash;", "-")
        .replace("&ndash;", "-")
        .replace("&hellip;", "...")
}

fn extract_attr(html: &str, attr: &str) -> Option<String> {
    extract_between(html, &format!("{}=\"", attr), "\"")
        .or_else(|| extract_between(html, &format!("{}='", attr), "'"))
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}
