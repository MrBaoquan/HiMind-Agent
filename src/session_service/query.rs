//! Public, bounded and restartable projections. Cursors contain no conversation text.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::session_history::{
    SessionSource, SessionSourceStatus, SessionSummary, SessionTranscriptPage,
};
use crate::workspace_registry::Registry;

pub(crate) const CONTRACT_VERSION: &str = "session_history.v1";
const MAX_BODY_PAGES: usize = 8;
const MAX_CONTENT_CHARS: usize = 8_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    #[default]
    All,
    Project,
    Unassigned,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    scope: Option<Scope>,
    project_id: Option<String>,
    #[serde(default)]
    include_children: bool,
    sources: Option<Vec<SessionSource>>,
    query: Option<String>,
    session_id: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    from: Option<String>,
    to: Option<String>,
    max_chars: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    operation: String,
    fingerprint: String,
    session: usize,
    message: Option<String>,
    // A page may yield more messages than fit in one response.
    within_page: usize,
    #[serde(default)]
    page_revision: Option<String>,
}

pub(crate) fn capability_schema(operation: &str) -> Value {
    let mut properties = json!({
        "scope":{"type":"string","enum":["all","project","unassigned"]},
        "project_id":{"type":"string","minLength":1},
        "include_children":{"type":"boolean"},
        "sources":{"type":"array","minItems":1,"uniqueItems":true,"maxItems":5,
            "items":{"type":"string","enum":["dsh","acp","claude_code","codex","opencode"]}},
        "query":{"type":"string","maxLength":200},
        "cursor":{"type":"string","maxLength":8192},
        "limit":{"type":"integer","minimum":1,"maximum":200}
    });
    let required = match operation {
        "messages" => {
            properties["session_id"] = json!({"type":"string","minLength":3,"maxLength":512});
            vec!["session_id"]
        }
        "activity" => {
            properties["from"] = json!({"type":"string","description":"Inclusive RFC3339 instant with explicit UTC offset"});
            properties["to"] = json!({"type":"string","description":"Exclusive RFC3339 instant, at most 31 days after from"});
            properties["max_chars"] =
                json!({"type":"integer","minimum":200,"maximum":MAX_CONTENT_CHARS});
            vec!["from", "to"]
        }
        _ => vec![],
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn selected_sources(request: &Request) -> Result<Vec<SessionSource>, String> {
    let selected = request
        .sources
        .clone()
        .unwrap_or_else(|| SessionSource::ALL.to_vec());
    if selected.is_empty() || selected.len() > 5 {
        return Err("SESSION_SOURCES_INVALID".into());
    }
    let mut seen = HashSet::new();
    if !selected.iter().all(|source| seen.insert(*source)) {
        return Err("SESSION_SOURCES_INVALID".into());
    }
    Ok(selected)
}

fn scoped_sessions(
    registry: &Registry,
    request: &Request,
    mut sessions: Vec<SessionSummary>,
) -> Result<Vec<SessionSummary>, String> {
    let scope = request.scope.unwrap_or(if request.project_id.is_some() {
        Scope::Project
    } else {
        Scope::All
    });
    if scope != Scope::Project && (request.project_id.is_some() || request.include_children) {
        return Err("SESSION_SCOPE_INVALID".into());
    }
    let ids = if scope == Scope::Project {
        let project = request
            .project_id
            .as_deref()
            .filter(|id| registry.project(id).is_some())
            .ok_or_else(|| "PROJECT_NOT_FOUND".to_string())?;
        registry.scope_ids(project, request.include_children)
    } else {
        HashSet::new()
    };
    let query = request.query.as_deref().unwrap_or("").trim().to_lowercase();
    if query.chars().count() > 200 {
        return Err("SESSION_QUERY_TOO_LONG".into());
    }
    sessions.retain(|s| {
        (match scope {
            Scope::All => true,
            Scope::Unassigned => s.project_id.is_none(),
            Scope::Project => s.project_id.as_ref().is_some_and(|id| ids.contains(id)),
        }) && (query.is_empty()
            || [
                &s.title,
                &s.working_directory,
                s.project_name.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|v| v.to_lowercase().contains(&query)))
    });
    // Stable ordering makes a cursor reproducible after service restart.
    sessions.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then(a.working_directory.cmp(&b.working_directory))
    });
    for pair in sessions.windows(2) {
        if pair[0].id == pair[1].id && pair[0].working_directory != pair[1].working_directory {
            // Never choose another profile/project silently for a colliding native ID.
            return Err("SESSION_ID_AMBIGUOUS".into());
        }
    }
    sessions.dedup_by(|a, b| a.id == b.id && a.working_directory == b.working_directory);
    Ok(sessions)
}

