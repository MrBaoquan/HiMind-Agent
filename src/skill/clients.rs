//! Agent Skills client registry shared by distribution and MCP presentation.

use crate::skill::types::SkillManifest;

pub(crate) const PORTABLE_PROFILE_ID: &str = "agent-skills";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SkillClientDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub env_key: &'static str,
    pub project_dir: &'static str,
    pub user_dir: &'static str,
    pub support_level: &'static str,
    pub support_note: &'static str,
    pub mcp_target_ids: &'static [&'static str],
}

pub(crate) const DIRECTORY_CLIENTS: &[SkillClientDefinition] = &[
    SkillClientDefinition {
        id: "github-copilot",
        name: "GitHub Copilot",
        env_key: "HIMIND_COPILOT_SKILL_DIR",
        project_dir: ".github/skills",
        user_dir: ".copilot/skills",
        support_level: "official",
        support_note: "VS Code、Copilot CLI 与 Copilot coding agent 官方支持 Agent Skills",
        mcp_target_ids: &["github-copilot", "vscode", "vscode-insiders"],
    },
    SkillClientDefinition {
        id: "workbuddy",
        name: "WorkBuddy",
        env_key: "HIMIND_WORKBUDDY_SKILL_DIR",
        project_dir: ".workbuddy/skills",
        user_dir: ".workbuddy/skills",
        support_level: "verified",
        support_note: "已按 WorkBuddy 本机原生 Skill 目录验证",
        mcp_target_ids: &["workbuddy"],
    },
    SkillClientDefinition {
        id: "claude",
        name: "Claude",
        env_key: "HIMIND_CLAUDE_SKILL_DIR",
        project_dir: ".claude/skills",
        user_dir: ".claude/skills",
        support_level: "official",
        support_note: "Claude Code 原生 Agent Skills",
        mcp_target_ids: &["claude-code", "claude-desktop"],
    },
    SkillClientDefinition {
        id: "cursor",
        name: "Cursor",
        env_key: "HIMIND_CURSOR_SKILL_DIR",
        project_dir: ".cursor/skills",
        user_dir: ".cursor/skills",
        support_level: "official",
        support_note: "Cursor 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["cursor"],
    },
    SkillClientDefinition {
        id: "windsurf",
        name: "Windsurf",
        env_key: "HIMIND_WINDSURF_SKILL_DIR",
        project_dir: ".windsurf/skills",
        user_dir: ".codeium/windsurf/skills",
        support_level: "official",
        support_note: "Windsurf 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["windsurf"],
    },
    SkillClientDefinition {
        id: "cline",
        name: "Cline",
        env_key: "HIMIND_CLINE_SKILL_DIR",
        project_dir: ".cline/skills",
        user_dir: ".cline/skills",
        support_level: "official",
        support_note: "Cline 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["cline"],
    },
    SkillClientDefinition {
        id: "trae",
        name: "Trae",
        env_key: "HIMIND_TRAE_SKILL_DIR",
        project_dir: ".trae/skills",
        user_dir: ".trae/skills",
        support_level: "compatible",
        support_note: "按客户端 Agent Skills 兼容目录分发",
        mcp_target_ids: &["trae"],
    },
    SkillClientDefinition {
        id: "codebuddy",
        name: "CodeBuddy",
        env_key: "HIMIND_CODEBUDDY_SKILL_DIR",
        project_dir: ".codebuddy/skills",
        user_dir: ".codebuddy/skills",
        support_level: "compatible",
        support_note: "按客户端 Agent Skills 兼容目录分发",
        mcp_target_ids: &["codebuddy-cli"],
    },
    SkillClientDefinition {
        id: "qoder",
        name: "Qoder",
        env_key: "HIMIND_QODER_SKILL_DIR",
        project_dir: ".qoder/skills",
        user_dir: ".qoder/skills",
        support_level: "official",
        support_note: "Qoder 官方支持用户级与项目级 Agent Skills",
        mcp_target_ids: &["qoder"],
    },
    SkillClientDefinition {
        id: "zcode",
        name: "ZCode",
        env_key: "HIMIND_ZCODE_SKILL_DIR",
        project_dir: ".zcode/skills",
        user_dir: ".zcode/skills",
        support_level: "official",
        support_note: "ZCode 原生支持 .zcode/skills，并兼容 .agents/skills",
        mcp_target_ids: &["zcode"],
    },
    SkillClientDefinition {
        id: "antigravity",
        name: "Antigravity",
        env_key: "HIMIND_ANTIGRAVITY_SKILL_DIR",
        project_dir: ".agent/skills",
        user_dir: ".agent/skills",
        support_level: "compatible",
        support_note: "按通用 Agent Skills 目录分发",
        mcp_target_ids: &["antigravity", "antigravity-ide"],
    },
    SkillClientDefinition {
        id: "gemini-cli",
        name: "Gemini CLI",
        env_key: "HIMIND_GEMINI_SKILL_DIR",
        project_dir: ".gemini/skills",
        user_dir: ".gemini/skills",
        support_level: "official",
        support_note: "Gemini CLI 官方支持 Agent Skills，并兼容 .agents/skills",
        mcp_target_ids: &["gemini-cli"],
    },
    SkillClientDefinition {
        id: "opencode",
        name: "OpenCode",
        env_key: "HIMIND_OPENCODE_SKILL_DIR",
        project_dir: ".opencode/skills",
        user_dir: ".config/opencode/skills",
        support_level: "official",
        support_note: "OpenCode 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["opencode"],
    },
    SkillClientDefinition {
        id: "kimi-code",
        name: "Kimi Code",
        env_key: "HIMIND_KIMI_SKILL_DIR",
        project_dir: ".kimi/skills",
        user_dir: ".kimi/skills",
        support_level: "official",
        support_note: "Kimi Code 官方支持 Agent Skills，并兼容 .agents/skills",
        mcp_target_ids: &["kimi-code"],
    },
    SkillClientDefinition {
        id: "kiro",
        name: "Kiro",
        env_key: "HIMIND_KIRO_SKILL_DIR",
        project_dir: ".kiro/skills",
        user_dir: ".kiro/skills",
        support_level: "official",
        support_note: "Kiro 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["kiro"],
    },
    SkillClientDefinition {
        id: "qwen-code",
        name: "Qwen Code",
        env_key: "HIMIND_QWEN_SKILL_DIR",
        project_dir: ".qwen/skills",
        user_dir: ".qwen/skills",
        support_level: "official",
        support_note: "Qwen Code 官方支持项目级与用户级 Agent Skills",
        mcp_target_ids: &["qwen-code"],
    },
];

