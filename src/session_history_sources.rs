//! Bounded, read-only metadata adapters for local session providers.

mod codex;
mod opencode;
mod path_cache;

pub(crate) use codex::{codex_transcript_page, scan_codex};
pub(crate) use opencode::{opencode_transcript_page, scan_opencode};

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use walkdir::WalkDir;

use crate::session_history::{
    self, SessionCapabilities, SessionSource, SessionSourceStatus, SessionSummary,
    SessionTranscriptMessage, SourceAvailability,
};
use crate::workspace_registry::Registry;

const MAX_CLAUDE_FILES: usize = 10_000;
const MAX_CLAUDE_HEAD_BYTES: usize = 64 * 1024;
const MAX_CLAUDE_TAIL_BYTES: usize = 64 * 1024;
const MAX_CLAUDE_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CLAUDE_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CLAUDE_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_JSONL_PAGE_SCAN_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) struct ProviderScan {
    pub sessions: Vec<SessionSummary>,
    pub status: SessionSourceStatus,
}

pub(crate) fn scan_claude_code(registry: &Registry) -> ProviderScan {
    let Some(config_root) = claude_config_root() else {
        return empty_scan(
            SessionSource::ClaudeCode,
            SourceAvailability::Unavailable,
            "home_directory_unavailable",
        );
    };
    scan_claude_code_at(registry, &config_root.join("projects"))
}

pub(crate) fn claude_transcript_page(
    registry: &Registry, project_id: Option<&str>, native_id: &str,
    end_offset: Option<u64>, page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    let session_id = SessionSource::ClaudeCode.stable_session_id(native_id)?;
    let Some(config_root) = claude_config_root() else {
        return Err("SESSION_NOT_FOUND".to_string());
    };
    let page = claude_transcript_page_scoped_at(
        registry,
        project_id,
        native_id,
        &config_root.join("projects"),
        end_offset,
        page_size,
    )?;
    Ok(session_history::SessionTranscriptPage { session_id, ..page })
}

#[cfg(test)]
fn claude_transcript_page_at(
    registry: &Registry,
    project_id: &str,
    native_id: &str,
    root: &Path,
    end_offset: Option<u64>,
    page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    claude_transcript_page_scoped_at(registry, Some(project_id), native_id, root, end_offset, page_size)
}

