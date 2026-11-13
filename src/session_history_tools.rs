//! Historical tool requests, never instructions to execute or edits to the live workspace.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct HistoryFile {
    pub path: String,
    pub operation: String,
    pub kind: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub patch: Option<String>,
    pub truncated: bool,
}

fn bounded(text: Option<&str>) -> (Option<String>, bool) {
    let Some(text) = text else {
        return (None, false);
    };
    let limited: String = text.chars().take(16_384).collect();
    let truncated = limited.len() != text.len();
    (Some(limited), truncated)
}

pub(crate) fn claude_files(message: &Value) -> Vec<HistoryFile> {
    let mut files: Vec<HistoryFile> = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_use"))
        .flat_map(|part| {
            let name = part.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(input) = part.get("input") else {
                return vec![];
            };
            if !matches!(name, "Edit" | "MultiEdit" | "Write" | "Read") {
                return vec![];
            }
            let Some(path) = input
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty() && p.len() <= 4096)
            else {
                return vec![];
            };
            let edits = if name == "MultiEdit" {
                input
                    .get("edits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                vec![input.clone()]
            };
            let omitted_edits = edits.len() > 32;
            edits
                .into_iter()
                .take(32)
                .map(|edit| {
                    let (before, a) = bounded(edit.get("old_string").and_then(Value::as_str));
                    let (after, b) = bounded(
                        edit.get(if name == "Write" {
                            "content"
                        } else {
                            "new_string"
                        })
                        .and_then(Value::as_str),
                    );
                    HistoryFile {
                        path: path.into(),
                        operation: name.into(),
                        kind: if name == "Write" {
                            "recorded_content"
                        } else if name == "Read" {
                            "reference"
                        } else {
                            "fragment"
                        }
                        .into(),
                        before,
                        after,
                        patch: None,
                        truncated: a || b || omitted_edits,
                    }
                })
                .collect::<Vec<_>>()
        })
        .take(65)
        .collect();
    if files.len() > 64 {
        files.truncate(64);
        if let Some(last) = files.last_mut() {
            last.truncated = true;
        }
    }
    files
}

pub(crate) fn codex_files(payload: &Value) -> Vec<HistoryFile> {
    if payload.get("name").and_then(Value::as_str) != Some("apply_patch") {
        return vec![];
    }
    let raw = payload
        .get("input")
        .and_then(Value::as_str)
        .or_else(|| payload.get("arguments").and_then(Value::as_str));
    let Some(raw) = raw.filter(|s| s.len() <= 2 * 1024 * 1024) else {
        return vec![];
    };
    let decoded = serde_json::from_str::<Value>(raw).ok();
    let patch = decoded
        .as_ref()
        .and_then(|v| v.get("patch").or_else(|| v.get("input")))
        .and_then(Value::as_str)
        .unwrap_or(raw);
    let lines: Vec<&str> = patch.lines().collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .any(|p| l.starts_with(p))
                .then_some(i)
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .take(64)
        .filter_map(|(n, &start)| {
            let (op, path) = lines[start].strip_prefix("*** ")?.split_once(" File: ")?;
            if path.is_empty() || path.len() > 4096 {
                return None;
            }
            let section =
                lines[start..starts.get(n + 1).copied().unwrap_or(lines.len())].join("\n");
            let (patch, truncated) = bounded(Some(&section));
            Some(HistoryFile {
                path: path.into(),
                operation: op.into(),
                kind: "patch".into(),
                before: None,
                after: None,
                patch,
                truncated: truncated || starts.len() > 64,
            })
        })
        .collect()
}

pub(crate) fn is_prompt(message: &crate::session_history::SessionTranscriptMessage) -> bool {
    if message.role != "user" {
        return false;
    }
    let text = message.content.trim_start();
    ![
        "<local-command",
        "<command-name>",
        "<system-reminder>",
        "<task-notification>",
        "<environment_context>",
        "# AGENTS.md instructions",
        "<user_instructions>",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn session_history_prompt_navigation_skips_context_and_notifications() {
        for text in [
            "<environment_context>synthetic",
            "<task-notification>done",
            "# AGENTS.md instructions",
            "<command-name>/help",
        ] {
            let message = crate::session_history::SessionTranscriptMessage {
                id: "s".into(),
                role: "user".into(),
                content: text.into(),
                timestamp: None,
                files: vec![],
            };
            assert!(!is_prompt(&message));
        }
        let mut message = crate::session_history::SessionTranscriptMessage {
            id: "s".into(),
            role: "context".into(),
            content: "context without marker".into(),
            timestamp: None,
            files: vec![],
        };
        assert!(!is_prompt(&message));
        message.role = "user".into();
        message.content = "请分析这个项目".into();
        assert!(is_prompt(&message));
    }
    #[test]
    fn session_history_tool_fragments_are_literal_and_bounded() {
        let files = claude_files(
            &json!({"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"a.ts","old_string":"<script>","new_string":"new"}}]}),
        );
        assert_eq!(files[0].kind, "fragment");
        assert_eq!(files[0].before.as_deref(), Some("<script>"));
        let big = "中".repeat(20_000);
        let files = claude_files(
            &json!({"content":[{"type":"tool_use","name":"Write","input":{"file_path":"a","content":big}}]}),
        );
        assert!(files[0].truncated);
        assert_eq!(files[0].after.as_ref().unwrap().chars().count(), 16384);
    }
    #[test]
    fn session_history_codex_patch_preserves_file_sections() {
        let files = codex_files(
            &json!({"name":"apply_patch","input":"*** Begin Patch\n*** Update File: a.ts\n@@\n-old\n+new\n*** Add File: b.ts\n+hello\n*** End Patch"}),
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.ts");
        assert!(!files[0].patch.as_ref().unwrap().contains("b.ts"));
        assert!(codex_files(&json!({"name":"exec_command","input":"rm -rf"})).is_empty());
        let patch = (0..65)
            .map(|i| format!("*** Add File: file-{i}\n+text-{i}\n"))
            .collect::<String>();
        let files = codex_files(&json!({"name":"apply_patch","input":patch}));
        assert_eq!(files.len(), 64);
        assert!(files[63].truncated);
        assert!(!files[63].patch.as_ref().unwrap().contains("file-64"));
    }
}
