//! Read-only, workspace-scoped projections of histories owned by local clients.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace_registry::{self, Registry};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum SessionSource {
    Dsh,
    Acp,
    ClaudeCode,
    Codex,
    OpenCode,
    External(String),
}

impl SessionSource {
    pub(crate) const ALL: [Self; 5] = [
        Self::Dsh,
        Self::Acp,
        Self::ClaudeCode,
        Self::Codex,
        Self::OpenCode,
    ];

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Dsh => "dsh",
            Self::Acp => "acp",
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::External(id) => id,
        }
    }

    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Dsh => "HiMind AI",
            Self::Acp => "ACP",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::External(id) => match id.as_str() {
                "claude-desktop" => "Claude Desktop",
                "github-copilot" => "GitHub Copilot",
                "vscode" => "VS Code Chat",
                "workbuddy" => "WorkBuddy",
                _ => id,
            },
        }
    }

    pub(crate) fn stable_session_id(&self, native_id: &str) -> Result<String, String> {
        if native_id.trim().is_empty() || native_id.chars().any(char::is_control) {
            return Err("session id is invalid".to_string());
        }
        Ok(format!("{}:{native_id}", self.id()))
    }
}

impl TryFrom<String> for SessionSource {
    type Error = String;
    fn try_from(id: String) -> Result<Self, Self::Error> {
        Ok(match id.as_str() {
            "dsh" => Self::Dsh,
            "acp" => Self::Acp,
            "claude_code" | "claude-code" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "opencode" | "open_code" => Self::OpenCode,
            _ if !id.is_empty()
                && id.len() <= 64
                && id.as_bytes()[0].is_ascii_alphanumeric()
                && id.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.')
                }) =>
            {
                Self::External(id)
            }
            _ => return Err("SESSION_SOURCE_INVALID".into()),
        })
    }
}

