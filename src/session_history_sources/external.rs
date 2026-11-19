//! Read-only adapters for additional clients and declared JSON session sources.
//! No shell, credentials, executable adapters, or file-mtime activity timestamps.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use walkdir::WalkDir;

use super::ProviderScan;
use crate::session_history::{
    self, SessionCapabilities, SessionSource, SessionSourceStatus, SessionSummary,
    SessionTranscriptMessage, SessionTranscriptPage, SourceAvailability,
};
use crate::workspace_registry::Registry;

const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_LINE: usize = 2 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_TOTAL: u64 = 128 * 1024 * 1024;
const MAX_MESSAGES: usize = 20_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SourceSpec {
    pub id: String,
    pub label: String,
    pub root: String,
    pub roots: Vec<String>,
    pub glob: String,
    pub format: String,
    pub session_id: String,
    pub time_fields: Vec<String>,
    pub time_unit: Option<i32>,
    pub title_field: String,
    pub directory_field: String,
    pub role_field: String,
    pub content_fields: Vec<String>,
    pub prompt_field: String,
    pub assistant_field: String,
    pub user_roles: Vec<String>,
    pub assistant_roles: Vec<String>,
    pub records_path: String,
    pub user_query_tag: String,
    pub session_from_file: bool,
    pub disabled: bool,
}

pub(crate) fn definitions() -> Result<Vec<SourceSpec>, String> {
    let mut specs: Vec<SourceSpec> = serde_json::from_str(include_str!("presets.json"))
        .map_err(|_| "SESSION_SOURCE_PRESETS_INVALID")?;
    let config = crate::store::paths::agent_home().join("data/session-sources.json");
    match fs::metadata(&config) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(specs),
        Ok(m) if m.len() <= 1024 * 1024 => {}
        _ => return Err("SESSION_SOURCE_CONFIG_UNREADABLE".into()),
    }
    let value: Value =
        serde_json::from_slice(&fs::read(config).map_err(|_| "SESSION_SOURCE_CONFIG_UNREADABLE")?)
            .map_err(|_| "SESSION_SOURCE_CONFIG_INVALID")?;
    merge_specs(specs, &value)
}

fn merge_specs(mut specs: Vec<SourceSpec>, value: &Value) -> Result<Vec<SourceSpec>, String> {
    let rows = value
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("SESSION_SOURCE_CONFIG_INVALID")?;
    if rows.len() > 32 {
        return Err("SESSION_SOURCE_LIMIT".into());
    }
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .ok_or("SESSION_SOURCE_INVALID")?;
        if !matches!(
            SessionSource::try_from(id.to_string())?,
            SessionSource::External(_)
        ) || !seen.insert(id)
        {
            return Err("SESSION_SOURCE_CONFIG_CONFLICT".into());
        }
        let at = specs.iter().position(|s| s.id == id);
        let mut merged = at
            .map(|i| serde_json::to_value(&specs[i]).unwrap())
            .unwrap_or(json!({}));
        let map = merged.as_object_mut().unwrap();
        for (key, value) in row.as_object().ok_or("SESSION_SOURCE_CONFIG_INVALID")? {
            map.insert(key.clone(), value.clone());
        }
        // An explicit single root overrides a preset's multi-profile defaults.
        if row.get("root").is_some() && row.get("roots").is_none() {
            map.insert("roots".into(), json!([]));
        }
        let spec: SourceSpec =
            serde_json::from_value(merged).map_err(|_| "SESSION_SOURCE_CONFIG_INVALID")?;
        if !matches!(spec.format.as_str(), "json" | "jsonl" | "json_array")
            || spec.glob.len() > 512
            || spec.records_path.len() > 256
        {
            return Err("SESSION_SOURCE_FORMAT_INVALID".into());
        }
        for root in if spec.roots.is_empty() {
            vec![&spec.root]
        } else {
            spec.roots.iter().collect()
        } {
            if expand_root(root).is_none() {
                return Err("SESSION_SOURCE_ROOT_INVALID".into());
            }
        }
        if let Some(at) = at {
            specs[at] = spec;
        } else {
            specs.push(spec);
        }
    }
    if specs.len() > 32 {
        return Err("SESSION_SOURCE_LIMIT".into());
    }
    Ok(specs.into_iter().filter(|s| !s.disabled).collect())
}

