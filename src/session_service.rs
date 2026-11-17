//! Shared read-only session service used by desktop commands and the capability gateway.

use serde_json::json;

pub(crate) mod host;
mod query;
pub(crate) use query::{capability_schema, invoke};

pub(crate) fn workspace_history_snapshot_scoped(
    project_id: String,
    cursor: Option<String>,
    query: Option<String>,
    source: Option<String>,
    sort: Option<String>,
    page_size: usize,
    include_children: bool,
) -> Result<crate::session_history::SessionHistorySnapshot, String> {
    const MAX_OFFSET: usize = 1_000_000;
    let offset = cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| "SESSION_CURSOR_INVALID".to_string())
        })
        .transpose()?
        .unwrap_or(0);
    if offset > MAX_OFFSET {
        return Err("SESSION_CURSOR_INVALID".to_string());
    }
    let query = query
        .as_deref()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(200)
        .collect::<String>()
        .to_lowercase();
    let source_filter = source
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| match value.trim() {
            "dsh" => Ok(crate::session_history::SessionSource::Dsh),
            "acp" => Ok(crate::session_history::SessionSource::Acp),
            "claude_code" => Ok(crate::session_history::SessionSource::ClaudeCode),
            "codex" => Ok(crate::session_history::SessionSource::Codex),
            "opencode" => Ok(crate::session_history::SessionSource::OpenCode),
            _ => Err("SESSION_SOURCE_INVALID".to_string()),
        })
        .transpose()?;
    let sort = match sort.as_deref().unwrap_or("recent") {
        "recent" => crate::session_history::SessionHistorySort::Recent,
        "oldest" => crate::session_history::SessionHistorySort::Oldest,
        "title" => crate::session_history::SessionHistorySort::Title,
        _ => return Err("SESSION_SORT_INVALID".to_string()),
    };
    let registry =
        crate::workspace_registry::Registry::load().map_err(|error| error.to_string())?;
    if registry.project(&project_id).is_none() {
        return Err("PROJECT_NOT_FOUND".to_string());
    }

    let (sessions, sources) = scan_sources(&registry, &crate::session_history::SessionSource::ALL)?;
    Ok(crate::session_history::scoped_project_history_snapshot(
        &registry,
        &project_id,
        include_children,
        &query,
        source_filter,
        sort,
        offset,
        page_size,
        sessions,
        sources,
    ))
}

pub(crate) fn workspace_transcript_page(
    project_id: String,
    session_id: String,
    cursor: Option<String>,
) -> Result<crate::session_history::SessionTranscriptPage, String> {
    let registry =
        crate::workspace_registry::Registry::load().map_err(|error| error.to_string())?;
    transcript_page(&registry, Some(&project_id), session_id, cursor)
}