impl From<SessionSource> for String {
    fn from(source: SessionSource) -> Self {
        source.id().to_string()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionCapabilities {
    pub can_preview: bool,
    pub can_open_original: bool,
    pub can_resume: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionSummary {
    pub id: String,
    pub source: SessionSource,
    pub native_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub working_directory: String,
    pub title: String,
    pub updated_at: Option<String>,
    pub message_count: Option<usize>,
    pub capabilities: SessionCapabilities,
}

impl SessionSummary {
    pub(crate) fn new(
        source: SessionSource,
        native_id: String,
        working_directory: String,
        title: String,
        updated_at: Option<String>,
        message_count: Option<usize>,
        capabilities: SessionCapabilities,
        association: Option<WorkspaceAssociation>,
    ) -> Result<Self, String> {
        let id = source.stable_session_id(&native_id)?;
        let (project_id, project_name) = association
            .map(|item| (Some(item.project_id), Some(item.project_name)))
            .unwrap_or((None, None));
        Ok(Self {
            id,
            source,
            native_id,
            project_id,
            project_name,
            working_directory,
            title,
            updated_at,
            message_count,
            capabilities,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct WorkspaceAssociation {
    pub project_id: String,
    pub project_name: String,
    pub binding_path: String,
}

pub(crate) fn associate_workspace(
    registry: &Registry,
    working_directory: &str,
) -> Result<Option<WorkspaceAssociation>, String> {
    let normalized = workspace_registry::normalize_path(working_directory)?;
    let Some(hit) = workspace_registry::resolve_path(registry, &normalized)? else {
        return Ok(None);
    };
    Ok(Some(WorkspaceAssociation {
        project_name: hit.project.display_name().to_string(),
        project_id: hit.project.id,
        binding_path: hit.workspace.path,
    }))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceAvailability {
    Available,
    Partial,
    Unavailable,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionHistorySort {
    Recent,
    Oldest,
    Title,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionSourceStatus {
    pub source: SessionSource,
    pub availability: SourceAvailability,
    pub session_count: usize,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionHistorySnapshot {
    pub sessions: Vec<SessionSummary>,
    pub sources: Vec<SessionSourceStatus>,
    pub total: usize,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionTranscriptMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::session_history_tools::HistoryFile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionTranscriptPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_cursor: Option<String>,
    pub session_id: String,
    pub through_seq: Option<i64>,
    pub messages: Vec<SessionTranscriptMessage>,
    pub next_cursor: Option<String>,
}

pub(crate) fn paired_transcript_page(
    session_id: &str,
    turns: &[(String, String)],
    end_offset: Option<usize>,
    page_size: usize,
) -> Result<SessionTranscriptPage, String> {
    if page_size == 0 {
        return Err("transcript page size is invalid".to_string());
    }
    let total = turns.len().saturating_mul(2);
    let end = end_offset.unwrap_or(total);
    if end > total {
        return Err("transcript cursor is invalid".to_string());
    }
    let start = end.saturating_sub(page_size);
    let messages = (start..end)
        .map(|index| {
            let turn_index = index / 2;
            let (role, content) = if index % 2 == 0 {
                ("user", &turns[turn_index].0)
            } else {
                ("assistant", &turns[turn_index].1)
            };
            SessionTranscriptMessage {
                files: Vec::new(),
                id: format!("{turn_index}:{role}"),
                role: role.to_string(),
                content: content.clone(),
                timestamp: None,
            }
        })
        .collect();
    Ok(SessionTranscriptPage {
        snapshot_cursor: Some(end.to_string()),
        session_id: session_id.to_string(),
        through_seq: Some(end.saturating_sub(1) as i64),
        messages,
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

#[cfg(test)]
pub(crate) fn dsh_projected_session_as_of_seq(
    registry: &Registry,
    value: &Value,
    native_id: &str,
    project_id: &str,
) -> Result<Option<i64>, String> {
    dsh_session_as_of_seq(registry, value, native_id, Some(project_id))
}

pub(crate) fn dsh_session_as_of_seq(
    registry: &Registry,
    value: &Value,
    native_id: &str,
    project_id: Option<&str>,
) -> Result<Option<i64>, String> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "DSH session list response is invalid".to_string())?;
    let Some(item) = items
        .iter()
        .find(|item| item.get("sessionId").and_then(Value::as_str) == Some(native_id))
    else {
        return Ok(None);
    };
    if item.get("blank").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let working_directory = item
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty());
    let Some(working_directory) = working_directory else {
        return Ok(None);
    };
    let association = associate_workspace(registry, working_directory)?;
    if project_id.is_some_and(|id| association.as_ref().map(|a| a.project_id.as_str()) != Some(id))
    {
        return Ok(None);
    }
    let sequence = item
        .pointer("/projections/asOfSeq")
        .and_then(Value::as_i64)
        .filter(|sequence| *sequence >= 0)
        .ok_or_else(|| "DSH session list is missing a valid sequence boundary".to_string())?;
    Ok(Some(sequence))
}

pub(crate) fn dsh_transcript_page(
    session_id: &str,
    through_seq: i64,
    value: &Value,
    page_size: usize,
) -> Result<SessionTranscriptPage, String> {
    let items = value
        .get("items")
        .or_else(|| value.get("events"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/page/items"))
        .or_else(|| value.pointer("/page/messages"))
        .and_then(Value::as_array)
        .ok_or_else(|| "DSH session page response is invalid".to_string())?;
    let mut messages = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| dsh_transcript_message(item, index))
        .collect::<Vec<_>>();
    messages.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.message.id.cmp(&right.message.id))
    });

    let has_older = messages.len() > page_size;
    if has_older {
        let drop_count = messages.len() - page_size;
        messages.drain(0..drop_count);
    }
    let next_cursor = has_older
        .then(|| messages.first().and_then(|item| item.sequence))
        .flatten()
        .and_then(|sequence| sequence.checked_sub(1))
        .filter(|sequence| *sequence >= 0)
        .map(|sequence| sequence.to_string());

    Ok(SessionTranscriptPage {
        snapshot_cursor: Some(through_seq.to_string()),
        session_id: session_id.to_string(),
        through_seq: Some(through_seq),
        messages: messages.into_iter().map(|item| item.message).collect(),
        next_cursor,
    })
}

struct SequencedTranscriptMessage {
    sequence: Option<i64>,
    message: SessionTranscriptMessage,
}

fn dsh_transcript_message(value: &Value, index: usize) -> Option<SequencedTranscriptMessage> {
    let event_type = value.get("type").and_then(Value::as_str);
    let (role, content) = match event_type {
        Some("user/message")
            if value.get("surfaceOp").and_then(Value::as_str) == Some("append")
                && value.pointer("/data/source/kind").and_then(Value::as_str) == Some("user") =>
        {
            ("user", value.pointer("/data/content")?)
        }
        Some("assistant/message")
            if value.get("surfaceOp").and_then(Value::as_str) == Some("append") =>
        {
            ("assistant", value.pointer("/data/message/content")?)
        }
        _ => {
            let role = value.get("role").and_then(Value::as_str)?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            (role, value.get("content")?)
        }
    };
    let content = visible_message_text(content);
    if content.trim().is_empty() {
        return None;
    }
    let sequence = value
        .get("seq")
        .or_else(|| value.get("sequence"))
        .and_then(Value::as_i64)
        .filter(|sequence| *sequence >= 0);
    let id = sequence
        .map(|sequence| sequence.to_string())
        .unwrap_or_else(|| format!("message-{index}"));
    Some(SequencedTranscriptMessage {
        sequence,
        message: SessionTranscriptMessage {
            files: Vec::new(),
            id,
            role: role.to_string(),
            content,
            timestamp: timestamp(value.get("time").or_else(|| value.get("timestamp"))),
        },
    })
}

fn visible_message_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(block) if block.get("type").and_then(Value::as_str) == Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

pub(crate) fn dsh_session_summaries(
    registry: &Registry,
    value: &Value,
) -> Result<Vec<SessionSummary>, String> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "DSH session list response is invalid".to_string())?;
    let mut sessions = Vec::with_capacity(items.len());
    for item in items {
        if item.get("blank").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(native_id) = item.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let working_directory = item
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let association = if working_directory.is_empty() {
            None
        } else {
            associate_workspace(registry, &working_directory)
                .ok()
                .flatten()
        };
        let title = item
            .get("title")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("HiMind AI 会话")
            .to_string();
        let capabilities = SessionCapabilities {
            can_preview: true,
            can_open_original: false,
            can_resume: false,
        };
        if let Ok(session) = SessionSummary::new(
            SessionSource::Dsh,
            native_id.to_string(),
            working_directory,
            title,
            timestamp(item.get("updatedAt")),
            item.get("messageCount")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok()),
            capabilities,
            association,
        ) {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

pub(crate) fn project_history_snapshot(
    project_id: &str,
    query: &str,
    source_filter: Option<SessionSource>,
    sort: SessionHistorySort,
    offset: usize,
    page_size: usize,
    sessions: Vec<SessionSummary>,
    sources: Vec<SessionSourceStatus>,
) -> SessionHistorySnapshot {
    history_snapshot_for_ids(
        &std::collections::HashSet::from([project_id.to_string()]),
        None,
        query,
        source_filter,
        sort,
        offset,
        page_size,
        sessions,
        sources,
    )
}

pub(crate) fn scoped_project_history_snapshot(
    registry: &Registry,
    project_id: &str,
    include_children: bool,
    query: &str,
    source_filter: Option<SessionSource>,
    sort: SessionHistorySort,
    offset: usize,
    page_size: usize,
    sessions: Vec<SessionSummary>,
    sources: Vec<SessionSourceStatus>,
) -> SessionHistorySnapshot {
    history_snapshot_for_ids(
        &registry.scope_ids(project_id, include_children),
        Some(registry),
        query,
        source_filter,
        sort,
        offset,
        page_size,
        sessions,
        sources,
    )
}

fn history_snapshot_for_ids(
    ids: &std::collections::HashSet<String>,
    registry: Option<&Registry>,
    query: &str,
    source_filter: Option<SessionSource>,
    sort: SessionHistorySort,
    offset: usize,
    page_size: usize,
    sessions: Vec<SessionSummary>,
    mut sources: Vec<SessionSourceStatus>,
) -> SessionHistorySnapshot {
    let query = query.trim().to_lowercase();
    let mut sessions = sessions
        .into_iter()
        .filter(|session| {
            session
                .project_id
                .as_ref()
                .is_some_and(|id| ids.contains(id))
        })
        .filter(|session| {
            query.is_empty()
                || session.title.to_lowercase().contains(&query)
                || session.working_directory.to_lowercase().contains(&query)
                || session
                    .project_name
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(&query))
                || registry
                    .and_then(|registry| {
                        session
                            .project_id
                            .as_deref()
                            .and_then(|id| registry.project(id))
                    })
                    .is_some_and(|project| {
                        project.name.to_lowercase().contains(&query)
                            || project
                                .aliases
                                .iter()
                                .any(|alias| alias.to_lowercase().contains(&query))
                    })
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        session_timestamp_key(&right.updated_at)
            .cmp(&session_timestamp_key(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut seen = std::collections::HashSet::new();
    sessions.retain(|session| seen.insert(session.id.clone()));
    sessions.sort_by(|left, right| match sort {
        SessionHistorySort::Recent => session_timestamp_key(&right.updated_at)
            .cmp(&session_timestamp_key(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id)),
        SessionHistorySort::Oldest => session_timestamp_key(&left.updated_at)
            .cmp(&session_timestamp_key(&right.updated_at))
            .then_with(|| left.id.cmp(&right.id)),
        SessionHistorySort::Title => left
            .title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.id.cmp(&right.id)),
    });
    for source in &mut sources {
        source.session_count = sessions
            .iter()
            .filter(|session| session.source == source.source)
            .count();
    }
    sources.sort_by_key(|source| {
        SessionSource::ALL
            .iter()
            .position(|candidate| *candidate == source.source)
            .unwrap_or(usize::MAX)
    });
    sessions.retain(|session| {
        source_filter
            .as_ref()
            .is_none_or(|source| &session.source == source)
    });
    let total = sessions.len();
    let next_offset = offset.saturating_add(page_size);
    let next_cursor = (next_offset < total).then(|| next_offset.to_string());
    let sessions = sessions.into_iter().skip(offset).take(page_size).collect();
    SessionHistorySnapshot {
        sessions,
        sources,
        total,
        next_cursor,
    }
}

fn session_timestamp_key(value: &Option<String>) -> i64 {
    let Some(value) = value.as_deref() else {
        return 0;
    };
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp_millis())
        .or_else(|_| value.parse::<i64>())
        .unwrap_or(0)
}

fn timestamp(value: Option<&Value>) -> Option<String> {
    if let Some(value) = value.and_then(Value::as_str) {
        let value = value.trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    let value = value?.as_i64()?;
    let date = if value >= 10_000_000_000 {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
    } else {
        chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0)
    }?;
    Some(date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_registry::{
        RegistryProject, RegistryWorkspace, CONFIDENCE_EXACT, KIND_PRODUCT, ROLE_ROOT,
        SOURCE_MANUAL,
    };

    fn project(id: &str, name: &str) -> RegistryProject {
        RegistryProject {
            id: id.to_string(),
            name: name.to_string(),
            kind: KIND_PRODUCT.to_string(),
            ..Default::default()
        }
    }

    fn binding(id: &str, project_id: &str, path: &str) -> RegistryWorkspace {
        RegistryWorkspace {
            id: id.to_string(),
            project_id: project_id.to_string(),
            path: path.to_string(),
            role: ROLE_ROOT.to_string(),
            source: SOURCE_MANUAL.to_string(),
            confidence: CONFIDENCE_EXACT.to_string(),
            ..Default::default()
        }
    }

    fn registry(projects: Vec<RegistryProject>, workspaces: Vec<RegistryWorkspace>) -> Registry {
        Registry {
            projects,
            workspaces,
            ..Default::default()
        }
    }

    #[test]
    fn source_ids_and_qualified_session_ids_are_stable_and_unambiguous() {
        let ids = SessionSource::ALL
            .iter()
            .map(|s| s.id().to_string())
            .collect::<Vec<_>>();
        let unique = ids.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), SessionSource::ALL.len());
        assert_eq!(SessionSource::ClaudeCode.label(), "Claude Code");
        assert_eq!(
            SessionSource::Codex.stable_session_id("session-1").unwrap(),
            "codex:session-1"
        );
        assert!(SessionSource::Codex.stable_session_id(" ").is_err());
        assert!(SessionSource::Codex.stable_session_id("bad\nid").is_err());
        assert_eq!(
            SessionSource::Codex
                .stable_session_id(" session-1 ")
                .unwrap(),
            "codex: session-1 "
        );
        assert_ne!(
            SessionSource::Codex.stable_session_id("session-1").unwrap(),
            SessionSource::Codex
                .stable_session_id(" session-1 ")
                .unwrap()
        );
    }

    #[test]
    fn source_wire_keys_match_filter_ids_and_accept_legacy_opencode() {
        let sources = SessionSource::ALL;
        for source in &sources {
            let wire = serde_json::to_value(source).unwrap();
            assert_eq!(wire, source.id());
            assert_eq!(
                serde_json::from_value::<SessionSource>(wire).unwrap(),
                source.clone()
            );
        }
        assert_eq!(
            serde_json::from_str::<SessionSource>("\"open_code\"").unwrap(),
            SessionSource::OpenCode
        );
        // Fixture consumed by the browser regression uses the actual Rust serialization.
        if let Some(path) = std::env::var_os("WORKSPACE_SOURCE_WIRE_FIXTURE") {
            let availability = [
                SourceAvailability::Unavailable,
                SourceAvailability::Available,
                SourceAvailability::Available,
                SourceAvailability::Partial,
                SourceAvailability::Error,
            ];
            let history = SessionHistorySnapshot {
                sessions: sources
                    .iter()
                    .map(|source| {
                        SessionSummary::new(
                            source.clone(),
                            "synthetic".into(),
                            "F:/synthetic".into(),
                            "Synthetic session".into(),
                            Some("2026-11-11T08:00:00Z".into()),
                            Some(101),
                            SessionCapabilities {
                                can_preview: true,
                                can_open_original: false,
                                can_resume: false,
                            },
                            Some(WorkspaceAssociation {
                                project_id: "p1".into(),
                                project_name: "审查项目".into(),
                                binding_path: "F:/synthetic".into(),
                            }),
                        )
                        .unwrap()
                    })
                    .collect(),
                sources: sources
                    .iter()
                    .enumerate()
                    .map(|(i, source)| SessionSourceStatus {
                        source: source.clone(),
                        availability: availability[i],
                        session_count: 11,
                        error_code: match i {
                            0 => Some("runtime_not_running".into()),
                            3 => Some("scan_partial".into()),
                            4 => Some("database_unreadable".into()),
                            _ => None,
                        },
                    })
                    .collect(),
                total: sources.len(),
                next_cursor: None,
            };
            let fixture = serde_json::json!({ "source_keys": sources, "history": history });
            std::fs::write(path, serde_json::to_vec(&fixture).unwrap()).unwrap();
        }
    }

    #[test]
    fn association_uses_the_most_specific_registered_worktree_path() {
        let registry = registry(
            vec![project("parent", "Parent"), project("child", "Child")],
            vec![
                binding("root", "parent", "F:/work/project"),
                binding("worktree", "child", "F:/work/project/packages/child"),
            ],
        );

        let hit = associate_workspace(&registry, "f:\\work\\project\\packages\\child\\src")
            .unwrap()
            .unwrap();
        assert_eq!(hit.project_id, "child");
        assert_eq!(hit.project_name, "Child");
    }

    #[test]
    fn association_does_not_match_a_path_prefix_without_a_directory_boundary() {
        let registry = registry(
            vec![project("app", "App")],
            vec![binding("root", "app", "F:/work/app")],
        );

        assert!(associate_workspace(&registry, "F:/work/application/src")
            .unwrap()
            .is_none());
    }

    #[test]
    fn association_fails_closed_when_equal_rank_bindings_conflict() {
        let registry = registry(
            vec![project("one", "One"), project("two", "Two")],
            vec![
                binding("one-root", "one", "F:/work/shared"),
                binding("two-root", "two", "F:/work/shared"),
            ],
        );

        let error = associate_workspace(&registry, "F:/work/shared/src").unwrap_err();
        assert!(error.starts_with(workspace_registry::ERR_PATH_CONFLICT));
    }

    #[test]
    fn dsh_list_projection_uses_only_metadata_and_drops_blank_sessions() {
        let registry = registry(
            vec![project("app", "App")],
            vec![binding("root", "app", "F:/work/app")],
        );
        let value = serde_json::json!({
            "items": [
                {
                    "sessionId": "run-1",
                    "cwd": "F:/work/app/src",
                    "title": "Inspect parser",
                    "updatedAt": 1_800_000_000_000_i64,
                    "messageCount": 8
                },
                { "sessionId": "empty", "cwd": "F:/work/app", "blank": true },
                { "cwd": "F:/work/app" }
            ]
        });

        let sessions = dsh_session_summaries(&registry, &value).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "dsh:run-1");
        assert_eq!(sessions[0].project_id.as_deref(), Some("app"));
        assert_eq!(sessions[0].title, "Inspect parser");
        assert_eq!(sessions[0].message_count, Some(8));
        assert!(sessions[0].capabilities.can_preview);
        assert!(!sessions[0].capabilities.can_open_original);
        assert!(!sessions[0].capabilities.can_resume);
    }

    #[test]
    fn dsh_projection_rejects_an_unexpected_response_shape() {
        assert!(dsh_session_summaries(&Registry::default(), &serde_json::json!({})).is_err());
    }

    #[test]
    fn dsh_preview_requires_the_session_to_belong_to_the_selected_project() {
        let registry = registry(
            vec![project("app", "App"), project("other", "Other")],
            vec![
                binding("app-root", "app", "F:/work/app"),
                binding("other-root", "other", "F:/work/other"),
            ],
        );
        let listing = serde_json::json!({
            "items": [{
                "sessionId": "run-1",
                "cwd": "F:/work/app/src",
                "projections": {"asOfSeq": 42}
            }]
        });

        assert_eq!(
            dsh_projected_session_as_of_seq(&registry, &listing, "run-1", "app").unwrap(),
            Some(42)
        );
        assert_eq!(
            dsh_projected_session_as_of_seq(&registry, &listing, "run-1", "other").unwrap(),
            None
        );
        assert_eq!(
            dsh_projected_session_as_of_seq(&registry, &listing, "missing", "app").unwrap(),
            None
        );
    }

    #[test]
    fn dsh_preview_returns_chronological_text_and_an_older_page_cursor() {
        let response = serde_json::json!({
            "items": [
                {"seq": 9, "type": "assistant/message", "surfaceOp": "append", "time": 1_800_000_009_i64,
                    "data": {"message": {"content": [{"type": "text", "text": "answer"}]}}},
                {"seq": 5, "type": "user/message", "surfaceOp": "append", "time": 1_800_000_005_i64,
                    "data": {"source": {"kind": "user"}, "content": "question"}},
                {"seq": 4, "type": "tool/result", "surfaceOp": "append", "data": {"content": "hidden tool result"}},
                {"seq": 2, "type": "user/message", "surfaceOp": "replace", "time": 1_800_000_002_i64,
                    "data": {"source": {"kind": "user"}, "content": "hidden edit"}},
                {"seq": 1, "type": "user/message", "surfaceOp": "append", "time": 1_800_000_001_i64,
                    "data": {"source": {"kind": "user"}, "content": "older"}}
            ]
        });

        let page = dsh_transcript_page("dsh:run-1", 9, &response, 2).unwrap();
        assert_eq!(page.session_id, "dsh:run-1");
        assert_eq!(page.through_seq, Some(9));
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[0].id, "5");
        assert_eq!(page.messages[0].role, "user");
        assert_eq!(page.messages[0].content, "question");
        assert_eq!(page.messages[1].content, "answer");
        assert_eq!(page.next_cursor.as_deref(), Some("4"));
    }

    #[test]
    fn dsh_preview_accepts_empty_pages_and_rejects_unknown_response_shapes() {
        assert!(
            dsh_transcript_page("dsh:run-1", 0, &serde_json::json!({"items": []}), 50)
                .unwrap()
                .messages
                .is_empty()
        );
        assert!(dsh_transcript_page("dsh:run-1", 0, &serde_json::json!({}), 50).is_err());
    }

    fn summary(
        source: SessionSource,
        native_id: &str,
        project_id: Option<&str>,
        title: &str,
        updated_at: &str,
    ) -> SessionSummary {
        SessionSummary {
            id: source.stable_session_id(native_id).unwrap(),
            source,
            native_id: native_id.to_string(),
            project_id: project_id.map(str::to_string),
            project_name: project_id.map(|_| "Project A".to_string()),
            working_directory: "F:/work/project-a".to_string(),
            title: title.to_string(),
            updated_at: Some(updated_at.to_string()),
            message_count: None,
            capabilities: SessionCapabilities::default(),
        }
    }

    #[test]
    fn scoped_history_keeps_actual_owner_alias_search_and_global_pagination() {
        use crate::workspace_registry::RegistryProject;
        let registry = Registry {
            projects: vec![
                RegistryProject {
                    id: "root".into(),
                    name: "平台".into(),
                    ..Default::default()
                },
                RegistryProject {
                    id: "child".into(),
                    name: "影院播控".into(),
                    aliases: vec!["CinemaControl".into()],
                    parent_id: "root".into(),
                    ..Default::default()
                },
                RegistryProject {
                    id: "nested".into(),
                    name: "嵌套".into(),
                    parent_id: "child".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sessions = vec![
            summary(
                SessionSource::Codex,
                "same",
                Some("root"),
                "A",
                "2026-01-01T00:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "same",
                Some("child"),
                "B",
                "2026-01-03T00:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "same",
                Some("child"),
                "B",
                "2026-01-03T00:00:00Z",
            ),
            summary(
                SessionSource::Acp,
                "third",
                Some("nested"),
                "C",
                "2026-01-02T00:00:00Z",
            ),
            summary(
                SessionSource::Dsh,
                "outside",
                Some("other"),
                "D",
                "2026-01-04T00:00:00Z",
            ),
        ];
        let page = scoped_project_history_snapshot(
            &registry,
            "root",
            true,
            "",
            None,
            SessionHistorySort::Recent,
            0,
            2,
            sessions.clone(),
            vec![],
        );
        assert_eq!(page.total, 3);
        assert_eq!(
            page.sessions
                .iter()
                .map(|s| s.project_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["child", "nested"]
        );
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        let own = scoped_project_history_snapshot(
            &registry,
            "root",
            false,
            "",
            None,
            SessionHistorySort::Recent,
            0,
            50,
            sessions.clone(),
            vec![],
        );
        assert_eq!(own.total, 1);
        let alias = scoped_project_history_snapshot(
            &registry,
            "root",
            true,
            "cinemacontrol",
            None,
            SessionHistorySort::Recent,
            0,
            50,
            sessions,
            vec![],
        );
        assert_eq!(alias.total, 1);
        assert_eq!(alias.sessions[0].project_id.as_deref(), Some("child"));
    }

    #[test]
    fn project_snapshot_filters_deduplicates_sorts_and_pages_across_sources() {
        let sessions = vec![
            summary(
                SessionSource::Dsh,
                "run-1",
                Some("project-a"),
                "Older task",
                "2026-11-11T09:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "claude-1",
                Some("project-a"),
                "Latest task",
                "2026-11-11T10:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "claude-1",
                Some("project-a"),
                "Latest task renamed",
                "2026-11-11T11:00:00Z",
            ),
            summary(
                SessionSource::Codex,
                "other-project",
                Some("project-b"),
                "Outside project",
                "2026-11-11T12:00:00Z",
            ),
        ];
        let sources = vec![
            SessionSourceStatus {
                source: SessionSource::ClaudeCode,
                availability: SourceAvailability::Partial,
                session_count: 999,
                error_code: Some("scan_partial".to_string()),
            },
            SessionSourceStatus {
                source: SessionSource::Dsh,
                availability: SourceAvailability::Available,
                session_count: 999,
                error_code: None,
            },
        ];

        let page = project_history_snapshot(
            "project-a",
            "task",
            None,
            SessionHistorySort::Recent,
            0,
            1,
            sessions,
            sources,
        );
        assert_eq!(page.total, 2);
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].title, "Latest task renamed");
        assert_eq!(page.sessions[0].source, SessionSource::ClaudeCode);
        assert_eq!(page.next_cursor.as_deref(), Some("1"));
        assert_eq!(page.sources[0].source, SessionSource::Dsh);
        assert_eq!(page.sources[0].session_count, 1);
        assert_eq!(page.sources[1].source, SessionSource::ClaudeCode);
        assert_eq!(page.sources[1].session_count, 1);
        assert_eq!(page.sources[1].availability, SourceAvailability::Partial);
    }

    #[test]
    fn project_snapshot_filters_by_source_and_keeps_source_counts() {
        let sessions = vec![
            summary(
                SessionSource::Dsh,
                "dsh-1",
                Some("project-a"),
                "Task",
                "2026-11-11T09:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "claude-1",
                Some("project-a"),
                "Task",
                "2026-11-11T10:00:00Z",
            ),
        ];
        let sources = vec![
            SessionSourceStatus {
                source: SessionSource::Dsh,
                availability: SourceAvailability::Available,
                session_count: 0,
                error_code: None,
            },
            SessionSourceStatus {
                source: SessionSource::ClaudeCode,
                availability: SourceAvailability::Available,
                session_count: 0,
                error_code: None,
            },
        ];

        let page = project_history_snapshot(
            "project-a",
            "",
            Some(SessionSource::Dsh),
            SessionHistorySort::Recent,
            0,
            50,
            sessions,
            sources,
        );
        assert_eq!(page.total, 1);
        assert_eq!(page.sessions[0].source, SessionSource::Dsh);
        assert_eq!(page.sources[0].session_count, 1);
        assert_eq!(page.sources[1].session_count, 1);
    }

    #[test]
    fn project_snapshot_sorts_before_pagination_with_stable_ties() {
        let sessions = vec![
            summary(
                SessionSource::Dsh,
                "later",
                Some("project-a"),
                "Zulu",
                "2026-11-11T11:00:00Z",
            ),
            summary(
                SessionSource::Codex,
                "earlier",
                Some("project-a"),
                "Alpha",
                "2026-11-11T09:00:00Z",
            ),
            summary(
                SessionSource::ClaudeCode,
                "middle",
                Some("project-a"),
                "Bravo",
                "2026-11-11T10:00:00Z",
            ),
        ];
        let snapshot = |sort| {
            project_history_snapshot(
                "project-a",
                "",
                None,
                sort,
                0,
                50,
                sessions.clone(),
                Vec::new(),
            )
        };

        assert_eq!(
            snapshot(SessionHistorySort::Recent)
                .sessions
                .iter()
                .map(|session| session.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Zulu", "Bravo", "Alpha"]
        );
        assert_eq!(
            snapshot(SessionHistorySort::Oldest)
                .sessions
                .iter()
                .map(|session| session.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Bravo", "Zulu"]
        );
        assert_eq!(
            snapshot(SessionHistorySort::Title)
                .sessions
                .iter()
                .map(|session| session.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Bravo", "Zulu"]
        );
    }

    #[test]
    fn paired_transcript_pages_are_chronological_and_cursor_bounded() {
        let turns = (0..3)
            .map(|index| (format!("user-{index}"), format!("assistant-{index}")))
            .collect::<Vec<_>>();

        let newest = paired_transcript_page("acp:run-1", &turns, None, 4).unwrap();
        assert_eq!(newest.messages.len(), 4);
        assert_eq!(newest.messages[0].content, "user-1");
        assert_eq!(newest.messages[3].content, "assistant-2");
        assert_eq!(newest.next_cursor.as_deref(), Some("2"));

        let older = paired_transcript_page("acp:run-1", &turns, Some(2), 4).unwrap();
        assert_eq!(older.messages.len(), 2);
        assert_eq!(older.messages[0].content, "user-0");
        assert_eq!(older.messages[1].content, "assistant-0");
        assert_eq!(older.next_cursor, None);
        assert!(paired_transcript_page("acp:run-1", &turns, Some(7), 4).is_err());
    }
}