fn claude_transcript_page_scoped_at(
    registry: &Registry, project_id: Option<&str>, native_id: &str, root: &Path,
    end_offset: Option<u64>, page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    let canonical_root = fs::canonicalize(root).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    let cache_key = path_cache::key(&canonical_root, "claude_code", project_id.unwrap_or("*"), native_id);
    let path = if let Some(path) = path_cache::get(&cache_key) {
        path
    } else {
        (|| -> Result<PathBuf, String> {
            let mut candidates = Vec::new();
            let mut files_seen = 0usize;
            for entry in WalkDir::new(&canonical_root)
                .follow_links(false)
                .max_depth(10)
            {
                let entry = entry.map_err(|_| "SESSION_NOT_FOUND".to_string())?;
                if entry.file_type().is_symlink()
                    || !entry.file_type().is_file()
                    || entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
                {
                    continue;
                }
                files_seen += 1;
                if files_seen > MAX_CLAUDE_FILES {
                    return Err("SESSION_PREVIEW_UNAVAILABLE".to_string());
                }
                let path =
                    fs::canonicalize(entry.path()).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
                if !path.starts_with(&canonical_root) {
                    continue;
                }
                let Ok(Some(summary)) = parse_claude_metadata(registry, &path) else {
                    continue;
                };
                if project_id.is_none_or(|id| summary.project_id.as_deref() == Some(id)) {
                    path_cache::remember(
                        path_cache::key(
                            &canonical_root,
                            "claude_code",
                            project_id.unwrap_or("*"),
                            &summary.native_id,
                        ),
                        &path,
                    );
                }
                if summary.native_id == native_id
                    && project_id.is_none_or(|id| summary.project_id.as_deref() == Some(id))
                {
                    let modified = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok();
                    candidates.push((modified, path));
                }
            }
            candidates
                .into_iter()
                .max_by_key(|(modified, _)| *modified)
                .map(|(_, path)| path)
                .ok_or_else(|| "SESSION_NOT_FOUND".to_string())
        })()?
    };
    let summary = parse_claude_metadata(registry, &path)
        .map_err(|_| "SESSION_NOT_FOUND".to_string())?
        .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    if summary.native_id != native_id || project_id.is_some_and(|id| summary.project_id.as_deref() != Some(id)) {
        return Err("SESSION_NOT_FOUND".to_string());
    }
    let metadata = fs::metadata(&path).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_CLAUDE_TRANSCRIPT_BYTES {
        return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
    }
    let (messages, next_cursor) = read_jsonl_messages_page(
        &path,
        end_offset,
        page_size,
        MAX_CLAUDE_TRANSCRIPT_BYTES,
        MAX_CLAUDE_LINE_BYTES,
        MAX_CLAUDE_MESSAGE_BYTES,
        |line, offset| {
            let value = serde_json::from_slice::<Value>(line).ok()?;
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(kind, "user" | "assistant")
                || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            let message = value.get("message").unwrap_or(&value);
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .filter(|role| matches!(*role, "user" | "assistant"))
                .unwrap_or(kind);
            let content = claude_text_content(message.get("content"));
            let files = crate::session_history_tools::claude_files(message);
            if content.trim().is_empty() && files.is_empty() {
                return None;
            }
            Some(SessionTranscriptMessage {
                files,
                id: value
                    .get("uuid")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| offset.to_string()),
                role: if role == "user"
                    && value.get("isMeta").and_then(Value::as_bool) == Some(true)
                {
                    "context".to_string()
                } else {
                    role.to_string()
                },
                content,
                timestamp: value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            })
        },
    )?;
    Ok(session_history::SessionTranscriptPage {
        session_id: SessionSource::ClaudeCode.stable_session_id(native_id)?,
        through_seq: None,
        messages,
        next_cursor,
    })
}

fn read_jsonl_messages_page(
    path: &Path,
    end_offset: Option<u64>,
    page_size: usize,
    max_file_bytes: u64,
    max_line_bytes: usize,
    max_page_message_bytes: usize,
    mut parse_message: impl FnMut(&[u8], u64) -> Option<SessionTranscriptMessage>,
) -> Result<(Vec<SessionTranscriptMessage>, Option<String>), String> {
    if page_size == 0 || page_size > 100 {
        return Err("SESSION_CURSOR_INVALID".to_string());
    }
    let metadata = fs::metadata(path).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    if !metadata.is_file() || metadata.len() > max_file_bytes {
        return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
    }
    let mut file = File::open(path).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    let file_len = metadata.len();
    let mut boundary = end_offset.unwrap_or(file_len);
    if boundary > file_len {
        return Err("SESSION_CURSOR_INVALID".to_string());
    }
    if end_offset.is_some() && boundary > 0 && boundary < file_len {
        file.seek(SeekFrom::Start(boundary - 1))
            .map_err(|_| "SESSION_NOT_FOUND".to_string())?;
        let mut previous = [0u8; 1];
        file.read_exact(&mut previous)
            .map_err(|_| "SESSION_NOT_FOUND".to_string())?;
        if previous[0] != b'\n' {
            return Err("SESSION_CURSOR_INVALID".to_string());
        }
    }

    let mut newest_first = Vec::with_capacity(page_size + 1);
    let mut scanned_bytes = 0u64;
    let mut message_bytes = 0usize;
    let mut oldest_scanned_line_start = None;
    while boundary > 0
        && newest_first.len() <= page_size
        && scanned_bytes < MAX_JSONL_PAGE_SCAN_BYTES
    {
        let previous_boundary = boundary;
        let Some((line_start, line)) =
            read_previous_jsonl_line(&mut file, &mut boundary, max_line_bytes).map_err(
                |error| {
                    if error.kind() == std::io::ErrorKind::InvalidData {
                        "SESSION_PREVIEW_TOO_LARGE".to_string()
                    } else {
                        "SESSION_NOT_FOUND".to_string()
                    }
                },
            )?
        else {
            break;
        };
        oldest_scanned_line_start = Some(line_start);
        scanned_bytes = scanned_bytes.saturating_add(previous_boundary - boundary);
        if let Some(message) = parse_message(&line, line_start) {
            message_bytes = message_bytes.saturating_add(message.content.len());
            if message_bytes > max_page_message_bytes {
                return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
            }
            newest_first.push((line_start, message));
        }
    }

    let has_older_page = newest_first.len() > page_size;
    let next_cursor = if has_older_page {
        newest_first
            .get(page_size - 1)
            .map(|(offset, _)| offset.to_string())
    } else if scanned_bytes >= MAX_JSONL_PAGE_SCAN_BYTES {
        oldest_scanned_line_start
            .filter(|offset| *offset > 0)
            .map(|offset| offset.to_string())
    } else {
        None
    };
    newest_first.truncate(page_size);
    let messages = newest_first
        .into_iter()
        .rev()
        .map(|(_, message)| message)
        .collect();
    Ok((messages, next_cursor))
}

