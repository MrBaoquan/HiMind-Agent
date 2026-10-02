//! AI client × scope × capability matrix.
//!
//! 每个客户端的能力（能否接 Agent Skills、能接哪些作用域、能否自动注册 MCP、
//! 插件由谁执行）只在这里声明一次，其它模块按能力查询，不再各自维护一份客户端表。
//!
//! 静态表刻意不包含"本机装没装"这类信息：那份数据属于运行期可用性，由
//! [`crate::app::client_matrix`] 以 availability 覆盖层的形式叠加在同一份矩阵上。
//! 这样同一份矩阵在任何机器上都描述同样的能力，只有可用性会变。

use crate::skill::types::SkillManifest;

pub(crate) const PORTABLE_PROFILE_ID: &str = "agent-skills";

/// Agent 原生技能库：Skill 存在这里就是"HiMind AI 可用"。
pub(crate) const HOST_CLIENT_HIMIND_AI: &str = "himind-ai";
/// Codex 既有原生适配器（`skill/codex.rs`），又参与 MCP 注册。
pub(crate) const HOST_CLIENT_CODEX: &str = "codex";

pub(crate) const SKILL_STANDARD_AGENT_SKILLS: &str = "agentskills.io";
pub(crate) const SKILL_STANDARD_HIMIND_STORE: &str = "himind-store";

pub(crate) const MATRIX_SCHEMA_VERSION: &str = "client_capability_matrix.v1";

/// 某个能力在某个作用域下的落点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientScopeTarget {
    /// 相对于作用域根目录的路径；作用域根由运行期决定（用户主目录 / 工作区根）。
    pub directory: &'static str,
    /// 用户显式指定落点的环境变量，空串表示该能力不支持指定目录。
    pub env_key: &'static str,
}

/// Agent Skills 能力：标准 + 支持的作用域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientSkillsCapability {
    pub standard: &'static str,
    pub user: Option<ClientScopeTarget>,
    pub project: Option<ClientScopeTarget>,
}

/// MCP 能力：能自动改写哪些客户端配置，以及这些配置对应哪些 MCP 目标 ID。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientMcpCapability {
    pub target_ids: &'static [&'static str],
    pub config_format: &'static str,
    pub auto_configure: bool,
}

/// 插件能力：HiMind 插件由谁执行。
///
/// 插件是 Agent 侧资产（capability/plugin.rs 在 Agent 进程里执行并按需注入 UI），
/// 因此外部客户端拿到的不是插件本身，而是 Agent 通过 MCP 暴露出来的工具。
/// 这里如实标注执行者，避免把"能连 MCP"误读成"客户端能装插件"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientPluginsCapability {
    pub executor: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub support_level: &'static str,
    pub support_note: &'static str,
    pub skills: Option<ClientSkillsCapability>,
    pub mcp: Option<ClientMcpCapability>,
    pub plugins: Option<ClientPluginsCapability>,
}

/// 客户端别名：历史配置、环境变量或用户习惯里出现的旧 ID。
///
/// 单独成函数是因为 `DIRECTORY_CLIENTS` 是 `const`，而 `const fn` 里还不能
/// 比较字符串。
pub(crate) fn client_aliases(client_id: &str) -> &'static [&'static str] {
    if client_id.eq_ignore_ascii_case("github-copilot") {
        return &["copilot", "vscode", "vscode-insiders"];
    }
    &[]
}

