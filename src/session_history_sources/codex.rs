use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;
use walkdir::WalkDir;

use crate::session_history::{
    self, SessionCapabilities, SessionSource, SessionSummary, SessionTranscriptMessage,
    SourceAvailability,
};
use crate::workspace_registry::Registry;

use super::ProviderScan;

const MAX_CODEX_FILES: usize = 20_000;
const MAX_CODEX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CODEX_INDEX_LINE_BYTES: usize = 64 * 1024;
const MAX_CODEX_HEAD_BYTES: u64 = 64 * 1024;
const MAX_CODEX_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CODEX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CODEX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn scan_codex(registry: &Registry) -> ProviderScan {
    let Some(config_root) = codex_config_root() else {
        return super::empty_scan(
            SessionSource::Codex,
            SourceAvailability::Unavailable,
            "home_directory_unavailable",
        );
    };
    let (titles, partial) = load_session_titles(&config_root);
    scan_codex_at(registry, &config_root, &titles, partial)
}

fn codex_config_root() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return resolve_user_path(PathBuf::from(value));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    Some(home.join(".codex"))
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
    path.is_absolute().then_some(path)
}

fn load_session_titles(config_root: &Path) -> (HashMap<String, String>, bool) {
    let mut titles = HashMap::new();
    let canonical_root = match fs::canonicalize(config_root) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (titles, false),
        Err(_) => return (titles, true),
    };
    let path = canonical_root.join("session_index.jsonl");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => return (titles, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (titles, false),
        Err(_) => return (titles, true),
    };
    if metadata.len() > MAX_CODEX_INDEX_BYTES {
        return (titles, true);
    }
    let path = match fs::canonicalize(path) {
        Ok(path) if path.starts_with(&canonical_root) => path,
        _ => return (titles, true),
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return (titles, true),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut partial = false;
    loop {
        match read_bounded_line(&mut reader, &mut line, MAX_CODEX_INDEX_LINE_BYTES) {
            Ok(false) => break,
            Ok(true) => {
                let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                    partial = true;
                    continue;
                };
                let Some(id) = value.get("id").and_then(Value::as_str).map(str::trim) else {
                    partial = true;
                    continue;
                };
                let Some(title) = value
                    .get("thread_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                else {
                    continue;
                };
                if id.is_empty() || id.chars().any(char::is_control) {
                    partial = true;
                    continue;
                }
                titles.insert(id.to_string(), truncate(title, 200));
            }
            Err(_) => {
                partial = true;
                break;
            }
        }
    }
    (titles, partial)
}