fn read_previous_jsonl_line(
    file: &mut File,
    boundary: &mut u64,
    max_line_bytes: usize,
) -> std::io::Result<Option<(u64, Vec<u8>)>> {
    const CHUNK_BYTES: usize = 8192;
    if *boundary == 0 {
        return Ok(None);
    }
    let mut end = *boundary;
    file.seek(SeekFrom::Start(end - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        end -= 1;
    }
    if end == 0 {
        *boundary = 0;
        return Ok(None);
    }

    let mut cursor = end;
    let mut reversed = Vec::new();
    while cursor > 0 {
        let chunk_start = cursor.saturating_sub(CHUNK_BYTES as u64);
        let chunk_len = usize::try_from(cursor - chunk_start).unwrap_or(CHUNK_BYTES);
        let mut chunk = vec![0; chunk_len];
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut chunk)?;
        for index in (0..chunk.len()).rev() {
            if chunk[index] == b'\n' {
                let line_start = chunk_start + index as u64 + 1;
                *boundary = chunk_start + index as u64;
                reversed.reverse();
                return Ok(Some((line_start, reversed)));
            }
            reversed.push(chunk[index]);
            if reversed.len() > max_line_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "JSONL record exceeds the local read limit",
                ));
            }
        }
        cursor = chunk_start;
    }
    *boundary = 0;
    reversed.reverse();
    Ok(Some((0, reversed)))
}

fn claude_text_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Some("tool_use") => block
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| format!("[工具: {name}]")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn scan_acp(registry: &Registry) -> ProviderScan {
    match crate::store::acp_sessions::list_metadata() {
        Ok(scan) => scan_acp_metadata(registry, scan.sessions, scan.partial),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => empty_scan(
            SessionSource::Acp,
            SourceAvailability::Unavailable,
            "source_not_found",
        ),
        Err(_) => empty_scan(
            SessionSource::Acp,
            SourceAvailability::Error,
            "source_unreadable",
        ),
    }
}

#[cfg(test)]
pub(crate) fn acp_transcript_page_for_project(
    registry: &Registry,
    project_id: &str,
    native_id: &str,
    working_directory: &str,
    turns: &[(String, String)],
    end_offset: Option<usize>,
    page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    let session_id = SessionSource::Acp.stable_session_id(native_id)?;
    let association = session_history::associate_workspace(registry, working_directory)
        .map_err(|_| "SESSION_NOT_FOUND".to_string())?
        .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    if association.project_id != project_id {
        return Err("SESSION_NOT_FOUND".to_string());
    }
    session_history::paired_transcript_page(&session_id, turns, end_offset, page_size)
}