impl ClientDefinition {
    /// 用户级 Skill 目录（相对于用户主目录）。没有该能力时返回 `None`。
    pub(crate) fn skill_user_dir(&self) -> Option<&'static str> {
        self.skills
            .and_then(|skills| skills.user)
            .map(|target| target.directory)
    }

    /// 项目级 Skill 目录（相对于工作区根）。没有该能力时返回 `None`。
    pub(crate) fn skill_project_dir(&self) -> Option<&'static str> {
        self.skills
            .and_then(|skills| skills.project)
            .map(|target| target.directory)
    }

    /// 覆盖用户级落点的环境变量。空串表示不可指定。
    pub(crate) fn skill_env_key(&self) -> Option<&'static str> {
        self.skills
            .and_then(|skills| skills.user)
            .map(|target| target.env_key)
            .filter(|key| !key.is_empty())
    }

    pub(crate) fn skill_standard(&self) -> &'static str {
        self.skills
            .map(|skills| skills.standard)
            .unwrap_or(SKILL_STANDARD_AGENT_SKILLS)
    }

    /// 该客户端在 MCP 侧的配置目标（可多个，例如 VS Code 与 VS Code Insiders）。
    pub(crate) fn mcp_target_ids(&self) -> &'static [&'static str] {
        self.mcp.map(|mcp| mcp.target_ids).unwrap_or(&[])
    }

    pub(crate) fn capabilities_json(&self) -> serde_json::Value {
        let mut capabilities = serde_json::Map::new();
        if let Some(skills) = self.skills {
            let mut scopes = serde_json::Map::new();
            if let Some(user) = skills.user {
                scopes.insert("user".to_string(), scope_target_json(&user));
            }
            if let Some(project) = skills.project {
                scopes.insert("project".to_string(), scope_target_json(&project));
            }
            capabilities.insert(
                "skills".to_string(),
                serde_json::json!({ "standard": skills.standard, "scopes": scopes }),
            );
        }
        if let Some(mcp) = self.mcp {
            capabilities.insert(
                "mcp".to_string(),
                serde_json::json!({
                    "transports": ["stdio"],
                    "target_ids": mcp.target_ids,
                    "config_format": mcp.config_format,
                    "auto_configure": mcp.auto_configure,
                }),
            );
        }
        if let Some(plugins) = self.plugins {
            capabilities.insert(
                "plugins".to_string(),
                serde_json::json!({ "executor": plugins.executor }),
            );
        }
        serde_json::Value::Object(capabilities)
    }

    pub(crate) fn static_json(&self) -> serde_json::Value {
        let mut client = serde_json::Map::new();
        client.insert("id".to_string(), serde_json::json!(self.id));
        client.insert("name".to_string(), serde_json::json!(self.name));
        let aliases = client_aliases(self.id);
        if !aliases.is_empty() {
            client.insert("aliases".to_string(), serde_json::json!(aliases));
        }
        client.insert(
            "support_level".to_string(),
            serde_json::json!(self.support_level),
        );
        if !self.support_note.is_empty() {
            client.insert(
                "support_note".to_string(),
                serde_json::json!(self.support_note),
            );
        }
        client.insert("capabilities".to_string(), self.capabilities_json());
        serde_json::Value::Object(client)
    }
}

fn scope_target_json(target: &ClientScopeTarget) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert("directory".to_string(), serde_json::json!(target.directory));
    if !target.env_key.is_empty() {
        value.insert("env_key".to_string(), serde_json::json!(target.env_key));
    }
    serde_json::Value::Object(value)
}

/// 目录型 Agent Skills 客户端：文件落点按"用户级 + 项目级"两个作用域声明。
pub(crate) const fn skill_directory_client(
    id: &'static str,
    name: &'static str,
    env_key: &'static str,
    project_dir: &'static str,
    user_dir: &'static str,
    support_level: &'static str,
    support_note: &'static str,
    mcp_targets: &'static [&'static str],
) -> ClientDefinition {
    ClientDefinition {
        id,
        name,
        support_level,
        support_note,
        skills: Some(ClientSkillsCapability {
            standard: SKILL_STANDARD_AGENT_SKILLS,
            user: Some(ClientScopeTarget {
                directory: user_dir,
                env_key,
            }),
            project: Some(ClientScopeTarget {
                directory: project_dir,
                env_key: "",
            }),
        }),
        mcp: if mcp_targets.is_empty() {
            None
        } else {
            Some(ClientMcpCapability {
                target_ids: mcp_targets,
                config_format: "JSON",
                auto_configure: false,
            })
        },
        plugins: None,
    }
}