pub(crate) fn directory_client(client_id: &str) -> Option<&'static SkillClientDefinition> {
    DIRECTORY_CLIENTS
        .iter()
        .find(|item| item.id.eq_ignore_ascii_case(client_id))
}

pub(crate) fn client_for_mcp_target(target_id: &str) -> Option<(&'static str, &'static str)> {
    if target_id == "himind-ai" {
        return Some(("himind-ai", "HiMind AI"));
    }
    if target_id == "codex" {
        return Some(("codex", "Codex"));
    }
    DIRECTORY_CLIENTS
        .iter()
        .find(|item| item.mcp_target_ids.contains(&target_id))
        .map(|item| (item.id, item.name))
}

pub(crate) fn is_portable_client(client_id: &str) -> bool {
    matches!(client_id, "himind-ai" | "codex") || directory_client(client_id).is_some()
}

pub(crate) fn declares_portable_skill(manifest: &SkillManifest) -> bool {
    manifest.supported_clients.iter().any(|client| {
        let client = client.trim().to_ascii_lowercase();
        matches!(
            client.as_str(),
            PORTABLE_PROFILE_ID | "codex" | "github-copilot" | "workbuddy"
        )
    })
}

pub(crate) fn manifest_supports_client(manifest: &SkillManifest, client_id: &str) -> bool {
    manifest
        .supported_clients
        .iter()
        .any(|item| item.eq_ignore_ascii_case(client_id))
        || (is_portable_client(client_id) && declares_portable_skill(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::types::{SkillManifest, SkillScope};
    use std::collections::BTreeSet;

    fn manifest(clients: &[&str]) -> SkillManifest {
        SkillManifest {
            id: "com.himind.skill.portable".to_string(),
            name: "Portable".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::User,
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: String::new(),
            supported_clients: clients.iter().map(|item| item.to_string()).collect(),
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        }
    }

    #[test]
    fn legacy_external_agent_skill_declaration_is_portable() {
        let skill = manifest(&["codex"]);
        assert!(manifest_supports_client(&skill, "qoder"));
        assert!(manifest_supports_client(&skill, "zcode"));
        assert!(manifest_supports_client(&skill, "github-copilot"));
    }

    #[test]
    fn himind_only_skill_stays_internal() {
        let skill = manifest(&["himind-ai"]);
        assert!(manifest_supports_client(&skill, "himind-ai"));
        assert!(!manifest_supports_client(&skill, "qoder"));
    }

    #[test]
    fn explicit_modern_client_declaration_stays_client_specific() {
        let skill = manifest(&["qoder"]);
        assert!(manifest_supports_client(&skill, "qoder"));
        assert!(!manifest_supports_client(&skill, "zcode"));
        assert!(!manifest_supports_client(&skill, "himind-ai"));
    }

    #[test]
    fn registry_ids_and_mcp_targets_are_unique() {
        let mut client_ids = BTreeSet::new();
        let mut target_ids = BTreeSet::new();
        for client in DIRECTORY_CLIENTS {
            assert!(
                client_ids.insert(client.id),
                "duplicate client id: {}",
                client.id
            );
            for target_id in client.mcp_target_ids {
                assert!(
                    target_ids.insert(*target_id),
                    "duplicate MCP target id: {target_id}"
                );
            }
        }
    }

    #[test]
    fn official_client_directories_match_native_conventions() {
        assert_eq!(
            directory_client("github-copilot").unwrap().user_dir,
            ".copilot/skills"
        );
        assert_eq!(
            directory_client("workbuddy").unwrap().user_dir,
            ".workbuddy/skills"
        );
        assert_eq!(directory_client("qoder").unwrap().user_dir, ".qoder/skills");
        assert_eq!(directory_client("zcode").unwrap().user_dir, ".zcode/skills");
        assert_eq!(
            directory_client("windsurf").unwrap().user_dir,
            ".codeium/windsurf/skills"
        );
        assert_eq!(
            directory_client("cline").unwrap().project_dir,
            ".cline/skills"
        );
        assert_eq!(client_for_mcp_target("rider"), None);
    }
}
