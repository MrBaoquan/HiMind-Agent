//! Governed, portable instruction packs.
//!
//! `AGENTS.md` and `CLAUDE.md` remain client-facing workspace files. This
//! module gives their reusable content a HiMind-owned lifecycle without
//! treating a workspace file as a Skill or as a permission source.

use crate::skill::manifest::{validate_relative_package_path, validate_skill_id};
use crate::VERSION;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const INSTRUCTION_PACK_SCHEMA_VERSION: &str = "instruction_pack.v1";
const MAX_CONTENT_BYTES: usize = 256 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionPackRef {
    pub id: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishedInstructionPackSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub digest: String,
    pub supported_clients: Vec<String>,
    pub scope: InstructionPackScope,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkspaceInstructionContext {
    pub workspace_root: String,
    pub selected: Vec<InstructionPackRef>,
    pub available: Vec<PublishedInstructionPackSummary>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstructionPackScope {
    Global,
    Project,
    Directory,
}

impl Default for InstructionPackScope {
    fn default() -> Self {
        Self::Project
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstructionPackManifest {
    pub schema_version: String,
    pub id: String,
    pub name: String,
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    pub description: String,
    pub release_notes: String,
    pub min_agent_version: String,
    pub supported_clients: Vec<String>,
    pub scope: InstructionPackScope,
    pub max_bytes: usize,
    #[serde(default)]
    pub skill_refs: Vec<String>,
    #[serde(default)]
    pub workflow_refs: Vec<String>,
    #[serde(default)]
    pub capability_refs: Vec<String>,
    pub contents: Vec<String>,
}

impl InstructionPackManifest {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != INSTRUCTION_PACK_SCHEMA_VERSION {
            return Err(format!(
                "unsupported instruction pack schema: {}",
                self.schema_version
            )
            .into());
        }
        validate_skill_id(&self.id)?;
        validate_version(&self.version)?;
        validate_version(&self.min_agent_version)?;
        if self.name.trim().is_empty() {
            return Err("instruction pack name is required".into());
        }
        if self.description.trim().is_empty() {
            return Err("instruction pack description is required".into());
        }
        if self.release_notes.trim().is_empty() {
            return Err("instruction pack release_notes is required".into());
        }
        if self.supported_clients.is_empty() {
            return Err("instruction pack supported_clients is required".into());
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_CONTENT_BYTES {
            return Err(format!(
                "instruction pack max_bytes must be between 1 and {MAX_CONTENT_BYTES}"
            )
            .into());
        }
        if !self
            .contents
            .iter()
            .any(|item| item.eq_ignore_ascii_case("INSTRUCTIONS.md"))
        {
            return Err("contents must include INSTRUCTIONS.md".into());
        }
        for path in &self.contents {
            validate_relative_package_path(path)?;
            if path == "instruction.json" || path == "checksums.sha256" {
                continue;
            }
        }
        for reference in self
            .skill_refs
            .iter()
            .chain(self.workflow_refs.iter())
            .chain(self.capability_refs.iter())
        {
            validate_skill_id(reference)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstructionPackDraftInput {
    pub id: String,
    pub name: String,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default = "default_agent_version")]
    pub min_agent_version: String,
    #[serde(default = "default_clients")]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub scope: InstructionPackScope,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    pub instructions: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// 工作区清单可以直接声明依赖；调用方留空时保持旧行为（无依赖）。
    #[serde(default)]
    pub skill_refs: Vec<String>,
    #[serde(default)]
    pub workflow_refs: Vec<String>,
    #[serde(default)]
    pub capability_refs: Vec<String>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstructionPackImportInput {
    pub package_path: PathBuf,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstructionPackDraft {
    pub manifest: InstructionPackManifest,
    pub instructions: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    #[serde(default)]
    pub source_path: Option<PathBuf>,
    /// Digest of the imported source file, when the draft came from a client file.
    /// This makes a later source edit explicit instead of silently publishing stale content.
    #[serde(default)]
    pub source_sha256: Option<String>,
    pub source: String,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub published_at: Option<String>,
    #[serde(default)]
    pub published_digest: Option<String>,
    #[serde(default)]
    pub test_report: Option<Value>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InstructionPackTestResult {
    pub draft: InstructionPackDraft,
    pub readiness: String,
    pub issues: Vec<String>,
    pub client_status: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishedInstructionPack {
    pub manifest: InstructionPackManifest,
    pub instructions: String,
    pub digest: String,
}

/// 内置起步规则：规则库第一次打开不能只有空态。
///
/// 这些内容是能直接用的真实规则，不是占位：用户可以预检、确认、发布到本机，
/// 也可以落地到工作区改成团队自己的版本。
fn builtin_starters() -> Vec<InstructionPackDraftInput> {
    let starter =
        |id: &str, name: &str, description: &str, instructions: &str| InstructionPackDraftInput {
            id: id.to_string(),
            name: name.to_string(),
            author: "HiMind".to_string(),
            categories: vec!["software-engineering".to_string()],
            version: "1.0.0".to_string(),
            description: description.to_string(),
            release_notes: "内置起步规则。".to_string(),
            min_agent_version: VERSION.to_string(),
            supported_clients: default_clients(),
            scope: InstructionPackScope::Project,
            max_bytes: DEFAULT_PACK_BYTES,
            instructions: instructions.to_string(),
            files: BTreeMap::new(),
            skill_refs: Vec::new(),
            workflow_refs: Vec::new(),
            capability_refs: Vec::new(),
            source: "builtin_starter".to_string(),
        };
    vec![
        starter(
            "com.himind.instruction.delivery-gate",
            "交付质量门禁",
            "提交或交付前的自检：先取证再下结论，验证不过不交付。",
            r#"# 交付质量门禁

- 动手前先复述目标、约束和验收标准；三者不清楚就先问，不靠猜。
- 每条结论都要能指到证据：命令输出、文件路径与行号、接口返回或截图。
- 改动完成后必须真的跑一次验证（构建、测试或手测），没跑过的不说"已完成"。
- 报告结果时区分"已验证""未验证""失败"，不把推测写成事实。
- 交付说明固定包含：改了什么、为什么、怎么验证、还有什么风险。
"#,
        ),
        starter(
            "com.himind.instruction.code-review",
            "代码评审规则",
            "评审的固定关注顺序和结论口径，避免只挑风格问题。",
            r#"# 代码评审规则

- 先看边界与契约：输入校验、错误路径、并发与幂等，再谈风格。
- 每个问题给出可执行的位置（文件:行）和触发条件，不接受"建议优化"这类空话。
- 区分阻断项与建议项：会导致数据错误、安全问题或线上故障的必须拦下。
- 检查测试是否真的覆盖了这次改动；没有测试要说明为什么可以没有。
- 结论落在"合并 / 修改后合并 / 不合并"三种明确状态上。
"#,
        ),
        starter(
            "com.himind.instruction.frontend-delivery",
            "前端交付规范",
            "信息层级、文案和控件选型的统一口径，含中文排版底线。",
            r#"# 前端交付规范

- 信息层级先于视觉：先定主次和路径，再决定颜色、阴影和留白。
- 文字不解释功能，不写"点击这里""此功能用于"这类元话术。
- 长中文名、长 ID、长路径必须验证不撑破容器，也不逐字折行。
- 交互控件按语义选型：开关用 toggle、模式用分段控件、视图用页签。
- 提交前在真实目标环境看一遍，不靠静态代码判断布局。
"#,
        ),
        starter(
            "com.himind.instruction.api-contract",
            "接口与错误处理约定",
            "接口契约、错误信息、超时重试和幂等的硬性要求。",
            r#"# 接口与错误处理约定

- 接口先定契约再写实现：字段名、类型、可空性、分页和错误码一次说清。
- 错误信息面向调用方：说明哪一步失败、期望什么、如何修正，不暴露内部路径。
- 外部调用必须有超时、重试上限和失败兜底，禁止无限等待。
- 写操作要么幂等，要么带幂等键；重复提交不能产生重复副作用。
- 变更接口时同步更新调用方、文档和回归用例，缺一项不算完成。
"#,
        ),
        starter(
            "com.himind.instruction.change-notes",
            "变更记录与文档同步",
            "让每次改动都能被未来的人（或 agent）复原判断。",
            r#"# 变更记录与文档同步

- 每次改动留下可追溯的记录：范围、影响面、验证方式、回滚办法。
- 文档与代码同一次改动更新，不把"稍后补文档"当成完成。
- 术语一旦确定就全仓库统一；换词要在同一次改动里改完。
- 废弃能力写清替代方案和下线时间，不留下无主的入口。
- 记录面向未来的自己：只看这段文字就能复原当时的判断。
"#,
        ),
    ]
}

fn starter_marker_path() -> PathBuf {
    drafts_root().join(".builtin-starters.v1")
}

/// 写入一次内置起步规则；标记文件存在时不再生成，用户删掉后也不会自动回来。
pub(crate) fn ensure_builtin_starters() -> Result<(), Box<dyn Error>> {
    let marker = starter_marker_path();
    if marker.is_file() {
        return Ok(());
    }
    for input in builtin_starters() {
        save(input)?;
    }
    fs::create_dir_all(drafts_root())?;
    fs::write(&marker, b"1")?;
    Ok(())
}

pub(crate) fn list() -> Result<Vec<InstructionPackDraft>, Box<dyn Error>> {
    ensure_builtin_starters()?;
    let root = drafts_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "draft.json" {
            if let Ok(draft) =
                serde_json::from_str::<InstructionPackDraft>(&fs::read_to_string(entry.path())?)
            {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(drafts)
}

pub(crate) fn save(
    input: InstructionPackDraftInput,
) -> Result<InstructionPackDraft, Box<dyn Error>> {
    if input.instructions.trim().is_empty() {
        return Err("INSTRUCTIONS.md 内容不能为空".into());
    }
    if input.instructions.len() > MAX_CONTENT_BYTES {
        return Err(format!("INSTRUCTIONS.md 不能超过 {MAX_CONTENT_BYTES} 字节").into());
    }
    if input.release_notes.trim().is_empty() {
        return Err("请填写本版本更新说明".into());
    }
    let mut contents = vec![
        "instruction.json".to_string(),
        "INSTRUCTIONS.md".to_string(),
    ];
    for path in input.files.keys() {
        validate_relative_package_path(path)?;
        if matches!(
            path.as_str(),
            "instruction.json" | "INSTRUCTIONS.md" | "checksums.sha256"
        ) || path.starts_with(".himind/")
        {
            return Err(format!("指令包附加文件使用了保留路径: {path}").into());
        }
        contents.push(path.clone());
    }
    contents.sort();
    contents.dedup();
    let manifest = InstructionPackManifest {
        schema_version: INSTRUCTION_PACK_SCHEMA_VERSION.to_string(),
        id: input.id.trim().to_string(),
        name: input.name.trim().to_string(),
        author: if input.author.trim().is_empty() {
            default_author()
        } else {
            input.author.trim().to_string()
        },
        categories: input.categories,
        version: input.version.trim().to_string(),
        description: input.description.trim().to_string(),
        release_notes: input.release_notes.trim().to_string(),
        min_agent_version: input.min_agent_version.trim().to_string(),
        supported_clients: normalize_clients(input.supported_clients),
        scope: input.scope,
        max_bytes: input.max_bytes,
        skill_refs: input.skill_refs,
        workflow_refs: input.workflow_refs,
        capability_refs: input.capability_refs,
        contents,
    };
    manifest.validate()?;
    let previous = read(&manifest.id, &manifest.version).ok();
    let root = draft_version_root(&manifest.id, &manifest.version);
    let package_root = root.join("package");
    if package_root.exists() {
        fs::remove_dir_all(&package_root)?;
    }
    fs::create_dir_all(&package_root)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(package_root.join("instruction.json"), &manifest_bytes)?;
    fs::write(
        package_root.join("INSTRUCTIONS.md"),
        input.instructions.as_bytes(),
    )?;
    for (path, content) in &input.files {
        let target = package_root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, content.as_bytes())?;
    }
    let mut package_files = BTreeMap::from([
        (
            "INSTRUCTIONS.md".to_string(),
            input.instructions.as_bytes().to_vec(),
        ),
        ("instruction.json".to_string(), manifest_bytes),
    ]);
    package_files.extend(
        input
            .files
            .iter()
            .map(|(path, content)| (path.clone(), content.as_bytes().to_vec())),
    );
    let checksums = package_checksums(&package_files);
    fs::write(package_root.join("checksums.sha256"), checksums.as_bytes())?;
    let candidate_path = root.join(format!(
        "{}-{}.hminstruction",
        manifest.id, manifest.version
    ));
    build_archive(&candidate_path, &package_files, checksums.as_bytes())?;
    let candidate_sha256 = sha256_file(&candidate_path)?;
    let unchanged = previous
        .as_ref()
        .is_some_and(|draft| draft.candidate_sha256 == candidate_sha256);
    let draft = InstructionPackDraft {
        manifest,
        instructions: input.instructions,
        files: input.files,
        candidate_path,
        candidate_sha256,
        source_path: None,
        source_sha256: None,
        source: if input.source.trim().is_empty() {
            "local_workspace".to_string()
        } else {
            input.source
        },
        tested_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|v| v.tested_at.clone()),
        confirmed_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|v| v.confirmed_at.clone()),
        published_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|v| v.published_at.clone()),
        published_digest: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|v| v.published_digest.clone()),
        test_report: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|v| v.test_report.clone()),
        updated_at: now_stamp(),
    };
    persist(&draft)?;
    Ok(draft)
}

/// 从扩展工作区目录构建候选包。
///
/// 目录约定与专家项目一致：清单在 `instruction.json`，正文在 `INSTRUCTIONS.md`，
/// 其余文件按清单 `contents` 声明原样进包。构建即写入本机规则库草稿，
/// 后续的预检、确认、发布仍走既有的草稿生命周期。
pub(crate) fn build_workspace_candidate(
    workspace: &Path,
) -> Result<InstructionPackDraft, Box<dyn Error>> {
    let manifest: InstructionPackManifest =
        serde_json::from_slice(&fs::read(workspace.join("instruction.json"))?)?;
    manifest.validate()?;
    let instructions = fs::read_to_string(workspace.join("INSTRUCTIONS.md"))?;
    if instructions.trim().is_empty() {
        return Err("INSTRUCTIONS.md 内容不能为空".into());
    }
    let mut files = BTreeMap::new();
    for path in &manifest.contents {
        let normalized = path.replace('\\', "/");
        if matches!(
            normalized.as_str(),
            "instruction.json" | "INSTRUCTIONS.md" | "checksums.sha256"
        ) {
            continue;
        }
        validate_relative_package_path(&normalized)?;
        if normalized.starts_with(".himind/") {
            return Err(format!("指令包附加文件使用了保留路径: {normalized}").into());
        }
        files.insert(
            normalized.clone(),
            fs::read_to_string(workspace.join(&normalized))?,
        );
    }
    save(InstructionPackDraftInput {
        id: manifest.id,
        name: manifest.name,
        author: manifest.author,
        categories: manifest.categories,
        version: manifest.version,
        description: manifest.description,
        release_notes: manifest.release_notes,
        min_agent_version: manifest.min_agent_version,
        supported_clients: manifest.supported_clients,
        scope: manifest.scope,
        max_bytes: manifest.max_bytes,
        instructions,
        files,
        skill_refs: manifest.skill_refs,
        workflow_refs: manifest.workflow_refs,
        capability_refs: manifest.capability_refs,
        source: "extension_workspace".to_string(),
    })
}

pub(crate) fn import_file(path: &Path) -> Result<InstructionPackDraft, Box<dyn Error>> {
    let path = path.canonicalize()?;
    if !path.is_file() {
        return Err("指令文件不是文件".into());
    }
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        file_name.as_str(),
        "agents.md" | "claude.md" | "agents.local.md" | "claude.local.md"
    ) {
        return Err("只支持导入 AGENTS.md、CLAUDE.md 及其 local 变体".into());
    }
    let instructions = fs::read_to_string(&path)?;
    let digest = format!("{:x}", Sha256::digest(instructions.as_bytes()));
    let id = format!("com.himind.instruction.imported-{}", &digest[..12]);
    let mut draft = save(InstructionPackDraftInput {
        id,
        name: format!(
            "导入的 {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
        author: default_author(),
        categories: vec!["imported".to_string()],
        version: "1.0.0".to_string(),
        description: "从客户端工作区指令导入的可治理指令包。".to_string(),
        release_notes: "首次从客户端指令文件导入。".to_string(),
        min_agent_version: VERSION.to_string(),
        supported_clients: default_clients(),
        scope: InstructionPackScope::Project,
        max_bytes: DEFAULT_PACK_BYTES,
        instructions,
        files: BTreeMap::new(),
        skill_refs: Vec::new(),
        workflow_refs: Vec::new(),
        capability_refs: Vec::new(),
        source: "imported_client_file".to_string(),
    })?;
    draft.source_path = Some(path);
    draft.source_sha256 = Some(digest);
    draft.source = "imported_client_file".to_string();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn import_package(
    input: InstructionPackImportInput,
) -> Result<InstructionPackDraft, Box<dyn Error>> {
    let source = input.package_path.canonicalize()?;
    if !source
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("hminstruction") || v.eq_ignore_ascii_case("zip"))
    {
        return Err("指令包必须使用 .hminstruction 或 .zip 扩展名".into());
    }
    if fs::metadata(&source)?.len() > MAX_ARCHIVE_BYTES {
        return Err("指令包超过 16 MiB".into());
    }
    let staging = drafts_root().join(format!(".import-{}", now_stamp()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<InstructionPackDraft, Box<dyn Error>> {
        crate::app::skill_manager::extract_archive(&source, &staging)?;
        let extracted_root = flatten_single_wrapper(&staging)?;
        let manifest: InstructionPackManifest = serde_json::from_str(&fs::read_to_string(
            extracted_root.join("instruction.json"),
        )?)?;
        manifest.validate()?;
        verify_package_integrity(&extracted_root, &manifest)?;
        let instructions = fs::read_to_string(extracted_root.join("INSTRUCTIONS.md"))?;
        if instructions.len() > MAX_CONTENT_BYTES {
            return Err("INSTRUCTIONS.md 超出大小限制".into());
        }
        let mut files = BTreeMap::new();
        for path in &manifest.contents {
            if matches!(path.as_str(), "instruction.json" | "INSTRUCTIONS.md") {
                continue;
            }
            let value = extracted_root.join(path);
            if value.is_file() {
                files.insert(path.clone(), fs::read_to_string(value)?);
            }
        }
        let draft_root = draft_version_root(&manifest.id, &manifest.version);
        fs::create_dir_all(&draft_root)?;
        let package_root = draft_root.join("package");
        if package_root.exists() {
            fs::remove_dir_all(&package_root)?;
        }
        fs::create_dir_all(&package_root)?;
        let candidate_path = draft_root.join(format!(
            "{}-{}.hminstruction",
            manifest.id, manifest.version
        ));
        fs::copy(&source, &candidate_path)?;
        let draft = InstructionPackDraft {
            manifest,
            instructions,
            files,
            candidate_path: candidate_path.clone(),
            candidate_sha256: sha256_file(&candidate_path)?,
            source_path: Some(source.clone()),
            source_sha256: None,
            source: if input.source.trim().is_empty() {
                "local_package".to_string()
            } else {
                input.source
            },
            tested_at: None,
            confirmed_at: None,
            published_at: None,
            published_digest: None,
            test_report: None,
            updated_at: now_stamp(),
        };
        fs::copy(
            extracted_root.join("instruction.json"),
            package_root.join("instruction.json"),
        )?;
        fs::copy(
            extracted_root.join("INSTRUCTIONS.md"),
            package_root.join("INSTRUCTIONS.md"),
        )?;
        for path in &draft.manifest.contents {
            if matches!(path.as_str(), "instruction.json" | "INSTRUCTIONS.md") {
                continue;
            }
            if extracted_root.join(path).is_file() {
                let target = package_root.join(path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(extracted_root.join(path), target)?;
            }
        }
        fs::copy(
            extracted_root.join("checksums.sha256"),
            package_root.join("checksums.sha256"),
        )?;
        persist(&draft)?;
        Ok(draft)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}

pub(crate) fn read(id: &str, version: &str) -> Result<InstructionPackDraft, Box<dyn Error>> {
    validate_skill_id(id)?;
    validate_version(version)?;
    Ok(serde_json::from_str(&fs::read_to_string(
        draft_version_root(id, version).join("draft.json"),
    )?)?)
}

pub(crate) fn read_published(
    id: &str,
    version: &str,
) -> Result<PublishedInstructionPack, Box<dyn Error>> {
    validate_skill_id(id)?;
    validate_version(version)?;
    let root = store_version_root(id, version);
    let manifest: InstructionPackManifest =
        serde_json::from_str(&fs::read_to_string(root.join("instruction.json"))?)?;
    manifest.validate()?;
    verify_package_integrity(&root, &manifest)?;
    let instructions = fs::read_to_string(root.join("INSTRUCTIONS.md"))?;
    if instructions.len() > manifest.max_bytes {
        return Err("已发布指令包超过 manifest.max_bytes".into());
    }
    let digest = format!("sha256:{}", sha256_directory(&root, &manifest)?);
    let expected = fs::read_to_string(root.join("published.sha256"))
        .map_err(|_| "已发布指令包缺少发布摘要")?;
    if !digest.eq_ignore_ascii_case(expected.trim()) {
        return Err("已发布指令包摘要不匹配".into());
    }
    Ok(PublishedInstructionPack {
        manifest,
        instructions,
        digest,
    })
}

pub(crate) fn workspace_context(
    workspace: &Path,
) -> Result<WorkspaceInstructionContext, Box<dyn Error>> {
    let workspace_root = workspace.canonicalize()?;
    if !workspace_root.is_dir() {
        return Err(format!("工作区不是目录: {}", workspace.display()).into());
    }
    let selected = load_workspace_selection(&workspace_root)?;
    let mut available = Vec::new();
    for draft in list()? {
        let Some(published_at) = draft.published_at.as_ref() else {
            continue;
        };
        if published_at.trim().is_empty() {
            continue;
        }
        let Ok(published) = read_published(&draft.manifest.id, &draft.manifest.version) else {
            continue;
        };
        available.push(PublishedInstructionPackSummary {
            id: published.manifest.id,
            name: published.manifest.name,
            version: published.manifest.version,
            description: published.manifest.description,
            digest: published.digest,
            supported_clients: published.manifest.supported_clients,
            scope: published.manifest.scope,
        });
    }
    available.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.version.cmp(&right.version))
    });
    Ok(WorkspaceInstructionContext {
        workspace_root: workspace_root.to_string_lossy().to_string(),
        selected,
        available,
    })
}

pub(crate) fn load_workspace_selection(
    workspace: &Path,
) -> Result<Vec<InstructionPackRef>, Box<dyn Error>> {
    let workspace_root = workspace.canonicalize()?;
    let path = instruction_selection_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let store = serde_json::from_slice::<InstructionSelectionStore>(&fs::read(&path)?)?;
    let mut selected = store
        .workspaces
        .get(&workspace_root.to_string_lossy().to_string())
        .cloned()
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    selected.retain(|reference| seen.insert(format!("{}@{}", reference.id, reference.version)));
    for reference in &selected {
        let published = read_published(&reference.id, &reference.version)?;
        if !reference.digest.is_empty() && !reference.digest.eq_ignore_ascii_case(&published.digest)
        {
            return Err(format!(
                "工作区选择的指令包摘要已变化，请重新选择: {} v{}",
                reference.id, reference.version
            )
            .into());
        }
    }
    Ok(selected)
}

pub(crate) fn selected_overlays(
    workspace: &Path,
) -> Result<
    (
        Vec<InstructionPackRef>,
        Vec<crate::workspace_instructions::InstructionOverlay>,
    ),
    Box<dyn Error>,
> {
    let selected = load_workspace_selection(workspace)?;
    let mut overlays = Vec::with_capacity(selected.len());
    for reference in &selected {
        let published = read_published(&reference.id, &reference.version)?;
        overlays.push(crate::workspace_instructions::InstructionOverlay {
            id: published.manifest.id,
            version: published.manifest.version,
            digest: published.digest,
            content: published.instructions,
        });
    }
    Ok((selected, overlays))
}

pub(crate) fn save_workspace_selection(
    workspace: &Path,
    selected: Vec<InstructionPackRef>,
) -> Result<WorkspaceInstructionContext, Box<dyn Error>> {
    let workspace_root = workspace.canonicalize()?;
    let mut validated = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for reference in selected {
        validate_skill_id(&reference.id)?;
        validate_version(&reference.version)?;
        let published = read_published(&reference.id, &reference.version)?;
        if !reference.digest.is_empty() && !reference.digest.eq_ignore_ascii_case(&published.digest)
        {
            return Err(
                format!("指令包摘要不匹配: {} v{}", reference.id, reference.version).into(),
            );
        }
        let key = format!("{}@{}", reference.id, reference.version);
        if seen.insert(key) {
            validated.push(InstructionPackRef {
                id: published.manifest.id,
                version: published.manifest.version,
                digest: published.digest,
            });
        }
    }
    let path = instruction_selection_path();
    let mut store = if path.is_file() {
        serde_json::from_slice::<InstructionSelectionStore>(&fs::read(&path)?)?
    } else {
        InstructionSelectionStore::default()
    };
    store
        .workspaces
        .insert(workspace_root.to_string_lossy().to_string(), validated);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _lock = crate::store::atomic_file::lock(&path)?;
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(&store)?)?;
    workspace_context(&workspace_root)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InstructionSelectionStore {
    schema_version: String,
    #[serde(default)]
    workspaces: BTreeMap<String, Vec<InstructionPackRef>>,
}

fn instruction_selection_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join("data")
        .join("workspace-instruction-packs.json")
}

pub(crate) fn test(id: &str, version: &str) -> Result<InstructionPackTestResult, Box<dyn Error>> {
    let mut draft = read(id, version)?;
    ensure_candidate_unchanged(&draft)?;
    draft.manifest.validate()?;
    let mut issues = Vec::new();
    if draft.instructions.trim().is_empty() {
        issues.push("INSTRUCTIONS.md 内容为空".to_string());
    }
    if draft.instructions.len() > draft.manifest.max_bytes {
        issues.push("内容超过 manifest.max_bytes".to_string());
    }
    if draft
        .manifest
        .supported_clients
        .iter()
        .any(|client| client.trim().is_empty())
    {
        issues.push("存在空客户端目标".to_string());
    }
    let client_status = draft
        .manifest
        .supported_clients
        .iter()
        .map(|client| (client.clone(), client_status(client)))
        .collect::<BTreeMap<_, _>>();
    if client_status.values().any(|status| status == "unsupported") {
        issues.push("存在当前未支持的客户端目标".to_string());
    }
    let passed = issues.is_empty();
    draft.tested_at = passed.then_some(now_stamp());
    draft.confirmed_at = None;
    draft.test_report = Some(
        serde_json::json!({"manifest":"passed","content":if passed {"passed"} else {"failed"},"clients":client_status,"issues":issues,"candidate_sha256":draft.candidate_sha256,"agent_version":VERSION,"tested_at":draft.tested_at}),
    );
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(InstructionPackTestResult {
        draft,
        readiness: if passed {
            "ready".to_string()
        } else {
            "blocked".to_string()
        },
        issues,
        client_status,
    })
}

pub(crate) fn confirm(id: &str, version: &str) -> Result<InstructionPackDraft, Box<dyn Error>> {
    let mut draft = read(id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先完成指令包本地预检".into());
    }
    draft.confirmed_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn publish_local(
    id: &str,
    version: &str,
) -> Result<InstructionPackDraft, Box<dyn Error>> {
    let mut draft = read(id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.confirmed_at.is_none() {
        return Err("请先确认指令包候选版本".into());
    }
    let package_root = draft_version_root(id, version).join("package");
    let target = store_version_root(id, version);
    if target.exists() {
        return Err("该指令包版本已经发布；已发布版本不可覆盖，请创建新的版本号".into());
    }
    fs::create_dir_all(&target)?;
    copy_tree(&package_root, &target)?;
    let published_digest = format!("sha256:{}", sha256_directory(&target, &draft.manifest)?);
    fs::write(target.join("published.sha256"), published_digest.as_bytes())?;
    draft.published_at = Some(now_stamp());
    draft.published_digest = Some(published_digest);
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

fn ensure_candidate_unchanged(draft: &InstructionPackDraft) -> Result<(), Box<dyn Error>> {
    if sha256_file(&draft.candidate_path)? != draft.candidate_sha256 {
        return Err("指令包候选制品已变化，请重新保存并测试".into());
    }
    if let (Some(path), Some(expected)) = (&draft.source_path, &draft.source_sha256) {
        let actual = sha256_file(path).map_err(|_| "导入来源文件已不存在，请重新导入")?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("导入来源文件已变化，请重新导入并测试".into());
        }
    }
    Ok(())
}

fn verify_package_integrity(
    root: &Path,
    manifest: &InstructionPackManifest,
) -> Result<(), Box<dyn Error>> {
    let checksums = fs::read_to_string(root.join("checksums.sha256"))
        .map_err(|_| "指令包缺少 checksums.sha256")?;
    let expected = crate::skill::manifest::parse_checksums(&checksums)?;
    let declared = manifest
        .contents
        .iter()
        .map(|value| value.replace('\\', "/"))
        .collect::<std::collections::HashSet<_>>();
    let mut actual = std::collections::HashSet::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if matches!(relative.as_str(), "checksums.sha256" | "published.sha256") {
            continue;
        }
        if !declared.contains(&relative) {
            return Err(format!("指令包包含 Manifest 未声明的文件: {relative}").into());
        }
        let digest = format!("{:x}", Sha256::digest(fs::read(entry.path())?));
        if expected.get(&relative) != Some(&digest) {
            return Err(format!("指令包文件摘要不匹配: {relative}").into());
        }
        actual.insert(relative);
    }
    for path in &declared {
        if !actual.contains(path) {
            return Err(format!("指令包缺少 Manifest 声明的文件: {path}").into());
        }
        if !expected.contains_key(path) {
            return Err(format!("checksums.sha256 未声明文件: {path}").into());
        }
    }
    if expected.len() != actual.len() {
        return Err("checksums.sha256 包含未打包文件".into());
    }
    Ok(())
}

fn persist(draft: &InstructionPackDraft) -> Result<(), Box<dyn Error>> {
    let root = draft_version_root(&draft.manifest.id, &draft.manifest.version);
    fs::create_dir_all(&root)?;
    fs::write(root.join("draft.json"), serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}

fn flatten_single_wrapper(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if root.join("instruction.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let mut directories = fs::read_dir(root)?
        .flatten()
        .filter(|entry| entry.path().is_dir());
    if let Some(entry) = directories.next() {
        if directories.next().is_none() && entry.path().join("instruction.json").is_file() {
            return Ok(entry.path());
        }
    }
    Err("指令包根目录必须包含 instruction.json".into())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn build_archive(
    target: &Path,
    files: &BTreeMap<String, Vec<u8>>,
    checksums: &[u8],
) -> Result<(), Box<dyn Error>> {
    let file = File::create(target)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    for (name, content) in files
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_slice()))
        .chain(std::iter::once(("checksums.sha256", checksums)))
    {
        writer.start_file(name, options)?;
        writer.write_all(content)?;
    }
    writer.finish()?;
    Ok(())
}

fn package_checksums(files: &BTreeMap<String, Vec<u8>>) -> String {
    files
        .iter()
        .map(|(name, content)| format!("{:x}  {name}\n", Sha256::digest(content)))
        .collect()
}
fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn sha256_directory(
    root: &Path,
    manifest: &InstructionPackManifest,
) -> Result<String, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    for path in &manifest.contents {
        let relative = path.replace('\\', "/");
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(fs::read(root.join(path))?);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn drafts_root() -> PathBuf {
    env::var_os("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::paths::agent_home().join("instruction-pack-drafts"))
}
fn store_root() -> PathBuf {
    env::var_os("HIMIND_INSTRUCTION_PACK_STORE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::paths::agent_home().join("instruction-packs"))
}
fn draft_version_root(id: &str, version: &str) -> PathBuf {
    drafts_root().join(id).join(version)
}
fn store_version_root(id: &str, version: &str) -> PathBuf {
    store_root().join(id).join("versions").join(version)
}
fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}
fn validate_version(value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return Err(format!("指令包版本无效: {value}").into());
    }
    Ok(())
}
fn normalize_clients(values: Vec<String>) -> Vec<String> {
    let mut values = values
        .into_iter()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}
fn client_status(client: &str) -> String {
    match client {
        "himind-dsh" | "codex" | "claude-code" | "github-copilot" | "workbuddy" => {
            "supported".to_string()
        }
        _ => "unsupported".to_string(),
    }
}
fn default_author() -> String {
    "未授权用户".to_string()
}
fn default_agent_version() -> String {
    VERSION.to_string()
}
fn default_clients() -> Vec<String> {
    vec!["himind-dsh".to_string(), "codex".to_string()]
}
fn default_max_bytes() -> usize {
    65_536
}
const DEFAULT_PACK_BYTES: usize = 65_536;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;

    fn input(instructions: &str) -> InstructionPackDraftInput {
        InstructionPackDraftInput {
            id: "com.himind.instruction.test".to_string(),
            name: "Test instructions".to_string(),
            author: "tester".to_string(),
            categories: vec!["test".to_string()],
            version: "1.0.0".to_string(),
            description: "test pack".to_string(),
            release_notes: "initial".to_string(),
            min_agent_version: VERSION.to_string(),
            supported_clients: default_clients(),
            scope: InstructionPackScope::Project,
            max_bytes: DEFAULT_PACK_BYTES,
            instructions: instructions.to_string(),
            files: BTreeMap::new(),
            skill_refs: Vec::new(),
            workflow_refs: Vec::new(),
            capability_refs: Vec::new(),
            source: "test".to_string(),
        }
    }

    #[test]
    fn save_test_confirm_publish_round_trip() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!(
            "himind-instruction-pack-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        env::set_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR", root.join("drafts"));
        env::set_var("HIMIND_INSTRUCTION_PACK_STORE_DIR", root.join("store"));
        let draft = save(input("# Rules\n- verify output\n")).unwrap();
        assert!(draft.candidate_path.is_file());
        let tested = test(&draft.manifest.id, &draft.manifest.version).unwrap();
        assert_eq!(tested.readiness, "ready");
        let confirmed = confirm(&draft.manifest.id, &draft.manifest.version).unwrap();
        let published = publish_local(&confirmed.manifest.id, &confirmed.manifest.version).unwrap();
        assert!(published.published_at.is_some());
        assert!(store_version_root(&published.manifest.id, &published.manifest.version).is_dir());
        let loaded = read_published(&published.manifest.id, &published.manifest.version).unwrap();
        assert_eq!(loaded.instructions, "# Rules\n- verify output\n");
        assert_eq!(loaded.digest, published.published_digest.unwrap());
        let nested_metadata =
            store_version_root(&published.manifest.id, &published.manifest.version)
                .join("references")
                .join("published.sha256");
        fs::create_dir_all(nested_metadata.parent().unwrap()).unwrap();
        fs::write(&nested_metadata, "unexpected").unwrap();
        assert!(read_published(&published.manifest.id, &published.manifest.version).is_err());
        env::remove_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR");
        env::remove_var("HIMIND_INSTRUCTION_PACK_STORE_DIR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imported_file_is_deterministically_identified() {
        let _guard = test_env_lock();
        let root =
            env::temp_dir().join(format!("himind-instruction-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("AGENTS.md");
        fs::write(&path, "# Project rules\n").unwrap();
        env::set_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR", root.join("drafts"));
        let draft = import_file(&path).unwrap();
        assert_eq!(draft.source, "imported_client_file");
        assert!(draft.source_path.is_some());
        env::remove_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imported_source_change_blocks_publish() {
        let _guard = test_env_lock();
        let root = env::temp_dir().join(format!(
            "himind-instruction-source-change-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("AGENTS.md");
        fs::write(&path, "# Original\n").unwrap();
        env::set_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR", root.join("drafts"));
        env::set_var("HIMIND_INSTRUCTION_PACK_STORE_DIR", root.join("store"));
        let draft = import_file(&path).unwrap();
        let tested = test(&draft.manifest.id, &draft.manifest.version).unwrap();
        assert_eq!(tested.readiness, "ready");
        fs::write(&path, "# Changed\n").unwrap();
        let confirmed = confirm(&draft.manifest.id, &draft.manifest.version);
        assert!(confirmed.is_err());
        env::remove_var("HIMIND_INSTRUCTION_PACK_DRAFTS_DIR");
        env::remove_var("HIMIND_INSTRUCTION_PACK_STORE_DIR");
        let _ = fs::remove_dir_all(root);
    }
}