pub(crate) const DIRECTORY_CLIENTS: &[ClientDefinition] = &[
    skill_directory_client(
        "github-copilot",
        "GitHub Copilot",
        "HIMIND_COPILOT_SKILL_DIR",
        ".github/skills",
        ".copilot/skills",
        "official",
        "VS Code、Copilot CLI 与 Copilot coding agent 官方支持 Agent Skills",
        &["github-copilot", "vscode", "vscode-insiders"],
    ),
    skill_directory_client(
        "workbuddy",
        "WorkBuddy",
        "HIMIND_WORKBUDDY_SKILL_DIR",
        ".workbuddy/skills",
        ".workbuddy/skills",
        "verified",
        "已按 WorkBuddy 本机原生 Skill 目录验证",
        &["workbuddy"],
    ),
    skill_directory_client(
        "claude",
        "Claude",
        "HIMIND_CLAUDE_SKILL_DIR",
        ".claude/skills",
        ".claude/skills",
        "official",
        "Claude Code 原生 Agent Skills",
        &["claude-code", "claude-desktop"],
    ),
    skill_directory_client(
        "cursor",
        "Cursor",
        "HIMIND_CURSOR_SKILL_DIR",
        ".cursor/skills",
        ".cursor/skills",
        "official",
        "Cursor 官方支持项目级与用户级 Agent Skills",
        &["cursor"],
    ),
    skill_directory_client(
        "windsurf",
        "Windsurf",
        "HIMIND_WINDSURF_SKILL_DIR",
        ".windsurf/skills",
        ".codeium/windsurf/skills",
        "official",
        "Windsurf 官方支持项目级与用户级 Agent Skills",
        &["windsurf"],
    ),
    skill_directory_client(
        "cline",
        "Cline",
        "HIMIND_CLINE_SKILL_DIR",
        ".cline/skills",
        ".cline/skills",
        "official",
        "Cline 官方支持项目级与用户级 Agent Skills",
        &["cline"],
    ),
    skill_directory_client(
        "trae",
        "Trae",
        "HIMIND_TRAE_SKILL_DIR",
        ".trae/skills",
        ".trae/skills",
        "compatible",
        "按客户端 Agent Skills 兼容目录分发",
        &["trae"],
    ),
    skill_directory_client(
        "codebuddy",
        "CodeBuddy",
        "HIMIND_CODEBUDDY_SKILL_DIR",
        ".codebuddy/skills",
        ".codebuddy/skills",
        "compatible",
        "按客户端 Agent Skills 兼容目录分发",
        &["codebuddy-cli"],
    ),
    skill_directory_client(
        "qoder",
        "Qoder",
        "HIMIND_QODER_SKILL_DIR",
        ".qoder/skills",
        ".qoder/skills",
        "official",
        "Qoder 官方支持用户级与项目级 Agent Skills",
        &["qoder"],
    ),
    skill_directory_client(
        "zcode",
        "ZCode",
        "HIMIND_ZCODE_SKILL_DIR",
        ".zcode/skills",
        ".zcode/skills",
        "official",
        "ZCode 原生支持 .zcode/skills，并兼容 .agents/skills",
        &["zcode"],
    ),
    skill_directory_client(
        "antigravity",
        "Antigravity",
        "HIMIND_ANTIGRAVITY_SKILL_DIR",
        ".agent/skills",
        ".agent/skills",
        "compatible",
        "按通用 Agent Skills 目录分发",
        &["antigravity", "antigravity-ide"],
    ),
    skill_directory_client(
        "gemini-cli",
        "Gemini CLI",
        "HIMIND_GEMINI_SKILL_DIR",
        ".gemini/skills",
        ".gemini/skills",
        "official",
        "Gemini CLI 官方支持 Agent Skills，并兼容 .agents/skills",
        &["gemini-cli"],
    ),
    skill_directory_client(
        "opencode",
        "OpenCode",
        "HIMIND_OPENCODE_SKILL_DIR",
        ".opencode/skills",
        ".config/opencode/skills",
        "official",
        "OpenCode 官方支持项目级与用户级 Agent Skills",
        &["opencode"],
    ),
    skill_directory_client(
        "kimi-code",
        "Kimi Code",
        "HIMIND_KIMI_SKILL_DIR",
        ".kimi/skills",
        ".kimi/skills",
        "official",
        "Kimi Code 官方支持 Agent Skills，并兼容 .agents/skills",
        &["kimi-code"],
    ),
    skill_directory_client(
        "kiro",
        "Kiro",
        "HIMIND_KIRO_SKILL_DIR",
        ".kiro/skills",
        ".kiro/skills",
        "official",
        "Kiro 官方支持项目级与用户级 Agent Skills",
        &["kiro"],
    ),
    skill_directory_client(
        "qwen-code",
        "Qwen Code",
        "HIMIND_QWEN_SKILL_DIR",
        ".qwen/skills",
        ".qwen/skills",
        "official",
        "Qwen Code 官方支持项目级与用户级 Agent Skills",
        &["qwen-code"],
    ),
];