pub(crate) fn config_get() -> Result<Value, String> {
    let path = crate::store::paths::agent_home().join("data/session-sources.json");
    if !path.exists() {
        return Ok(json!({"schema_version":"session_sources.v1","sources":[],"native_roots":{}}));
    }
    if fs::metadata(&path)
        .map_err(|_| "SESSION_SOURCE_CONFIG_UNREADABLE")?
        .len()
        > 1024 * 1024
    {
        return Err("SESSION_SOURCE_LIMIT".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "SESSION_SOURCE_CONFIG_UNREADABLE")?)
        .map_err(|_| "SESSION_SOURCE_CONFIG_INVALID".into())
}
pub(crate) fn configure(input: &Value) -> Result<Value, String> {
    let mut value = input.clone();
    if value.get("sources").is_none() {
        value["sources"] = json!([]);
    }
    value["schema_version"] = json!("session_sources.v1");
    let specs: Vec<SourceSpec> = serde_json::from_str(include_str!("presets.json")).unwrap();
    merge_specs(specs, &value)?;
    if let Some(roots) = value.get("native_roots") {
        for (id, path) in roots.as_object().ok_or("SESSION_SOURCE_ROOT_INVALID")? {
            if !matches!(id.as_str(), "codex" | "claude_code" | "opencode")
                || path.as_str().and_then(expand_root).is_none()
            {
                return Err("SESSION_SOURCE_ROOT_INVALID".into());
            }
        }
    }
    let body = serde_json::to_vec_pretty(&value).map_err(|_| "SESSION_SOURCE_CONFIG_INVALID")?;
    if body.len() > 1024 * 1024 {
        return Err("SESSION_SOURCE_LIMIT".into());
    }
    let path = crate::store::paths::agent_home().join("data/session-sources.json");
    crate::store::atomic_file::atomic_write(&path, &body)
        .map_err(|_| "SESSION_SOURCE_CONFIG_UNREADABLE")?;
    Ok(json!({"saved":true,"path":path,"source_count":definitions()?.len()}))
}
pub(crate) fn native_root(source: &str) -> Option<PathBuf> {
    config_get()
        .ok()?
        .get("native_roots")?
        .get(source)?
        .as_str()
        .and_then(expand_root)
}

pub(crate) fn sources() -> Result<Vec<SessionSource>, String> {
    definitions()?
        .into_iter()
        .map(|s| SessionSource::try_from(s.id))
        .collect()
}

pub(crate) fn scan(registry: &Registry, source: &SessionSource) -> ProviderScan {
    let (rows, availability, error) = match load(registry, source) {
        Ok((rows, partial, exists)) => (
            rows,
            if !exists {
                SourceAvailability::Unavailable
            } else if partial {
                SourceAvailability::Partial
            } else {
                SourceAvailability::Available
            },
            if !exists {
                Some("source_not_found".into())
            } else if partial {
                Some("source_read_partial".into())
            } else {
                None
            },
        ),
        Err(error) => (Vec::new(), SourceAvailability::Error, Some(error)),
    };
    ProviderScan {
        status: SessionSourceStatus {
            source: source.clone(),
            availability,
            session_count: rows.len(),
            error_code: error,
        },
        sessions: rows.into_iter().map(|r| r.summary).collect(),
    }
}