fn scan_acp_metadata(
    registry: &Registry,
    metadata: Vec<crate::store::acp_sessions::StoredAcpSessionMetadata>,
    mut partial: bool,
) -> ProviderScan {
    let mut sessions = Vec::new();
    for item in metadata {
        let association = match session_history::associate_workspace(registry, &item.cwd) {
            Ok(association) => association,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        let capabilities = SessionCapabilities {
            can_preview: true,
            can_open_original: false,
            can_resume: false,
        };
        match SessionSummary::new(
            SessionSource::Acp,
            item.session_id,
            item.cwd,
            item.title,
            Some(timestamp_from_unix(item.updated_at)),
            Some(item.message_count),
            capabilities,
            association,
        ) {
            Ok(summary) => sessions.push(summary),
            Err(_) => partial = true,
        }
    }
    let session_count = sessions.len();
    ProviderScan {
        sessions,
        status: SessionSourceStatus {
            source: SessionSource::Acp,
            availability: if partial {
                SourceAvailability::Partial
            } else {
                SourceAvailability::Available
            },
            session_count,
            error_code: partial.then(|| "metadata_scan_partial".to_string()),
        },
    }
}

fn timestamp_from_unix(value: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(value.min(i64::MAX as u64) as i64, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn claude_config_root() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .map(|home| home.join(".claude"))
        })
}

fn scan_claude_code_at(registry: &Registry, root: &Path) -> ProviderScan {
    let canonical_root = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return empty_scan(
                SessionSource::ClaudeCode,
                SourceAvailability::Unavailable,
                "source_not_found",
            );
        }
        Err(_) => {
            return empty_scan(
                SessionSource::ClaudeCode,
                SourceAvailability::Error,
                "source_root_unreadable",
            );
        }
    };

    let mut sessions = Vec::new();
    let mut issues = false;
    let mut files_seen = 0usize;
    for entry in WalkDir::new(&canonical_root)
        .follow_links(false)
        .max_depth(10)
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                issues = true;
                continue;
            }
        };
        if entry.file_type().is_symlink() {
            issues = true;
            continue;
        }
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            continue;
        }
        files_seen += 1;
        if files_seen > MAX_CLAUDE_FILES {
            issues = true;
            break;
        }
        let canonical_file = match fs::canonicalize(entry.path()) {
            Ok(path) if path.starts_with(&canonical_root) => path,
            _ => {
                issues = true;
                continue;
            }
        };
        match parse_claude_metadata(registry, &canonical_file) {
            Ok(Some(session)) => {
                if let Some(project) = session.project_id.as_deref() {
                    path_cache::remember(
                        path_cache::key(
                            &canonical_root,
                            "claude_code",
                            project,
                            &session.native_id,
                        ),
                        &canonical_file,
                    );
                }
                sessions.push(session);
            }
            Ok(None) => {}
            Err(_) => issues = true,
        }
    }

    let (availability, error_code) = if issues {
        (
            SourceAvailability::Partial,
            Some("scan_partial".to_string()),
        )
    } else {
        (SourceAvailability::Available, None)
    };
    let session_count = sessions.len();
    ProviderScan {
        sessions,
        status: SessionSourceStatus {
            source: SessionSource::ClaudeCode,
            availability,
            session_count,
            error_code,
        },
    }
}