fn fingerprint(
    registry: &Registry,
    request: &Request,
    sessions: &[SessionSummary],
) -> Result<String, String> {
    let mut binding = request.clone();
    binding.cursor = None;
    // Binding includes limit/max_chars: resuming uses the same request options.
    let value = json!([registry.revision(), binding, sessions]);
    let bytes = serde_json::to_vec(&value).map_err(|_| "SESSION_CURSOR_INVALID".to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn decode_cursor(
    request: &Request,
    operation: &str,
    fingerprint: &str,
    count: usize,
) -> Result<Cursor, String> {
    let Some(text) = request.cursor.as_deref() else {
        return Ok(Cursor {
            version: 1,
            operation: operation.into(),
            fingerprint: fingerprint.into(),
            ..Default::default()
        });
    };
    if text.len() > 8192 {
        return Err("SESSION_CURSOR_INVALID".into());
    }
    let cursor: Cursor = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| "SESSION_CURSOR_INVALID".to_string())?,
    )
    .map_err(|_| "SESSION_CURSOR_INVALID".to_string())?;
    if cursor.version != 1
        || cursor.operation != operation
        || cursor.session > count
        || cursor.within_page > 50
    {
        return Err("SESSION_CURSOR_INVALID".into());
    }
    if cursor.fingerprint != fingerprint {
        return Err("SESSION_SNAPSHOT_CHANGED".into());
    }
    Ok(cursor)
}

fn encode_cursor(cursor: &Cursor) -> Result<String, String> {
    Ok(URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(cursor).map_err(|_| "SESSION_CURSOR_INVALID".to_string())?))
}

fn limit(request: &Request) -> Result<usize, String> {
    let limit = request.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err("SESSION_LIMIT_INVALID".into());
    }
    Ok(limit)
}

fn max_chars(request: &Request) -> Result<usize, String> {
    let value = request.max_chars.unwrap_or(4_000);
    if !(200..=MAX_CONTENT_CHARS).contains(&value) {
        return Err("SESSION_MAX_CHARS_INVALID".into());
    }
    Ok(value)
}

fn bounded_message(
    message: &crate::session_history::SessionTranscriptMessage,
    max_chars: usize,
    activity: bool,
) -> (crate::session_history::SessionTranscriptMessage, bool) {
    let mut visible = message.clone();
    visible.content = visible.content.chars().take(max_chars).collect();
    let mut truncated = visible.content.len() != message.content.len() || visible.files.len() > 4;
    visible.files.truncate(4);
    for file in &mut visible.files {
        let path: String = file.path.chars().take(1024).collect();
        truncated |= path.len() != file.path.len();
        file.path = path;
        for value in [&mut file.before, &mut file.after, &mut file.patch] {
            if let Some(text) = value {
                let clipped: String = text.chars().take(if activity { 0 } else { 512 }).collect();
                let omitted = clipped.len() != text.len();
                file.truncated |= omitted;
                truncated |= omitted;
                *value = if activity { None } else { Some(clipped) };
            }
        }
    }
    (visible, truncated)
}

fn window(request: &Request) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>), String> {
    let parse = |value: &Option<String>| {
        value
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .ok_or_else(|| "SESSION_TIME_RANGE_INVALID".to_string())
    };
    let from = parse(&request.from)?;
    let to = parse(&request.to)?;
    if to <= from || to.signed_duration_since(from).num_seconds() > 31 * 86400 {
        return Err("SESSION_TIME_RANGE_INVALID".into());
    }
    Ok((from, to))
}

