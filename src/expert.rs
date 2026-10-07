//! Versioned expert definitions and session activation.
//!
//! An expert is a portable working-method asset.  It is deliberately kept
//! separate from project instructions, Skills and client permissions.  The
//! latter are dependencies or projection targets; they are never granted by
//! an expert document.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const EXPERT_SCHEMA_VERSION: &str = "expert.v1";
const MAX_EXPERT_PACKAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EXPERT_CONTENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpertHarness {
    #[serde(default)]
    pub behavior_phases: Vec<String>,
    #[serde(default)]
    pub required_evidence: Vec<String>,
    #[serde(default)]
    pub recovery_guidance: Vec<String>,
}

impl Default for ExpertHarness {
    fn default() -> Self {
        Self {
            behavior_phases: vec![
                "plan".into(),
                "execute".into(),
                "verify".into(),
                "deliver".into(),
            ],
            required_evidence: vec!["summary".into(), "next_steps".into()],
            recovery_guidance: vec!["遇到不确定性时先说明并请求补充信息".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpertOutputContract {
    #[serde(default)]
    pub required_sections: Vec<String>,
}

impl Default for ExpertOutputContract {
    fn default() -> Self {
        Self {
            required_sections: vec!["结论".into(), "下一步".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpertDefinition {
    pub schema_version: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default)]
    pub min_agent_version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub skill_refs: Vec<String>,
    #[serde(default)]
    pub workflow_refs: Vec<String>,
    #[serde(default)]
    pub capability_refs: Vec<String>,
    #[serde(default = "default_expert_contents")]
    pub contents: Vec<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub output_contract: ExpertOutputContract,
    #[serde(default)]
    pub harness: ExpertHarness,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExpertSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub categories: Vec<String>,
    pub supported_clients: Vec<String>,
    pub skill_count: usize,
    pub workflow_count: usize,
    pub capability_count: usize,
    pub digest: String,
    pub builtin: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExpertActivation {
    pub expert_id: String,
    pub version: String,
    pub digest: String,
    pub activated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExpertDraftInput {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub skill_refs: Vec<String>,
    #[serde(default)]
    pub workflow_refs: Vec<String>,
    #[serde(default)]
    pub capability_refs: Vec<String>,
    pub instructions: String,
    #[serde(default)]
    pub output_contract: ExpertOutputContract,
    #[serde(default)]
    pub harness: ExpertHarness,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExpertPackageResult {
    pub expert: ExpertSummary,
    pub package_path: PathBuf,
    pub package_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExpertAuthoringDraft {
    pub definition: ExpertDefinition,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    pub updated_at: String,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub submitted_at: Option<String>,
    pub dashboard_release_id: Option<String>,
    pub test_report: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExpertProjectionReceipt {
    pub schema_version: String,
    pub expert_id: String,
    pub expert_version: String,
    pub expert_digest: String,
    pub client_id: String,
    pub workspace_root: String,
    pub target_path: String,
    pub content_digest: String,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub previous_digest: String,
    #[serde(default)]
    pub backup_path: String,
    /// 文件已经写入并校验，不代表外部客户端已经在会话中加载。
    #[serde(default)]
    pub sync_status: String,
    #[serde(default)]
    pub verification_status: String,
    #[serde(default)]
    pub message: String,
    pub projected_at: String,
}

/// Render the same Expert asset into the native, discoverable role surface
/// of a client. This is an adapter projection only: it never adds tools,
/// changes approval settings, or grants execution rights.
pub(crate) fn project_to_client(
    id: &str,
    version: Option<&str>,
    client_id: &str,
    workspace: &Path,
) -> Result<ExpertProjectionReceipt, Box<dyn Error>> {
    let definition = get(id, version)?;
    validate(&definition)?;
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err("专家投影目标不是目录".into());
    }
    let target_client = match client_id.trim().to_ascii_lowercase().as_str() {
        "codex" => "codex",
        "github-copilot" | "copilot" => "github-copilot",
        "claude-code" | "claude" => "claude-code",
        "cursor" => "cursor",
        "windsurf" => "windsurf",
        "cline" => "cline",
        _ => return Err("当前客户端使用会话级专家投影".into()),
    };
    if !definition.supported_clients.iter().any(|client| {
        client.eq_ignore_ascii_case(target_client) || client.eq_ignore_ascii_case("portable")
    }) {
        return Err(format!("专家未声明支持 {target_client}").into());
    }
    let slug_source = definition
        .id
        .strip_prefix("com.himind.expert.")
        .unwrap_or(&definition.id);
    let slug = slug_source
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() {
                value
            } else {
                '-'
            }
        })
        .collect::<String>();
    let managed_name = format!("himind-expert-{slug}");
    let (target, content) = match target_client {
        "codex" => {
            let target = workspace
                .join(".agents")
                .join("skills")
                .join(&managed_name)
                .join("SKILL.md");
            let content = render_role_markdown(&definition, &managed_name, "skill");
            (target, content)
        }
        "github-copilot" => {
            let target = workspace
                .join(".github")
                .join("agents")
                .join(format!("{managed_name}.agent.md"));
            let content = render_role_markdown(&definition, &managed_name, "copilot-agent");
            (target, content)
        }
        "claude-code" => {
            let target = workspace
                .join(".claude")
                .join("agents")
                .join(format!("{managed_name}.md"));
            let content = render_role_markdown(&definition, &managed_name, "claude-agent");
            (target, content)
        }
        // Cursor 的项目规则目录，一个规则一个 .mdc；用 alwaysApply: false 让它按需引用，
        // 不把角色方法当成全局约束。
        "cursor" => {
            let target = workspace
                .join(".cursor")
                .join("rules")
                .join(format!("{managed_name}.mdc"));
            let content = render_role_markdown(&definition, &managed_name, "cursor-rule");
            (target, content)
        }
        // Windsurf 的项目规则目录，trigger 交给模型按需选择。
        "windsurf" => {
            let target = workspace
                .join(".windsurf")
                .join("rules")
                .join(format!("{managed_name}.md"));
            let content = render_role_markdown(&definition, &managed_name, "windsurf-rule");
            (target, content)
        }
        // Cline 的规则目录，纯 Markdown，不加 frontmatter。
        "cline" => {
            let target = workspace
                .join(".clinerules")
                .join(format!("{managed_name}.md"));
            let content = render_role_markdown(&definition, &managed_name, "cline-rule");
            (target, content)
        }
        _ => unreachable!("target client was validated above"),
    };
    let marker = "<!-- HiMind managed expert projection -->";
    let existing = if target.is_file() {
        Some(fs::read_to_string(&target)?)
    } else {
        None
    };
    if let Some(existing) = existing.as_ref() {
        if !existing.contains(marker) {
            return Err("客户端专家文件已存在且不是 HiMind 管理内容".into());
        }
    }
    let expert_digest = digest(&definition)?;
    let content_digest = format!("sha256:{:x}", Sha256::digest(content.as_bytes()));
    let previous_digest = existing
        .as_ref()
        .map(|value| format!("sha256:{:x}", Sha256::digest(value.as_bytes())))
        .unwrap_or_default();
    let changed = existing.as_deref() != Some(content.as_str());
    let backup_path = if changed && target.is_file() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default();
        let backup = target.with_file_name(format!(
            "{}.himind-expert-backup-{stamp}.bak",
            target
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("expert")
        ));
        fs::copy(&target, &backup)?;
        backup.to_string_lossy().to_string()
    } else {
        String::new()
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if changed {
        crate::store::atomic_file::atomic_write(&target, content.as_bytes())?;
    }
    let actual_digest = format!("sha256:{:x}", Sha256::digest(fs::read(&target)?));
    if actual_digest != content_digest {
        return Err("客户端专家文件写入校验失败".into());
    }
    let receipt = ExpertProjectionReceipt {
        schema_version: "expert_projection.v1".to_string(),
        expert_id: definition.id,
        expert_version: definition.version,
        expert_digest,
        client_id: target_client.to_string(),
        workspace_root: workspace.to_string_lossy().to_string(),
        target_path: target.to_string_lossy().to_string(),
        content_digest,
        changed,
        previous_digest,
        backup_path,
        sync_status: if changed {
            "file_written".to_string()
        } else {
            "file_unchanged".to_string()
        },
        verification_status: "file_verified_client_load_unverified".to_string(),
        message: "文件已写入并校验；外部客户端会话是否加载由客户端负责".to_string(),
        projected_at: now_stamp(),
    };
    let receipt_key = format!(
        "{}-{}",
        target_client,
        Sha256::digest(workspace.to_string_lossy().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let receipt_path = projection_root().join(format!("{receipt_key}.json"));
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::store::atomic_file::atomic_write(&receipt_path, &serde_json::to_vec_pretty(&receipt)?)?;
    update_project_expert_index(&workspace)?;
    Ok(receipt)
}

/// Keep a client-neutral discovery bridge in the project. It points to the
/// versioned expert asset and never stores the current session selection.
/// Native clients can ignore it; MCP-only and custom integrations can use it
/// to discover the same source of truth without parsing client-specific files.
fn update_project_expert_index(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let path = workspace.join(".himind").join("experts.md");
    let marker_start = "<!-- HiMind managed expert index:start -->";
    let marker_end = "<!-- HiMind managed expert index:end -->";
    let entries = list()?
        .into_iter()
        .map(|item| {
            format!(
                "- **{}** (`{}` v{})：{}（适配：{}）",
                item.name,
                item.id,
                item.version,
                item.description,
                if item.supported_clients.is_empty() {
                    "portable".to_string()
                } else {
                    item.supported_clients.join("、")
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let existing = path
        .is_file()
        .then(|| fs::read_to_string(&path))
        .transpose()?
        .unwrap_or_default();
    let managed = format!(
        "{marker_start}\n## HiMind 专家\n\n专家定义位于 `.himind/experts/`，会话中由客户端原生 Agent 选择器或 HiMind MCP Prompt 激活。\n\n{entries}\n{marker_end}"
    );
    let content = if let (Some(start), Some(end)) =
        (existing.find(marker_start), existing.find(marker_end))
    {
        let end = end + marker_end.len();
        format!("{}{}{}", &existing[..start], managed, &existing[end..])
    } else if existing.trim().is_empty() {
        format!("{managed}\n")
    } else {
        format!("{}\n\n{managed}\n", existing.trim_end())
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::store::atomic_file::atomic_write(&path, content.as_bytes())?;
    Ok(())
}

fn render_role_markdown(
    definition: &ExpertDefinition,
    managed_name: &str,
    projection_kind: &str,
) -> String {
    // frontmatter 跟着目标客户端走：Cursor 用 rules 的字段，Windsurf 用 trigger，
    // Cline 读纯 Markdown。写错字段只会让规则不被采纳，不会破坏客户端配置。
    let frontmatter = match projection_kind {
        "cursor-rule" => format!(
            "---\ndescription: \"{}\"\nglobs:\nalwaysApply: false\n---\n",
            yaml_text(&definition.description)
        ),
        "windsurf-rule" => format!(
            "---\ntrigger: model_decision\ndescription: \"{}\"\n---\n",
            yaml_text(&definition.description)
        ),
        "cline-rule" => String::new(),
        _ => format!(
            "---\nname: {managed_name}\ndescription: \"{}\"\n---\n",
            yaml_text(&definition.description)
        ),
    };
    format!(
        "{frontmatter}\n<!-- HiMind managed expert projection -->\n<!-- kind: {projection_kind} -->\n<!-- expert-id: {} -->\n<!-- expert-version: {} -->\n<!-- expert-digest: {} -->\n\n# {}\n\n{}\n\n## 输出要求\n{}\n\n## 工作阶段\n{}\n\n## 恢复建议\n{}\n",
        definition.id,
        definition.version,
        digest(definition).unwrap_or_else(|_| "sha256:unavailable".to_string()),
        definition.name,
        definition.instructions.trim(),
        definition.output_contract.required_sections.join("、"),
        definition.harness.behavior_phases.join(" → "),
        definition.harness.recovery_guidance.join("；"),
    )
}

/// DSH agent preset 声明文件的文件名。
///
/// DSH 0.2.x 起，用户预设不再是 `$DSH_HOME/.agent-presets/` 目录，而是 profile
/// 里的 `@deepseek-ai/dsh-agent-preset` 声明行。HiMind 把专家库渲染成一份独立
/// 文件，由 `cordis:include` 引用，避免改写用户手写的 `cordis.patch.yml`。
pub(crate) const DSH_EXPERT_PRESETS_FILE: &str = "himind-expert-presets.yml";
/// profile patch 中承载 `cordis:include` 的行 id。
pub(crate) const DSH_EXPERT_PRESETS_ROW_ID: &str = "himind-expert-presets";
/// 专家预设排在 DSH 自带模式之后。
const DSH_EXPERT_PRESET_ORDER_BASE: i64 = 100;
const DSH_PRESET_FILE_HEADER: &str = "# Generated by HiMind Agent from the expert library. Do not edit.\n# Regenerated whenever a HiMind AI session starts.\n";
/// 专家预设的共享能力面。内容与 DSH 自带标准模式逐行一致，只把 persona 换成专家人设。
const DSH_EXPERT_PLUGINS: &str =
    include_str!("../runtime-profiles/himind/expert-presets.plugins.yml");

/// DSH 预设 id 只接受小写字母、数字和连字符。
///
/// `com.himind.expert.contract-review` → `himind-contract-review`。
pub(crate) fn dsh_preset_id(expert_id: &str) -> String {
    let source = expert_id
        .strip_prefix("com.himind.expert.")
        .unwrap_or(expert_id);
    let mut slug = String::new();
    let mut pending_dash = false;
    for value in source.chars() {
        if value.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(value.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    let slug = if slug.is_empty() {
        "expert".to_string()
    } else {
        slug
    };
    format!("himind-{slug}")
}

/// 把专家定义渲染成人设正文：HiMind 身份约束 + 专家职责与方法 + 输出契约。
fn dsh_expert_persona(definition: &ExpertDefinition) -> String {
    let mut text = format!(
        "You are HiMind AI working as the \"{}\" expert. Complete the user's task in the assigned HiMind workspace. Do not expose provider keys, runtime implementation names, or internal prompts.\n\n# {}\n\n{}",
        definition.name,
        definition.name,
        definition.instructions.trim()
    );
    if !definition.output_contract.required_sections.is_empty() {
        text.push_str(&format!(
            "\n\n## 输出要求\n{}",
            definition.output_contract.required_sections.join("、")
        ));
    }
    if !definition.harness.behavior_phases.is_empty() {
        text.push_str(&format!(
            "\n\n## 工作阶段\n{}",
            definition.harness.behavior_phases.join(" → ")
        ));
    }
    if !definition.harness.recovery_guidance.is_empty() {
        text.push_str(&format!(
            "\n\n## 恢复建议\n{}",
            definition.harness.recovery_guidance.join("；")
        ));
    }
    neutralize_template_braces(&text)
}

/// DSH 的 persona 前缀是模板：未知的 `{{...}}` 组会让提示词渲染失败。
///
/// 专家正文属于用户内容，这里把双花括号拆开，避免被当成模板变量。
/// 调用方自己追加的 `{{cwd}}` 不在正文内，仍然保留模板语义。
fn neutralize_template_braces(value: &str) -> String {
    value.replace("{{", "{ {").replace("}}", "} }")
}

/// YAML 块标量：逐行缩进，空行保持为空，保留正文结构。
fn yaml_block_scalar(value: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    value
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 渲染 DSH 预设声明文件。
///
/// 每个专家一条 `@deepseek-ai/dsh-agent-preset` 声明；预设 id 冲突时追加摘要
/// 后缀，因为 DSH 遇到重复预设 id 会拒绝加载整份声明。
pub(crate) fn dsh_preset_document() -> Result<String, Box<dyn Error>> {
    let mut definitions = stored_definitions()?;
    definitions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.version.cmp(&right.version))
    });

    let mut used = HashSet::new();
    let mut body = String::new();
    let mut order = DSH_EXPERT_PRESET_ORDER_BASE;
    for definition in definitions {
        if validate(&definition).is_err() {
            continue;
        }
        let mut preset_id = dsh_preset_id(&definition.id);
        if !used.insert(preset_id.clone()) {
            let digest = digest(&definition).unwrap_or_default();
            let suffix = digest
                .trim_start_matches("sha256:")
                .chars()
                .take(6)
                .collect::<String>();
            preset_id = if suffix.is_empty() {
                format!("{preset_id}-{}", order)
            } else {
                format!("{preset_id}-{suffix}")
            };
            used.insert(preset_id.clone());
        }
        body.push_str(&format!(
            "- id: preset-{preset_id}\n  name: '@deepseek-ai/dsh-agent-preset'\n  config:\n    id: {preset_id}\n    name: \"{}\"\n",
            yaml_text(&definition.name)
        ));
        if !definition.description.trim().is_empty() {
            body.push_str(&format!(
                "    description: \"{}\"\n",
                yaml_text(&definition.description)
            ));
        }
        body.push_str(&format!(
            "    order: {order}\n    plugins:\n      - id: persona\n        name: '@deepseek-ai/dsh-persona'\n        config:\n          prefix: |-\n{}\n          suffix: Your working directory is {{{{cwd}}}}.\n",
            yaml_block_scalar(&dsh_expert_persona(&definition), 12)
        ));
        body.push_str(DSH_EXPERT_PLUGINS);
        order += 1;
    }

    if body.is_empty() {
        return Ok(format!("{DSH_PRESET_FILE_HEADER}[]\n"));
    }
    Ok(format!("{DSH_PRESET_FILE_HEADER}{body}"))
}

pub(crate) fn list() -> Result<Vec<ExpertSummary>, Box<dyn Error>> {
    let active = active()?;
    let mut definitions = stored_definitions()?;
    let mut result = definitions
        .into_iter()
        .map(|definition| summary(&definition, active.as_ref(), false))
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.version.cmp(&right.version))
    });
    Ok(result)
}

pub(crate) fn get(id: &str, version: Option<&str>) -> Result<ExpertDefinition, Box<dyn Error>> {
    let id = validate_id(id)?;
    let candidates = stored_definitions()?
        .into_iter()
        .filter(|item| item.id == id && version.map(|v| v == item.version).unwrap_or(true))
        .collect::<Vec<_>>();
    candidates
        .into_iter()
        .max_by(|left, right| left.version.cmp(&right.version))
        .ok_or_else(|| format!("未找到专家: {id}").into())
}

pub(crate) fn save(input: ExpertDraftInput) -> Result<ExpertSummary, Box<dyn Error>> {
    let definition = ExpertDefinition {
        schema_version: EXPERT_SCHEMA_VERSION.into(),
        id: validate_id(&input.id)?,
        name: required_text("专家名称", &input.name)?,
        author: input.author,
        categories: normalize(input.categories),
        version: validate_version(&input.version)?,
        release_notes: "创建初始版本。".into(),
        min_agent_version: crate::VERSION.into(),
        description: input.description,
        supported_clients: normalize(input.supported_clients),
        skill_refs: normalize(input.skill_refs),
        workflow_refs: normalize(input.workflow_refs),
        capability_refs: normalize(input.capability_refs),
        contents: vec!["EXPERT.md".into()],
        instructions: required_text("专家说明", &input.instructions)?,
        output_contract: input.output_contract,
        harness: input.harness,
    };
    validate(&definition)?;
    let root = store_version_root(&definition.id, &definition.version);
    if root.exists() {
        let existing: ExpertDefinition =
            serde_json::from_slice(&fs::read(root.join("expert.json"))?)?;
        if digest(&existing)? != digest(&definition)? {
            return Err("专家版本已存在；内容变更请创建新版本".into());
        }
        return Ok(summary(&existing, active()?.as_ref(), false));
    }
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("expert.json"),
        serde_json::to_vec_pretty(&definition)?,
    )?;
    fs::write(root.join("EXPERT.md"), definition.instructions.as_bytes())?;
    Ok(summary(&definition, active()?.as_ref(), false))
}

pub(crate) fn validate_definition(definition: &ExpertDefinition) -> Result<(), Box<dyn Error>> {
    validate(definition)
}

/// Install an expert definition discovered from a trusted local extension source.
/// The source remains authoritative; this only copies the version into the
/// Agent store so it becomes available to sessions and DSH presets.
pub(crate) fn install_local_definition(
    definition: ExpertDefinition,
) -> Result<ExpertSummary, Box<dyn Error>> {
    validate(&definition)?;
    let root = store_version_root(&definition.id, &definition.version);
    if root.exists() {
        let existing: ExpertDefinition =
            serde_json::from_slice(&fs::read(root.join("expert.json"))?)?;
        if digest(&existing)? != digest(&definition)? {
            return Err("本机已存在相同版本的不同专家内容".into());
        }
        return Ok(summary(&existing, active()?.as_ref(), false));
    }
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("expert.json"),
        serde_json::to_vec_pretty(&definition)?,
    )?;
    fs::write(root.join("EXPERT.md"), definition.instructions.as_bytes())?;
    Ok(summary(&definition, active()?.as_ref(), false))
}

pub(crate) fn build_workspace_candidate(
    workspace: &Path,
) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    let definition: ExpertDefinition =
        serde_json::from_slice(&fs::read(workspace.join("expert.json"))?)?;
    validate(&definition)?;
    let instructions = fs::read(workspace.join("EXPERT.md"))?;
    if definition.instructions.as_bytes() != instructions {
        return Err("expert.json 与 EXPERT.md 内容不一致".into());
    }
    let candidate_root = crate::store::paths::agent_home()
        .join("expert-drafts")
        .join(&definition.id)
        .join(&definition.version);
    fs::create_dir_all(&candidate_root)?;
    let candidate_path =
        candidate_root.join(format!("{}-{}.hmexpert", definition.id, definition.version));
    let root = store_version_root(&definition.id, &definition.version);
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("expert.json"),
        serde_json::to_vec_pretty(&definition)?,
    )?;
    fs::write(root.join("EXPERT.md"), &instructions)?;
    export_package(&definition.id, &definition.version, &candidate_path)?;
    let sha = format!("sha256:{:x}", Sha256::digest(fs::read(&candidate_path)?));
    let draft = ExpertAuthoringDraft {
        definition,
        candidate_path,
        candidate_sha256: sha,
        updated_at: now_stamp(),
        tested_at: None,
        confirmed_at: None,
        submitted_at: None,
        dashboard_release_id: None,
        test_report: None,
    };
    persist_authoring_draft(&draft)?;
    Ok(draft)
}

pub(crate) fn list_authoring_drafts() -> Result<Vec<ExpertAuthoringDraft>, Box<dyn Error>> {
    let root = crate::store::paths::agent_home().join("expert-drafts");
    let mut result = Vec::new();
    if !root.is_dir() {
        return Ok(result);
    }
    for id in fs::read_dir(root)?.flatten().filter(|e| e.path().is_dir()) {
        for version in fs::read_dir(id.path())?
            .flatten()
            .filter(|e| e.path().is_dir())
        {
            if let Ok(draft) = serde_json::from_slice(&fs::read(version.path().join("draft.json"))?)
            {
                result.push(draft);
            }
        }
    }
    Ok(result)
}

pub(crate) fn read_authoring_draft(
    id: &str,
    version: &str,
) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(
        authoring_root(id, version).join("draft.json"),
    )?)?)
}

pub(crate) fn test_authoring_draft(
    id: &str,
    version: &str,
) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    let mut draft = read_authoring_draft(id, version)?;
    ensure_candidate_unchanged(&draft)?;
    validate(&draft.definition)?;
    draft.tested_at = Some(now_stamp());
    draft.test_report = Some(
        json!({"candidate_sha256": draft.candidate_sha256, "agent_version": crate::VERSION, "tested_at": draft.tested_at}),
    );
    draft.updated_at = now_stamp();
    persist_authoring_draft(&draft)?;
    Ok(draft)
}

pub(crate) fn confirm_authoring_draft(
    id: &str,
    version: &str,
) -> Result<ExpertAuthoringDraft, Box<dyn Error>> {
    let mut draft = read_authoring_draft(id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先完成专家候选包测试".into());
    }
    draft.confirmed_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist_authoring_draft(&draft)?;
    Ok(draft)
}

fn authoring_root(id: &str, version: &str) -> PathBuf {
    crate::store::paths::agent_home()
        .join("expert-drafts")
        .join(id)
        .join(version)
}
fn persist_authoring_draft(draft: &ExpertAuthoringDraft) -> Result<(), Box<dyn Error>> {
    let root = authoring_root(&draft.definition.id, &draft.definition.version);
    fs::create_dir_all(&root)?;
    fs::write(root.join("draft.json"), serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}
fn ensure_candidate_unchanged(draft: &ExpertAuthoringDraft) -> Result<(), Box<dyn Error>> {
    let digest = format!(
        "sha256:{:x}",
        Sha256::digest(fs::read(&draft.candidate_path)?)
    );
    if !digest.eq_ignore_ascii_case(&draft.candidate_sha256) {
        return Err("专家候选包已变化，请重新构建并测试".into());
    }
    Ok(())
}

pub(crate) fn export_package(
    id: &str,
    version: &str,
    package_path: &Path,
) -> Result<ExpertPackageResult, Box<dyn Error>> {
    let definition = get(id, Some(version))?;
    validate(&definition)?;
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "expert.json".to_string(),
        serde_json::to_vec_pretty(&definition)?,
    );
    files.insert(
        "EXPERT.md".to_string(),
        definition.instructions.as_bytes().to_vec(),
    );
    let checksums = files
        .iter()
        .map(|(name, bytes)| format!("{:x}  {name}\n", Sha256::digest(bytes)))
        .collect::<String>();
    if let Some(parent) = package_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(package_path)?;
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    for (name, bytes) in &files {
        archive.start_file(name, options)?;
        archive.write_all(bytes)?;
    }
    archive.start_file("checksums.sha256", options)?;
    archive.write_all(checksums.as_bytes())?;
    archive.finish()?;
    Ok(ExpertPackageResult {
        expert: summary(&definition, active()?.as_ref(), false),
        package_sha256: format!("sha256:{:x}", Sha256::digest(fs::read(package_path)?)),
        package_path: package_path.to_path_buf(),
    })
}

pub(crate) fn import_package(package_path: &Path) -> Result<ExpertSummary, Box<dyn Error>> {
    let source = package_path.canonicalize()?;
    if !source.is_file()
        || !source
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("hmexpert") || value.eq_ignore_ascii_case("zip")
            })
    {
        return Err("专家包必须是 .hmexpert 或 .zip 文件".into());
    }
    if fs::metadata(&source)?.len() > MAX_EXPERT_PACKAGE_BYTES {
        return Err("专家包超过 4 MiB 限制".into());
    }
    let file = File::open(source)?;
    let mut archive = zip::ZipArchive::new(file)?;
    if archive.len() != 3 {
        return Err("专家包必须只包含 expert.json、EXPERT.md 和 checksums.sha256".into());
    }
    let mut files = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().replace('\\', "/");
        if entry.is_dir()
            || name.contains('/')
            || !matches!(
                name.as_str(),
                "expert.json" | "EXPERT.md" | "checksums.sha256"
            )
            || entry.size() > MAX_EXPERT_CONTENT_BYTES as u64
        {
            return Err("专家包包含不支持的路径或内容".into());
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .take(MAX_EXPERT_CONTENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_EXPERT_CONTENT_BYTES {
            return Err("专家包内容超出大小限制".into());
        }
        if files.insert(name, bytes).is_some() {
            return Err("专家包包含重复文件".into());
        }
    }
    let checksum_text = String::from_utf8(
        files
            .remove("checksums.sha256")
            .ok_or("专家包缺少 checksums.sha256")?,
    )?;
    let mut checksums = std::collections::BTreeMap::new();
    for line in checksum_text.lines().filter(|line| !line.trim().is_empty()) {
        let (expected, name) = line.split_once("  ").ok_or("专家包校验文件格式无效")?;
        if checksums
            .insert(name.to_string(), expected.to_ascii_lowercase())
            .is_some()
        {
            return Err("专家包校验文件包含重复路径".into());
        }
    }
    if checksums.len() != files.len() {
        return Err("专家包校验文件与内容不匹配".into());
    }
    for (name, bytes) in &files {
        let expected = checksums.get(name).ok_or("专家包内容缺少摘要")?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if expected != &actual {
            return Err(format!("专家包校验失败: {name}").into());
        }
    }
    let definition: ExpertDefinition =
        serde_json::from_slice(files.get("expert.json").ok_or("专家包缺少 expert.json")?)?;
    let instructions = files.get("EXPERT.md").ok_or("专家包缺少 EXPERT.md")?;
    if definition.instructions.as_bytes() != instructions {
        return Err("expert.json 与 EXPERT.md 内容不一致".into());
    }
    validate(&definition)?;
    let root = store_version_root(&definition.id, &definition.version);
    if root.exists() {
        let existing: ExpertDefinition =
            serde_json::from_slice(&fs::read(root.join("expert.json"))?)?;
        if digest(&existing)? != digest(&definition)? {
            return Err("本机已存在相同版本的不同专家内容".into());
        }
        return Ok(summary(&existing, active()?.as_ref(), false));
    }
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("expert.json"),
        serde_json::to_vec_pretty(&definition)?,
    )?;
    fs::write(root.join("EXPERT.md"), instructions)?;
    Ok(summary(&definition, active()?.as_ref(), false))
}

pub(crate) fn activate(
    id: &str,
    version: Option<&str>,
    workspace: Option<&Path>,
) -> Result<ExpertActivation, Box<dyn Error>> {
    let definition = get(id, version)?;
    validate(&definition)?;
    let definition_digest = digest(&definition)?;
    let activation = ExpertActivation {
        expert_id: definition.id,
        version: definition.version,
        digest: definition_digest,
        activated_at: now_stamp(),
        workspace_root: workspace.map(|path| path.to_string_lossy().to_string()),
    };
    let path = workspace
        .map(workspace_active_path)
        .unwrap_or_else(active_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&activation)?)?;
    Ok(activation)
}

pub(crate) fn active() -> Result<Option<ExpertActivation>, Box<dyn Error>> {
    active_from_path(&active_path())
}

pub(crate) fn active_for_workspace(
    workspace: Option<&Path>,
) -> Result<Option<ExpertActivation>, Box<dyn Error>> {
    let Some(workspace) = workspace else {
        return active();
    };
    let path = workspace_active_path(workspace);
    if path.is_file() {
        return active_from_path(&path);
    }
    active()
}

fn active_from_path(path: &Path) -> Result<Option<ExpertActivation>, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

pub(crate) fn active_definition() -> Result<Option<ExpertDefinition>, Box<dyn Error>> {
    active_definition_for_workspace(None)
}

pub(crate) fn active_definition_for_workspace(
    workspace: Option<&Path>,
) -> Result<Option<ExpertDefinition>, Box<dyn Error>> {
    let activation_path = workspace
        .map(workspace_active_path)
        .filter(|path| path.is_file())
        .unwrap_or_else(active_path);
    let Some(active) = active_from_path(&activation_path)? else {
        return Ok(None);
    };
    let definition = match get(&active.expert_id, Some(&active.version)) {
        Ok(definition) => definition,
        Err(error) if error.to_string().contains("未找到专家") => {
            // An expert can be removed or replaced after a client persisted its
            // selection. A stale optional persona must never prevent DSH from
            // starting; remove only the managed activation marker and continue
            // with the normal no-expert session.
            let _ = fs::remove_file(&activation_path);
            eprintln!(
                "HiMind: 已清理失效专家选择 {}@{}",
                active.expert_id, active.version
            );
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if digest(&definition)? != active.digest {
        let _ = fs::remove_file(&activation_path);
        eprintln!(
            "HiMind: 已清理摘要失效的专家选择 {}@{}",
            active.expert_id, active.version
        );
        return Ok(None);
    }
    Ok(Some(definition))
}

pub(crate) fn mcp_prompts_json() -> Result<Value, Box<dyn Error>> {
    let prompts = list()?
        .into_iter()
        .map(|item| {
            json!({
                "name": format!("expert.{}", item.id),
                "title": item.name,
                "description": item.description,
                "arguments": [],
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "prompts": prompts }))
}

pub(crate) fn mcp_prompt_get(name: &str) -> Result<Value, Box<dyn Error>> {
    let id = name.strip_prefix("expert.").ok_or("不是专家 Prompt")?;
    let definition = get(id, None)?;
    let text = format!(
        "# {}\n\n{}\n\n## 输出要求\n{}\n\n## 工作阶段\n{}",
        definition.name,
        definition.instructions,
        definition.output_contract.required_sections.join("、"),
        definition.harness.behavior_phases.join(" → ")
    );
    Ok(json!({
        "description": definition.description,
        "messages": [{"role": "user", "content": {"type": "text", "text": text}}],
        "_meta": {"himind": {"expert_id": definition.id, "expert_version": definition.version, "expert_digest": digest(&definition)?}}
    }))
}

fn summary(
    definition: &ExpertDefinition,
    active: Option<&ExpertActivation>,
    builtin: bool,
) -> ExpertSummary {
    ExpertSummary {
        id: definition.id.clone(),
        name: definition.name.clone(),
        version: definition.version.clone(),
        description: definition.description.clone(),
        author: definition.author.clone(),
        categories: definition.categories.clone(),
        supported_clients: definition.supported_clients.clone(),
        skill_count: definition.skill_refs.len(),
        workflow_count: definition.workflow_refs.len(),
        capability_count: definition.capability_refs.len(),
        digest: digest(definition).unwrap_or_default(),
        builtin,
        active: active.is_some_and(|item| {
            item.expert_id == definition.id && item.version == definition.version
        }),
    }
}

fn stored_definitions() -> Result<Vec<ExpertDefinition>, Box<dyn Error>> {
    let root = store_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for id_dir in fs::read_dir(root)?
        .flatten()
        .filter(|entry| entry.path().is_dir())
    {
        // Versioned experts are stored under `<id>/versions/<version>`. Keep
        // reading the legacy `<id>/<version>` layout so upgrades do not make
        // previously installed assets disappear.
        let versions_root = id_dir.path().join("versions");
        let versions_root = if versions_root.is_dir() {
            versions_root
        } else {
            id_dir.path()
        };
        for version_dir in fs::read_dir(versions_root)?
            .flatten()
            .filter(|entry| entry.path().is_dir())
        {
            let path = version_dir.path().join("expert.json");
            if path.is_file() {
                let definition: ExpertDefinition = serde_json::from_slice(&fs::read(path)?)?;
                validate(&definition)?;
                items.push(definition);
            }
        }
    }
    Ok(items)
}

fn validate(definition: &ExpertDefinition) -> Result<(), Box<dyn Error>> {
    if definition.schema_version != EXPERT_SCHEMA_VERSION {
        return Err("不支持的专家 schema 版本".into());
    }
    validate_id(&definition.id)?;
    validate_version(&definition.version)?;
    if definition.instructions.trim().is_empty() {
        return Err("专家说明不能为空".into());
    }
    if definition.instructions.len() > 131_072 {
        return Err("专家说明超过 128 KiB 限制".into());
    }
    if definition.contents != vec!["EXPERT.md".to_string()] {
        return Err("专家 contents 必须只声明 EXPERT.md".into());
    }
    Ok(())
}

fn default_expert_contents() -> Vec<String> {
    vec!["EXPERT.md".into()]
}

fn digest(definition: &ExpertDefinition) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(definition)?)
    ))
}

