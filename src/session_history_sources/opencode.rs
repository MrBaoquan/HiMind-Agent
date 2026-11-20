use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::session_history::{
    self, SessionCapabilities, SessionSource, SessionSummary, SessionTranscriptMessage,
    SourceAvailability,
};
use crate::workspace_registry::Registry;

use super::ProviderScan;

const MAX_OPENCODE_DATABASES: usize = 50;
const MAX_OPENCODE_SESSIONS: usize = 10_000;
const MAX_OPENCODE_MESSAGES: i64 = 20_000;
const MAX_OPENCODE_TRANSCRIPT_BYTES: i64 = 32 * 1024 * 1024;
const MAX_OPENCODE_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn scan_opencode(registry: &Registry) -> ProviderScan {
    let Some(base) = opencode_base_dir() else {
        return super::empty_scan(
            SessionSource::OpenCode,
            SourceAvailability::Unavailable,
            "data_directory_unavailable",
        );
    };
    let paths = opencode_database_paths(&base);
    scan_opencode_at(registry, &paths, base.exists())
}

fn opencode_base_dir() -> Option<PathBuf> {
    if let Some(path) = crate::session_history_sources::external::native_root("opencode") {
        return Some(path);
    }
    if let Some(value) = std::env::var_os("OPENCODE_DATA_DIR").filter(|value| !value.is_empty()) {
        let path = resolve_user_path(PathBuf::from(value))?;
        return path.is_absolute().then_some(path);
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        let path = resolve_user_path(PathBuf::from(xdg))?;
        return path.is_absolute().then_some(path.join("opencode"));
    }
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            let current = PathBuf::from(app_data).join("opencode");
            if current.join("opencode.db").is_file() {
                return Some(current);
            }
            if let Some(home) = std::env::var_os("USERPROFILE") {
                let legacy = PathBuf::from(home).join(".local/share/opencode");
                if legacy.join("opencode.db").is_file() {
                    return Some(legacy);
                }
            }
            return Some(current);
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    return Some(home.join("Library/Application Support/opencode"));
    #[cfg(not(target_os = "macos"))]
    Some(home.join(".local/share/opencode"))
}

fn resolve_user_path(path: PathBuf) -> Option<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)?;
        return Some(home.join(rest));
    }
    Some(path)
}

fn opencode_database_paths(base: &Path) -> Vec<PathBuf> {
    let configured = std::env::var_os("OPENCODE_DB")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let disable_channels = matches!(
        std::env::var("OPENCODE_DISABLE_CHANNEL_DB").as_deref(),
        Ok("1" | "true")
    );
    opencode_database_paths_at(base, configured.as_deref(), disable_channels)
}

fn opencode_database_paths_at(
    base: &Path,
    configured: Option<&Path>,
    disable_channels: bool,
) -> Vec<PathBuf> {
    let mut paths = if let Some(value) = configured {
        if value == Path::new(":memory:") {
            return Vec::new();
        }
        vec![if value.is_absolute() {
            value.to_path_buf()
        } else {
            base.join(value)
        }]
    } else {
        let mut paths = vec![base.join("opencode.db")];
        if !disable_channels {
            if let Ok(entries) = fs::read_dir(base) {
                for entry in entries.flatten().take(MAX_OPENCODE_DATABASES * 4) {
                    let name = entry.file_name();
                    let Some(channel) = name
                        .to_str()
                        .and_then(|name| name.strip_prefix("opencode-"))
                        .and_then(|name| name.strip_suffix(".db"))
                    else {
                        continue;
                    };
                    let safe_channel = !channel.is_empty()
                        && channel.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                        });
                    if safe_channel && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                        paths.push(entry.path());
                    }
                }
            }
        }
        paths
    };
    let canonical_base = fs::canonicalize(base).ok();
    paths.retain(|path| {
        let is_file = fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        is_file
            && path.canonicalize().is_ok_and(|actual| {
                actual.is_file()
                    && canonical_base
                        .as_ref()
                        .is_some_and(|root| actual.starts_with(root))
            })
    });
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_OPENCODE_DATABASES);
    paths
}