fn parse_claude_metadata(
    registry: &Registry,
    path: &Path,
) -> Result<Option<SessionSummary>, String> {
    let metadata = fs::metadata(path).map_err(|_| "metadata_unreadable".to_string())?;
    if !metadata.is_file() {
        return Err("not_regular_file".to_string());
    }
    let mut file = File::open(path).map_err(|_| "file_unreadable".to_string())?;
    let mut head = vec![0; (metadata.len() as usize).min(MAX_CLAUDE_HEAD_BYTES)];
    file.read_exact(&mut head)
        .map_err(|_| "head_unreadable".to_string())?;

    let mut session_id = None;
    let mut working_directory = None;
    for line in String::from_utf8_lossy(&head).lines().take(10) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if session_id.is_none() {
            session_id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if working_directory.is_none() {
            working_directory = value.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if session_id.is_some() && working_directory.is_some() {
            break;
        }
    }
    let Some(native_id) = session_id else {
        return Err("session_id_missing_or_invalid".to_string());
    };
    let Some(working_directory) = working_directory
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let association = session_history::associate_workspace(registry, &working_directory)
        .map_err(|_| "workspace_association_ambiguous".to_string())?;

    let title = read_claude_custom_title(path, metadata.len()).unwrap_or_else(|| {
        let short_id = native_id.chars().take(8).collect::<String>();
        format!("Claude Code 会话 {short_id}")
    });
    let updated_at = metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|value| value.to_rfc3339());
    let capabilities = SessionCapabilities {
        can_preview: true,
        can_open_original: false,
        can_resume: false,
    };
    SessionSummary::new(
        SessionSource::ClaudeCode,
        native_id,
        working_directory,
        title,
        updated_at,
        None,
        capabilities,
        association,
    )
    .map(Some)
}

fn read_claude_custom_title(path: &Path, file_len: u64) -> Option<String> {
    let tail_len = (file_len as usize).min(MAX_CLAUDE_TAIL_BYTES);
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::End(-(tail_len as i64))).ok()?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    tail.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            (value.get("type").and_then(Value::as_str) == Some("custom-title"))
                .then(|| value.get("customTitle").and_then(Value::as_str))
                .flatten()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_string)
        })
}