fn scan_codex_at(
    registry: &Registry,
    config_root: &Path,
    titles: &HashMap<String, String>,
    mut partial: bool,
) -> ProviderScan {
    let canonical_config_root = match fs::canonicalize(config_root) {
        Ok(root) => root,
        Err(_) => {
            return super::empty_scan(
                SessionSource::Codex,
                SourceAvailability::Error,
                "source_unreadable",
            );
        }
    };
    let mut roots = Vec::new();
    for directory in ["sessions", "archived_sessions"] {
        let path = canonical_config_root.join(directory);
        match fs::canonicalize(&path) {
            Ok(path) if path.is_dir() && path.starts_with(&canonical_config_root) => {
                roots.push(path)
            }
            Ok(_) => partial = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => partial = true,
        }
    }
    if roots.is_empty() {
        return super::empty_scan(
            SessionSource::Codex,
            if partial {
                SourceAvailability::Partial
            } else {
                SourceAvailability::Unavailable
            },
            if partial {
                "scan_partial"
            } else {
                "source_not_found"
            },
        );
    }

    let mut sessions = Vec::new();
    let mut files_seen = 0usize;
    for root in roots {
        for entry in WalkDir::new(&root).follow_links(false).max_depth(10) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    partial = true;
                    continue;
                }
            };
            if entry.file_type().is_symlink() {
                partial = true;
                continue;
            }
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
            {
                continue;
            }
            files_seen += 1;
            if files_seen > MAX_CODEX_FILES {
                partial = true;
                break;
            }
            let path = match fs::canonicalize(entry.path()) {
                Ok(path) if path.starts_with(&root) => path,
                _ => {
                    partial = true;
                    continue;
                }
            };
            match parse_codex_metadata(registry, &path, titles) {
                Ok(Some(session)) => {
                    if let Some(project) = session.project_id.as_deref() {
                        super::path_cache::remember(
                            super::path_cache::key(
                                &canonical_config_root,
                                "codex",
                                project,
                                &session.native_id,
                            ),
                            &path,
                        );
                    }
                    sessions.push(session);
                }
                Ok(None) => {}
                Err(_) => partial = true,
            }
        }
    }
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let session_count = sessions.len();
    ProviderScan {
        sessions,
        status: crate::session_history::SessionSourceStatus {
            source: SessionSource::Codex,
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

fn parse_codex_metadata(
    registry: &Registry,
    path: &Path,
    titles: &HashMap<String, String>,
) -> Result<Option<SessionSummary>, String> {
    let file_metadata = fs::metadata(path).map_err(|_| "metadata_unreadable".to_string())?;
    if !file_metadata.is_file() {
        return Err("not_regular_file".to_string());
    }
    let file = File::open(path).map_err(|_| "file_unreadable".to_string())?;
    let mut reader = BufReader::new(file.take(MAX_CODEX_HEAD_BYTES));
    let mut line = Vec::new();
    let mut session_id = None;
    let mut working_directory = None;
    let mut created_at = None;
    for _ in 0..16 {
        if !read_bounded_line(&mut reader, &mut line, MAX_CODEX_LINE_BYTES)
            .map_err(|_| "header_unreadable".to_string())?
        {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.pointer("/source/subagent").is_some() {
            return Ok(None);
        }
        session_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        working_directory = payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        created_at = timestamp(value.get("timestamp").or_else(|| payload.get("timestamp")));
        break;
    }
    let Some(native_id) = session_id else {
        return Err("session_id_missing_or_invalid".to_string());
    };
    let Some(working_directory) = working_directory else {
        return Ok(None);
    };
    let Some(association) = session_history::associate_workspace(registry, &working_directory)
        .map_err(|_| "workspace_association_ambiguous".to_string())?
    else {
        return Ok(None);
    };
    let title = titles.get(&native_id).cloned().unwrap_or_else(|| {
        format!(
            "Codex 会话 {}",
            native_id.chars().take(8).collect::<String>()
        )
    });
    let updated_at = file_metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|value| value.to_rfc3339());
    SessionSummary::new(
        SessionSource::Codex,
        native_id,
        working_directory,
        title,
        updated_at.or(created_at),
        None,
        SessionCapabilities {
            can_preview: true,
            can_open_original: false,
            can_resume: false,
        },
        Some(association),
    )
    .map(Some)
}

pub(crate) fn codex_transcript_page_for_project(
    registry: &Registry,
    project_id: &str,
    native_id: &str,
    end_offset: Option<u64>,
    page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    SessionSource::Codex.stable_session_id(native_id)?;
    let config_root = codex_config_root().ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    let (titles, _) = load_session_titles(&config_root);
    codex_transcript_page_at(
        registry,
        project_id,
        native_id,
        &config_root,
        &titles,
        end_offset,
        page_size,
    )
}

fn codex_transcript_page_at(
    registry: &Registry,
    project_id: &str,
    native_id: &str,
    config_root: &Path,
    titles: &HashMap<String, String>,
    end_offset: Option<u64>,
    page_size: usize,
) -> Result<session_history::SessionTranscriptPage, String> {
    let session_id = SessionSource::Codex.stable_session_id(native_id)?;
    let canonical_config_root =
        fs::canonicalize(config_root).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    let cache_key = super::path_cache::key(&canonical_config_root, "codex", project_id, native_id);
    let path = if let Some(path) = super::path_cache::get(&cache_key) {
        path
    } else {
        (|| -> Result<PathBuf, String> {
            let mut candidates = Vec::new();
            let mut files_seen = 0usize;
            for directory in ["sessions", "archived_sessions"] {
                let root = canonical_config_root.join(directory);
                let Ok(canonical_root) = fs::canonicalize(&root) else {
                    continue;
                };
                if !canonical_root.is_dir() || !canonical_root.starts_with(&canonical_config_root) {
                    continue;
                }
                for entry in WalkDir::new(&canonical_root)
                    .follow_links(false)
                    .max_depth(10)
                {
                    let Ok(entry) = entry else { continue };
                    if !entry.file_type().is_file()
                        || entry.path().extension().and_then(|value| value.to_str())
                            != Some("jsonl")
                    {
                        continue;
                    }
                    files_seen += 1;
                    if files_seen > MAX_CODEX_FILES {
                        return Err("SESSION_PREVIEW_UNAVAILABLE".into());
                    }
                    let Ok(path) = fs::canonicalize(entry.path()) else {
                        continue;
                    };
                    if !path.starts_with(&canonical_root) {
                        continue;
                    }
                    let Ok(Some(summary)) = parse_codex_metadata(registry, &path, &titles) else {
                        continue;
                    };
                    if summary.project_id.as_deref() == Some(project_id) {
                        super::path_cache::remember(
                            super::path_cache::key(
                                &canonical_config_root,
                                "codex",
                                project_id,
                                &summary.native_id,
                            ),
                            &path,
                        );
                    }
                    if summary.native_id == native_id
                        && summary.project_id.as_deref() == Some(project_id)
                    {
                        let updated = fs::metadata(&path)
                            .and_then(|metadata| metadata.modified())
                            .ok();
                        candidates.push((updated, path));
                    }
                }
            }
            candidates
                .into_iter()
                .max_by_key(|(updated, _)| *updated)
                .map(|(_, path)| path)
                .ok_or_else(|| "SESSION_NOT_FOUND".to_string())
        })()?
    };
    let summary = parse_codex_metadata(registry, &path, titles)
        .map_err(|_| "SESSION_NOT_FOUND".to_string())?
        .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    if summary.native_id != native_id || summary.project_id.as_deref() != Some(project_id) {
        return Err("SESSION_NOT_FOUND".to_string());
    }
    let metadata = fs::metadata(&path).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_CODEX_TRANSCRIPT_BYTES {
        return Err("SESSION_PREVIEW_TOO_LARGE".to_string());
    }
    let response_item_format = codex_uses_response_items(&path)?;
    let (messages, next_cursor) = super::read_jsonl_messages_page(
        &path,
        end_offset,
        page_size,
        MAX_CODEX_TRANSCRIPT_BYTES,
        MAX_CODEX_LINE_BYTES,
        MAX_CODEX_MESSAGE_BYTES,
        |line, offset| {
            let value = serde_json::from_slice::<Value>(line).ok()?;
            let timestamp = timestamp(value.get("timestamp"));
            let message = if response_item_format {
                (value.get("type").and_then(Value::as_str) == Some("response_item"))
                    .then(|| codex_message(value.get("payload"), timestamp))
                    .flatten()
            } else {
                (value.get("type").and_then(Value::as_str) == Some("event_msg"))
                    .then(|| codex_event_message(value.get("payload"), timestamp))
                    .flatten()
            }?;
            Some(SessionTranscriptMessage {
                id: offset.to_string(),
                ..message
            })
        },
    )?;
    Ok(session_history::SessionTranscriptPage {
        session_id,
        through_seq: None,
        messages,
        next_cursor,
    })
}

fn codex_uses_response_items(path: &Path) -> Result<bool, String> {
    let file = File::open(path).map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    let mut head = Vec::new();
    file.take(MAX_CODEX_HEAD_BYTES)
        .read_to_end(&mut head)
        .map_err(|_| "SESSION_NOT_FOUND".to_string())?;
    Ok(String::from_utf8_lossy(&head).lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|kind| kind == "response_item")
    }))
}