fn scan_opencode_at(registry: &Registry, paths: &[PathBuf], base_exists: bool) -> ProviderScan {
    if paths.is_empty() {
        return super::empty_scan(
            SessionSource::OpenCode,
            SourceAvailability::Unavailable,
            if base_exists {
                "database_not_found"
            } else {
                "source_not_found"
            },
        );
    }
    let mut sessions = Vec::new();
    let mut successes = 0usize;
    let mut failures = 0usize;
    let mut partial = false;
    for path in paths.iter().take(MAX_OPENCODE_DATABASES) {
        match scan_database(registry, path) {
            Ok((mut rows, truncated)) => {
                successes += 1;
                partial |= truncated;
                sessions.append(&mut rows);
            }
            Err(_) => failures += 1,
        }
    }
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let session_count = sessions.len();
    let (availability, error_code) = if failures == 0 && !partial {
        (SourceAvailability::Available, None)
    } else if successes > 0 {
        (
            SourceAvailability::Partial,
            Some("database_scan_partial".to_string()),
        )
    } else {
        (
            SourceAvailability::Error,
            Some("database_unreadable".to_string()),
        )
    };
    ProviderScan {
        sessions,
        status: crate::session_history::SessionSourceStatus {
            source: SessionSource::OpenCode,
            availability,
            session_count,
            error_code,
        },
    }
}