/// 宿主客户端：由 Agent 自己承载，Skill 落点是 Agent 技能库，插件也由 Agent 执行。
pub(crate) const HOST_CLIENTS: &[ClientDefinition] = &[
    ClientDefinition {
        id: HOST_CLIENT_HIMIND_AI,
        name: "HiMind AI",
        support_level: "official",
        support_note: "Agent 原生技能库，安装即可用，无需向外部目录复制文件",
        skills: None,
        mcp: None,
        plugins: Some(ClientPluginsCapability { executor: "agent" }),
    },
    ClientDefinition {
        id: HOST_CLIENT_CODEX,
        name: "Codex",
        support_level: "official",
        support_note: "Codex 原生 Agent Skills",
        skills: Some(ClientSkillsCapability {
            standard: SKILL_STANDARD_AGENT_SKILLS,
            user: Some(ClientScopeTarget {
                directory: ".agents/skills",
                env_key: "HIMIND_CODEX_SKILL_DIR",
            }),
            project: Some(ClientScopeTarget {
                directory: ".agents/skills",
                env_key: "",
            }),
        }),
        mcp: Some(ClientMcpCapability {
            target_ids: &[HOST_CLIENT_CODEX],
            config_format: "TOML",
            auto_configure: true,
        }),
        plugins: Some(ClientPluginsCapability { executor: "agent" }),
    },
];

/// 静态矩阵（不含可用性覆盖层），按契约 schema 输出。
pub(crate) fn static_matrix_json() -> serde_json::Value {
    let clients = HOST_CLIENTS
        .iter()
        .chain(DIRECTORY_CLIENTS.iter())
        .map(ClientDefinition::static_json)
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": MATRIX_SCHEMA_VERSION,
        "clients": clients,
    })
}

pub(crate) fn directory_client(client_id: &str) -> Option<&'static ClientDefinition> {
    DIRECTORY_CLIENTS
        .iter()
        .find(|item| item.id.eq_ignore_ascii_case(client_id))
}

pub(crate) fn client_for_mcp_target(target_id: &str) -> Option<(&'static str, &'static str)> {
    if target_id == HOST_CLIENT_HIMIND_AI {
        return Some((HOST_CLIENT_HIMIND_AI, "HiMind AI"));
    }
    if target_id == HOST_CLIENT_CODEX {
        return Some((HOST_CLIENT_CODEX, "Codex"));
    }
    DIRECTORY_CLIENTS
        .iter()
        .find(|item| item.mcp_target_ids().contains(&target_id))
        .map(|item| (item.id, item.name))
}