fn codex_message(
    payload: Option<&Value>,
    timestamp: Option<String>,
) -> Option<SessionTranscriptMessage> {
    let payload = payload?;
    let files = crate::session_history_tools::codex_files(payload);
    if !files.is_empty() {
        return Some(SessionTranscriptMessage {
            id: String::new(),
            role: "assistant".into(),
            content: "文件操作记录".into(),
            timestamp,
            files,
        });
    }
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = payload.get("role").and_then(Value::as_str)?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    if role == "assistant"
        && payload
            .get("phase")
            .and_then(Value::as_str)
            .is_some_and(|phase| phase == "analysis")
    {
        return None;
    }
    let content = text_content(payload.get("content")?);
    (!content.trim().is_empty()).then(|| SessionTranscriptMessage {
        files: Vec::new(),
        id: String::new(),
        role: role.to_string(),
        content,
        timestamp,
    })
}

fn codex_event_message(
    payload: Option<&Value>,
    timestamp: Option<String>,
) -> Option<SessionTranscriptMessage> {
    let payload = payload?;
    let (role, content) = match payload.get("type").and_then(Value::as_str)? {
        "user_message" => ("user", payload.get("message")?.as_str()?.to_string()),
        "agent_message" => ("assistant", payload.get("message")?.as_str()?.to_string()),
        _ => return None,
    };
    (!content.trim().is_empty()).then(|| SessionTranscriptMessage {
        files: Vec::new(),
        id: String::new(),
        role: role.to_string(),
        content,
        timestamp,
    })
}