fn empty_scan(
    source: SessionSource,
    availability: SourceAvailability,
    error_code: &str,
) -> ProviderScan {
    ProviderScan {
        sessions: Vec::new(),
        status: SessionSourceStatus {
            source,
            availability,
            session_count: 0,
            error_code: Some(error_code.to_string()),
        },
    }
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
                .join(format!("session-history-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
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

    fn write_session(path: &Path, session_id: &str, cwd: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            serde_json::json!({"type":"user","sessionId":session_id,"cwd":cwd,"timestamp":"2026-11-11T10:00:00Z","message":{"role":"user","content":"private prompt text"}}),
            serde_json::json!({"type":"custom-title","customTitle":"Synthetic title"}),
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":"private response text"}}),
        ];
        fs::write(
            path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }

    #[test]
    fn claude_history_tool_only_message_retains_source_id_and_fragment() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        let path = root.join("encoded-project/session-tools.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            serde_json::json!({"type":"user","uuid":"question","sessionId":"tool-session","cwd":"F:/work/project-a","message":{"role":"user","content":"synthetic question"}}),
            serde_json::json!({"type":"assistant","uuid":"edit-request","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool1","name":"Edit","input":{"file_path":"src/test.ts","old_string":"old needle","new_string":"new"}}]}}),
        ];
        fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let page =
            claude_transcript_page_at(&registry(), "project-a", "tool-session", &root, None, 1)
                .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].id, "edit-request");
        assert_eq!(
            page.messages[0].files[0].before.as_deref(),
            Some("old needle")
        );
        assert!(page.next_cursor.is_some());
        let older = claude_transcript_page_at(
            &registry(),
            "project-a",
            "tool-session",
            &root,
            page.next_cursor.unwrap().parse().ok(),
            1,
        )
        .unwrap();
        assert_eq!(older.messages[0].id, "question");
    }

    #[test]
    fn claude_scanner_returns_project_metadata_without_prompt_or_response_text() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        write_session(
            &root.join("encoded-project/session-1.jsonl"),
            "session-1",
            "F:/work/project-a/src",
        );

        let scan = scan_claude_code_at(&registry(), &root);
        assert_eq!(scan.status.availability, SourceAvailability::Available);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(scan.sessions[0].title, "Synthetic title");
        assert!(!scan.sessions[0].title.contains("private"));
        assert!(scan.sessions[0].capabilities.can_preview);
        assert!(!scan.sessions[0].capabilities.can_open_original);
        assert!(!scan.sessions[0].capabilities.can_resume);
    }

    #[test]
    fn claude_preview_pages_visible_text_and_rechecks_project_ownership() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        let path = root.join("encoded-project/session-1.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            serde_json::json!({"type":"user","sessionId":"session-1","cwd":"F:/work/project-a","timestamp":"2026-11-11T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"synthetic question"}]}}),
            serde_json::json!({"type":"assistant","timestamp":"2026-11-11T10:00:01Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"text","text":"synthetic answer"},{"type":"tool_use","name":"read_file","input":{"path":"private/path"}}]}}),
        ];
        fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let page =
            claude_transcript_page_at(&registry(), "project-a", "session-1", &root, None, 50)
                .unwrap();
        assert_eq!(page.session_id, "claude_code:session-1");
        assert_eq!(page.messages.len(), 2);
        assert!(page
            .messages
            .iter()
            .any(|message| message.content == "synthetic question"));
        assert!(page
            .messages
            .iter()
            .any(|message| message.content.contains("synthetic answer")));
        assert!(!page
            .messages
            .iter()
            .any(|message| message.content.contains("private reasoning")));
        assert!(!page
            .messages
            .iter()
            .any(|message| message.content.contains("private/path")));
        assert_eq!(
            claude_transcript_page_at(
                &registry(),
                "another-project",
                "session-1",
                &root,
                None,
                50,
            )
            .unwrap_err(),
            "SESSION_NOT_FOUND"
        );
    }

    #[test]
    fn claude_preview_reads_large_transcripts_in_bounded_pages() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        let path = root.join("encoded-project/session-large.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = (0..120)
            .map(|index| {
                serde_json::json!({
                    "type": if index % 2 == 0 { "user" } else { "assistant" },
                    "sessionId": "session-large",
                    "cwd": "F:/work/project-a",
                    "uuid": format!("claude-{index}"),
                    "message": {"content": format!("synthetic message {index:03}")}
                })
                .to_string()
            })
            .collect::<Vec<_>>();
        fs::write(&path, rows.join("\n") + "\n").unwrap();

        let newest =
            claude_transcript_page_at(&registry(), "project-a", "session-large", &root, None, 50)
                .unwrap();
        assert_eq!(newest.messages.len(), 50);
        assert_eq!(
            newest.messages.first().unwrap().content,
            "synthetic message 070"
        );
        assert_eq!(
            newest.messages.last().unwrap().content,
            "synthetic message 119"
        );
        assert_eq!(newest.through_seq, None);

        let middle = claude_transcript_page_at(
            &registry(),
            "project-a",
            "session-large",
            &root,
            newest
                .next_cursor
                .as_deref()
                .map(|value| value.parse().unwrap()),
            50,
        )
        .unwrap();
        assert_eq!(
            middle.messages.first().unwrap().content,
            "synthetic message 020"
        );
        assert_eq!(
            middle.messages.last().unwrap().content,
            "synthetic message 069"
        );

        let oldest = claude_transcript_page_at(
            &registry(),
            "project-a",
            "session-large",
            &root,
            middle
                .next_cursor
                .as_deref()
                .map(|value| value.parse().unwrap()),
            50,
        )
        .unwrap();
        assert_eq!(oldest.messages.len(), 20);
        assert_eq!(
            oldest.messages.first().unwrap().content,
            "synthetic message 000"
        );
        assert_eq!(
            oldest.messages.last().unwrap().content,
            "synthetic message 019"
        );
        assert_eq!(oldest.next_cursor, None);
    }

    #[test]
    fn jsonl_page_reader_parses_only_the_requested_page_and_probe_row() {
        let temp = TempRoot::new();
        let path = temp.path().join("large.jsonl");
        let rows = (0..10_000)
            .map(|index| serde_json::json!({"message": index}).to_string())
            .collect::<Vec<_>>();
        fs::write(&path, rows.join("\n") + "\n").unwrap();
        let parsed = std::cell::Cell::new(0usize);

        let (messages, next_cursor) = read_jsonl_messages_page(
            &path,
            None,
            50,
            64 * 1024 * 1024,
            MAX_CLAUDE_LINE_BYTES,
            MAX_CLAUDE_MESSAGE_BYTES,
            |line, offset| {
                parsed.set(parsed.get() + 1);
                let value = serde_json::from_slice::<Value>(line).ok()?;
                Some(SessionTranscriptMessage {
                    files: Vec::new(),
                    id: offset.to_string(),
                    role: "user".to_string(),
                    content: value.get("message")?.as_u64()?.to_string(),
                    timestamp: None,
                })
            },
        )
        .unwrap();

        assert_eq!(parsed.get(), 51);
        assert_eq!(messages.len(), 50);
        assert_eq!(messages.first().unwrap().content, "9950");
        assert_eq!(messages.last().unwrap().content, "9999");
        assert!(next_cursor.is_some());
    }

    #[test]
    fn jsonl_page_reader_rejects_cursors_outside_line_boundaries() {
        let temp = TempRoot::new();
        let path = temp.path().join("cursor.jsonl");
        let first = r#"{"message":0}"#;
        fs::write(&path, format!("{first}\n{{\"message\":1}}\n")).unwrap();
        let file_len = fs::metadata(&path).unwrap().len();
        let parse_message = |line: &[u8], offset: u64| {
            let value = serde_json::from_slice::<Value>(line).ok()?;
            Some(SessionTranscriptMessage {
                files: Vec::new(),
                id: offset.to_string(),
                role: "user".to_string(),
                content: value.get("message")?.as_u64()?.to_string(),
                timestamp: None,
            })
        };

        assert_eq!(
            read_jsonl_messages_page(&path, Some(1), 1, 1024, 128, 128, parse_message,)
                .unwrap_err(),
            "SESSION_CURSOR_INVALID"
        );
        assert_eq!(
            read_jsonl_messages_page(&path, Some(file_len + 1), 1, 1024, 128, 128, parse_message,)
                .unwrap_err(),
            "SESSION_CURSOR_INVALID"
        );

        let page = read_jsonl_messages_page(
            &path,
            Some(first.len() as u64 + 1),
            1,
            1024,
            128,
            128,
            parse_message,
        )
        .unwrap();
        assert_eq!(page.0.len(), 1);
        assert_eq!(page.0[0].content, "0");
        assert_eq!(page.1, None);
    }

    #[test]
    fn reverse_jsonl_reader_handles_crlf_and_unterminated_final_rows() {
        let temp = TempRoot::new();
        let path = temp.path().join("line-endings.jsonl");
        let content = b"{\"message\":\"first\"}\r\n{\"message\":\"last\"}";
        fs::write(&path, content).unwrap();
        let mut file = File::open(&path).unwrap();
        let mut boundary = content.len() as u64;

        let (last_offset, last) = read_previous_jsonl_line(&mut file, &mut boundary, 128)
            .unwrap()
            .unwrap();
        assert_eq!(
            last_offset,
            content.iter().rposition(|byte| *byte == b'\n').unwrap() as u64 + 1
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&last).unwrap()["message"],
            "last"
        );

        let (first_offset, first) = read_previous_jsonl_line(&mut file, &mut boundary, 128)
            .unwrap()
            .unwrap();
        assert_eq!(first_offset, 0);
        assert_eq!(
            serde_json::from_slice::<Value>(&first).unwrap()["message"],
            "first"
        );
        assert_eq!(boundary, 0);
    }

    #[test]
    fn jsonl_page_reader_rejects_oversized_rows() {
        let temp = TempRoot::new();
        let path = temp.path().join("oversized.jsonl");
        fs::write(&path, format!("{{\"message\":\"{}\"}}", "x".repeat(256))).unwrap();

        let result = read_jsonl_messages_page(&path, None, 1, 1024, 64, 1024, |_, _| None);

        assert_eq!(result.unwrap_err(), "SESSION_PREVIEW_TOO_LARGE");
    }

    #[test]
    fn claude_scanner_retains_unassociated_sessions_and_marks_corrupt_files_partial() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        write_session(
            &root.join("encoded-project/session-1.jsonl"),
            "session-1",
            "F:/work/project-a",
        );
        write_session(
            &root.join("other/session-2.jsonl"),
            "session-2",
            "F:/work/another-project",
        );
        fs::write(root.join("corrupt.jsonl"), "not-json").unwrap();

        let scan = scan_claude_code_at(&registry(), &root);
        assert_eq!(scan.sessions.len(), 2);
        assert!(scan.sessions.iter().any(|s|s.native_id=="session-2" && s.project_id.is_none()));
        let page=claude_transcript_page_scoped_at(&registry(),None,"session-2",&root,None,50).unwrap();
        assert!(!page.messages.is_empty());
        assert!(claude_transcript_page_at(&registry(),"project-a","session-2",&root,None,50).is_err());
        assert_eq!(scan.status.availability, SourceAvailability::Partial);
        assert_eq!(scan.status.error_code.as_deref(), Some("scan_partial"));
    }

    #[test]
    fn claude_scanner_does_not_follow_a_symlink_outside_its_root() {
        let temp = TempRoot::new();
        let root = temp.path().join(".claude/projects");
        let outside = temp.path().join("outside.jsonl");
        write_session(&outside, "session-outside", "F:/work/project-a");
        fs::create_dir_all(&root).unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, root.join("linked.jsonl"));
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, root.join("linked.jsonl"));
        if linked.is_err() {
            return;
        }

        let scan = scan_claude_code_at(&registry(), &root);
        assert!(scan.sessions.is_empty());
        assert_eq!(scan.status.availability, SourceAvailability::Partial);
    }

    #[test]
    fn acp_metadata_scan_associates_projects_without_exposing_resume_capabilities() {
        let scan = scan_acp_metadata(
            &registry(),
            vec![crate::store::acp_sessions::StoredAcpSessionMetadata {
                schema_version: "acp_session_index.v1".to_string(),
                session_id: "acp-session-1".to_string(),
                cwd: "F:/work/project-a/src".to_string(),
                created_at: 1_800_000_000,
                updated_at: 1_800_000_100,
                title: "Synthetic ACP title".to_string(),
                message_count: 4,
            }],
            false,
        );

        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].id, "acp:acp-session-1");
        assert_eq!(scan.sessions[0].project_id.as_deref(), Some("project-a"));
        assert!(scan.sessions[0].capabilities.can_preview);
        assert!(!scan.sessions[0].capabilities.can_open_original);
        assert!(!scan.sessions[0].capabilities.can_resume);
        assert_eq!(scan.status.availability, SourceAvailability::Available);
    }

    #[test]
    fn acp_preview_rechecks_project_ownership_before_returning_messages() {
        let turns = vec![("private prompt".to_string(), "private reply".to_string())];
        let page = acp_transcript_page_for_project(
            &registry(),
            "project-a",
            "acp-session-1",
            "F:/work/project-a/src",
            &turns,
            None,
            50,
        )
        .unwrap();
        assert_eq!(page.session_id, "acp:acp-session-1");
        assert_eq!(page.messages.len(), 2);
        assert_eq!(
            acp_transcript_page_for_project(
                &registry(),
                "another-project",
                "acp-session-1",
                "F:/work/project-a/src",
                &turns,
                None,
                50,
            )
            .unwrap_err(),
            "SESSION_NOT_FOUND"
        );
    }
}