pub(crate) fn is_portable_client(client_id: &str) -> bool {
    matches!(client_id, HOST_CLIENT_HIMIND_AI | HOST_CLIENT_CODEX)
        || directory_client(client_id).is_some()
}

/// 声明里是否包含"可移植"档位。安装计划在拿到目录项、还没拿到本地
/// `SkillRecord` 时也要判断同一件事，因此判断逻辑只写一份。
pub(crate) fn declares_portable(supported_clients: &[String]) -> bool {
    supported_clients.iter().any(|client| {
        let client = client.trim().to_ascii_lowercase();
        matches!(
            client.as_str(),
            PORTABLE_PROFILE_ID | HOST_CLIENT_CODEX | "github-copilot" | "workbuddy"
        )
    })
}

pub(crate) fn declares_portable_skill(manifest: &SkillManifest) -> bool {
    declares_portable(&manifest.supported_clients)
}

pub(crate) fn manifest_supports_client(manifest: &SkillManifest, client_id: &str) -> bool {
    supported_clients_include(&manifest.supported_clients, client_id)
}

/// 同一套判定，入参放宽到 `supported_clients` 声明。
///
/// 安装计划拿到的是目录项而不是本地 `SkillRecord`，但"这个客户端能不能用这份
/// 技能"必须只有一个答案，否则计划会承诺一个实际投不进去的目标。
pub(crate) fn supported_clients_include(supported_clients: &[String], client_id: &str) -> bool {
    let client_id = client_id.trim();
    supported_clients
        .iter()
        .any(|item| item.trim().eq_ignore_ascii_case(client_id))
        || (is_portable_client(client_id) && declares_portable(supported_clients))
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
        for client in HOST_CLIENTS.iter().chain(DIRECTORY_CLIENTS.iter()) {
            assert!(
                client_ids.insert(client.id),
                "duplicate client id: {}",
                client.id
            );
            for target_id in client.mcp_target_ids() {
                assert!(
                    target_ids.insert(*target_id),
                    "duplicate MCP target id: {target_id}"
                );
            }
        }
        for client in HOST_CLIENTS.iter().chain(DIRECTORY_CLIENTS.iter()) {
            for alias in client_aliases(client.id) {
                assert!(
                    !client_ids.contains(alias),
                    "alias shadows another client id: {alias}"
                );
            }
        }
    }

    #[test]
    fn every_directory_client_declares_both_skill_scopes() {
        for client in DIRECTORY_CLIENTS {
            let skills = client.skills.expect("directory client must declare skills");
            assert!(
                skills.user.is_some() && skills.project.is_some(),
                "{} must declare user and project skill scopes",
                client.id
            );
            assert_eq!(skills.standard, SKILL_STANDARD_AGENT_SKILLS);
            assert!(
                client.skill_env_key().is_some(),
                "{} must allow an explicit skill directory",
                client.id
            );
        }
    }

    #[test]
    fn official_client_directories_match_native_conventions() {
        assert_eq!(
            directory_client("github-copilot").unwrap().skill_user_dir(),
            Some(".copilot/skills")
        );
        assert_eq!(
            directory_client("workbuddy").unwrap().skill_user_dir(),
            Some(".workbuddy/skills")
        );
        assert_eq!(
            directory_client("qoder").unwrap().skill_user_dir(),
            Some(".qoder/skills")
        );
        assert_eq!(
            directory_client("zcode").unwrap().skill_user_dir(),
            Some(".zcode/skills")
        );
        assert_eq!(
            directory_client("windsurf").unwrap().skill_user_dir(),
            Some(".codeium/windsurf/skills")
        );
        assert_eq!(
            directory_client("cline").unwrap().skill_project_dir(),
            Some(".cline/skills")
        );
        assert_eq!(client_for_mcp_target("rider"), None);
    }
}