fn text_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text" | "text")
                )
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn timestamp(value: Option<&Value>) -> Option<String> {
    if let Some(value) = value.and_then(Value::as_str) {
        let value = value.trim();
        if !value.is_empty() {
            return chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|value| value.to_rfc3339());
        }
    }
    let value = value?.as_i64()?;
    let date = if value >= 10_000_000_000 {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
    } else {
        chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0)
    }?;
    Some(date.to_rfc3339())
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<bool> {
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(!buffer.is_empty());
        }
        let newline = available.iter().position(|value| *value == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if buffer.len().saturating_add(count) > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL record exceeds the local read limit",
            ));
        }
        buffer.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(true);
        }
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
                .join(format!("codex-history-{}-{nonce}", std::process::id()));
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

    fn write_codex_session(path: &Path, id: &str, cwd: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            serde_json::json!({"timestamp":"2026-11-11T10:00:00Z","type":"session_meta","payload":{"id":id,"cwd":cwd}}),
            serde_json::json!({"timestamp":"2026-11-11T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":"private synthetic prompt"}}),
            serde_json::json!({"timestamp":"2026-11-11T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"analysis","content":"private reasoning"}}),
            serde_json::json!({"timestamp":"2026-11-11T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final","content":[{"type":"output_text","text":"synthetic answer"}]}}),
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
    fn codex_history_patch_has_offset_source_id_and_pages_without_execution() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let path = root.join("sessions/patch.jsonl");
        write_codex_session(&path, "patch-session", "F:/work/project-a");
        let original = fs::read_to_string(&path).unwrap();
        let patch = serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/test.ts\n@@\n-old\n+new\n*** End Patch"}});
        fs::write(&path, format!("{original}\n{patch}\n")).unwrap();
        let page = codex_transcript_page_at(
            &registry(),
            "project-a",
            "patch-session",
            &root,
            &HashMap::new(),
            None,
            1,
        )
        .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(
            page.messages[0].id.parse::<usize>().unwrap(),
            original.len() + 1
        );
        assert_eq!(page.messages[0].files[0].path, "src/test.ts");
        assert!(page.messages[0].files[0]
            .patch
            .as_ref()
            .unwrap()
            .contains("+new"));
        assert!(page.next_cursor.is_some());
    }

    #[test]
    fn codex_scan_reads_header_and_title_index_without_reading_transcript() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let path = root.join("sessions/2026/11/11/rollout-test.jsonl");
        write_codex_session(&path, "codex-session-1", "F:/work/project-a/src");
        fs::write(
            root.join("session_index.jsonl"),
            serde_json::json!({"id":"codex-session-1","thread_name":"Synthetic task title"})
                .to_string(),
        )
        .unwrap();
        let (titles, partial) = load_session_titles(&root);
        let scan = scan_codex_at(&registry(), &root, &titles, partial);

        assert_eq!(scan.status.availability, SourceAvailability::Available);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].title, "Synthetic task title");
        assert_eq!(scan.sessions[0].project_id.as_deref(), Some("project-a"));
        assert!(scan.sessions[0].capabilities.can_preview);
        assert!(!scan.sessions[0].capabilities.can_open_original);
        assert!(!scan.sessions[0].capabilities.can_resume);
    }

    #[test]
    fn codex_preview_is_workspace_scoped_and_hides_analysis_messages() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let path = root.join("sessions/2026/11/11/rollout-test.jsonl");
        write_codex_session(&path, "codex-session-1", "F:/work/project-a/src");
        let (titles, _) = load_session_titles(&root);
        let page = codex_transcript_page_at(
            &registry(),
            "project-a",
            "codex-session-1",
            &root,
            &titles,
            None,
            50,
        )
        .unwrap();
        assert_eq!(page.session_id, "codex:codex-session-1");
        assert_eq!(page.messages.len(), 2);
        assert!(!page
            .messages
            .iter()
            .any(|message| message.content.contains("reasoning")));
        assert_eq!(
            codex_transcript_page_at(
                &registry(),
                "another-project",
                "codex-session-1",
                &root,
                &titles,
                None,
                50,
            )
            .unwrap_err(),
            "SESSION_NOT_FOUND"
        );
    }

    #[test]
    fn codex_preview_pages_large_transcripts_without_loading_all_messages() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let path = root.join("sessions/2026/11/11/large.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut rows = vec![serde_json::json!({
            "type": "session_meta",
            "payload": {"id": "codex-large", "cwd": "F:/work/project-a"}
        })
        .to_string()];
        rows.extend((0..120).map(|index| {
            serde_json::json!({
                "type": "response_item",
                "timestamp": format!("2026-11-11T10:{:02}:00Z", index % 60),
                "payload": {
                    "type": "message",
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "phase": "final",
                    "content": format!("synthetic codex message {index:03}")
                }
            })
            .to_string()
        }));
        fs::write(&path, rows.join("\n") + "\n").unwrap();

        let newest = codex_transcript_page_at(
            &registry(),
            "project-a",
            "codex-large",
            &root,
            &HashMap::new(),
            None,
            50,
        )
        .unwrap();
        assert_eq!(newest.messages.len(), 50);
        assert_eq!(
            newest.messages.first().unwrap().content,
            "synthetic codex message 070"
        );
        assert_eq!(
            newest.messages.last().unwrap().content,
            "synthetic codex message 119"
        );
        assert_eq!(newest.through_seq, None);

        let middle = codex_transcript_page_at(
            &registry(),
            "project-a",
            "codex-large",
            &root,
            &HashMap::new(),
            newest
                .next_cursor
                .as_deref()
                .map(|value| value.parse().unwrap()),
            50,
        )
        .unwrap();
        assert_eq!(
            middle.messages.first().unwrap().content,
            "synthetic codex message 020"
        );
        assert_eq!(
            middle.messages.last().unwrap().content,
            "synthetic codex message 069"
        );

        let oldest = codex_transcript_page_at(
            &registry(),
            "project-a",
            "codex-large",
            &root,
            &HashMap::new(),
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
            "synthetic codex message 000"
        );
        assert_eq!(
            oldest.messages.last().unwrap().content,
            "synthetic codex message 019"
        );
        assert_eq!(oldest.next_cursor, None);
    }

    #[test]
    fn codex_preview_supports_legacy_event_message_transcripts() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let path = root.join("sessions/2026/11/11/legacy.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": "codex-legacy", "cwd": "F:/work/project-a"}
            }),
            serde_json::json!({
                "type": "event_msg",
                "timestamp": "2026-11-11T10:00:00Z",
                "payload": {"type": "user_message", "message": "synthetic legacy prompt"}
            }),
            serde_json::json!({
                "type": "event_msg",
                "timestamp": "2026-11-11T10:00:01Z",
                "payload": {"type": "agent_message", "message": "synthetic legacy answer"}
            }),
        ];
        fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let page = codex_transcript_page_at(
            &registry(),
            "project-a",
            "codex-legacy",
            &root,
            &HashMap::new(),
            None,
            50,
        )
        .unwrap();

        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[0].role, "user");
        assert_eq!(page.messages[0].content, "synthetic legacy prompt");
        assert_eq!(page.messages[1].role, "assistant");
        assert_eq!(page.messages[1].content, "synthetic legacy answer");
        assert_eq!(page.through_seq, None);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn codex_scan_rejects_session_roots_redirected_outside_config_root() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        let outside = temp.0.join("outside-sessions");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        write_codex_session(
            &outside.join("2026/11/11/rollout-test.jsonl"),
            "codex-session-outside",
            "F:/work/project-a/src",
        );
        let sessions = root.join("sessions");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &sessions);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &sessions);
        if linked.is_err() {
            return;
        }

        let scan = scan_codex_at(&registry(), &root, &HashMap::new(), false);
        assert!(scan.sessions.is_empty());
        assert_eq!(scan.status.availability, SourceAvailability::Partial);
        #[cfg(windows)]
        let _ = fs::remove_dir(&sessions);
        #[cfg(unix)]
        let _ = fs::remove_file(&sessions);
    }

    #[test]
    fn codex_scan_rejects_a_session_index_symlink() {
        let temp = TempRoot::new();
        let root = temp.0.join(".codex");
        write_codex_session(
            &root.join("sessions/2026/11/11/rollout-test.jsonl"),
            "codex-session-1",
            "F:/work/project-a/src",
        );
        let outside = temp.0.join("outside-index.jsonl");
        fs::write(
            &outside,
            serde_json::json!({"id":"codex-session-1","thread_name":"outside title"}).to_string(),
        )
        .unwrap();
        let index = root.join("session_index.jsonl");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, &index);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &index);
        if linked.is_err() {
            return;
        }

        let (titles, partial) = load_session_titles(&root);
        assert!(titles.is_empty());
        assert!(partial);
        let scan = scan_codex_at(&registry(), &root, &titles, partial);
        assert_eq!(scan.status.availability, SourceAvailability::Partial);
        assert_eq!(scan.sessions.len(), 1);
        assert_ne!(scan.sessions[0].title, "outside title");
        let _ = fs::remove_file(index);
    }
}