pub(crate) fn invoke(operation: &str, input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_value(input.clone())?;
    // Validate before opening any source.
    limit(&request)?;
    if operation == "activity" {
        window(&request)?;
        max_chars(&request)?;
    }
    if request
        .query
        .as_ref()
        .is_some_and(|q| q.chars().count() > 200)
    {
        return Err("SESSION_QUERY_TOO_LONG".into());
    }
    if request.cursor.as_ref().is_some_and(|c| c.len() > 8192) {
        return Err("SESSION_CURSOR_INVALID".into());
    }
    let selected = selected_sources(&request)?;
    let registry = Registry::load()?;
    // Reject a foreign/invalid scope before touching client storage.
    scoped_sessions(&registry, &request, Vec::new())?;
    let (sessions, sources) = super::scan_sources(&registry, &selected)?;
    project(
        &registry,
        operation,
        &request,
        sessions,
        sources,
        |session, cursor| {
            super::transcript_page(
                &registry,
                session.project_id.as_deref(),
                session.id.clone(),
                cursor,
            )
        },
    )
    .map_err(Into::into)
}

fn project(
    registry: &Registry,
    operation: &str,
    request: &Request,
    sessions: Vec<SessionSummary>,
    sources: Vec<SessionSourceStatus>,
    mut load: impl FnMut(&SessionSummary, Option<String>) -> Result<SessionTranscriptPage, String>,
) -> Result<Value, String> {
    let sessions = scoped_sessions(registry, request, sessions)?;
    let fingerprint = fingerprint(registry, request, &sessions)?;
    let mut cursor = decode_cursor(request, operation, &fingerprint, sessions.len())?;
    let limit = limit(request)?;
    let complete = sources.iter().all(|s| {
        matches!(
            s.availability,
            crate::session_history::SourceAvailability::Available
        )
    });
    let mut response = json!({"schema_version":CONTRACT_VERSION,"snapshot_revision":fingerprint,
        "sources":sources,"complete":complete,"next_cursor":null});
    match operation {
        "sources" => {
            response["total"] = json!(sessions.len());
        }
        "list" => {
            let end = cursor.session.saturating_add(limit).min(sessions.len());
            response["sessions"] = json!(&sessions[cursor.session..end]);
            response["total"] = json!(sessions.len());
            cursor.session = end;
            if end < sessions.len() {
                response["next_cursor"] = json!(encode_cursor(&cursor)?);
            }
        }
        "messages" => {
            let session = sessions
                .iter()
                .find(|s| Some(&s.id) == request.session_id.as_ref())
                .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
            let page = load(session, cursor.message.clone())?;
            response["session"] = json!(session);
            if cursor.within_page > page.messages.len() {
                return Err("SESSION_SNAPSHOT_CHANGED".into());
            }
            let revision = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&page).map_err(|_| "SESSION_PAGE_INVALID".to_string())?
                )
            );
            if cursor
                .page_revision
                .as_ref()
                .is_some_and(|r| r != &revision)
            {
                return Err("SESSION_SNAPSHOT_CHANGED".into());
            }
            let end = cursor
                .within_page
                .saturating_add(limit)
                .min(page.messages.len());
            let mut truncated = false;
            let messages: Vec<_> = page.messages[cursor.within_page..end]
                .iter()
                .map(|message| {
                    let (message, clipped) = bounded_message(message, MAX_CONTENT_CHARS, false);
                    truncated |= clipped;
                    message
                })
                .collect();
            response["messages"] = json!(messages);
            response["truncated"] = json!(truncated);
            response["through_seq"] = json!(page.through_seq);
            response["usage"] = Value::Null;
            response["usage_status"] = json!("not_projected");
            if end < page.messages.len() {
                cursor.within_page = end;
                cursor.page_revision = Some(revision);
                response["next_cursor"] = json!(encode_cursor(&cursor)?);
            } else {
                if page.next_cursor.is_some() && page.next_cursor == cursor.message {
                    return Err("SESSION_CURSOR_NOT_ADVANCING".into());
                }
                cursor.message = page.next_cursor;
                cursor.within_page = 0;
                cursor.page_revision = None;
                if cursor.message.is_some() {
                    response["next_cursor"] = json!(encode_cursor(&cursor)?);
                }
            }
        }
        "activity" => {
            let (from, to) = window(request)?;
            let max_chars = max_chars(request)?;
            let mut rows = Vec::new();
            let mut skipped = Vec::new();
            let mut undated = 0;
            let mut pages = 0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while cursor.session < sessions.len()
                && rows.len() < limit
                && pages < MAX_BODY_PAGES
                && std::time::Instant::now() < deadline
            {
                let session = &sessions[cursor.session];
                let page = match load(session, cursor.message.clone()) {
                    Ok(page) => page,
                    Err(error) => {
                        skipped.push(json!({"session_id":session.id,"reason":error}));
                        cursor.session += 1;
                        cursor.message = None;
                        cursor.within_page = 0;
                        cursor.page_revision = None;
                        pages += 1;
                        continue;
                    }
                };
                pages += 1;
                if cursor.within_page > page.messages.len() {
                    return Err("SESSION_SNAPSHOT_CHANGED".into());
                }
                let page_revision = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&page).map_err(|_| "SESSION_PAGE_INVALID".to_string())?
                    )
                );
                if cursor
                    .page_revision
                    .as_ref()
                    .is_some_and(|expected| expected != &page_revision)
                {
                    return Err("SESSION_SNAPSHOT_CHANGED".into());
                }
                let mut consumed = cursor.within_page;
                for message in page.messages.iter().skip(cursor.within_page) {
                    consumed += 1;
                    if !matches!(message.role.as_str(), "user" | "assistant") {
                        continue;
                    }
                    let Some(time) = message
                        .timestamp
                        .as_deref()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    else {
                        undated += 1;
                        continue;
                    };
                    if time < from || time >= to {
                        continue;
                    }
                    let (visible, truncated) = bounded_message(message, max_chars, true);
                    let content_sha256 =
                        format!("{:x}", Sha256::digest(message.content.as_bytes()));
                    let kind = if !message.files.is_empty() {
                        "file_request"
                    } else {
                        "dialogue"
                    };
                    rows.push(json!({"session":session,"message":visible,"kind":kind,"truncated":truncated,
                        "evidence":{"session_id":session.id,"message_id":message.id,"snapshot_revision":fingerprint,"content_sha256":content_sha256}}));
                    if rows.len() >= limit {
                        break;
                    }
                }
                if consumed < page.messages.len() {
                    cursor.within_page = consumed;
                    cursor.page_revision = Some(page_revision);
                } else {
                    cursor.within_page = 0;
                    cursor.page_revision = None;
                    if page.next_cursor.is_some() && page.next_cursor == cursor.message {
                        return Err("SESSION_CURSOR_NOT_ADVANCING".into());
                    }
                    cursor.message = page.next_cursor;
                    if cursor.message.is_none() {
                        cursor.session += 1;
                    }
                }
            }
            response["activities"] = json!(rows);
            response["skipped"] = json!(skipped);
            response["undated_messages"] = json!(undated);
            response["usage_status"] = json!("not_projected");
            response["budget_exhausted"] = json!(
                (pages >= MAX_BODY_PAGES || std::time::Instant::now() >= deadline)
                    && cursor.session < sessions.len()
            );
            response["complete"] = json!(complete && skipped.is_empty() && undated == 0);
            if cursor.session < sessions.len() {
                response["next_cursor"] = json!(encode_cursor(&cursor)?);
            }
        }
        _ => return Err("SESSION_OPERATION_INVALID".into()),
    }
    if serde_json::to_vec(&response)
        .map_err(|_| "SESSION_RESPONSE_INVALID".to_string())?
        .len()
        > 12 * 1024 * 1024
    {
        return Err("SESSION_RESPONSE_TOO_LARGE".into());
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::{
        SessionCapabilities, SessionTranscriptMessage, SourceAvailability,
    };
    use crate::workspace_registry::RegistryProject;

    fn registry() -> Registry {
        Registry {
            projects: vec![
                RegistryProject {
                    id: "root".into(),
                    name: "设备平台".into(),
                    ..Default::default()
                },
                RegistryProject {
                    id: "child".into(),
                    name: "设备配置".into(),
                    parent_id: "root".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }
    fn session(id: &str, owner: Option<&str>) -> SessionSummary {
        SessionSummary::new(
            SessionSource::Codex,
            id.into(),
            format!("G:/synthetic/{id}"),
            "工作".into(),
            Some("2026-11-17T00:00:00Z".into()),
            None,
            SessionCapabilities::default(),
            owner.map(|id| crate::session_history::WorkspaceAssociation {
                project_id: id.into(),
                project_name: "别名".into(),
                binding_path: "G:/synthetic".into(),
            }),
        )
        .unwrap()
    }
    fn status() -> Vec<SessionSourceStatus> {
        vec![SessionSourceStatus {
            source: SessionSource::Codex,
            availability: SourceAvailability::Available,
            session_count: 1,
            error_code: None,
        }]
    }
    fn page(times: &[Option<&str>]) -> SessionTranscriptPage {
        SessionTranscriptPage {
            session_id: "codex:a".into(),
            through_seq: None,
            next_cursor: None,
            messages: times
                .iter()
                .enumerate()
                .map(|(i, time)| SessionTranscriptMessage {
                    id: i.to_string(),
                    role: "user".into(),
                    content: format!("实现功能 {i}"),
                    timestamp: time.map(Into::into),
                    files: vec![],
                })
                .collect(),
        }
    }
    fn request(input: Value) -> Request {
        serde_json::from_value(input).unwrap()
    }

    #[test]
    fn messages_limit_resumes_without_skipping_the_rest_of_a_native_page() {
        let mut r = request(json!({"session_id":"codex:a","limit":1}));
        let load = |_: &SessionSummary, _| Ok(page(&[None, None]));
        let one = project(
            &registry(),
            "messages",
            &r,
            vec![session("a", None)],
            status(),
            load,
        )
        .unwrap();
        assert_eq!(one["messages"].as_array().unwrap().len(), 1);
        r.cursor = one["next_cursor"].as_str().map(Into::into);
        let two = project(
            &registry(),
            "messages",
            &r,
            vec![session("a", None)],
            status(),
            load,
        )
        .unwrap();
        assert_eq!(two["messages"][0]["id"], "1");
        assert!(two["next_cursor"].is_null());
    }

    #[test]
    fn scopes_preserve_child_ownership_and_unassigned_sessions() {
        let data = vec![
            session("a", Some("root")),
            session("b", Some("child")),
            session("c", None),
        ];
        let run = |r: Request| {
            project(&registry(), "list", &r, data.clone(), status(), |_, _| {
                panic!("metadata must not read body")
            })
            .unwrap()
        };
        assert_eq!(run(request(json!({})))["total"], 3);
        let result = run(request(
            json!({"scope":"project","project_id":"root","include_children":true}),
        ));
        assert_eq!(result["total"], 2);
        assert_eq!(result["sessions"][1]["project_id"], "child");
        assert_eq!(
            run(request(json!({"scope":"unassigned"})))["sessions"][0]["native_id"],
            "c"
        );
        assert!(scoped_sessions(
            &registry(),
            &request(json!({"scope":"all","project_id":"root"})),
            data
        )
        .is_err());
    }

    #[test]
    fn date_window_is_half_open_and_not_based_on_session_update() {
        let r =
            request(json!({"from":"2026-11-16T00:00:00-07:00","to":"2026-11-17T00:00:00-07:00"}));
        let result = project(
            &registry(),
            "activity",
            &r,
            vec![session("a", None)],
            status(),
            |_, _| {
                Ok(page(&[
                    Some("2026-11-16T06:59:59Z"),
                    Some("2026-11-16T07:00:00Z"),
                    Some("2026-11-17T06:59:59Z"),
                    Some("2026-11-17T07:00:00Z"),
                    None,
                ]))
            },
        )
        .unwrap();
        let rows = result["activities"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["message"]["id"], "1");
        assert_eq!(result["undated_messages"], 1);
        assert_eq!(result["complete"], false);
        assert_eq!(
            rows[0]["evidence"]["content_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn activity_cursor_resumes_within_page_across_service_restart() {
        let mut r =
            request(json!({"from":"2026-11-16T00:00:00Z","to":"2026-11-17T00:00:00Z","limit":1}));
        let load = |_: &SessionSummary, _| {
            Ok(page(&[
                Some("2026-11-16T10:00:00Z"),
                Some("2026-11-16T11:00:00Z"),
            ]))
        };
        let one = project(
            &registry(),
            "activity",
            &r,
            vec![session("a", None)],
            status(),
            load,
        )
        .unwrap();
        r.cursor = one["next_cursor"].as_str().map(Into::into);
        let two = project(
            &registry(),
            "activity",
            &r,
            vec![session("a", None)],
            status(),
            load,
        )
        .unwrap();
        assert_eq!(one["activities"][0]["message"]["id"], "0");
        assert_eq!(two["activities"][0]["message"]["id"], "1");
        assert!(two["next_cursor"].is_null());
        let mut changed = session("a", None);
        changed.updated_at = Some("2026-11-18T00:00:00Z".into());
        assert_eq!(
            project(&registry(), "activity", &r, vec![changed], status(), load).unwrap_err(),
            "SESSION_SNAPSHOT_CHANGED"
        );
        let changed_page = |_: &SessionSummary, _| {
            let mut p = page(&[Some("2026-11-16T10:00:00Z"), Some("2026-11-16T11:00:00Z")]);
            p.messages[0].content = "changed".into();
            Ok(p)
        };
        assert_eq!(
            project(
                &registry(),
                "activity",
                &r,
                vec![session("a", None)],
                status(),
                changed_page
            )
            .unwrap_err(),
            "SESSION_SNAPSHOT_CHANGED"
        );
    }

    #[test]
    fn page_budget_returns_restartable_cursor_even_without_matching_rows() {
        let r = request(json!({"from":"2026-11-16T00:00:00Z","to":"2026-11-17T00:00:00Z"}));
        let data = (0..12)
            .map(|i| session(&format!("a{i:02}"), None))
            .collect();
        let result = project(&registry(), "activity", &r, data, status(), |_, _| {
            Ok(page(&[Some("2026-11-10T00:00:00Z")]))
        })
        .unwrap();
        assert!(result["activities"].as_array().unwrap().is_empty());
        assert_eq!(result["budget_exhausted"], true);
        assert!(result["next_cursor"].is_string());
    }

    #[test]
    fn failures_are_reported_and_cursor_advances_past_bad_session() {
        let r = request(json!({"from":"2026-11-16T00:00:00Z","to":"2026-11-17T00:00:00Z"}));
        let result = project(
            &registry(),
            "activity",
            &r,
            vec![session("a", None), session("b", None)],
            status(),
            |s, _| {
                if s.native_id == "a" {
                    Err("source_unreadable".into())
                } else {
                    Ok(page(&[Some("2026-11-16T00:00:00Z")]))
                }
            },
        )
        .unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(result["skipped"][0]["reason"], "source_unreadable");
        assert_eq!(result["activities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_ambiguous_ids_invalid_windows_and_foreign_scope_reads() {
        let mut twin = session("a", Some("root"));
        twin.working_directory = "G:/other".into();
        assert_eq!(
            scoped_sessions(
                &registry(),
                &Request::default(),
                vec![session("a", None), twin]
            )
            .unwrap_err(),
            "SESSION_ID_AMBIGUOUS"
        );
        assert!(window(&request(json!({"from":"2026-11-16","to":"2026-11-17"}))).is_err());
        assert!(window(&request(
            json!({"from":"2026-09-16T00:00:00Z","to":"2026-11-17T00:00:00Z"})
        ))
        .is_err());
        let r = request(json!({"scope":"project","project_id":"root","session_id":"codex:a"}));
        assert_eq!(
            project(
                &registry(),
                "messages",
                &r,
                vec![session("a", Some("child"))],
                status(),
                |_, _| panic!("out of scope")
            )
            .unwrap_err(),
            "SESSION_NOT_FOUND"
        );
    }
}
