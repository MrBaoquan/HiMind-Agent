//! Short, cancellable inspection batches. Jobs cache only a bounded snapshot and progress;
//! transcript bodies are not persisted. Every page is loaded through its source's scope checks.
use crate::session_history::{
    SessionHistorySnapshot, SessionSourceStatus, SessionSummary, SessionTranscriptPage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const MAX_SESSIONS: usize = 2000;
const MAX_PAGES: usize = 2000;
const MAX_HITS: usize = 500;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InspectHit {
    pub session: SessionSummary,
    pub message_id: String,
    pub cursor: Option<String>,
    pub previous: Vec<Option<String>>,
    pub excerpt: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InspectBatch {
    pub token: String,
    pub hits: Vec<InspectHit>,
    pub sources: Vec<SessionSourceStatus>,
    pub scanned_pages: usize,
    pub scanned_sessions: usize,
    pub total_sessions: usize,
    pub done: bool,
    pub limited: bool,
    pub cancelled: bool,
    pub issues: Vec<String>,
}

struct Job {
    project_id: String,
    sessions: Vec<SessionSummary>,
    sources: Vec<SessionSourceStatus>,
    position: usize,
    cursor: Option<String>,
    previous: Vec<Option<String>>,
    query: String,
    mode: String,
    pages: usize,
    hits: usize,
    bytes: usize,
    limited: bool,
    cancel: Arc<AtomicBool>,
    created: Instant,
}
struct Entry {
    project_id: String,
    job: Arc<Mutex<Job>>,
    cancel: Arc<AtomicBool>,
    created: Instant,
}
static JOBS: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
fn jobs() -> &'static Mutex<HashMap<String, Entry>> {
    JOBS.get_or_init(Default::default)
}

pub(crate) fn start(
    project_id: String,
    snapshot: SessionHistorySnapshot,
    session_id: Option<String>,
    query: String,
    mode: String,
) -> Result<String, String> {
    if !matches!(mode.as_str(), "text" | "prompts" | "files")
        || query.chars().count() > 200
        || (mode == "text" && query.trim().is_empty())
    {
        return Err("SESSION_SEARCH_INVALID".into());
    }
    let snapshot_truncated = snapshot.total > snapshot.sessions.len();
    let mut sessions: Vec<_> = snapshot
        .sessions
        .into_iter()
        .filter(|s| session_id.as_ref().is_none_or(|id| s.id == *id))
        .filter(|s| {
            mode != "files"
                || matches!(
                    s.source,
                    crate::session_history::SessionSource::ClaudeCode
                        | crate::session_history::SessionSource::Codex
                )
        })
        .collect();
    if session_id.is_some() && sessions.is_empty() {
        return Err("SESSION_NOT_FOUND".into());
    }
    let limited = session_id.is_none() && (snapshot_truncated || sessions.len() > MAX_SESSIONS);
    sessions.truncate(MAX_SESSIONS);
    let cancel = Arc::new(AtomicBool::new(false));
    let created = Instant::now();
    let entry = Entry {
        project_id: project_id.clone(),
        job: Arc::new(Mutex::new(Job {
            project_id,
            sessions,
            sources: snapshot.sources,
            position: 0,
            cursor: None,
            previous: vec![],
            query: query.trim().to_lowercase(),
            mode,
            pages: 0,
            hits: 0,
            bytes: 0,
            limited,
            cancel: cancel.clone(),
            created,
        })),
        cancel,
        created,
    };
    let token = format!("{:032x}", rand::random::<u128>());
    let mut store = jobs().lock().map_err(|_| "SESSION_SEARCH_BUSY")?;
    store.retain(|_, entry| entry.created.elapsed() < Duration::from_secs(300));
    if store.len() >= 8 {
        return Err("SESSION_SEARCH_BUSY".into());
    }
    store.insert(token.clone(), entry);
    Ok(token)
}

pub(crate) fn cancel(project_id: &str, token: &str) -> Result<(), String> {
    let mut store = jobs().lock().map_err(|_| "SESSION_SEARCH_BUSY")?;
    if let Some(entry) = store.get(token) {
        if entry.project_id != project_id {
            return Err("PROJECT_NOT_FOUND".into());
        }
        let flag = entry.cancel.clone();
        // Cancellation never waits for the job lock held by a running batch.
        flag.store(true, Ordering::Relaxed);
        store.remove(token);
    }
    Ok(())
}

pub(crate) fn step(
    project_id: &str,
    token: &str,
    mut load: impl FnMut(&str, Option<String>) -> Result<SessionTranscriptPage, String>,
) -> Result<InspectBatch, String> {
    let job = jobs()
        .lock()
        .map_err(|_| "SESSION_SEARCH_BUSY")?
        .get(token)
        .map(|entry| entry.job.clone())
        .ok_or("SESSION_SEARCH_EXPIRED")?;
    let mut job = job.lock().map_err(|_| "SESSION_SEARCH_BUSY")?;
    if job.project_id != project_id {
        return Err("PROJECT_NOT_FOUND".into());
    }
    if job.created.elapsed() >= Duration::from_secs(300) {
        return Err("SESSION_SEARCH_EXPIRED".into());
    }
    let mut hits = vec![];
    let mut issues = vec![];
    let started = Instant::now();
    for _ in 0..2 {
        if job.position >= job.sessions.len() || job.cancel.load(Ordering::Relaxed) {
            break;
        }
        if job.pages >= MAX_PAGES || job.hits >= MAX_HITS || job.bytes >= MAX_BYTES {
            job.limited = true;
            break;
        }
        let session = job.sessions[job.position].clone();
        let cursor = job.cursor.clone();
        job.pages += 1;
        match load(&session.id, cursor.clone()) {
            Ok(page) => {
                if page.session_id != session.id {
                    return Err("SESSION_ID_INVALID".into());
                }
                for message in page.messages {
                    if message.files.iter().any(|file| file.truncated) {
                        let issue = format!("{}: file_content_truncated", session.source.id());
                        if !issues.contains(&issue) {
                            issues.push(issue);
                        }
                    }
                    let file_bytes: usize = message
                        .files
                        .iter()
                        .map(|f| {
                            f.path.len()
                                + f.before.as_ref().map_or(0, String::len)
                                + f.after.as_ref().map_or(0, String::len)
                                + f.patch.as_ref().map_or(0, String::len)
                        })
                        .sum();
                    job.bytes = job
                        .bytes
                        .saturating_add(message.content.len())
                        .saturating_add(file_bytes);
                    let file_match = message.files.iter().find(|file| {
                        [
                            Some(file.path.as_str()),
                            file.before.as_deref(),
                            file.after.as_deref(),
                            file.patch.as_deref(),
                        ]
                        .into_iter()
                        .flatten()
                        .any(|text| text.to_lowercase().contains(&job.query))
                    });
                    let matches = match job.mode.as_str() {
                        "prompts" => crate::session_history_tools::is_prompt(&message),
                        "files" => {
                            !message.files.is_empty()
                                && (job.query.is_empty()
                                    || message
                                        .files
                                        .iter()
                                        .any(|file| file.path.to_lowercase().contains(&job.query)))
                        }
                        _ => {
                            message.content.to_lowercase().contains(&job.query)
                                || file_match.is_some()
                        }
                    };
                    if matches && job.hits < MAX_HITS && job.bytes < MAX_BYTES {
                        let excerpt = if !message.content.to_lowercase().contains(&job.query)
                            && file_match.is_some()
                        {
                            let file = file_match.unwrap();
                            let text = [
                                file.before.as_deref(),
                                file.after.as_deref(),
                                file.patch.as_deref(),
                                Some(file.path.as_str()),
                            ]
                            .into_iter()
                            .flatten()
                            .find(|text| text.to_lowercase().contains(&job.query))
                            .unwrap_or(&file.path);
                            excerpt(text, &job.query)
                        } else {
                            excerpt(&message.content, &job.query)
                        };
                        hits.push(InspectHit {
                            session: session.clone(),
                            message_id: message.id,
                            cursor: cursor.clone(),
                            previous: job.previous.clone(),
                            excerpt,
                            paths: message.files.iter().map(|f| f.path.clone()).collect(),
                        });
                        job.hits += 1;
                    }
                }
                if let Some(next) = page
                    .next_cursor
                    .filter(|next| Some(next) != cursor.as_ref())
                {
                    job.previous.push(cursor);
                    job.cursor = Some(next);
                } else {
                    job.position += 1;
                    job.cursor = None;
                    job.previous.clear();
                }
            }
            Err(error) => {
                issues.push(format!("{}: {error}", session.source.id()));
                job.position += 1;
                job.cursor = None;
                job.previous.clear();
            }
        }
        if started.elapsed() > Duration::from_millis(150) {
            break;
        }
    }
    let cancelled = job.cancel.load(Ordering::Relaxed);
    let budget = job.pages >= MAX_PAGES || job.hits >= MAX_HITS || job.bytes >= MAX_BYTES;
    let done = job.position >= job.sessions.len() || cancelled || budget;
    let result = InspectBatch {
        token: token.into(),
        hits,
        sources: job.sources.clone(),
        scanned_pages: job.pages,
        scanned_sessions: job.position,
        total_sessions: job.sessions.len(),
        done,
        limited: job.limited || budget,
        cancelled,
        issues,
    };
    drop(job);
    if done {
        jobs()
            .lock()
            .map_err(|_| "SESSION_SEARCH_BUSY")?
            .remove(token);
    }
    Ok(result)
}

fn excerpt(content: &str, query: &str) -> String {
    let lower = content.to_lowercase();
    let position = lower
        .find(query)
        .map(|offset| lower[..offset].chars().count())
        .unwrap_or(0);
    let start = position.saturating_sub(45);
    let mut text: String = content.chars().skip(start).take(220).collect();
    if start > 0 {
        text.insert(0, '…');
    }
    if start + 220 < content.chars().count() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::{SessionCapabilities, SessionSource, SessionTranscriptMessage};
    fn snapshot() -> SessionHistorySnapshot {
        let sessions = SessionSource::ALL
            .into_iter()
            .map(|source| {
                SessionSummary::new(
                    source,
                    "s".into(),
                    "C:/synthetic".into(),
                    "Synthetic".into(),
                    None,
                    None,
                    SessionCapabilities::default(),
                    None,
                )
                .unwrap()
            })
            .collect();
        SessionHistorySnapshot {
            sessions,
            sources: vec![],
            total: 5,
            next_cursor: None,
        }
    }
    #[test]
    fn session_history_search_visits_five_sources_and_cross_page_location() {
        let token = start("p".into(), snapshot(), None, "needle".into(), "text".into()).unwrap();
        let mut hits = vec![];
        loop {
            let batch = step("p", &token, |session, cursor| {
                Ok(SessionTranscriptPage {
                    session_id: session.into(),
                    through_seq: None,
                    messages: vec![SessionTranscriptMessage {
                        id: cursor.clone().unwrap_or("latest".into()),
                        role: "user".into(),
                        content: "needle 中".into(),
                        timestamp: None,
                        files: vec![],
                    }],
                    next_cursor: cursor.is_none().then(|| "50".into()),
                })
            })
            .unwrap();
            hits.extend(batch.hits);
            if batch.done {
                assert!(!batch.limited);
                break;
            }
        }
        assert_eq!(hits.len(), 10);
        assert_eq!(hits[1].cursor.as_deref(), Some("50"));
        assert_eq!(hits[1].previous, vec![None]);
        assert!(step("p", &token, |_, _| panic!()).is_err());
    }
    #[test]
    fn session_history_search_is_cancellable_and_reports_read_failures() {
        let token = start("p".into(), snapshot(), None, "x".into(), "text".into()).unwrap();
        let batch = step("p", &token, |_, _| Err("unavailable".into())).unwrap();
        assert_eq!(batch.issues.len(), 2);
        assert!(cancel("other-project", &token).is_err());
        assert!(step("other-project", &token, |_, _| panic!()).is_err());
        cancel("p", &token).unwrap();
        assert!(step("p", &token, |_, _| panic!()).is_err());
        assert!(start("p".into(), snapshot(), None, " ".into(), "text".into()).is_err());
    }
    #[test]
    fn session_history_search_limits_results_and_includes_file_fragments() {
        let token = start("p".into(), snapshot(), None, "needle".into(), "text".into()).unwrap();
        let mut count = 0;
        loop {
            let batch = step("p", &token, |session, cursor| {
                let offset = cursor.as_deref().unwrap_or("0").parse::<usize>().unwrap();
                Ok(SessionTranscriptPage {
                    session_id: session.into(),
                    through_seq: None,
                    messages: (0..50)
                        .map(|i| SessionTranscriptMessage {
                            id: format!("{offset}:{i}"),
                            role: "assistant".into(),
                            content: "tool request".into(),
                            timestamp: None,
                            files: vec![crate::session_history_tools::HistoryFile {
                                path: "a.ts".into(),
                                operation: "Edit".into(),
                                kind: "fragment".into(),
                                before: Some("needle 中".into()),
                                after: None,
                                patch: None,
                                truncated: false,
                            }],
                        })
                        .collect(),
                    next_cursor: Some((offset + 1).to_string()),
                })
            })
            .unwrap();
            count += batch.hits.len();
            if batch.done {
                assert!(batch.limited);
                break;
            }
        }
        assert_eq!(count, MAX_HITS);
        assert!(excerpt(&format!("{}needle", "中".repeat(1000)), "needle").contains("needle"));
    }
}