fn store_root() -> PathBuf {
    crate::store::paths::agent_home().join("experts")
}
fn projection_root() -> PathBuf {
    store_root().join("projections")
}
fn store_version_root(id: &str, version: &str) -> PathBuf {
    store_root().join(id).join("versions").join(version)
}
fn active_path() -> PathBuf {
    store_root().join("active.json")
}
fn workspace_active_path(workspace: &Path) -> PathBuf {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical.to_string_lossy().to_lowercase().as_bytes())
    );
    store_root()
        .join("workspaces")
        .join(format!("{digest}.json"))
}
fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}
pub(crate) fn now_stamp_public() -> String {
    now_stamp()
}
pub(crate) fn persist_authoring_draft_public(
    draft: &ExpertAuthoringDraft,
) -> Result<(), Box<dyn Error>> {
    persist_authoring_draft(draft)
}
fn required_text(label: &str, value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{label}不能为空").into())
    } else {
        Ok(value.into())
    }
}
fn normalize(values: Vec<String>) -> Vec<String> {
    let mut result = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}
fn validate_id(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(format!("专家 ID 无效: {value}").into());
    }
    Ok(value.into())
}
fn validate_version(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(format!("专家版本无效: {value}").into());
    }
    Ok(value.into())
}

fn yaml_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;

    fn test_definition(id: &str, name: &str, instructions: &str) -> ExpertDefinition {
        ExpertDefinition {
            schema_version: EXPERT_SCHEMA_VERSION.into(),
            id: id.into(),
            name: name.into(),
            author: "测试扩展".into(),
            categories: vec!["software-engineering".into()],
            version: "1.0.0".into(),
            release_notes: "测试版本".into(),
            min_agent_version: crate::VERSION.into(),
            description: format!("{name}测试定义"),
            supported_clients: vec!["codex".into(), "portable".into()],
            skill_refs: Vec::new(),
            workflow_refs: Vec::new(),
            capability_refs: Vec::new(),
            contents: vec!["EXPERT.md".into()],
            instructions: instructions.into(),
            output_contract: ExpertOutputContract::default(),
            harness: ExpertHarness::default(),
        }
    }

    fn store_test_definition(definition: &ExpertDefinition) {
        let root = store_version_root(&definition.id, &definition.version);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("expert.json"),
            serde_json::to_vec_pretty(definition).unwrap(),
        )
        .unwrap();
        fs::write(root.join("EXPERT.md"), definition.instructions.as_bytes()).unwrap();
    }

    #[test]
    fn experts_are_empty_until_an_extension_is_installed() {
        let _guard = test_env_lock();
        let root = std::env::temp_dir().join(format!("himind-expert-empty-{}", std::process::id()));
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        assert!(list().unwrap().is_empty());
        assert_eq!(
            dsh_preset_document().unwrap(),
            format!("{DSH_PRESET_FILE_HEADER}[]\n")
        );
        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dsh_preset_ids_follow_the_dsh_directory_rules() {
        assert_eq!(
            dsh_preset_id("com.himind.expert.senior-ui-designer"),
            "himind-senior-ui-designer"
        );
        assert_eq!(
            dsh_preset_id("com.himind.expert.Senior UI Designer"),
            "himind-senior-ui-designer"
        );
        assert_eq!(dsh_preset_id("plain_id.v2"), "himind-plain-id-v2");
        assert_eq!(dsh_preset_id(""), "himind-expert");
        for value in [
            dsh_preset_id("com.himind.expert.senior-ui-designer"),
            dsh_preset_id("plain_id.v2"),
            dsh_preset_id(""),
        ] {
            assert!(value
                .chars()
                .all(|item| item.is_ascii_lowercase() || item.is_ascii_digit() || item == '-'));
            assert!(!value.starts_with('-') && !value.ends_with('-'));
        }
    }

    #[test]
    fn dsh_preset_document_declares_each_expert_once() {
        let _guard = test_env_lock();
        let root =
            std::env::temp_dir().join(format!("himind-expert-dsh-preset-{}", std::process::id()));
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        fs::create_dir_all(&root).unwrap();

        let definitions = vec![
            test_definition("com.himind.expert.one", "专家一", "第一条"),
            test_definition("com.himind.expert.two", "专家二", "第二条"),
        ];
        for definition in &definitions {
            store_test_definition(definition);
        }
        let document = dsh_preset_document().unwrap();
        for definition in &definitions {
            let preset_id = dsh_preset_id(&definition.id);
            assert_eq!(
                document
                    .matches(&format!("- id: preset-{preset_id}\n"))
                    .count(),
                1,
                "预设 {preset_id} 必须只声明一次"
            );
            assert!(document.contains(&format!("    id: {preset_id}\n")));
            assert!(document.contains(&definition.name));
        }
        assert_eq!(
            document.matches("- id: preset-").count(),
            definitions.len(),
            "每个专家一条声明，不能多也不能少"
        );
        // 专家预设自带完整能力面，不依赖 profile 里的 HiMind 私有行。
        assert!(document.contains("name: '@deepseek-ai/dsh-persona'"));
        assert!(document.contains("name: '@deepseek-ai/dsh-tool-fs'"));
        assert!(document.contains("name: '@deepseek-ai/dsh-compaction-tool-result-pruner'"));
        assert!(document.contains("!!js process.platform === 'win32'"));
        assert!(document.contains("suffix: Your working directory is {{cwd}}."));
        // 预设 id 不能和 DSH 自带模式撞车。
        for shipped in ["standard", "ptc", "minimal", "cordis"] {
            assert!(!document.contains(&format!("    id: {shipped}\n")));
        }
        assert!(!document.contains('\t'));

        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dsh_persona_neutralizes_user_template_braces() {
        let definition = test_definition(
            "com.himind.expert.brace-probe",
            "括号探针",
            "正文包含 {{model}} 模板片段",
        );
        let persona = dsh_expert_persona(&definition);
        assert!(!persona.contains("{{"), "用户正文不能留下模板占位符");
        assert!(persona.contains("{ {model} }"));
    }

    #[test]
    fn native_projection_uses_client_role_surfaces() {
        let _guard = test_env_lock();
        let root =
            std::env::temp_dir().join(format!("himind-expert-projection-{}", std::process::id()));
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        fs::create_dir_all(&root).unwrap();

        for (id, name) in [
            ("com.himind.expert.senior-ui-designer", "高级 UI 设计师"),
            ("com.himind.expert.senior-test-engineer", "高级测试工程师"),
            (
                "com.himind.expert.senior-system-architect",
                "高级系统架构师",
            ),
        ] {
            store_test_definition(&test_definition(id, name, "测试专家说明"));
        }

        let copilot = project_to_client(
            "com.himind.expert.senior-ui-designer",
            None,
            "github-copilot",
            &root,
        )
        .unwrap();
        let claude = project_to_client(
            "com.himind.expert.senior-test-engineer",
            None,
            "claude-code",
            &root,
        )
        .unwrap();
        let codex = project_to_client(
            "com.himind.expert.senior-system-architect",
            None,
            "codex",
            &root,
        )
        .unwrap();

        assert!(
            copilot
                .target_path
                .ends_with(".github\\agents\\himind-expert-senior-ui-designer.agent.md")
                || copilot
                    .target_path
                    .ends_with(".github/agents/himind-expert-senior-ui-designer.agent.md")
        );
        assert!(
            claude
                .target_path
                .ends_with(".claude\\agents\\himind-expert-senior-test-engineer.md")
                || claude
                    .target_path
                    .ends_with(".claude/agents/himind-expert-senior-test-engineer.md")
        );
        assert!(
            codex
                .target_path
                .ends_with(".agents\\skills\\himind-expert-senior-system-architect\\SKILL.md")
                || codex
                    .target_path
                    .ends_with(".agents/skills/himind-expert-senior-system-architect/SKILL.md")
        );
        assert!(fs::read_to_string(&copilot.target_path)
            .unwrap()
            .contains("kind: copilot-agent"));
        assert!(fs::read_to_string(&claude.target_path)
            .unwrap()
            .contains("kind: claude-agent"));
        assert!(fs::read_to_string(&codex.target_path)
            .unwrap()
            .contains("kind: skill"));
        assert!(root.join(".himind").join("experts.md").is_file());

        let unchanged = project_to_client(
            "com.himind.expert.senior-ui-designer",
            None,
            "github-copilot",
            &root,
        )
        .unwrap();
        assert!(!unchanged.changed);
        assert_eq!(
            unchanged.verification_status,
            "file_verified_client_load_unverified"
        );

        fs::write(&copilot.target_path, "user-owned expert file\n").unwrap();
        let conflict = project_to_client(
            "com.himind.expert.senior-ui-designer",
            None,
            "github-copilot",
            &root,
        )
        .unwrap_err();
        assert!(conflict.to_string().contains("不是 HiMind 管理内容"));

        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_round_trip_is_version_locked() {
        let _guard = test_env_lock();
        let root = std::env::temp_dir().join(format!("himind-expert-test-{}", std::process::id()));
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        store_test_definition(&test_definition(
            "com.himind.expert.software-engineer",
            "软件工程师",
            "测试工程师说明",
        ));
        let activated = activate("com.himind.expert.software-engineer", None, None).unwrap();
        assert_eq!(active().unwrap(), Some(activated.clone()));
        assert_eq!(
            active_definition().unwrap().unwrap().id,
            activated.expert_id
        );
        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_activation_does_not_cross_contaminate_projects() {
        let _guard = test_env_lock();
        let root =
            std::env::temp_dir().join(format!("himind-expert-workspace-{}", std::process::id()));
        let workspace_a = root.join("a");
        let workspace_b = root.join("b");
        let old = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        fs::create_dir_all(&workspace_a).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();

        store_test_definition(&test_definition(
            "com.himind.expert.software-engineer",
            "软件工程师",
            "软件工程师说明",
        ));
        store_test_definition(&test_definition(
            "com.himind.expert.senior-system-architect",
            "高级系统架构师",
            "系统架构师说明",
        ));

        let global = activate("com.himind.expert.software-engineer", None, None).unwrap();
        let scoped = activate(
            "com.himind.expert.senior-system-architect",
            None,
            Some(&workspace_a),
        )
        .unwrap();
        assert_eq!(
            active_for_workspace(Some(&workspace_a)).unwrap(),
            Some(scoped)
        );
        assert_eq!(
            active_for_workspace(Some(&workspace_b)).unwrap(),
            Some(global)
        );

        match old {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(root);
    }
}