fn scan_database(registry: &Registry, path: &Path) -> Result<(Vec<SessionSummary>, bool), String> {
    let connection = open_read_only(path)?;
    let table = session_table(&connection)?;
    let columns = table_columns(&connection, table)?;
    if !["id", "title", "directory"]
        .iter()
        .all(|name| columns.contains(*name))
    {
        return Err("OpenCode session schema is unsupported".to_string());
    }
    let created = if columns.contains("time_created") {
        "CAST(time_created AS INTEGER)"
    } else {
        "NULL"
    };
    let updated = if columns.contains("time_updated") {
        "CAST(time_updated AS INTEGER)"
    } else {
        created
    };
    let order = if columns.contains("time_updated") {
        "time_updated DESC"
    } else {
        "id DESC"
    };
    let sql = format!(
        "SELECT id, title, directory, {created}, {updated} FROM {table} ORDER BY {order} LIMIT {}",
        MAX_OPENCODE_SESSIONS + 1
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut sessions = Vec::new();
    let mut partial = false;
    let mut rows_seen = 0usize;
    for row in rows {
        rows_seen += 1;
        if rows_seen > MAX_OPENCODE_SESSIONS {
            partial = true;
            break;
        }
        let (native_id, title, directory, created, updated) = match row {
            Ok(row) => row,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        if native_id.trim().is_empty() || directory.trim().is_empty() {
            partial = true;
            continue;
        }
        let association = match session_history::associate_workspace(registry, &directory) {
            Ok(value) => value,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        let title = if title.trim().is_empty() {
            format!(
                "OpenCode 会话 {}",
                native_id.chars().take(8).collect::<String>()
            )
        } else {
            title.chars().take(200).collect()
        };
        if let Ok(summary) = SessionSummary::new(
            SessionSource::OpenCode,
            native_id,
            directory,
            title,
            updated.or(created).and_then(timestamp_from_millis),
            None,
            SessionCapabilities {
                can_preview: true,
                can_open_original: false,
                can_resume: false,
            },
            association,
        ) {
            sessions.push(summary);
        } else {
            partial = true;
        }
    }
    Ok((sessions, partial))
}

pub(crate) fn opencode_transcript_page(
    registry: &Registry,
    project_id: Option<&str>,
    native_id: &str,
    end_offset: Option<usize>,
    page_size: usize,
    activity: bool,
) -> Result<session_history::SessionTranscriptPage, String> {
    SessionSource::OpenCode.stable_session_id(native_id)?;
    let base = opencode_base_dir().ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    opencode_transcript_page_scoped_from_paths(
        registry,
        project_id,
        native_id,
        &opencode_database_paths(&base),
        end_offset,
        page_size,
        activity,
    )
}

#[cfg(test)]
fn opencode_transcript_page_from_paths(
    registry: &Registry,
    project_id: &str,
    native_id: &str,
    paths: &[PathBuf],
    end_offset: Option<usize>,
    page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    opencode_transcript_page_scoped_from_paths(
        registry,
        Some(project_id),
        native_id,
        paths,
        end_offset,
        page_size,
        false,
    )
}

fn opencode_transcript_page_scoped_from_paths(
    registry: &Registry,
    project_id: Option<&str>,
    native_id: &str,
    paths: &[PathBuf],
    end_offset: Option<usize>,
    page_size: usize,
    activity: bool,
) -> Result<session_history::SessionTranscriptPage, String> {
    let session_id = SessionSource::OpenCode.stable_session_id(native_id)?;
    for path in paths.iter().take(MAX_OPENCODE_DATABASES) {
        let Ok(connection) = open_read_only(path) else {
            continue;
        };
        let Ok(directory) = session_directory(&connection, native_id) else {
            continue;
        };
        let association = match session_history::associate_workspace(registry, &directory) {
            Ok(association) => association,
            Err(_) => continue,
        };
        if project_id
            .is_some_and(|id| association.as_ref().map(|a| a.project_id.as_str()) != Some(id))
        {
            continue;
        }
        let selection =
            read_opencode_messages(&connection, native_id, end_offset, page_size, activity)?;
        return Ok(session_history::SessionTranscriptPage {
            snapshot_cursor: Some(selection.snapshot_cursor),
            session_id,
            through_seq: Some(selection.through_seq),
            messages: selection.messages,
            next_cursor: selection.next_cursor,
        });
    }
    Err("SESSION_NOT_FOUND".to_string())
}

struct TranscriptSelection {
    snapshot_cursor: String,
    messages: Vec<SessionTranscriptMessage>,
    through_seq: i64,
    next_cursor: Option<String>,
}

fn read_opencode_messages(
    connection: &Connection,
    session_id: &str,
    end_offset: Option<usize>,
    page_size: usize,
    activity: bool,
) -> Result<TranscriptSelection, String> {
    if page_size == 0 || page_size > 100 {
        return Err("transcript page size is invalid".to_string());
    }
    let v2_columns = table_columns(connection, "session_message").unwrap_or_default();
    let has_v2 = ["session_id", "type", "seq", "time_created", "data"]
        .iter()
        .all(|name| v2_columns.contains(*name));
    if has_v2 {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM session_message WHERE session_id=?1",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if count > 0
            || !table_columns(connection, "message")
                .is_ok_and(|columns| columns.contains("session_id"))
        {
            return read_v2_page(
                connection, session_id, count, end_offset, page_size, activity,
            );
        }
    }
    read_legacy_page(connection, session_id, end_offset, page_size)
}

fn read_v2_page(
    connection: &Connection,
    session_id: &str,
    total: i64,
    end_offset: Option<usize>,
    page_size: usize,
    activity: bool,
) -> Result<TranscriptSelection, String> {
    if !activity {
        check_transcript_budget(connection, "session_message", session_id, total)?;
    }
    let total = usize::try_from(total).map_err(|_| "SESSION_PREVIEW_TOO_LARGE".to_string())?;
    let end = end_offset.unwrap_or(total);
    if end > total {
        return Err("SESSION_CURSOR_INVALID".to_string());
    }
    let offset = total.saturating_sub(end);
    let start = end.saturating_sub(page_size);
    let mut statement = connection
        .prepare(
            "SELECT type, time_created, CASE WHEN LENGTH(CAST(data AS BLOB)) <= ?4 \
             THEN data ELSE NULL END, id FROM session_message \
             WHERE session_id=?1 ORDER BY seq DESC LIMIT ?2 OFFSET ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![
                session_id,
                page_size as i64,
                offset as i64,
                if activity {
                    2 * 1024 * 1024
                } else {
                    MAX_OPENCODE_TRANSCRIPT_BYTES
                }
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    let mut page_bytes = 0usize;
    for row in rows {
        let (kind, created, data, id) = row.map_err(|error| error.to_string())?;
        let data = data.ok_or_else(|| "SESSION_PREVIEW_TOO_LARGE".to_string())?;
        page_bytes += data.len();
        if page_bytes > MAX_OPENCODE_MESSAGE_BYTES {
            return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
        }
        if activity && kind == "assistant" {
            messages.extend(opencode_execution(&id, &data));
        }
        if let Some(mut message) = parse_opencode_message(&kind, created, &data, messages.len()) {
            message.id = id;
            messages.push(message);
        }
    }
    messages.reverse();
    Ok(TranscriptSelection {
        snapshot_cursor: end.to_string(),
        messages,
        through_seq: end.saturating_sub(1) as i64,
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

fn read_legacy_page(
    connection: &Connection,
    session_id: &str,
    end_offset: Option<usize>,
    page_size: usize,
) -> Result<TranscriptSelection, String> {
    let message_columns = table_columns(connection, "message")?;
    if !["id", "session_id", "time_created", "data"]
        .iter()
        .all(|name| message_columns.contains(*name))
    {
        return Err("OpenCode transcript schema is unsupported".to_string());
    }
    let total: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM message WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    check_transcript_budget(connection, "message", session_id, total)?;
    let part_columns = table_columns(connection, "part").unwrap_or_default();
    if part_columns.contains("message_id") && part_columns.contains("data") {
        let (part_rows, part_bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(data AS BLOB))), 0) \
                 FROM part WHERE message_id IN (SELECT id FROM message WHERE session_id=?1)",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        if total.saturating_add(part_rows) > MAX_OPENCODE_MESSAGES
            || part_bytes > MAX_OPENCODE_TRANSCRIPT_BYTES
        {
            return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
        }
    }
    let total = usize::try_from(total).map_err(|_| "SESSION_PREVIEW_TOO_LARGE".to_string())?;
    let end = end_offset.unwrap_or(total);
    if end > total {
        return Err("SESSION_CURSOR_INVALID".to_string());
    }
    let offset = total.saturating_sub(end);
    let start = end.saturating_sub(page_size);
    let mut statement = connection
        .prepare(
            "SELECT id, time_created, data FROM message WHERE session_id=?1 \
             ORDER BY time_created DESC, id DESC LIMIT ?2 OFFSET ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![session_id, page_size as i64, offset as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    for row in rows {
        let (id, created, data) = row.map_err(|error| error.to_string())?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let role = value
            .get("role")
            .or_else(|| value.pointer("/info/role"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let mut content = value
            .get("content")
            .or_else(|| value.get("text"))
            .map(extract_text)
            .unwrap_or_default();
        if part_columns.contains("message_id") && part_columns.contains("data") {
            content.push_str(&load_legacy_parts(connection, &id, &part_columns)?);
        }
        if content.trim().is_empty() {
            continue;
        }
        messages.push(SessionTranscriptMessage {
            execution: None,
            files: Vec::new(),
            id,
            role: role.to_string(),
            content,
            timestamp: timestamp_from_millis(created),
        });
        if messages.iter().fold(0usize, |sum, message| {
            sum.saturating_add(message.content.len())
        }) > MAX_OPENCODE_MESSAGE_BYTES
        {
            return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
        }
    }
    messages.reverse();
    Ok(TranscriptSelection {
        snapshot_cursor: end.to_string(),
        messages,
        through_seq: end.saturating_sub(1) as i64,
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

fn load_legacy_parts(
    connection: &Connection,
    message_id: &str,
    columns: &HashSet<String>,
) -> Result<String, String> {
    let order = if columns.contains("time_created") {
        "time_created ASC"
    } else {
        "rowid ASC"
    };
    let mut statement = connection
        .prepare(&format!(
            "SELECT data FROM part WHERE message_id=?1 ORDER BY {order}"
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([message_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut content = String::new();
    for row in rows {
        let data = row.map_err(|error| error.to_string())?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = value.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
            }
            Some("tool") => {
                let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
                content.push_str(&format!("\n[工具: {name}]"));
            }
            _ => {}
        }
        if content.len() > MAX_OPENCODE_MESSAGE_BYTES {
            return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
        }
    }
    Ok(content)
}

fn opencode_execution(id: &str, data: &str) -> Vec<SessionTranscriptMessage> {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    let mut add = |key: String, start: Option<i64>, end: Option<i64>| {
        if let (Some(started_at_ms), Some(completed_at_ms)) = (start, end) {
            if started_at_ms > 0 && completed_at_ms > started_at_ms {
                messages.push(SessionTranscriptMessage {
                    execution: Some(session_history::SessionExecutionTiming {
                        basis: "opencode_item_timing_v1".into(),
                        started_at_ms,
                        completed_at_ms,
                    }),
                    id: format!("execution:{id}:{key}"),
                    role: "execution".into(),
                    content: String::new(),
                    timestamp: None,
                    files: Vec::new(),
                });
            }
        }
    };
    let millis = |v: &Value, key: &str| v.get(key).and_then(Value::as_i64);
    let timing = &value["time"];
    let mut stream_end = millis(timing, "streamed");
    if let Some(parts) = value.get("content").and_then(Value::as_array) {
        for (index, part) in parts.iter().enumerate() {
            let timing = &part["time"];
            match part.get("type").and_then(Value::as_str) {
                Some("reasoning") => add(
                    format!("reasoning:{index}"),
                    millis(timing, "created"),
                    millis(timing, "completed"),
                ),
                Some("tool") => {
                    let start = millis(timing, "ran");
                    if let (Some(end), Some(start)) = (stream_end, start) {
                        stream_end = Some(end.min(start));
                    }
                    if matches!(
                        part.get("name").and_then(Value::as_str),
                        Some(
                            "shell"
                                | "read"
                                | "grep"
                                | "edit"
                                | "write"
                                | "glob"
                                | "patch"
                                | "websearch"
                                | "webfetch"
                        )
                    ) {
                        add(format!("tool:{index}"), start, millis(timing, "completed"));
                    }
                }
                _ => {}
            }
        }
    }
    add("stream".into(), millis(timing, "created"), stream_end);
    messages
}

fn parse_opencode_message(
    kind: &str,
    created: i64,
    data: &str,
    index: usize,
) -> Option<SessionTranscriptMessage> {
    let value = serde_json::from_str::<Value>(data).ok()?;
    let role = match kind {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let content = if role == "user" {
        value
            .get("text")
            .or_else(|| value.get("content"))
            .map(extract_text)
            .unwrap_or_default()
    } else {
        value
            .get("content")
            .map(extract_assistant_content)
            .unwrap_or_default()
    };
    (!content.trim().is_empty()).then(|| SessionTranscriptMessage {
        execution: None,
        files: Vec::new(),
        id: index.to_string(),
        role: role.to_string(),
        content,
        timestamp: timestamp_from_millis(created),
    })
}

fn extract_assistant_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part.get("text").and_then(Value::as_str).map(str::to_string),
                Some("tool") => part
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| format!("[工具: {name}]")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn extract_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn check_transcript_budget(
    connection: &Connection,
    table: &str,
    session_id: &str,
    count: i64,
) -> Result<(), String> {
    if count > MAX_OPENCODE_MESSAGES {
        return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
    }
    let sql = format!(
        "SELECT COALESCE(SUM(LENGTH(CAST(data AS BLOB))), 0) FROM {table} WHERE session_id=?1"
    );
    let bytes: i64 = connection
        .query_row(&sql, [session_id], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if bytes > MAX_OPENCODE_TRANSCRIPT_BYTES {
        return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
    }
    Ok(())
}

fn session_table(connection: &Connection) -> Result<&'static str, String> {
    for table in ["session_v2", "session"] {
        let columns = table_columns(connection, table)?;
        if ["id", "directory", "title"]
            .iter()
            .all(|name| columns.contains(*name))
        {
            return Ok(table);
        }
    }
    Err("OpenCode session schema is unsupported".into())
}

fn session_directory(connection: &Connection, session_id: &str) -> Result<String, String> {
    let table = session_table(connection)?;
    let columns = table_columns(connection, table)?;
    if !columns.contains("id") || !columns.contains("directory") {
        return Err("OpenCode session schema is unsupported".to_string());
    }
    connection
        .query_row(
            &format!("SELECT directory FROM {table} WHERE id=?1"),
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("PRAGMA query_only = ON")
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| error.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .map(|row| row.map_err(|error| error.to_string()))
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(columns)
}

fn timestamp_from_millis(value: i64) -> Option<String> {
    let timestamp = if value.unsigned_abs() >= 10_000_000_000 {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
    } else {
        chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0)
    };
    timestamp.map(|value| value.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_registry::{
        RegistryProject, RegistryWorkspace, CONFIDENCE_EXACT, KIND_PRODUCT, ROLE_ROOT,
        SOURCE_MANUAL,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("opencode-history-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn registry() -> Registry {
        Registry {
            projects: vec![RegistryProject {
                id: "project-a".to_string(),
                name: "Project A".to_string(),
                kind: KIND_PRODUCT.to_string(),
                ..Default::default()
            }],
            workspaces: vec![RegistryWorkspace {
                id: "root-a".to_string(),
                project_id: "project-a".to_string(),
                path: "F:/work/project-a".to_string(),
                role: ROLE_ROOT.to_string(),
                source: SOURCE_MANUAL.to_string(),
                confidence: CONFIDENCE_EXACT.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn create_v2_database(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
                 CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, data TEXT);
                 INSERT INTO session VALUES ('ses_synthetic', 'Synthetic OpenCode task', 'F:/work/project-a', 1794391200000, 1794391203000);
                 INSERT INTO session_message VALUES ('m1', 'ses_synthetic', 'user', 1, 1794391201000, '{\"text\":\"synthetic prompt\"}');
                 INSERT INTO session_message VALUES ('m2', 'ses_synthetic', 'assistant', 2, 1794391202000, '{\"content\":[{\"type\":\"text\",\"text\":\"synthetic reply\"},{\"type\":\"reasoning\",\"text\":\"private reasoning\"}]}');",
            )
            .unwrap();
        for seq in 3..=102 {
            let kind = if seq % 2 == 0 { "assistant" } else { "user" };
            let data = if kind == "user" {
                serde_json::json!({"text":format!("synthetic message {seq}")})
            } else {
                serde_json::json!({"content":[{"type":"text","text":format!("synthetic message {seq}")}]})
            };
            connection
                .execute(
                    "INSERT INTO session_message VALUES (?1, 'ses_synthetic', ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        format!("m{seq}"),
                        kind,
                        seq,
                        1_794_391_200_000i64 + i64::from(seq),
                        data.to_string()
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn opencode_session_unassigned_retains_metadata_and_read_only_body() {
        let temp = TempRoot::new();
        let path = temp.0.join("session.db");
        create_v2_database(&path);
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE session SET directory='G:/synthetic-unregistered'",
                [],
            )
            .unwrap();
        let paths = vec![path];
        let scan = scan_opencode_at(&registry(), &paths, true);
        assert!(scan.sessions.iter().any(|s| s.project_id.is_none()));
        assert!(opencode_transcript_page_scoped_from_paths(
            &registry(),
            None,
            "ses_synthetic",
            &paths,
            None,
            50,
            false,
        )
        .is_ok());
        assert!(opencode_transcript_page_from_paths(
            &registry(),
            "project-a",
            "ses_synthetic",
            &paths,
            None,
            50
        )
        .is_err());
    }

    #[test]
    fn opencode_database_discovery_restricts_explicit_paths_to_the_source_root() {
        let temp = TempRoot::new();
        let base = temp.0.join("source");
        fs::create_dir_all(&base).unwrap();
        let allowed = base.join("custom.db");
        let outside = temp.0.join("outside.db");
        fs::write(&allowed, b"synthetic database").unwrap();
        fs::write(&outside, b"outside database").unwrap();

        assert_eq!(
            opencode_database_paths_at(&base, Some(Path::new("custom.db")), false),
            vec![allowed.clone()]
        );
        assert_eq!(
            opencode_database_paths_at(&base, Some(&allowed), false),
            vec![allowed]
        );
        assert!(opencode_database_paths_at(&base, Some(&outside), false).is_empty());
        assert!(
            opencode_database_paths_at(&base, Some(Path::new("../outside.db")), false).is_empty()
        );
        assert!(opencode_database_paths_at(&base, Some(Path::new(":memory:")), false).is_empty());
        assert!(opencode_database_paths_at(&base, Some(Path::new("missing.db")), false).is_empty());
    }

    #[test]
    fn opencode_database_discovery_rejects_redirected_parent_directories() {
        let temp = TempRoot::new();
        let base = temp.0.join("source");
        let outside = temp.0.join("outside");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let database = outside.join("external.db");
        fs::write(&database, b"outside database").unwrap();
        let directory_link = base.join("redirect");
        #[cfg(windows)]
        {
            // Junctions exercise canonical containment without requiring Windows
            // developer mode or the symbolic-link creation privilege.
            let result = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&directory_link)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "junction creation failed: {result:?}"
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &directory_link).unwrap();
        assert!(opencode_database_paths_at(
            &base,
            Some(&directory_link.join("external.db")),
            false
        )
        .is_empty());
        fs::remove_dir(&directory_link).unwrap_or_else(|error| {
            #[cfg(unix)]
            if fs::remove_file(&directory_link).is_ok() {
                return;
            }
            panic!("failed to remove test directory link: {error}");
        });
        assert!(database.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn opencode_database_discovery_rejects_default_and_explicit_file_symlinks() {
        let temp = TempRoot::new();
        let base = temp.0.join("source");
        fs::create_dir_all(&base).unwrap();
        let database = temp.0.join("outside.db");
        fs::write(&database, b"outside database").unwrap();
        let file_link = base.join("opencode.db");
        std::os::unix::fs::symlink(&database, &file_link).unwrap();
        assert!(opencode_database_paths_at(&base, None, false).is_empty());
        assert!(opencode_database_paths_at(&base, Some(&file_link), false).is_empty());
    }

    #[test]
    fn opencode_metadata_scan_is_project_scoped_and_read_only() {
        let temp = TempRoot::new();
        let path = temp.0.join("opencode.db");
        create_v2_database(&path);

        let scan = scan_opencode_at(&registry(), std::slice::from_ref(&path), true);
        assert_eq!(scan.status.availability, SourceAvailability::Available);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].id, "opencode:ses_synthetic");
        assert_eq!(scan.sessions[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(scan.sessions[0].title, "Synthetic OpenCode task");
        assert!(scan.sessions[0].capabilities.can_preview);
        assert!(!scan.sessions[0].capabilities.can_open_original);
        assert!(!scan.sessions[0].capabilities.can_resume);
        let connection = open_read_only(&path).unwrap();
        assert!(connection
            .execute("DELETE FROM session WHERE id=?1", ["ses_synthetic"])
            .is_err());
    }

    #[test]
    fn opencode_preview_pages_messages_and_rechecks_project_ownership() {
        let temp = TempRoot::new();
        let path = temp.0.join("opencode.db");
        create_v2_database(&path);
        let page = opencode_transcript_page_from_paths(
            &registry(),
            "project-a",
            "ses_synthetic",
            std::slice::from_ref(&path),
            None,
            50,
        )
        .unwrap();
        assert_eq!(page.session_id, "opencode:ses_synthetic");
        assert_eq!(page.messages.len(), 50);
        assert_eq!(page.next_cursor.as_deref(), Some("52"));
        let older = opencode_transcript_page_from_paths(
            &registry(),
            "project-a",
            "ses_synthetic",
            std::slice::from_ref(&path),
            Some(52),
            50,
        )
        .unwrap();
        assert_eq!(older.messages.len(), 50);
        assert_eq!(older.next_cursor.as_deref(), Some("2"));
        let oldest = opencode_transcript_page_from_paths(
            &registry(),
            "project-a",
            "ses_synthetic",
            std::slice::from_ref(&path),
            Some(2),
            50,
        )
        .unwrap();
        assert_eq!(oldest.messages.len(), 2);
        assert!(oldest
            .messages
            .iter()
            .any(|message| message.content == "synthetic prompt"));
        assert!(!oldest
            .messages
            .iter()
            .any(|message| message.content.contains("reasoning")));
        assert_eq!(
            opencode_transcript_page_from_paths(
                &registry(),
                "another-project",
                "ses_synthetic",
                std::slice::from_ref(&path),
                None,
                50,
            )
            .unwrap_err(),
            "SESSION_NOT_FOUND"
        );
    }

    #[test]
    fn opencode_activity_snapshot_keeps_expanded_page_stable_after_append() {
        let temp = TempRoot::new();
        let path = temp.0.join("activity-page.db");
        create_v2_database(&path);
        let connection = Connection::open(&path).unwrap();
        let parts: Vec<_> = (0..250).map(|_| serde_json::json!({
            "type":"reasoning", "time":{"created":1794391200000i64,"completed":1794391201000i64}
        })).collect();
        let data = serde_json::json!({"content":parts}).to_string();
        connection
            .execute(
                "UPDATE session_message SET data=?1 WHERE id='m102'",
                [&data],
            )
            .unwrap();
        let load = |end| {
            opencode_transcript_page_scoped_from_paths(
                &registry(),
                Some("project-a"),
                "ses_synthetic",
                std::slice::from_ref(&path),
                end,
                50,
                true,
            )
            .unwrap()
        };
        let first = load(None);
        assert!(first.messages.len() > 200);
        assert_eq!(first.snapshot_cursor.as_deref(), Some("102"));
        connection.execute_batch(
            "INSERT INTO session_message VALUES ('m103','ses_synthetic','user',103,1794391203000,'{\"text\":\"new message\"}');"
        ).unwrap();
        let resumed = load(Some(
            first.snapshot_cursor.as_ref().unwrap().parse().unwrap(),
        ));
        assert_eq!(first, resumed);
    }

    #[test]
    fn opencode_legacy_message_parts_are_read_with_a_bounded_page() {
        let temp = TempRoot::new();
        let path = temp.0.join("opencode-legacy.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, time_created INTEGER, data TEXT);
                 INSERT INTO session VALUES ('ses_legacy', 'Legacy task', 'F:/work/project-a', 1794391200000, 1794391201000);
                 INSERT INTO message VALUES ('msg-1', 'ses_legacy', 1794391200500, '{\"info\":{\"role\":\"user\"}}');
                 INSERT INTO part VALUES ('part-1', 'msg-1', 1794391200600, '{\"type\":\"text\",\"text\":\"legacy synthetic prompt\"}');",
            )
            .unwrap();
        drop(connection);

        let page = opencode_transcript_page_from_paths(
            &registry(),
            "project-a",
            "ses_legacy",
            std::slice::from_ref(&path),
            None,
            50,
        )
        .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].content, "legacy synthetic prompt");
    }
    #[test]
    fn opencode_session_v2_table_uses_native_message_ids_across_pages() {
        let temp = TempRoot::new();
        let path = temp.0.join("session-v2.db");
        create_v2_database(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("ALTER TABLE session RENAME TO session_v2")
            .unwrap();
        drop(connection);
        let paths = vec![path.clone()];
        let scan = scan_opencode_at(&registry(), &paths, true);
        assert_eq!(scan.sessions.len(), 1);
        let connection = open_read_only(&path).unwrap();
        assert_eq!(
            session_directory(&connection, "ses_synthetic").unwrap(),
            "F:/work/project-a"
        );
        let first = read_opencode_messages(&connection, "ses_synthetic", None, 1, false).unwrap();
        let second = read_opencode_messages(
            &connection,
            "ses_synthetic",
            first.next_cursor.as_ref().map(|v| v.parse().unwrap()),
            1,
            false,
        )
        .unwrap();
        assert_eq!(first.messages[0].id, "m102");
        assert_eq!(second.messages[0].id, "m101");
    }
}