pub(crate) fn transcript(
    registry: &Registry,
    source: &SessionSource,
    project_id: Option<&str>,
    native_id: &str,
    end: Option<u64>,
    size: usize,
) -> Result<SessionTranscriptPage, String> {
    let (mut rows, _, _) = load(registry, source)?;
    let matches: Vec<_> = rows
        .iter()
        .filter(|r| {
            r.summary.native_id == native_id
                && project_id.is_none_or(|p| r.summary.project_id.as_deref() == Some(p))
        })
        .collect();
    if matches.len() > 1 {
        return Err("SESSION_ID_AMBIGUOUS".into());
    }
    let at = rows
        .iter()
        .position(|r| {
            r.summary.native_id == native_id
                && project_id.is_none_or(|p| r.summary.project_id.as_deref() == Some(p))
        })
        .ok_or("SESSION_NOT_FOUND")?;
    let record = rows.swap_remove(at);
    let end = end.unwrap_or(record.messages.len() as u64) as usize;
    if end > record.messages.len() {
        return Err("SESSION_SNAPSHOT_CHANGED".into());
    }
    let start = end.saturating_sub(size);
    Ok(SessionTranscriptPage {
        snapshot_cursor: Some(end.to_string()),
        session_id: record.summary.id,
        through_seq: end.checked_sub(1).map(|n| n as i64),
        messages: record.messages[start..end].to_vec(),
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

struct Record {
    summary: SessionSummary,
    messages: Vec<SessionTranscriptMessage>,
}
fn load(registry: &Registry, source: &SessionSource) -> Result<(Vec<Record>, bool, bool), String> {
    let spec = definitions()?
        .into_iter()
        .find(|s| s.id == source.id())
        .ok_or("SESSION_SOURCE_INVALID")?;
    let roots = if spec.roots.is_empty() {
        vec![spec.root.clone()]
    } else {
        spec.roots.clone()
    };
    let mut records = Vec::new();
    let mut partial = false;
    let mut exists = false;
    let mut total = 0;
    for root in roots {
        let Some(root) = expand_root(&root) else {
            partial = true;
            continue;
        };
        if !root.exists() {
            continue;
        };
        exists = true;
        if source.id() == "github-copilot" {
            match copilot(registry, source, &root) {
                Ok(mut rows) => records.append(&mut rows),
                Err(_) => partial = true,
            };
            continue;
        }
        let Ok(root) = root.canonicalize() else {
            partial = true;
            continue;
        };
        let mut seen = 0;
        let mut walked = 0;
        for item in WalkDir::new(&root).follow_links(false).max_depth(12) {
            walked += 1;
            if walked > MAX_FILES * 4 {
                partial = true;
                break;
            }
            let Ok(item) = item else {
                partial = true;
                continue;
            };
            if item.file_type().is_symlink() {
                partial = true;
                continue;
            }
            if !item.file_type().is_file() {
                continue;
            }
            let relative = item
                .path()
                .strip_prefix(&root)
                .unwrap_or(item.path())
                .to_string_lossy()
                .replace('\\', "/");
            if !glob_matches(&spec.glob, &relative) {
                continue;
            }
            seen += 1;
            if seen > MAX_FILES {
                partial = true;
                break;
            }
            let Ok(path) = item.path().canonicalize() else {
                partial = true;
                continue;
            };
            if !path.starts_with(&root) {
                partial = true;
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                partial = true;
                continue;
            };
            total += meta.len();
            if meta.len() > MAX_FILE || total > MAX_TOTAL {
                partial = true;
                break;
            }
            let parsed = match source.id() {
                "vscode" => vscode(registry, source, &path),
                "claude-desktop" => desktop(registry, source, &path),
                _ => generic(registry, source, &spec, &path),
            };
            match parsed {
                Ok((mut rows, p)) => {
                    records.append(&mut rows);
                    partial |= p;
                }
                Err(_) => partial = true,
            }
        }
    }
    records.sort_by(|a, b| {
        a.summary.id.cmp(&b.summary.id).then(
            a.summary
                .working_directory
                .cmp(&b.summary.working_directory),
        )
    });
    // Identical native identities mirrored between local profiles have one owner.
    records.dedup_by(|a, b| {
        a.summary.id == b.summary.id && a.summary.working_directory == b.summary.working_directory
    });
    Ok((records, partial, exists))
}

fn expand_root(raw: &str) -> Option<PathBuf> {
    let mut text = raw.to_string();
    for name in ["APPDATA", "LOCALAPPDATA", "USERPROFILE"] {
        if text.contains(&format!("%{name}%")) {
            text = text.replace(&format!("%{name}%"), &std::env::var(name).ok()?);
        }
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        text = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    let path = PathBuf::from(text);
    (path.is_absolute()
        && path.parent().is_some()
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)))
    .then_some(path)
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    let (mut i, mut j, mut star, mut back) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == v[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            back = j;
        } else if let Some(at) = star {
            i = at + 1;
            back += 1;
            j = back;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn glob_matches(pattern: &str, value: &str) -> bool {
    let p: Vec<_> = pattern.split('/').collect();
    let v: Vec<_> = value.split('/').collect();
    let mut positions = std::collections::HashSet::from([0usize]);
    for part in p {
        let mut next = std::collections::HashSet::new();
        for at in positions {
            if part == "**" {
                next.extend(at..=v.len());
            } else if at < v.len() && segment_matches(part, v[at]) {
                next.insert(at + 1);
            }
        }
        positions = next;
    }
    positions.contains(&v.len())
}
fn field<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    let mut current = value;
    for part in path.split('.') {
        current = if let Ok(i) = part.parse::<usize>() {
            current.get(i)?
        } else {
            current.get(part)?
        };
    }
    Some(current)
}
fn scalar(value: Option<&Value>) -> String {
    value
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_default()
}
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => {
            if matches!(
                value.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking" | "tool_use" | "tool_result")
            ) {
                return String::new();
            }
            for key in ["text", "content", "value", "message"] {
                if let Some(v) = value.get(key) {
                    let s = text(v);
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}
fn time(value: Option<&Value>, unit: Option<i32>) -> Option<String> {
    let value = value?;
    if let Some(s) = value.as_str() {
        if let Ok(t) = DateTime::parse_from_rfc3339(s) {
            return Some(t.to_rfc3339());
        }
        // SQLite CURRENT_TIMESTAMP is UTC by contract.
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
            return Some(t.and_utc().to_rfc3339());
        }
    }
    let n = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    let millis = match unit {
        Some(0) => n.checked_mul(1000)?,
        Some(6) => n / 1000,
        Some(3) => n,
        _ if n.abs() < 100_000_000_000 => n.checked_mul(1000)?,
        _ => n,
    };
    DateTime::<Utc>::from_timestamp_millis(millis).map(|t| t.to_rfc3339())
}
fn timestamp(record: &Value, document: &Value, spec: &SourceSpec) -> Option<String> {
    for path in &spec.time_fields {
        if let Some(t) = time(
            field(record, path).or_else(|| field(document, path)),
            spec.time_unit,
        ) {
            return Some(t);
        }
    }
    None
}
fn make_record(
    registry: &Registry,
    source: &SessionSource,
    id: String,
    directory: String,
    title: String,
    messages: Vec<SessionTranscriptMessage>,
    updated: Option<String>,
) -> Result<Record, String> {
    let association = if directory.is_empty() {
        None
    } else {
        session_history::associate_workspace(registry, &directory)
            .map_err(|_| "SESSION_DIRECTORY_INVALID")?
    };
    let mut summary = SessionSummary::new(
        source.clone(),
        id,
        directory,
        title,
        updated,
        Some(messages.len()),
        SessionCapabilities {
            can_preview: true,
            can_open_original: false,
            can_resume: false,
        },
        association,
    )?;
    if summary.title.trim().is_empty() {
        summary.title = messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.chars().take(100).collect())
            .unwrap_or_else(|| source.label().to_string());
    }
    Ok(Record { summary, messages })
}
fn push(
    messages: &mut Vec<SessionTranscriptMessage>,
    id: String,
    role: &str,
    content: String,
    timestamp: Option<String>,
) {
    if !content.trim().is_empty() && messages.len() < MAX_MESSAGES {
        messages.push(SessionTranscriptMessage {
            execution: None,
            id,
            role: role.to_string(),
            content,
            timestamp,
            files: Vec::new(),
        });
    }
}
fn documents(path: &Path, format: &str) -> Result<(Vec<Value>, bool), String> {
    if fs::metadata(path).map_err(|_| "source_unreadable")?.len() > MAX_FILE {
        return Err("source_size_limit".into());
    }
    if format != "jsonl" {
        return Ok((
            vec![
                serde_json::from_slice(&fs::read(path).map_err(|_| "source_unreadable")?)
                    .map_err(|_| "source_invalid_json")?,
            ],
            false,
        ));
    }
    let mut input = BufReader::new(File::open(path).map_err(|_| "source_unreadable")?);
    let mut values = Vec::new();
    let mut bytes = Vec::new();
    let mut partial = false;
    while Read::by_ref(&mut input)
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "source_unreadable")?
        > 0
    {
        if bytes.len() > MAX_LINE {
            return Err("source_line_limit".into());
        }
        if bytes.len() > MAX_LINE {
            partial = true;
        } else if !bytes.iter().all(u8::is_ascii_whitespace) {
            match serde_json::from_slice(&bytes) {
                Ok(v) => values.push(v),
                Err(_) => partial = true,
            }
        }
        bytes.clear();
        if values.len() > 100_000 {
            return Err("source_record_limit".into());
        }
    }
    Ok((values, partial))
}
fn generic(
    registry: &Registry,
    source: &SessionSource,
    spec: &SourceSpec,
    path: &Path,
) -> Result<(Vec<Record>, bool), String> {
    let (documents, partial) = documents(path, &spec.format)?;
    struct Bucket {
        directory: String,
        title: String,
        messages: Vec<SessionTranscriptMessage>,
    }
    let mut groups: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut inherited_id = String::new();
    let mut inherited_dir = String::new();
    let fallback = if spec.session_from_file {
        path.parent().and_then(Path::file_name)
    } else {
        path.file_stem()
    }
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
    for document in &documents {
        let docid = scalar(field(document, &spec.session_id));
        if !docid.is_empty() {
            inherited_id = docid;
        }
        let docdir = scalar(
            field(document, &spec.directory_field)
                .or_else(|| document.get("cwd"))
                .or_else(|| document.pointer("/message/cwd")),
        );
        if !docdir.is_empty() {
            inherited_dir = docdir;
        }
        let records = if !spec.records_path.is_empty() {
            field(document, &spec.records_path)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        } else if spec.format == "json_array" {
            document.as_array().cloned().unwrap_or_default()
        } else {
            vec![document.clone()]
        };
        for (index, record) in records.iter().enumerate() {
            let rawid = scalar(field(record, &spec.session_id));
            let id = if !rawid.is_empty() {
                rawid
            } else if !inherited_id.is_empty() {
                inherited_id.clone()
            } else {
                fallback.clone()
            };
            let directory =
                scalar(field(record, &spec.directory_field).or_else(|| record.get("cwd")));
            let directory = if directory.is_empty() {
                inherited_dir.clone()
            } else {
                directory
            };
            let bucket = groups.entry(id).or_insert_with(|| Bucket {
                directory: directory.clone(),
                title: scalar(field(document, &spec.title_field)),
                messages: Vec::new(),
            });
            if !directory.is_empty() {
                bucket.directory = directory;
            }
            let at = timestamp(record, document, spec);
            let native = scalar(record.get("uuid").or_else(|| record.get("id")));
            let message_id = if native.is_empty() {
                format!("{}:{index}", bucket.messages.len())
            } else {
                native
            };
            let role = scalar(field(record, &spec.role_field));
            let role = if role.is_empty() {
                scalar(
                    record
                        .get("role")
                        .or_else(|| record.pointer("/message/role")),
                )
            } else {
                role
            };
            let user = role == "user" || role == "human" || spec.user_roles.contains(&role);
            let assistant =
                role == "assistant" || role == "ai" || spec.assistant_roles.contains(&role);
            if !role.is_empty() {
                if !user && !assistant {
                    continue;
                }
                let mut content = spec
                    .content_fields
                    .iter()
                    .filter_map(|f| field(record, f))
                    .map(text)
                    .find(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| text(record));
                if user && !spec.user_query_tag.is_empty() {
                    let open = format!("<{}>", spec.user_query_tag);
                    let close = format!("</{}>", spec.user_query_tag);
                    content = content
                        .split_once(&open)
                        .and_then(|(_, v)| v.split_once(&close))
                        .map(|(v, _)| v.trim().to_string())
                        .unwrap_or_default();
                }
                push(
                    &mut bucket.messages,
                    format!("{message_id}:{}", if user { "user" } else { "assistant" }),
                    if user { "user" } else { "assistant" },
                    content,
                    at,
                );
            } else {
                if let Some(v) = field(record, &spec.prompt_field) {
                    push(
                        &mut bucket.messages,
                        format!("{message_id}:user"),
                        "user",
                        text(v),
                        at.clone(),
                    );
                }
                if let Some(v) = field(record, &spec.assistant_field) {
                    push(
                        &mut bucket.messages,
                        format!("{message_id}:assistant"),
                        "assistant",
                        text(v),
                        at,
                    );
                }
            }
        }
    }
    let updated = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from)
        .map(|t| t.to_rfc3339());
    let mut result = Vec::new();
    for (id, b) in groups {
        if !b.messages.is_empty() {
            result.push(make_record(
                registry,
                source,
                id,
                b.directory,
                b.title,
                b.messages,
                updated.clone(),
            )?);
        }
    }
    Ok((result, partial))
}

fn copilot(
    registry: &Registry,
    source: &SessionSource,
    path: &Path,
) -> Result<Vec<Record>, String> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "source_unreadable")?;
    connection
        .busy_timeout(std::time::Duration::from_millis(300))
        .map_err(|_| "source_unreadable")?;
    let mut query=connection.prepare("SELECT id, cwd, summary, updated_at FROM sessions ORDER BY created_at DESC LIMIT 10000").map_err(|_|"source_schema_unsupported")?;
    let sessions = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ))
        })
        .map_err(|_| "source_unreadable")?;
    let mut result = Vec::new();
    for row in sessions {
        let (id, cwd, title, updated) = row.map_err(|_| "source_unreadable")?;
        let mut messages = Vec::new();
        let mut turns=connection.prepare("SELECT user_message, assistant_response, timestamp FROM turns WHERE session_id=?1 ORDER BY timestamp LIMIT 20000").map_err(|_|"source_schema_unsupported")?;
        let rows = turns
            .query_map([&id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .map_err(|_| "source_unreadable")?;
        for (i, row) in rows.enumerate() {
            let (user, assistant, at) = row.map_err(|_| "source_unreadable")?;
            let at = time(Some(&json!(at)), None);
            push(&mut messages, format!("{i}:user"), "user", user, at.clone());
            push(
                &mut messages,
                format!("{i}:assistant"),
                "assistant",
                assistant,
                at,
            );
        }
        result.push(make_record(
            registry,
            source,
            id,
            cwd,
            title,
            messages,
            Some(updated),
        )?);
    }
    Ok(result)
}

fn assign(doc: &mut Value, keys: &[Value], value: Value, append: bool) {
    if keys.is_empty() {
        if append {
            if !doc.is_array() {
                *doc = json!([]);
            }
            let a = doc.as_array_mut().unwrap();
            if let Value::Array(mut rows) = value {
                a.append(&mut rows)
            } else {
                a.push(value)
            }
        } else {
            *doc = value;
        }
        return;
    }
    if let Some(index) = keys[0].as_u64() {
        if index > 10000 {
            return;
        }
        if !doc.is_array() {
            *doc = json!([]);
        }
        let a = doc.as_array_mut().unwrap();
        while a.len() <= index as usize {
            a.push(Value::Null);
        }
        assign(&mut a[index as usize], &keys[1..], value, append);
    } else if let Some(key) = keys[0].as_str() {
        if !doc.is_object() {
            *doc = json!({});
        }
        assign(
            doc.as_object_mut()
                .unwrap()
                .entry(key.to_string())
                .or_insert(Value::Null),
            &keys[1..],
            value,
            append,
        );
    }
}
fn vscode(
    registry: &Registry,
    source: &SessionSource,
    path: &Path,
) -> Result<(Vec<Record>, bool), String> {
    let (lines, partial) = documents(path, "jsonl")?;
    let mut doc = Value::Null;
    for line in lines {
        match line.get("kind").and_then(Value::as_u64) {
            Some(0) => doc = line.get("v").cloned().unwrap_or(line),
            Some(kind @ (1 | 2)) => {
                if let Some(keys) = line.get("k").and_then(Value::as_array) {
                    assign(
                        &mut doc,
                        keys,
                        line.get("v").cloned().unwrap_or(Value::Null),
                        kind == 2,
                    );
                }
            }
            _ => doc = line,
        }
    }
    let id = scalar(doc.get("sessionId"));
    let id = if id.is_empty() {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        id
    };
    let workspace = path
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("workspace.json"));
    let cwd = workspace
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("folder").and_then(Value::as_str).map(str::to_string))
        .and_then(|s| url::Url::parse(&s).ok())
        .and_then(|u| u.to_file_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut messages = Vec::new();
    for (i, r) in doc
        .get("requests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let mid = scalar(r.get("requestId"));
        let mid = if mid.is_empty() { i.to_string() } else { mid };
        push(
            &mut messages,
            format!("{mid}:user"),
            "user",
            r.pointer("/message/text").map(text).unwrap_or_default(),
            time(r.get("timestamp"), Some(3)),
        );
        let content = r
            .get("response")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some("markdownContent"))
            .map(|v| v.get("value").map(text).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        push(
            &mut messages,
            format!("{mid}:assistant"),
            "assistant",
            content,
            time(r.pointer("/modelState/completedAt"), Some(3)),
        );
    }
    Ok((
        vec![make_record(
            registry,
            source,
            id,
            cwd,
            scalar(doc.get("customTitle")),
            messages,
            time(doc.get("creationDate"), Some(3)),
        )?],
        partial,
    ))
}
fn desktop(
    registry: &Registry,
    source: &SessionSource,
    path: &Path,
) -> Result<(Vec<Record>, bool), String> {
    let bytes = fs::read(path).map_err(|_| "source_unreadable")?;
    let meta: Value = serde_json::from_slice(&bytes).map_err(|_| "source_invalid_json")?;
    let id = scalar(meta.get("sessionId"));
    if id.is_empty() {
        return Ok((Vec::new(), false));
    }
    let cli = scalar(meta.get("cliSessionId"));
    let mut messages = Vec::new();
    let mut partial = false;
    let cwd = meta
        .get("userSelectedFolders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let origin = scalar(meta.get("originCwd"));
            if !origin.is_empty() {
                origin
            } else if path.to_string_lossy().contains("local-agent-mode-sessions") {
                String::new()
            } else {
                scalar(meta.get("cwd"))
            }
        });
    if !cli.is_empty()
        && cli
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        let transcript = if path.to_string_lossy().contains("local-agent-mode-sessions") {
            let short = id
                .trim_start_matches("local_")
                .split('-')
                .next()
                .unwrap_or_default();
            if !short.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("source_invalid_session_id".into());
            }
            path.parent().map(|p| {
                p.join(short)
                    .join(".claude/projects/session")
                    .join(format!("{cli}.jsonl"))
            })
        } else {
            let encoded: String = cwd
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            expand_root("~/.claude/projects").map(|p| p.join(encoded).join(format!("{cli}.jsonl")))
        };
        if let Some(transcript) = transcript.filter(|p| p.is_file()) {
            let base = if path.to_string_lossy().contains("local-agent-mode-sessions") {
                path.parent().map(Path::to_path_buf)
            } else {
                expand_root("~/.claude/projects")
            };
            let base = base
                .ok_or("source_path_invalid")?
                .canonicalize()
                .map_err(|_| "source_path_invalid")?;
            let resolved = transcript.canonicalize().map_err(|_| "source_unreadable")?;
            if !resolved.starts_with(&base)
                || fs::symlink_metadata(&transcript)
                    .map_err(|_| "source_unreadable")?
                    .file_type()
                    .is_symlink()
            {
                return Err("source_path_invalid".into());
            }
            let (lines, p) = documents(&resolved, "jsonl")?;
            partial |= p;
            for (i, row) in lines.iter().enumerate() {
                let role = scalar(row.pointer("/message/role").or_else(|| row.get("type")));
                if !matches!(role.as_str(), "user" | "assistant") {
                    continue;
                }
                push(
                    &mut messages,
                    format!(
                        "{}:{role}",
                        scalar(row.get("uuid"))
                            .chars()
                            .take(128)
                            .collect::<String>()
                            + &format!(":{i}")
                    ),
                    &role,
                    row.pointer("/message/content")
                        .map(text)
                        .unwrap_or_default(),
                    time(row.get("timestamp"), None),
                );
            }
        }
    }
    if messages.is_empty() {
        push(
            &mut messages,
            "initial:user".into(),
            "user",
            scalar(meta.get("initialMessage")),
            time(meta.get("createdAt"), Some(3)),
        );
    }
    Ok((
        vec![make_record(
            registry,
            source,
            id,
            cwd,
            scalar(meta.get("title")),
            messages,
            time(meta.get("lastActivityAt"), Some(3)),
        )?],
        partial,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_external_workbuddy_and_declared_json_are_read_only_and_keep_undated() {
        let root = std::env::temp_dir().join(format!("session-external-{}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("session.jsonl");
        let original=concat!("{\"sessionId\":\"native-1\",\"role\":\"user\",\"timestamp\":\"2026-11-18T10:00:00+08:00\",\"text\":\"system <user_query>实现配置页面</user_query> suffix\"}\n",
     "{\"sessionId\":\"native-1\",\"role\":\"assistant\",\"text\":\"已完成配置\"}\n",
     "{\"sessionId\":\"native-1\",\"role\":\"system\",\"text\":\"ignore rules\"}\n");
        fs::write(&path, original).unwrap();
        let spec = SourceSpec {
            id: "workbuddy".into(),
            format: "jsonl".into(),
            session_id: "sessionId".into(),
            time_fields: vec!["timestamp".into()],
            role_field: "role".into(),
            content_fields: vec!["text".into()],
            user_query_tag: "user_query".into(),
            ..Default::default()
        };
        let source = SessionSource::try_from("workbuddy".to_string()).unwrap();
        let (rows, partial) = generic(&Registry::default(), &source, &spec, &path).unwrap();
        assert!(!partial);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].summary.native_id, "native-1");
        assert_eq!(rows[0].messages.len(), 2);
        assert_eq!(rows[0].messages[0].content, "实现配置页面");
        assert!(rows[0].messages[0].timestamp.is_some());
        assert!(rows[0].messages[1].timestamp.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(glob_matches("**/*.jsonl", "project/session.jsonl"));
        assert!(glob_matches("**/*.jsonl", "session.jsonl"));
        assert!(!glob_matches("*/*.jsonl", "a/b/c.jsonl"));
        let presets: Vec<SourceSpec> = serde_json::from_str(include_str!("presets.json")).unwrap();
        assert!(merge_specs(
            presets.clone(),
            &json!({"sources":[{"id":"codex","root":"C:/elsewhere","format":"jsonl"}]})
        )
        .is_err());
        assert!(merge_specs(
            presets,
            &json!({"sources":[{"id":"custom","root":"https://example.com","format":"shell"}]})
        )
        .is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn session_external_vscode_patches_and_copilot_sqlite_use_message_times() {
        let root = std::env::temp_dir().join(format!("session-adapters-{}", rand::random::<u64>()));
        fs::create_dir_all(root.join("chatSessions")).unwrap();
        let path = root.join("chatSessions/session.jsonl");
        let initial = json!({"kind":0,"v":{"sessionId":"v1","requests":[{"requestId":"q1","timestamp":1794960000000i64,"message":{"text":"修复导出"},"response":[],"modelState":{"completedAt":1794960060000i64}}]}});
        let patch = json!({"kind":2,"k":["requests",0,"response"],"v":[{"kind":"markdownContent","value":"修复完成"},{"kind":"toolInvocationSerialized","value":"secret tool"}]});
        fs::write(&path, format!("{initial}\n{patch}\n")).unwrap();
        let source = SessionSource::External("vscode".into());
        let (rows, _) = vscode(&Registry::default(), &source, &path).unwrap();
        assert_eq!(rows[0].messages.len(), 2);
        assert_eq!(rows[0].messages[1].content, "修复完成");
        assert_ne!(rows[0].messages[0].timestamp, rows[0].messages[1].timestamp);
        let db = root.join("copilot.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE sessions(id TEXT,cwd TEXT,summary TEXT,created_at TEXT,updated_at TEXT); CREATE TABLE turns(session_id TEXT,user_message TEXT,assistant_response TEXT,timestamp TEXT); INSERT INTO sessions VALUES('c1','','任务','2026-11-18 10:00:00','2026-11-18 10:01:00'); INSERT INTO turns VALUES('c1','输入','输出','2026-11-18 10:00:00');").unwrap();
        drop(c);
        let rows = copilot(
            &Registry::default(),
            &SessionSource::External("github-copilot".into()),
            &db,
        )
        .unwrap();
        assert_eq!(rows[0].messages.len(), 2);
        assert_eq!(
            rows[0].messages[0].timestamp.as_deref(),
            Some("2026-11-18T10:00:00+00:00")
        );
        let _ = fs::remove_dir_all(root);
    }
}