pub(crate) fn transcript_page(
    registry: &crate::workspace_registry::Registry,
    project_id: Option<&str>,
    session_id: String,
    cursor: Option<String>,
) -> Result<crate::session_history::SessionTranscriptPage, String> {
    const PAGE_SIZE: usize = 50;
    let (source_id, native_id) = session_id
        .split_once(':')
        .ok_or_else(|| "SESSION_ID_INVALID".to_string())?;
    let source = match source_id {
        "dsh" => crate::session_history::SessionSource::Dsh,
        "acp" => crate::session_history::SessionSource::Acp,
        "claude_code" => crate::session_history::SessionSource::ClaudeCode,
        "codex" => crate::session_history::SessionSource::Codex,
        "opencode" => crate::session_history::SessionSource::OpenCode,
        _ => return Err("SESSION_PREVIEW_UNAVAILABLE".to_string()),
    };
    if source.stable_session_id(native_id)? != session_id {
        return Err("SESSION_ID_INVALID".to_string());
    }
    let requested_cursor = cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u64>()
                .ok()
                .ok_or_else(|| "SESSION_CURSOR_INVALID".to_string())
        })
        .transpose()?;
    if project_id.is_some_and(|id| registry.project(id).is_none()) {
        return Err("PROJECT_NOT_FOUND".to_string());
    }

    if source == crate::session_history::SessionSource::Acp {
        let stored = crate::store::acp_sessions::load_for_history(native_id)
            .map_err(|_| "ACP_SESSION_UNAVAILABLE".to_string())?
            .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
        let turns = stored
            .turns
            .iter()
            .map(|turn| (turn.user.clone(), turn.assistant.clone()))
            .collect::<Vec<_>>();
        let end_offset = requested_cursor
            .map(|value| usize::try_from(value).map_err(|_| "SESSION_CURSOR_INVALID".to_string()))
            .transpose()?;
        let owner = crate::session_history::associate_workspace(registry, &stored.cwd)
            .map_err(|_| "SESSION_NOT_FOUND".to_string())?;
        if project_id.is_some_and(|id| owner.as_ref().map(|a| a.project_id.as_str()) != Some(id)) {
            return Err("SESSION_NOT_FOUND".to_string());
        }
        let mut page = crate::session_history::paired_transcript_page(
            &session_id,
            &turns,
            end_offset,
            PAGE_SIZE,
        )?;
        for message in &mut page.messages {
            let Some((index, role)) = message.id.split_once(':') else {
                continue;
            };
            if let Some(turn) = index
                .parse::<usize>()
                .ok()
                .and_then(|i| stored.turns.get(i))
            {
                let at = if role == "user" {
                    turn.user_at
                } else {
                    turn.assistant_at
                };
                message.timestamp = at
                    .and_then(|at| i64::try_from(at).ok())
                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                    .map(|at| at.to_rfc3339());
            }
        }
        return Ok(page);
    }

    if matches!(
        source,
        crate::session_history::SessionSource::ClaudeCode
            | crate::session_history::SessionSource::Codex
    ) {
        return match source {
            crate::session_history::SessionSource::ClaudeCode => {
                crate::session_history_sources::claude_transcript_page(
                    registry,
                    project_id,
                    native_id,
                    requested_cursor,
                    PAGE_SIZE,
                )
            }
            crate::session_history::SessionSource::Codex => {
                crate::session_history_sources::codex_transcript_page(
                    registry,
                    project_id,
                    native_id,
                    requested_cursor,
                    PAGE_SIZE,
                )
            }
            _ => unreachable!(),
        };
    }

    if source == crate::session_history::SessionSource::OpenCode {
        let end_offset = requested_cursor
            .map(|value| usize::try_from(value).map_err(|_| "SESSION_CURSOR_INVALID".to_string()))
            .transpose()?;
        return crate::session_history_sources::opencode_transcript_page(
            registry, project_id, native_id, end_offset, PAGE_SIZE,
        );
    }

    let controls = host::controls()?;
    if controls.is_empty() {
        return Err("runtime_not_running".to_string());
    }
    let mut failures = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for (_, control) in controls {
        if std::time::Instant::now() >= deadline {
            failures += 1;
            break;
        }
        let listing = control
            .probe_runtime_api("session/list", json!({ "_request": {} }))
            .and_then(|response| {
                crate::app::builtin_ai_proxy::runtime_result_value(
                    &response,
                    "读取 HiMind AI 会话列表",
                )
                .map(serde_json::Value::clone)
            });
        let listing = match listing {
            Ok(value) => value,
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        let latest_seq = match crate::session_history::dsh_session_as_of_seq(
            registry, &listing, native_id, project_id,
        ) {
            Ok(Some(sequence)) => sequence,
            Ok(None) => continue,
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        let through_seq = requested_cursor
            .map(|value| i64::try_from(value).map_err(|_| "SESSION_CURSOR_INVALID".to_string()))
            .transpose()?
            .unwrap_or(latest_seq);
        if through_seq > latest_seq {
            return Err("SESSION_CURSOR_INVALID".to_string());
        }
        let response = control
            .probe_runtime_api(
                "session/page",
                json!({
                    "request": {
                        "address": {"kind": "session", "sessionId": native_id},
                        "throughSeq": through_seq,
                        "maxMessages": PAGE_SIZE + 1
                    }
                }),
            )
            .and_then(|response| {
                crate::app::builtin_ai_proxy::runtime_result_value(
                    &response,
                    "读取 HiMind AI 会话消息页",
                )
                .map(serde_json::Value::clone)
            });
        match response.and_then(|value| {
            crate::session_history::dsh_transcript_page(&session_id, through_seq, &value, PAGE_SIZE)
        }) {
            Ok(page) => return Ok(page),
            Err(_) => failures += 1,
        }
    }
    if failures > 0 {
        Err("runtime_query_failed".to_string())
    } else {
        Err("SESSION_NOT_FOUND".to_string())
    }
}

pub(crate) fn scan_sources(
    registry: &crate::workspace_registry::Registry,
    selected: &[crate::session_history::SessionSource],
) -> Result<
    (
        Vec<crate::session_history::SessionSummary>,
        Vec<crate::session_history::SessionSourceStatus>,
    ),
    String,
> {
    let controls = if selected.contains(&crate::session_history::SessionSource::Dsh) {
        host::controls()?
    } else {
        Vec::new()
    };
    let mut sessions = Vec::new();
    let mut successes = 0usize;
    let mut failures = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    for (_, control) in &controls {
        if std::time::Instant::now() >= deadline {
            failures += 1;
            break;
        }
        let result = control
            .probe_runtime_api("session/list", json!({ "_request": {} }))
            .and_then(|response| {
                crate::app::builtin_ai_proxy::runtime_result_value(
                    &response,
                    "读取 HiMind AI 会话列表",
                )
                .map(serde_json::Value::clone)
            })
            .and_then(|value| crate::session_history::dsh_session_summaries(&registry, &value));
        match result {
            Ok(items) => {
                successes += 1;
                sessions.extend(items);
            }
            Err(_) => failures += 1,
        }
    }

    let (availability, error_code) = if controls.is_empty() {
        (
            crate::session_history::SourceAvailability::Unavailable,
            Some("runtime_not_running".to_string()),
        )
    } else if failures == 0 {
        (crate::session_history::SourceAvailability::Available, None)
    } else if successes == 0 {
        (
            crate::session_history::SourceAvailability::Error,
            Some("runtime_query_failed".to_string()),
        )
    } else {
        (
            crate::session_history::SourceAvailability::Partial,
            Some("runtime_query_partial".to_string()),
        )
    };

    let mut sources = vec![crate::session_history::SessionSourceStatus {
        source: crate::session_history::SessionSource::Dsh,
        availability,
        session_count: 0,
        error_code,
    }];
    if !selected.contains(&crate::session_history::SessionSource::Dsh) {
        sources.clear();
    }
    for source in selected {
        let scan = match source {
            crate::session_history::SessionSource::ClaudeCode => {
                crate::session_history_sources::scan_claude_code(registry)
            }
            crate::session_history::SessionSource::Acp => {
                crate::session_history_sources::scan_acp(registry)
            }
            crate::session_history::SessionSource::Codex => {
                crate::session_history_sources::scan_codex(registry)
            }
            crate::session_history::SessionSource::OpenCode => {
                crate::session_history_sources::scan_opencode(registry)
            }
            crate::session_history::SessionSource::Dsh => continue,
        };
        sessions.extend(scan.sessions);
        sources.push(scan.status);
    }

    let claude_available = crate::session_history_actions::available(
        crate::session_history::SessionSource::ClaudeCode,
        "probe",
    );
    let codex_available = crate::session_history_actions::available(
        crate::session_history::SessionSource::Codex,
        "probe",
    );
    for session in &mut sessions {
        session.capabilities.can_resume =
            crate::session_history_actions::valid_resume_id(&session.native_id)
                && match session.source {
                    crate::session_history::SessionSource::ClaudeCode => claude_available,
                    crate::session_history::SessionSource::Codex => codex_available,
                    _ => false,
                };
    }
    for status in &mut sources {
        status.session_count = sessions
            .iter()
            .filter(|s| s.source == status.source)
            .count();
    }
    Ok((sessions, sources))
}
