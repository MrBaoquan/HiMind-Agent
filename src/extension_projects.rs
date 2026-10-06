use crate::capability::plugin::{parse_plugin_manifest, PluginManifest};
use crate::extension_contracts::{
    clamp_distribution_targets, declared_distribution_targets, default_distribution_targets,
    distribution_targets_allow, distribution_targets_are_subset, normalize_distribution_targets,
    DistributionTarget,
};
use crate::skill::manifest::load_skill_manifest;
use crate::skill::types::SkillManifest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEVELOPMENT_TOOLS_PLUGIN_ID: &str = "com.himind.extension-development-tools";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionProjectKind {
    Plugin,
    Skill,
    Workflow,
    Expert,
    Instruction,
}

impl ExtensionProjectKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::Expert => "expert",
            Self::Instruction => "instruction",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value.trim() {
            "plugin" => Ok(Self::Plugin),
            "skill" => Ok(Self::Skill),
            "workflow" => Ok(Self::Workflow),
            "expert" => Ok(Self::Expert),
            "instruction" => Ok(Self::Instruction),
            other => Err(format!(
                "扩展类型必须是 plugin、skill、workflow、expert 或 instruction，收到: {other}"
            )
            .into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProjectRecord {
    id: String,
    kind: ExtensionProjectKind,
    extension_id: String,
    name: String,
    description: String,
    version: String,
    workspace_path: PathBuf,
    /// 工作区标识：扩展身份回答"这是哪个扩展"，工作区标识回答"在哪份源码里"。
    /// 同一个扩展 ID 出现在两个工作区时靠它区分成两条登记，谁都不覆盖谁。
    #[serde(default)]
    workspace_key: String,
    source: String,
    #[serde(default)]
    source_repository: String,
    #[serde(default)]
    source_default_branch: String,
    #[serde(default)]
    source_subdirectory: String,
    #[serde(default)]
    source_commit: String,
    /// 项目级分发目标覆盖。`None` 表示继承所属分发单元的默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    distribution_targets: Option<Vec<DistributionTarget>>,
    updated_at: String,
    /// 由扩展源实时派生的分发单元键，不落盘。
    #[serde(skip)]
    source_unit_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtensionProject {
    pub id: String,
    pub kind: ExtensionProjectKind,
    pub extension_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// UTF-8 display path for the Tauri/JSON boundary. Keep the internal
    /// registry as PathBuf so filesystem operations remain lossless.
    pub workspace_path: String,
    pub workspace_available: bool,
    pub source: String,
    pub source_repository: String,
    pub source_default_branch: String,
    pub source_subdirectory: String,
    pub source_commit: String,
    /// 生效的分发目标（项目覆盖 → 分发单元默认 → 仅工作台）。
    #[serde(default)]
    pub distribution_targets: Vec<DistributionTarget>,
    /// 清单里作者声明的分发落点，即本机设置的上限；空表示清单未声明。
    #[serde(default)]
    pub distribution_targets_declared: Vec<DistributionTarget>,
    /// 生效目标的来源：`project` / `unit` / `manifest` / `default`。
    #[serde(default)]
    pub distribution_targets_source: String,
    pub updated_at: String,
    /// 所属扩展分发单元键，由扩展源实时派生，不落盘。
    #[serde(default)]
    pub source_unit_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExtensionProjectSourceInput {
    pub source_repository: String,
    #[serde(default)]
    pub source_default_branch: String,
    #[serde(default)]
    pub source_subdirectory: String,
    #[serde(default)]
    pub source_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AssociateExtensionProjectInput {
    pub kind: ExtensionProjectKind,
    pub extension_id: String,
    #[serde(flatten)]
    pub source: ExtensionProjectSourceInput,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExtensionSubmissionSource {
    pub source_repository: String,
    pub source_default_branch: String,
    pub source_subdirectory: String,
    pub source_commit: String,
    pub distribution_id: String,
    pub channel: String,
    pub catalog_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateExtensionProjectInput {
    pub kind: ExtensionProjectKind,
    pub slug: String,
    #[serde(default)]
    pub extension_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "draft", rename_all = "snake_case")]
pub(crate) enum ExtensionCandidate {
    Plugin(crate::plugin_authoring::PluginDraft),
    Skill(crate::skill::authoring::AuthoringDraft),
    Workflow(crate::workflow::WorkflowDraft),
    Expert(crate::expert::ExpertAuthoringDraft),
    Instruction(crate::instruction_pack::InstructionPackDraft),
}

pub(crate) fn list() -> Result<Vec<ExtensionProject>, Box<dyn Error>> {
    let sources = crate::app::extension_source::authoritative_local_sources();
    let workspaces: Vec<_> = sources
        .iter()
        .flat_map(|snapshot| snapshot.workspaces.iter().cloned())
        .collect();
    let ((), mut records) = with_registry_mut(|records| {
        let mut changed = migrate_legacy_projects(records, &workspaces);
        changed |= merge_shared_workspace_projects(records);
        changed |= rebind_extension_source_workspaces(records, &workspaces);
        changed |= reconcile_source_declared_projects(records, &sources, &workspaces);
        for record in records.iter_mut() {
            if !record.workspace_path.is_dir() {
                continue;
            }
            if let Ok(current) = project_record_from_path(&record.workspace_path, &record.source) {
                if record.extension_id == current.extension_id && record.kind == current.kind {
                    if record.name != current.name
                        || record.description != current.description
                        || record.version != current.version
                    {
                        record.name = current.name;
                        record.description = current.description;
                        record.version = current.version;
                        record.updated_at = current.updated_at;
                        changed = true;
                    }
                }
            }
            changed |= ensure_source_commit(record);
        }
        Ok(((), changed))
    })?;

    let settings = crate::app::extension_source::settings().unwrap_or_default();
    for record in &mut records {
        let candidates: Vec<_> = workspaces
            .iter()
            .filter(|item| {
                item.kind == record.kind.as_str() && item.extension_id == record.extension_id
            })
            .collect();
        record.source_unit_key = candidates
            .iter()
            .find(|item| workspace_matches(item.path.as_path(), &record.workspace_path))
            .or_else(|| candidates.first())
            .map(|item| item.unit_key.clone())
            .unwrap_or_default();
    }
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(records
        .iter()
        .map(|record| project_view_with(record, &settings))
        .collect())
}

/// 项目登记与源工作区是否指同一个目录。
fn workspace_matches(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Reconciles the selected aggregate Git workspace into the current Agent profile.
/// The source directory is shared, while this registry (and all drafts/candidates)
/// remains profile-local. Existing manually registered projects are preserved.
fn merge_shared_workspace_projects(records: &mut Vec<ProjectRecord>) -> bool {
    let mut changed = false;
    if !crate::extension_workspace::settings().valid {
        return false;
    }
    let discovered_items = crate::extension_workspace::discover();
    // 扩展身份而不是登记 id：同名扩展在多个工作区登记时，第二条的 id 会带
    // `@<工作区摘要>` 后缀，拿 id 去比对会把共享清单里的登记误判成"清单已经不要了"，
    // 于是在每次刷新里删掉又加回来。
    let discovered_ids: std::collections::HashSet<String> = discovered_items
        .iter()
        .map(|item| format!("{}:{}", item.kind, item.id))
        .collect();
    let before = records.len();
    records.retain(|record| {
        record.source != "git_workspace" || discovered_ids.contains(&record_identity(record))
    });
    changed |= records.len() != before;
    for discovered in discovered_items {
        let Ok(mut candidate) = project_record_from_path(&discovered.path, "git_workspace") else {
            continue;
        };
        if candidate.extension_id != discovered.id {
            continue;
        }
        candidate.source_repository = discovered.source_repository.clone();
        candidate.source_default_branch = discovered.source_default_branch.clone();
        candidate.source_subdirectory = discovered.source_subdirectory.clone();
        let identity = record_identity(&candidate);
        let workspace_key = candidate.workspace_key.clone();
        // 先找同一工作区的那条登记；找不到才回落到共享清单自己名下的登记（源码目录
        // 搬了位置时把路径同步过来）。开发者手动打开的工作区永远不会被这条覆盖，
        // 因为它的 source 不是 git_workspace —— 同名扩展因此可以并存两条登记。
        let index = records
            .iter()
            .position(|record| {
                record_identity(record) == identity && record.workspace_key == workspace_key
            })
            .or_else(|| {
                records.iter().position(|record| {
                    record_identity(record) == identity && record.source == "git_workspace"
                })
            });
        let Some(existing) = index.map(|index| &mut records[index]) else {
            records.push(candidate);
            changed = true;
            continue;
        };
        // Do not replace a developer's explicitly opened workspace. Once a record
        // came from the shared catalog, keep its path synchronized with the catalog.
        if existing.source == "git_workspace" || existing.workspace_path == candidate.workspace_path
        {
            if existing.kind != candidate.kind
                || existing.extension_id != candidate.extension_id
                || existing.name != candidate.name
                || existing.description != candidate.description
                || existing.version != candidate.version
                || existing.workspace_path != candidate.workspace_path
                || existing.source != "git_workspace"
                || existing.source_repository != candidate.source_repository
                || existing.source_default_branch != candidate.source_default_branch
                || existing.source_subdirectory != candidate.source_subdirectory
            {
                let source_commit = existing.source_commit.clone();
                *existing = candidate;
                existing.source_commit = source_commit;
                changed = true;
            }
        }
    }
    changed
}

pub(crate) fn get(project_id: &str) -> Result<ExtensionProject, Box<dyn Error>> {
    let ((), records) = with_registry_mut(|records| {
        let index = records
            .iter()
            .position(|record| record.id == project_id)
            .ok_or("扩展项目不存在")?;
        // 单项目刷新走这条路径，必须和列表页用同一套提交号兜底逻辑，否则列表和
        // 详情会给出互相矛盾的"可提交"判断。
        let changed = ensure_source_commit(&mut records[index]);
        Ok(((), changed))
    })?;
    let record = records
        .iter()
        .find(|record| record.id == project_id)
        .ok_or("扩展项目不存在")?;
    Ok(project_view(record))
}

/// 返回本次调用应该使用的扩展工作区。
///
/// `requested` 是调用方按次传入的 `workspace_root`：同一个 Agent 进程会同时服务
/// 多个 HiMind AI 工作区会话，只有按次传入的值才代表"现在这个会话在哪"。
pub(crate) fn current_workspace(requested: Option<&Value>) -> Result<Value, Box<dyn Error>> {
    let requested = requested.and_then(Value::as_str);
    let (workspace, source, bound) = crate::extension_workspace::resolve_root(requested)?;
    let project = project_record_from_path(&workspace, "ai_workspace")
        .ok()
        .map(ExtensionProject::from);
    let kind = project
        .as_ref()
        .map(|item| item.kind.as_str())
        .unwrap_or_else(|| crate::extension_workspace::classify_path(&workspace));
    Ok(json!({
        "workspace_root": crate::extension_workspace::display_path(&workspace),
        "source": source,
        "bound": bound,
        "kind": kind,
        "project": project,
    }))
}

pub(crate) fn current_workspace_path() -> Result<PathBuf, Box<dyn Error>> {
    crate::extension_workspace::session_root().ok_or_else(|| "无法确定当前会话的工作区".into())
}

pub(crate) fn register(path: &Path) -> Result<ExtensionProject, Box<dyn Error>> {
    register_in(&registry_path(), path)
}

/// 登记一个扩展工作区。同一扩展 ID 出现在多个目录时并列成多条登记（`assign_record_ids`
/// 会给后来者加 `@<工作区摘要>` 后缀），先登记的那条继续占用规范 id。
fn register_in(registry: &Path, path: &Path) -> Result<ExtensionProject, Box<dyn Error>> {
    let canonical = path.canonicalize()?;
    let mut record = project_record_from_path(&canonical, "local_workspace")?;
    let canonical_id = canonical_project_id(record.kind, &record.extension_id);
    let workspace_key = record.workspace_key.clone();
    let ((), records) = with_registry_mut_at(registry, |records| {
        // 只继承「同一个目录」上一次记录的源码出处：提交号是某个工作树的具体状态，
        // 从别的工作区继承过来会给出错误的溯源信息。
        if let Some(existing) = records.iter().find(|item| {
            canonical_project_id(item.kind, &item.extension_id) == canonical_id
                && (item.workspace_key == workspace_key
                    || workspace_key_of(&item.workspace_path) == workspace_key)
        }) {
            record.source_repository = existing.source_repository.clone();
            record.source_default_branch = existing.source_default_branch.clone();
            record.source_subdirectory = existing.source_subdirectory.clone();
            record.source_commit = existing.source_commit.clone();
        }
        // 同一个目录再打开一次：替换那条登记。不同目录打开同一个扩展：并列成
        // 两条登记，谁也不覆盖谁 —— 但指向已不存在目录的旧登记让位给这次选择。
        records.retain(|item| {
            if canonical_project_id(item.kind, &item.extension_id) != canonical_id {
                return true;
            }
            item.workspace_key != workspace_key && item.workspace_path.is_dir()
        });
        records.push(record.clone());
        Ok(((), true))
    })?;
    let record = records
        .iter()
        .find(|item| {
            canonical_project_id(item.kind, &item.extension_id) == canonical_id
                && item.workspace_key == workspace_key
        })
        .ok_or("扩展项目不存在")?;
    Ok(project_view(record))
}

pub(crate) fn associate(
    path: &Path,
    input: AssociateExtensionProjectInput,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let canonical = path.canonicalize()?;
    let detected = project_record_from_path(&canonical, "local_workspace")?;
    if detected.kind != input.kind || detected.extension_id != input.extension_id.trim() {
        return Err("所选目录与协作项目不匹配".into());
    }
    let project = register(&canonical)?;
    update_source(&project.id, input.source)
}

pub(crate) fn update_source(
    project_id: &str,
    input: ExtensionProjectSourceInput,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let ((), records) = with_registry_mut(|records| {
        let record = records
            .iter_mut()
            .find(|record| record.id == project_id)
            .ok_or("扩展项目不存在")?;
        record.source_repository = input.source_repository.trim().to_string();
        record.source_default_branch = input.source_default_branch.trim().to_string();
        record.source_subdirectory = input.source_subdirectory.trim().replace('\\', "/");
        record.source_commit = input.source_commit.trim().to_string();
        record.updated_at = now_stamp();
        Ok(((), true))
    })?;
    let record = records
        .iter()
        .find(|record| record.id == project_id)
        .ok_or("扩展项目不存在")?;
    Ok(project_view(record))
}

/// 设置扩展项目的分发目标覆盖。
///
/// 传 `None` 表示清除覆盖、回到「分发单元默认 → 仅工作台」的继承链；
/// 传空数组会被拒绝，避免出现「哪也不发」的不可判定状态。
///
/// 选择越出清单声明时直接拒绝：让越界在写入前暴露，而不是发布时才发现被裁掉，
/// 也避免本机记录与制品声明长期不一致。
pub(crate) fn set_distribution_targets(
    kind: ExtensionProjectKind,
    extension_id: &str,
    targets: Option<&[DistributionTarget]>,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let normalized = match targets {
        Some(targets) => Some(normalize_distribution_targets(targets)?),
        None => None,
    };
    let ((id, ()), records) = with_registry_mut(|records| {
        let id = record_id_for(records, kind, extension_id).ok_or("扩展项目不存在")?;
        let record = records
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or("扩展项目不存在")?;
        if let Some(requested) = normalized.as_ref() {
            let declared = read_declared_distribution_targets(record);
            if !distribution_targets_are_subset(requested, &declared) {
                return Err(format!(
                    "扩展清单声明的分发落点为 [{}]，不能再选择 [{}]。请先修改清单里的 distribution_targets。",
                    declared
                        .iter()
                        .map(|target| target.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    requested
                        .iter()
                        .map(|target| target.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into());
            }
        }
        record.distribution_targets = normalized;
        record.updated_at = now_stamp();
        Ok(((id, ()), true))
    })?;
    let record = records
        .iter()
        .find(|record| record.id == id)
        .ok_or("扩展项目不存在")?;
    Ok(project_view(record))
}

/// 解析某个扩展制品当前生效的分发目标。
///
/// 没有项目记录的草稿（例如刚创建的候选）按默认目标处理，保证既有行为不变。
pub(crate) fn effective_distribution_targets(
    kind: ExtensionProjectKind,
    extension_id: &str,
) -> Vec<DistributionTarget> {
    let Ok(records) = read_records(&registry_path()) else {
        return default_distribution_targets();
    };
    let Some(id) = record_id_for(&records, kind, extension_id) else {
        return default_distribution_targets();
    };
    let Some(record) = records.iter().find(|record| record.id == id) else {
        return default_distribution_targets();
    };
    project_view(record).distribution_targets
}

/// 读取项目所属分发单元的默认分发目标，用于 UI 说明「继承值是什么」。
pub(crate) fn unit_distribution_targets_for(
    kind: ExtensionProjectKind,
    extension_id: &str,
) -> Vec<DistributionTarget> {
    let Ok(records) = read_records(&registry_path()) else {
        return default_distribution_targets();
    };
    let Some(id) = record_id_for(&records, kind, extension_id) else {
        return default_distribution_targets();
    };
    let Some(record) = records.iter().find(|record| record.id == id) else {
        return default_distribution_targets();
    };
    let settings = crate::app::extension_source::settings().unwrap_or_default();
    crate::app::extension_source::unit_distribution_targets(&settings, &derive_unit_key(record))
}

/// 发布前的分发目标门禁：目标集合不包含指定落点时立即阻断。
///
/// 门禁放在发布入口而不是各发布器内部，保证 UI、CLI、MCP 三条入口行为一致。
pub(crate) fn ensure_distribution_target(
    kind: ExtensionProjectKind,
    extension_id: &str,
    target: DistributionTarget,
) -> Result<(), Box<dyn Error>> {
    let targets = effective_distribution_targets(kind, extension_id);
    if distribution_targets_allow(&targets, target) {
        return Ok(());
    }
    let current = targets
        .iter()
        .map(|item| item.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "扩展 {extension_id} 的分发目标不包含 {}，当前目标为 [{current}]。请在扩展开发工作区调整分发目标后重试。",
        target.as_str()
    )
    .into())
}

pub(crate) fn submission_source(
    kind: ExtensionProjectKind,
    extension_id: &str,
) -> Result<ExtensionSubmissionSource, Box<dyn Error>> {
    let records = read_records(&registry_path())?;
    let Some(id) = record_id_for(&records, kind, extension_id) else {
        return Ok(ExtensionSubmissionSource::default());
    };
    let Some(record) = records.into_iter().find(|record| record.id == id) else {
        return Ok(ExtensionSubmissionSource::default());
    };
    let unit_key = if !record.source_unit_key.trim().is_empty() {
        record.source_unit_key.clone()
    } else {
        crate::app::extension_source::local_source_workspaces()
            .into_iter()
            .find(|workspace| {
                workspace.kind == kind.as_str()
                    && workspace.extension_id == extension_id.trim()
                    && workspace.repository == record.source_repository
                    && workspace.subdirectory == record.source_subdirectory
            })
            .map(|workspace| workspace.unit_key)
            .unwrap_or_default()
    };
    let (distribution_id, channel, catalog_id) =
        parse_distribution_unit_key(&unit_key, &record.source_repository);
    Ok(ExtensionSubmissionSource {
        // 工作区允许填 `owner/repo` 简写（界面就是这么显示的），但控制面
        // 校验的是完整仓库 URL；提交前统一补齐，否则会得到 422。
        source_repository: normalize_repository_url(&record.source_repository),
        source_default_branch: record.source_default_branch,
        source_subdirectory: record.source_subdirectory,
        // 控制面要求“仓库提交号”必填，而工作区记录里常常是空的。提交时
        // 直接用工作区 git 的当前提交兜底，避免走到提审才 422。
        source_commit: resolve_source_commit(
            &record.workspace_path.to_string_lossy(),
            &record.source_commit,
        ),
        distribution_id,
        channel,
        catalog_id,
    })
}

/// 已记录的提交号优先；为空时用工作区 git 的当前提交兜底（提交后不再变化，
/// 保证“候选制品 ↔ 源码提交”这层溯源成立）。
fn resolve_source_commit(workspace_path: &str, recorded: &str) -> String {
    let recorded = recorded.trim();
    if !recorded.is_empty() {
        return recorded.to_string();
    }
    let workspace = workspace_path.trim();
    if workspace.is_empty() {
        return String::new();
    }
    let path = workspace.strip_prefix(r"\\?\").unwrap_or(workspace);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

/// 把 `owner/repo` 简写补成 GitHub HTTPS 地址；已经是 URL 或 SSH 形式时原样返回。
fn normalize_repository_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains("://") || trimmed.starts_with("git@") {
        return trimmed.to_string();
    }
    let parts = trimmed.split('/').count();
    if parts == 2 && !trimmed.contains(' ') {
        return format!("https://github.com/{trimmed}");
    }
    trimmed.to_string()
}

fn parse_distribution_unit_key(unit_key: &str, repository: &str) -> (String, String, String) {
    let mut parts = unit_key.splitn(3, '#');
    if let (Some(distribution_id), Some(channel), Some(catalog_id)) =
        (parts.next(), parts.next(), parts.next())
    {
        if !distribution_id.trim().is_empty()
            && !channel.trim().is_empty()
            && !catalog_id.trim().is_empty()
        {
            return (
                distribution_id.to_ascii_lowercase(),
                channel.to_ascii_lowercase(),
                catalog_id.to_ascii_lowercase(),
            );
        }
    }
    let distribution_id = repository
        .trim()
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    (distribution_id, "stable".to_string(), "public".to_string())
}

pub(crate) fn create(
    parent: &Path,
    input: CreateExtensionProjectInput,
    author: &str,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let parent = parent.canonicalize()?;
    let slug = normalize_slug(&input.slug)?;
    let category = if input.category.trim().is_empty() {
        "software-engineering"
    } else {
        input.category.trim()
    };
    let release_notes = "创建初始版本。";
    let result = match input.kind {
        ExtensionProjectKind::Plugin => invoke_development_tool(
            "extension.plugin.scaffold",
            json!({
                "workspace_root": parent,
                "output_dir": parent,
                "name": slug,
                "display_name": input.name.trim(),
                "description": input.description.trim(),
                "author": author.trim(),
                "categories": [category],
                "release_notes": release_notes,
                "template": if input.template.trim().is_empty() { "readonly-tool" } else { input.template.trim() },
            }),
        )?,
        ExtensionProjectKind::Skill => invoke_development_tool(
            "extension.skill.scaffold",
            json!({
                "workspace_root": parent,
                "output_dir": parent,
                "slug": slug,
                "id": input.extension_id.trim(),
                "name": input.name.trim(),
                "version": "0.1.0",
                "min_agent_version": crate::VERSION,
                "description": input.description.trim(),
                "author": author.trim(),
                "categories": [category],
                "release_notes": release_notes,
                "supported_clients": ["agent-skills"],
            }),
        )?,
        ExtensionProjectKind::Workflow => {
            let output_dir = parent.join("workflows");
            match invoke_development_tool(
                "extension.workflow.scaffold",
                json!({
                    "workspace_root": parent,
                    "output_dir": output_dir,
                    "slug": slug,
                    "id": input.extension_id.trim(),
                    "name": input.name.trim(),
                    "version": "0.1.0",
                    "min_agent_version": crate::VERSION,
                    "description": input.description.trim(),
                    "author": author.trim(),
                    "categories": [category],
                    "release_notes": release_notes,
                    "template": if input.template.trim().is_empty() { "strict" } else { input.template.trim() },
                }),
            ) {
                Ok(result) => result,
                Err(error) => {
                    let message = error.to_string();
                    let tool_unavailable = message.contains("请先安装扩展开发工具")
                        || message.contains("扩展开发工具当前不可用")
                        || message.contains("缺少能力: extension.workflow.scaffold");
                    if !tool_unavailable {
                        return Err(error);
                    }
                    create_workflow_skeleton(parent.as_path(), &slug, &input)?
                }
            }
        }
        ExtensionProjectKind::Expert => create_expert_skeleton(parent.as_path(), &slug, &input, author)?,
        ExtensionProjectKind::Instruction => {
            create_instruction_skeleton(parent.as_path(), &slug, &input, author)?
        }
    };
    let root = result
        .get("root")
        .and_then(Value::as_str)
        .ok_or("扩展开发工具未返回项目目录")?;
    register(Path::new(root))
}

fn create_expert_skeleton(
    parent: &Path,
    slug: &str,
    input: &CreateExtensionProjectInput,
    author: &str,
) -> Result<Value, Box<dyn Error>> {
    let root = parent.join("experts").join(slug);
    if root.exists() {
        return Err(format!("专家项目目录已存在: {}", root.display()).into());
    }
    let id = if input.extension_id.trim().is_empty() {
        format!("com.himind.expert.{slug}")
    } else {
        input.extension_id.trim().to_string()
    };
    let definition = json!({
        "schema_version": crate::expert::EXPERT_SCHEMA_VERSION,
        "id": id,
        "name": input.name.trim(),
        "author": author.trim(),
        "categories": [if input.category.trim().is_empty() { "software-engineering" } else { input.category.trim() }],
        "version": "0.1.0",
        "release_notes": "创建初始版本。",
        "min_agent_version": crate::VERSION,
        "description": input.description.trim(),
        "supported_clients": ["portable", "himind-dsh", "codex", "github-copilot", "claude-code", "cursor", "windsurf", "cline"],
        "skill_refs": [],
        "workflow_refs": [],
        "capability_refs": [],
        "contents": ["EXPERT.md"],
        "instructions": "先理解任务目标与约束，再按阶段推进并验证结果。",
        "output_contract": { "required_sections": ["结论", "下一步"] },
        "harness": { "behavior_phases": ["plan", "execute", "verify", "deliver"], "required_evidence": ["summary", "next_steps"], "recovery_guidance": ["遇到不确定性时先说明并请求补充信息"] }
    });
    fs::create_dir_all(&root)?;
    fs::write(root.join("expert.json"), serde_json::to_vec_pretty(&definition)?)?;
    fs::write(root.join("EXPERT.md"), "先理解任务目标与约束，再按阶段推进并验证结果。")?;
    fs::write(root.join("README.md"), format!("# {}\n\n{}\n\n专家项目由 expert.json 与 EXPERT.md 组成。\n", input.name.trim(), input.description.trim()))?;
    Ok(json!({ "root": root.to_string_lossy(), "expert_id": definition["id"], "version": "0.1.0" }))
}

/// 取某个项目规则在本机规则库里的最新草稿版本。
fn latest_instruction_draft(
    id: &str,
) -> Result<crate::instruction_pack::InstructionPackDraft, Box<dyn Error>> {
    let mut candidates = crate::instruction_pack::list()?
        .into_iter()
        .filter(|draft| draft.manifest.id == id.trim())
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        crate::skill::resolver::compare_versions(&left.manifest.version, &right.manifest.version)
    });
    candidates
        .pop()
        .ok_or_else(|| format!("未找到项目规则: {id}").into())
}

/// 把本机规则库里的一个项目规则落地成可编辑的工作区项目（`rules/<slug>/`）。
///
/// 写的是当前草稿版本的快照：清单、正文和附加文件都按原样落盘，
/// 之后改的是这个工作区项目，发布动作仍回到规则库。
pub(crate) fn materialize_instruction_project(
    parent: &Path,
    instruction_pack_id: &str,
    version: Option<&str>,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let parent = parent.canonicalize()?;
    let draft = match version.map(str::trim).filter(|value| !value.is_empty()) {
        Some(version) => crate::instruction_pack::read(instruction_pack_id, version)?,
        None => latest_instruction_draft(instruction_pack_id)?,
    };
    let slug = project_slug_from_id(&draft.manifest.id);
    let root = parent.join("rules").join(&slug);
    if root.exists() {
        return Err(format!("项目规则项目目录已存在: {}", root.display()).into());
    }
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("instruction.json"),
        serde_json::to_vec_pretty(&draft.manifest)?,
    )?;
    fs::write(root.join("INSTRUCTIONS.md"), &draft.instructions)?;
    for (path, content) in &draft.files {
        let target = root.join(path);
        if let Some(directory) = target.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::write(target, content.as_bytes())?;
    }
    fs::write(
        root.join("README.md"),
        format!(
            "# {}\n\n{}\n\n项目规则项目由 instruction.json 与 INSTRUCTIONS.md 组成。\n",
            draft.manifest.name, draft.manifest.description
        ),
    )?;
    register(&root)
}

/// 项目规则项目：清单在 `instruction.json`，正文在 `INSTRUCTIONS.md`。
///
/// 目录放在 `rules/<slug>`，与插件、技能、工作流、专家共用同一份工作区登记与构建链路。
fn create_instruction_skeleton(
    parent: &Path,
    slug: &str,
    input: &CreateExtensionProjectInput,
    author: &str,
) -> Result<Value, Box<dyn Error>> {
    let root = parent.join("rules").join(slug);
    if root.exists() {
        return Err(format!("项目规则项目目录已存在: {}", root.display()).into());
    }
    let id = if input.extension_id.trim().is_empty() {
        format!("com.himind.instruction.{slug}")
    } else {
        input.extension_id.trim().to_string()
    };
    let instructions = "# 工作规则\n\n先确认目标与约束，再按步骤推进，并在完成前说明验证方式。\n";
    let manifest = json!({
        "schema_version": crate::instruction_pack::INSTRUCTION_PACK_SCHEMA_VERSION,
        "id": id,
        "name": input.name.trim(),
        "author": author.trim(),
        "categories": [if input.category.trim().is_empty() { "software-engineering" } else { input.category.trim() }],
        "version": "0.1.0",
        "description": input.description.trim(),
        "release_notes": "创建初始版本。",
        "min_agent_version": crate::VERSION,
        "supported_clients": ["codex", "claude-code", "github-copilot"],
        "scope": "project",
        "max_bytes": 65536,
        "skill_refs": [],
        "workflow_refs": [],
        "capability_refs": [],
        "contents": ["instruction.json", "INSTRUCTIONS.md"]
    });
    fs::create_dir_all(&root)?;
    fs::write(root.join("instruction.json"), serde_json::to_vec_pretty(&manifest)?)?;
    fs::write(root.join("INSTRUCTIONS.md"), instructions)?;
    fs::write(
        root.join("README.md"),
        format!(
            "# {}\n\n{}\n\n项目规则项目由 instruction.json 与 INSTRUCTIONS.md 组成。\n",
            input.name.trim(),
            input.description.trim()
        ),
    )?;
    Ok(json!({ "root": root.to_string_lossy(), "instruction_pack_id": manifest["id"], "version": "0.1.0" }))
}

fn create_workflow_skeleton(
    parent: &Path,
    slug: &str,
    input: &CreateExtensionProjectInput,
) -> Result<Value, Box<dyn Error>> {
    let root = parent.join("workflows").join(slug);
    if root.exists() {
        return Err(format!("Workflow 项目目录已存在: {}", root.display()).into());
    }
    let workflow_id = if input.extension_id.trim().is_empty() {
        format!("com.himind.workflow.{slug}")
    } else {
        input.extension_id.trim().to_string()
    };
    let package = json!({
        "schema_version": "workflow_package.v1",
        "id": workflow_id,
        "version": "0.1.0",
        "name": input.name.trim(),
        "description": input.description.trim(),
        "release_notes": "创建初始版本。",
        "min_agent_version": crate::VERSION,
        "local_requirements": {},
        "optional_providers": [],
        "capabilities": [],
        "dependencies": {
            "skills": [],
            "plugins": [],
            "connectors": [],
            "runtimes": []
        },
        "steps": [{
            "id": "START",
            "title": "开始",
            "kind": "manual",
            "execution_mode": "sync",
            "risk_level": "read_only",
            "depends_on": []
        }],
        "artifacts": [],
        "ui": {
            "mode": "declarative",
            "entry": "ui/workflow-view.json",
            "surfaces": ["agent"]
        },
        "supported_runtimes": []
    });
    for directory in [
        "artifacts",
        "connectors",
        "examples",
        "schemas",
        "tests/contract",
        "ui",
    ] {
        fs::create_dir_all(root.join(directory))?;
    }
    fs::write(
        root.join("workflow.json"),
        serde_json::to_vec_pretty(&package)?,
    )?;
    fs::write(
        root.join("ui/workflow-view.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": "workflow_view.v1",
            "title": input.name.trim(),
            "sections": [],
            "actions": ["start"]
        }))?,
    )?;
    fs::write(
        root.join("README.md"),
        format!(
            "# {}\n\n{}\n\nWorkflow 源码使用 `workflow.json` 作为执行契约，`ui/workflow-view.json` 作为声明式 UI。\n",
            input.name.trim(),
            input.description.trim()
        ),
    )?;
    fs::write(root.join("examples/input.json"), b"{}\n")?;
    fs::write(
        root.join("tests/contract/README.md"),
        b"# Workflow Contract Tests\n\nPlace contract fixtures and negative cases here.\n",
    )?;
    Ok(json!({
        "root": root.to_string_lossy(),
        "workflow_id": workflow_id,
        "version": "0.1.0"
    }))
}

pub(crate) fn build(project_id: &str) -> Result<ExtensionCandidate, Box<dyn Error>> {
    let project = find_record(project_id)?;
    let workspace = project.workspace_path.canonicalize()?;
    // The commit is provenance metadata for Dashboard submissions; developers do not need to manage it.
    if !project.source_repository.trim().is_empty() {
        if let Some(commit) = git_head(&workspace) {
            let _ = update_source_commit(project_id, &commit);
        }
    }
    cleanup_temporary_candidates(&workspace);
    if project.kind == ExtensionProjectKind::Workflow {
        return Ok(ExtensionCandidate::Workflow(
            crate::workflow::save_authoring_candidate(&workspace)?,
        ));
    }
    if project.kind == ExtensionProjectKind::Expert {
        return Ok(ExtensionCandidate::Expert(crate::expert::build_workspace_candidate(&workspace)?));
    }
    if project.kind == ExtensionProjectKind::Instruction {
        return Ok(ExtensionCandidate::Instruction(
            crate::instruction_pack::build_workspace_candidate(&workspace)?,
        ));
    }
    let extension = match project.kind {
        ExtensionProjectKind::Plugin => "hmpkg",
        ExtensionProjectKind::Skill => "hmskill",
        ExtensionProjectKind::Workflow
        | ExtensionProjectKind::Expert
        | ExtensionProjectKind::Instruction => unreachable!("handled above"),
    };
    let temporary = workspace.join(format!(".himind-candidate-{}.{}", now_stamp(), extension));
    let result = (|| -> Result<ExtensionCandidate, Box<dyn Error>> {
        if project.kind == ExtensionProjectKind::Plugin {
            invoke_development_tool(
                "extension.plugin.build",
                json!({"workspace_root": workspace, "path": workspace}),
            )?;
        }
        invoke_development_tool(
            match project.kind {
                ExtensionProjectKind::Plugin => "extension.plugin.package",
                ExtensionProjectKind::Skill => "extension.skill.package",
                ExtensionProjectKind::Workflow
                | ExtensionProjectKind::Expert
                | ExtensionProjectKind::Instruction => unreachable!("handled above"),
            },
            json!({"workspace_root": workspace, "path": workspace, "output": temporary}),
        )?;
        match project.kind {
            ExtensionProjectKind::Plugin => {
                let draft =
                    crate::plugin_authoring::save(crate::plugin_authoring::PluginDraftInput {
                        package_path: temporary.clone(),
                        revision_of_version: None,
                        parent_submission_id: None,
                    })?;
                Ok(ExtensionCandidate::Plugin(
                    crate::plugin_authoring::associate_workspace(draft, &workspace)?,
                ))
            }
            ExtensionProjectKind::Skill => {
                let draft = crate::skill::authoring::import_package(
                    crate::skill::authoring::SkillPackageInput {
                        package_path: temporary.clone(),
                        revision_of_version: None,
                        parent_submission_id: None,
                    },
                )?;
                Ok(ExtensionCandidate::Skill(
                    crate::skill::authoring::associate_workspace(draft, &workspace)?,
                ))
            }
            ExtensionProjectKind::Workflow => {
                unreachable!("workflow candidate returned above")
            }
            ExtensionProjectKind::Expert => unreachable!("handled above"),
            ExtensionProjectKind::Instruction => unreachable!("handled above"),
        }
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn update_source_commit(project_id: &str, commit: &str) -> Result<(), Box<dyn Error>> {
    with_registry_mut(|records| {
        let Some(record) = records.iter_mut().find(|record| record.id == project_id) else {
            return Ok(((), false));
        };
        if record.source_commit == commit {
            return Ok(((), false));
        }
        record.source_commit = commit.to_string();
        record.updated_at = now_stamp();
        Ok(((), true))
    })?;
    Ok(())
}

/// 已声明源码仓库、但记录里还没有提交号时，用工作区当前 git HEAD 补齐。
///
/// 界面判断"能否提交审核"看的就是这个提交号，而提交动作自己会用 git HEAD 兜底，
/// 两边口径不一致时开发者会看到"未能读取代码版本"却仍然提交成功，排障方向被误导。
/// 只补不覆盖：提交号是"候选制品 ↔ 源码"的溯源锚点，一经记录就保持稳定，否则
/// 开发者切分支会悄悄改写已提交制品的出处。
fn ensure_source_commit(record: &mut ProjectRecord) -> bool {
    if record.source_repository.trim().is_empty() || !record.source_commit.trim().is_empty() {
        return false;
    }
    let Some(commit) = git_head(&record.workspace_path) else {
        return false;
    };
    record.source_commit = commit;
    true
}

fn git_head(workspace: &Path) -> Option<String> {
    let output = crate::runtime::process::hidden_command("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

pub(crate) fn remove(project_id: &str) -> Result<(), Box<dyn Error>> {
    with_registry_mut(|records| {
        let before = records.len();
        records.retain(|record| record.id != project_id);
        if records.len() == before {
            return Err("扩展项目不存在".into());
        }
        Ok(((), true))
    })?;
    Ok(())
}

fn find_record(project_id: &str) -> Result<ProjectRecord, Box<dyn Error>> {
    read_records(&registry_path())?
        .into_iter()
        .find(|record| record.id == project_id)
        .ok_or_else(|| "扩展项目不存在".into())
}

fn invoke_development_tool(capability_id: &str, input: Value) -> Result<Value, Box<dyn Error>> {
    let plugin = crate::capability::plugin::find_plugin(DEVELOPMENT_TOOLS_PLUGIN_ID)?
        .ok_or("请先安装扩展开发工具")?;
    if !plugin.enabled || plugin.circuit_open {
        return Err("扩展开发工具当前不可用".into());
    }
    if !plugin
        .capabilities
        .iter()
        .any(|capability| capability.id == capability_id)
    {
        return Err(format!("扩展开发工具缺少能力: {capability_id}").into());
    }
    crate::capability::plugin::invoke_plugin_capability_for_plugin(
        DEVELOPMENT_TOOLS_PLUGIN_ID,
        capability_id,
        input,
        None,
    )
    .map_err(|error| friendly_tool_error(&error.to_string()).into())
}

fn friendly_tool_error(error: &str) -> String {
    let detail = error
        .find('{')
        .and_then(|index| serde_json::from_str::<Value>(&error[index..]).ok())
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| error.to_string());
    match detail.as_str() {
        "skill categories is required" => "请先在 skill.json 中选择功能分类".to_string(),
        "release_notes is required" => "请先填写本版本更新说明".to_string(),
        "author is required; use the current Agent authorized user" => {
            "请先在扩展清单中填写作者".to_string()
        }
        _ => detail,
    }
}

fn project_record_from_path(path: &Path, source: &str) -> Result<ProjectRecord, Box<dyn Error>> {
    let plugin_path = path.join("plugin.json");
    let skill_path = path.join("skill.json");
    let workflow_path = path.join("workflow.json");
    let expert_path = path.join("expert.json");
    let instruction_path = path.join("instruction.json");
    let marker_count = [
        plugin_path.is_file(),
        skill_path.is_file() || path.join("SKILL.md").is_file(),
        workflow_path.is_file(),
        expert_path.is_file() && path.join("EXPERT.md").is_file(),
        instruction_path.is_file() && path.join("INSTRUCTIONS.md").is_file(),
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    if marker_count > 1 {
        return Err("项目目录不能同时包含多种扩展 Manifest".into());
    }
    if plugin_path.is_file() {
        let manifest = parse_plugin_manifest(&fs::read_to_string(plugin_path)?)?;
        validate_plugin_identity(&manifest)?;
        return Ok(record(
            ExtensionProjectKind::Plugin,
            manifest.id,
            manifest.name,
            manifest.description,
            manifest.version,
            path,
            source,
        ));
    }
    if skill_path.is_file() || path.join("SKILL.md").is_file() {
        let manifest = load_skill_manifest(path)?;
        return Ok(skill_record(manifest, path, source));
    }
    if workflow_path.is_file() {
        let package = crate::workflow::load_from_directory(path)?;
        return Ok(record(
            ExtensionProjectKind::Workflow,
            package.id,
            package.name,
            package.description,
            package.version,
            path,
            source,
        ));
    }
    if expert_path.is_file() && path.join("EXPERT.md").is_file() {
        let definition: crate::expert::ExpertDefinition = serde_json::from_slice(&fs::read(expert_path)?)?;
        crate::expert::validate_definition(&definition)?;
        return Ok(record(ExtensionProjectKind::Expert, definition.id, definition.name, definition.description, definition.version, path, source));
    }
    if instruction_path.is_file() && path.join("INSTRUCTIONS.md").is_file() {
        let manifest: crate::instruction_pack::InstructionPackManifest =
            serde_json::from_slice(&fs::read(instruction_path)?)?;
        manifest.validate()?;
        return Ok(record(
            ExtensionProjectKind::Instruction,
            manifest.id,
            manifest.name,
            manifest.description,
            manifest.version,
            path,
            source,
        ));
    }
    Err("所选目录不是 HiMind 插件、Skill、Workflow、专家或项目规则项目".into())
}

fn skill_record(manifest: SkillManifest, path: &Path, source: &str) -> ProjectRecord {
    record(
        ExtensionProjectKind::Skill,
        manifest.id,
        manifest.name,
        manifest.description,
        manifest.version,
        path,
        source,
    )
}

fn record(
    kind: ExtensionProjectKind,
    extension_id: String,
    name: String,
    description: String,
    version: String,
    path: &Path,
    source: &str,
) -> ProjectRecord {
    ProjectRecord {
        id: format!("{}:{extension_id}", kind.as_str()),
        kind,
        extension_id,
        name,
        description,
        version,
        workspace_path: path.to_path_buf(),
        workspace_key: workspace_key_of(path),
        source: source.to_string(),
        source_repository: String::new(),
        source_default_branch: String::new(),
        source_subdirectory: String::new(),
        source_commit: String::new(),
        distribution_targets: None,
        updated_at: now_stamp(),
        source_unit_key: String::new(),
    }
}

fn validate_plugin_identity(manifest: &PluginManifest) -> Result<(), Box<dyn Error>> {
    if manifest.id.trim().is_empty()
        || manifest.id.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err("plugin.json 中的插件 ID 无效".into());
    }
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err("plugin.json 缺少名称或版本".into());
    }
    Ok(())
}

/// 从扩展 ID 派生工作区目录名：取最后一段并归一成小写连字符。
fn project_slug_from_id(id: &str) -> String {
    let tail = id.rsplit('.').next().unwrap_or(id);
    let mut slug = String::new();
    for ch in tail.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "expert".to_string() } else { slug }
}

/// 把专家库里的一个专家落地成可编辑的工作区项目（`experts/<slug>/`）。
///
/// 落地写的是当前定义的快照：内置专家也能落地，之后改的是这个工作区项目，
/// 不会回写专家库，避免把内置角色悄悄改成团队资产却没有版本记录。
pub(crate) fn materialize_expert_project(
    parent: &Path,
    expert_id: &str,
    version: Option<&str>,
) -> Result<ExtensionProject, Box<dyn Error>> {
    let parent = parent.canonicalize()?;
    let definition = crate::expert::get(expert_id, version)?;
    let slug = project_slug_from_id(&definition.id);
    let root = parent.join("experts").join(&slug);
    if root.exists() {
        return Err(format!("专家项目目录已存在: {}", root.display()).into());
    }
    fs::create_dir_all(&root)?;
    fs::write(root.join("expert.json"), serde_json::to_vec_pretty(&definition)?)?;
    // 工作区校验要求 EXPERT.md 与 expert.json 的 instructions 完全一致。
    fs::write(root.join("EXPERT.md"), &definition.instructions)?;
    fs::write(
        root.join("README.md"),
        format!(
            "# {}\n\n{}\n\n专家项目由 expert.json 与 EXPERT.md 组成。\n",
            definition.name, definition.description
        ),
    )?;
    register(&root)
}

fn normalize_slug(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.starts_with('-')
        || value.ends_with('-')
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-')
    {
        return Err("项目标识只能使用小写字母、数字和连字符".into());
    }
    Ok(value)
}

fn migrate_legacy_projects(
    records: &mut Vec<ProjectRecord>,
    workspaces: &[crate::app::extension_source::LocalSourceWorkspace],
) -> bool {
    let mut changed = false;
    for draft in crate::plugin_authoring::list().unwrap_or_default() {
        if records.iter().any(|record| {
            record.kind == ExtensionProjectKind::Plugin && record.extension_id == draft.manifest.id
        }) {
            continue;
        }
        let Some(record) = draft_project_record(
            ExtensionProjectKind::Plugin,
            &draft.manifest.id,
            draft.workspace_path.as_deref(),
            workspaces,
        ) else {
            continue;
        };
        records.push(record);
        changed = true;
    }
    for draft in crate::skill::authoring::list().unwrap_or_default() {
        if records.iter().any(|record| {
            record.kind == ExtensionProjectKind::Skill && record.extension_id == draft.manifest.id
        }) {
            continue;
        }
        let Some(record) = draft_project_record(
            ExtensionProjectKind::Skill,
            &draft.manifest.id,
            draft.workspace_path.as_deref(),
            workspaces,
        ) else {
            continue;
        };
        records.push(record);
        changed = true;
    }
    for draft in crate::workflow::list_authoring_drafts().unwrap_or_default() {
        if records.iter().any(|record| {
            record.kind == ExtensionProjectKind::Workflow && record.extension_id == draft.package_id
        }) {
            continue;
        }
        let Some(record) = draft_project_record(
            ExtensionProjectKind::Workflow,
            &draft.package_id,
            Some(&draft.source_root),
            workspaces,
        ) else {
            continue;
        };
        records.push(record);
        changed = true;
    }
    changed
}

/// 只接受真正的扩展源码工作区：优先用扩展源 `extensions.json` 声明的目录，
/// 其次用候选包所在目录，绝不绑定 Agent 内部草稿目录。
fn draft_project_record(
    kind: ExtensionProjectKind,
    extension_id: &str,
    workspace: Option<&Path>,
    workspaces: &[crate::app::extension_source::LocalSourceWorkspace],
) -> Option<ProjectRecord> {
    let kind_name = kind.as_str();
    if let Some(discovered) = workspaces
        .iter()
        .find(|item| item.kind == kind_name && item.extension_id == extension_id)
    {
        if let Ok(mut record) = project_record_from_path(&discovered.path, "extension_source") {
            record.source_repository = discovered.repository.clone();
            record.source_subdirectory = discovered.subdirectory.clone();
            return Some(record);
        }
    }
    let workspace = workspace.filter(|path| !is_agent_managed(path))?;
    let record = project_record_from_path(workspace, "candidate_workspace").ok()?;
    (record.kind == kind && record.extension_id == extension_id).then_some(record)
}

/// 把绑定到 Agent 内部草稿目录或已失效目录的项目记录重新绑回扩展源声明的源码工作区，
/// 并把历史遗留的来源标记归一为 `extension_source`。这是 `legacy_candidate` 误绑的修复通道，
/// 保证「用 AI 开发」始终改到源码。
fn rebind_extension_source_workspaces(
    records: &mut Vec<ProjectRecord>,
    workspaces: &[crate::app::extension_source::LocalSourceWorkspace],
) -> bool {
    if workspaces.is_empty() {
        return false;
    }
    let mut changed = false;
    for record in records.iter_mut() {
        let Some(discovered) = workspaces.iter().find(|item| {
            item.kind == record.kind.as_str() && item.extension_id == record.extension_id
        }) else {
            continue;
        };
        let bound = record.workspace_path.is_dir()
            && !is_agent_managed(&record.workspace_path)
            && project_record_from_path(&record.workspace_path, &record.source).is_ok();
        if bound && record.workspace_path != discovered.path {
            continue;
        }
        let Ok(candidate) = project_record_from_path(&discovered.path, "extension_source") else {
            continue;
        };
        if !bound {
            record.workspace_path = candidate.workspace_path;
        }
        if record.source == "extension_source"
            && record.source_repository == discovered.repository
            && record.source_subdirectory == discovered.subdirectory
        {
            continue;
        }
        record.source = "extension_source".to_string();
        record.source_repository = discovered.repository.clone();
        record.source_subdirectory = discovered.subdirectory.clone();
        record.updated_at = now_stamp();
        changed = true;
    }
    changed
}

fn is_agent_managed(path: &Path) -> bool {
    crate::extension_workspace::is_agent_managed_path(path)
}

/// 让项目登记与扩展源清单对齐。
///
/// 扩展源清单是「有哪些扩展」的唯一权威：扩展改名或删除后，只按扩展 ID 匹配的
/// 重绑定逻辑无法把它认出来，旧登记就会以「目录不可用」长期留在开发页，甚至继续
/// 挂着一份早已失效的测试制品。这里按源归属做一次对账：
///
/// - 清单里存在、登记里没有的扩展补登记；
/// - 归属某个可读源、但该源清单里已经没有的登记移除。
///
/// 只有清单可读的源才有删除资格；源目录不可读时保留登记，等到下次刷新再判，
/// 避免一次拔盘就丢掉开发者的项目绑定。
fn reconcile_source_declared_projects(
    records: &mut Vec<ProjectRecord>,
    sources: &[crate::app::extension_source::LocalSourceSnapshot],
    workspaces: &[crate::app::extension_source::LocalSourceWorkspace],
) -> bool {
    if sources.is_empty() {
        return false;
    }
    let mut changed = false;
    let before = records.len();
    records.retain(|record| {
        if record.source != "extension_source" {
            return true;
        }
        let Some(owner) = sources
            .iter()
            .find(|snapshot| snapshot.owns(&record.source_repository, &record.workspace_path))
        else {
            return true;
        };
        owner.workspaces.iter().any(|workspace| {
            workspace.kind == record.kind.as_str() && workspace.extension_id == record.extension_id
        })
    });
    changed |= records.len() != before;

    for workspace in workspaces {
        if records.iter().any(|record| {
            record.kind.as_str() == workspace.kind && record.extension_id == workspace.extension_id
        }) {
            continue;
        }
        let Ok(candidate) = project_record_from_path(&workspace.path, "extension_source") else {
            continue;
        };
        // 目录与清单声明不符时宁可不登记，也不要把别的扩展登记成这一条。
        if candidate.kind.as_str() != workspace.kind
            || candidate.extension_id != workspace.extension_id
        {
            continue;
        }
        let default_branch = sources
            .iter()
            .find(|snapshot| {
                snapshot.owns(&workspace.repository, &workspace.path)
                    && !snapshot.default_branch.is_empty()
            })
            .map(|snapshot| snapshot.default_branch.clone())
            .unwrap_or_default();
        records.push(ProjectRecord {
            source_repository: workspace.repository.clone(),
            source_default_branch: default_branch,
            source_subdirectory: workspace.subdirectory.clone(),
            ..candidate
        });
        changed = true;
    }
    changed
}

fn cleanup_temporary_candidates(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_file()
            && name.starts_with(".himind-candidate-")
            && (name.ends_with(".hmpkg") || name.ends_with(".hmskill"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn read_records(path: &Path) -> Result<Vec<ProjectRecord>, Box<dyn Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// 注册表是所有会话共享的一份文件：GUI Agent 与每个 DSH 会话的 MCP 伴生进程都
/// 会读写它。所有「读-改-写」都必须拿着同一把锁走完，否则两个并发会话各自读到
/// 旧快照、再各自写回，后写的那份会把先写的那份挤掉。
///
/// 返回值是被改动后的完整登记集合，调用方据此取回最终 id（id 可能在写盘前被
/// `assign_record_ids` 收敛过）。
fn with_registry_mut<T>(
    mutate: impl FnOnce(&mut Vec<ProjectRecord>) -> Result<(T, bool), Box<dyn Error>>,
) -> Result<(T, Vec<ProjectRecord>), Box<dyn Error>> {
    with_registry_mut_at(&registry_path(), mutate)
}

/// 与 `with_registry_mut` 同语义，只是把登记表文件显式传入 —— 测试要能在临时目录里
/// 反复跑「多个工作区并发写同一份登记表」，而不是把进程级的 `HIMIND_EXTENSION_PROJECTS_FILE`
/// 改来改去（测试并行跑，改环境变量会串到别的用例）。
fn with_registry_mut_at<T>(
    path: &Path,
    mutate: impl FnOnce(&mut Vec<ProjectRecord>) -> Result<(T, bool), Box<dyn Error>>,
) -> Result<(T, Vec<ProjectRecord>), Box<dyn Error>> {
    let _lock = crate::store::atomic_file::lock(path)?;
    let mut records = read_records(path)?;
    let (output, changed) = mutate(&mut records)?;
    if changed {
        assign_record_ids(&mut records);
        write_records(path, &records)?;
    }
    Ok((output, records))
}

/// 工作区标识：小写、统一分隔符，跨平台比较时不受大小写与反斜杠影响。
pub(crate) fn workspace_key_of(path: &Path) -> String {
    crate::extension_workspace::display_path(path)
        .replace('\\', "/")
        .to_lowercase()
}

fn workspace_key_digest(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))[..12].to_string()
}

/// 把登记 id 收敛成稳定值。
///
/// 扩展身份（`kind:extension_id`）是主键，工作区只在真的撞车时才参与命名：
///
/// - 同一个扩展 ID 只出现在一个工作区时，id 保持规范形式 `{kind}:{extension_id}`，
///   既有安装不需要迁移；
/// - 同一扩展 ID 出现在多个工作区（同一个仓库的两个工作树、或两个分支）时，
///   先到的那条继续占用规范 id，其余登记追加 `@<工作区短摘要>`，互不覆盖；
/// - 判定是「粘性」的：已经占据规范 id 的登记一直保留它，直到自己被移除。
///   否则两个工作区交替刷新会让 id 前后横跳，界面选中项会跟着丢。
fn assign_record_ids(records: &mut Vec<ProjectRecord>) {
    for record in records.iter_mut() {
        if record.workspace_key.is_empty() {
            record.workspace_key = workspace_key_of(&record.workspace_path);
        }
    }
    // 第一遍：把上一轮已经占着规范 id 的登记认成主登记。
    let mut primaries: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for record in records.iter() {
        let canonical = canonical_project_id(record.kind, &record.extension_id);
        if record.id == canonical {
            primaries
                .entry(canonical)
                .or_insert_with(|| record.workspace_key.clone());
        }
    }
    // 第二遍：补齐 id，并把同一工作区的重复登记收敛成一条。
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut deduped: Vec<ProjectRecord> = Vec::with_capacity(records.len());
    for mut record in records.drain(..) {
        let canonical = canonical_project_id(record.kind, &record.extension_id);
        if !seen.insert((canonical.clone(), record.workspace_key.clone())) {
            continue;
        }
        let is_primary = match primaries.get(&canonical) {
            Some(owner) => *owner == record.workspace_key,
            None => {
                primaries.insert(canonical.clone(), record.workspace_key.clone());
                true
            }
        };
        record.id = if is_primary {
            canonical
        } else {
            format!(
                "{canonical}@{}",
                workspace_key_digest(&record.workspace_key)
            )
        };
        deduped.push(record);
    }
    *records = deduped;
}

fn canonical_project_id(kind: ExtensionProjectKind, extension_id: &str) -> String {
    format!("{}:{}", kind.as_str(), extension_id.trim())
}

/// 登记的「扩展身份」：与工作区无关，同名扩展在不同工作区登记时两条记录共用它。
fn record_identity(record: &ProjectRecord) -> String {
    canonical_project_id(record.kind, &record.extension_id)
}

/// 解析调用方给出的扩展身份：优先规范 id，否则回落到任一工作区变体。
///
/// `kind:extension_id` 是跨进程、跨界面的稳定引用（CLI、MCP、界面都用它），
/// 同一个扩展在多个工作区登记时不能让它变成"找不到"。
fn record_id_for(
    records: &[ProjectRecord],
    kind: ExtensionProjectKind,
    extension_id: &str,
) -> Option<String> {
    let canonical = canonical_project_id(kind, extension_id);
    if records.iter().any(|record| record.id == canonical) {
        return Some(canonical);
    }
    let prefix = format!("{canonical}@");
    records
        .iter()
        .filter(|record| record.id.starts_with(&prefix))
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
        .map(|record| record.id.clone())
}

fn write_records(path: &Path, records: &[ProjectRecord]) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::store::atomic_file::atomic_write(path, &serde_json::to_vec_pretty(records)?)?;
    Ok(())
}

fn registry_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_EXTENSION_PROJECTS_FILE") {
        return PathBuf::from(path);
    }
    crate::store::paths::agent_home().join("extension-projects.json")
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

impl From<ProjectRecord> for ExtensionProject {
    fn from(value: ProjectRecord) -> Self {
        Self {
            workspace_available: value.workspace_path.is_dir(),
            id: value.id,
            kind: value.kind,
            extension_id: value.extension_id,
            name: value.name,
            description: value.description,
            version: value.version,
            workspace_path: crate::extension_workspace::display_path(&value.workspace_path),
            source: value.source,
            source_repository: value.source_repository,
            source_default_branch: value.source_default_branch,
            source_subdirectory: value.source_subdirectory,
            source_commit: value.source_commit,
            // 目标解析需要读取扩展源设置，由 `project_view` 补齐；直接转换时
            // 回落到项目覆盖或默认值，保证纯数据路径不产生 IO。
            distribution_targets: value
                .distribution_targets
                .clone()
                .filter(|targets| !targets.is_empty())
                .unwrap_or_else(default_distribution_targets),
            // 清单声明需要读盘，由 `project_view` 补齐；纯数据路径只保留默认值，
            // 避免投影函数产生 IO。
            distribution_targets_declared: Vec::new(),
            distribution_targets_source: if value
                .distribution_targets
                .as_ref()
                .is_some_and(|targets| !targets.is_empty())
            {
                "project".to_string()
            } else {
                "default".to_string()
            },
            updated_at: value.updated_at,
            source_unit_key: value.source_unit_key,
        }
    }
}

/// 解析生效的分发目标，返回目标集合与来源标记。
///
/// 清单声明是硬上限：它是作者写进制品、随制品走的约束，本机设置只能在范围内
/// 收窄，不能扩权。收窄顺序为 项目覆盖 → 分发单元默认 → 清单声明 → 仅工作台，
/// 任何一步越界都会被裁回声明范围内，并把来源标记为 `manifest`，让 UI 能解释
/// 「为什么这里选不了 GitHub」。单元默认值等于出厂默认时标记为 `default`。
fn resolve_distribution_targets(
    overriding: Option<&Vec<DistributionTarget>>,
    unit_key: &str,
    settings: &crate::app::extension_source::ExtensionSourceSettings,
    declared: &[DistributionTarget],
) -> (Vec<DistributionTarget>, &'static str) {
    if let Some(targets) = overriding.filter(|targets| !targets.is_empty()) {
        let narrowed = clamp_distribution_targets(targets, declared);
        let source = if distribution_targets_are_subset(targets, declared) {
            "project"
        } else {
            "manifest"
        };
        return (narrowed, source);
    }
    // 只有显式登记的单元默认值才算设置；`["workbench"]` 与出厂默认同值，但显式
    // 选择它意味着「这个单元只发工作台」，必须能压住清单里的 GitHub 声明。
    if let Some(unit_targets) =
        crate::app::extension_source::unit_distribution_targets_setting(settings, unit_key)
    {
        let narrowed = clamp_distribution_targets(&unit_targets, declared);
        let source = if narrowed == unit_targets {
            "unit"
        } else {
            "manifest"
        };
        return (narrowed, source);
    }
    // 本机没有单独设置分发单元默认值：有声明时以声明为准，否则按出厂默认。
    if declared.is_empty() {
        return (default_distribution_targets(), "default");
    }
    (declared.to_vec(), "manifest")
}

/// 读取项目目录里清单声明的分发落点。
///
/// 只做 JSON 级读取，不走完整清单校验：扩展正在编辑时清单可能暂时不合法，
/// 那时仍应沿用作者上一次写下的约束，而不是把约束判断整个丢掉。
fn read_declared_distribution_targets(record: &ProjectRecord) -> Vec<DistributionTarget> {
    let manifest = match record.kind {
        ExtensionProjectKind::Plugin => record.workspace_path.join("plugin.json"),
        ExtensionProjectKind::Skill => record.workspace_path.join("skill.json"),
        ExtensionProjectKind::Workflow => record.workspace_path.join("workflow.json"),
        ExtensionProjectKind::Expert => record.workspace_path.join("expert.json"),
        ExtensionProjectKind::Instruction => record.workspace_path.join("instruction.json"),
    };
    let Ok(source) = fs::read_to_string(&manifest) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&source) else {
        return Vec::new();
    };
    declared_distribution_targets(&value)
}

/// 把记录投影成对外的项目视图，并补齐分发单元与目标字段。
///
/// 读取扩展源设置失败时不阻断列表：退回默认目标，保持项目可见。
fn project_view(record: &ProjectRecord) -> ExtensionProject {
    let settings = crate::app::extension_source::settings().unwrap_or_default();
    project_view_with(record, &settings)
}

fn project_view_with(
    record: &ProjectRecord,
    settings: &crate::app::extension_source::ExtensionSourceSettings,
) -> ExtensionProject {
    let mut view = ExtensionProject::from(record.clone());
    if view.source_unit_key.trim().is_empty() {
        view.source_unit_key = derive_unit_key(record);
    }
    let declared = read_declared_distribution_targets(record);
    let (targets, source) = resolve_distribution_targets(
        record.distribution_targets.as_ref(),
        &view.source_unit_key,
        settings,
        &declared,
    );
    view.distribution_targets = targets;
    view.distribution_targets_declared = declared.clone();
    view.distribution_targets_source = source.to_string();
    view
}

/// 由本地目录源工作区反查分发单元键；查不到时返回空字符串。
fn derive_unit_key(record: &ProjectRecord) -> String {
    crate::app::extension_source::local_source_workspaces()
        .into_iter()
        .find(|workspace| {
            workspace.kind == record.kind.as_str()
                && workspace.extension_id == record.extension_id
                && workspace.repository == record.source_repository
                && workspace.subdirectory == record.source_subdirectory
        })
        .map(|workspace| workspace.unit_key)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::extension_source::LocalSourceWorkspace;

    /// 提交号是「候选制品 ↔ 源码」的溯源锚点，补号逻辑必须足够保守：
    /// 没有声明仓库、已记录过提交号、或工作区根本不是仓库时，都不能凭空造一个。
    #[test]
    fn source_commit_backfill_only_fills_missing_provenance() {
        let root = env::temp_dir().join(format!("himind-source-commit-{}", now_stamp()));
        fs::create_dir_all(&root).unwrap();
        let mut project = record(
            ExtensionProjectKind::Plugin,
            "com.himind.source-commit-test".to_string(),
            "提交号测试".to_string(),
            "测试提交号兜底".to_string(),
            "0.1.0".to_string(),
            &root,
            "manual",
        );

        // 没声明源码仓库：不补，也不去碰 git。
        assert!(!ensure_source_commit(&mut project));
        assert!(project.source_commit.is_empty());

        // 声明了仓库但目录不是 git 仓库：宁可留空让界面提示，也不能编一个提交号。
        project.source_repository = "owner/repo".to_string();
        assert!(!ensure_source_commit(&mut project));
        assert!(project.source_commit.is_empty());

        // 已经记录过提交号：只补不覆盖，避免开发者切分支时改写已提交制品的出处。
        project.source_commit = "0123456789abcdef".to_string();
        assert!(!ensure_source_commit(&mut project));
        assert_eq!(project.source_commit, "0123456789abcdef");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_plugin_skill_and_workflow_projects_with_stable_ids() {
        let root = env::temp_dir().join(format!("himind-project-detect-{}", now_stamp()));
        let plugin = root.join("plugin");
        let skill = root.join("skill");
        let workflow = root.join("workflow");
        fs::create_dir_all(&plugin).unwrap();
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir_all(workflow.join("ui")).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"id":"com.himind.project-test","name":"项目测试插件","description":"测试插件项目识别","version":"0.1.0"}"#,
        )
        .unwrap();
        fs::write(
            skill.join("skill.json"),
            r#"{"id":"com.himind.skill.project-test","name":"项目测试技能","author":"测试用户","categories":["software-engineering"],"version":"0.1.0","scope":"organization","description":"测试技能项目识别","release_notes":"创建初始版本。","min_agent_version":"0.3.0","supported_clients":["codex"],"capabilities":[],"plugin_dependencies":[],"risk_summary":"read_only","contents":["skill.json","SKILL.md"]}"#,
        )
        .unwrap();
        fs::write(skill.join("SKILL.md"), "# 项目测试技能\n").unwrap();
        fs::write(
            workflow.join("workflow.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": "workflow_package.v1",
                "id": "com.himind.workflow.project-test",
                "version": "0.1.0",
                "name": "项目测试工作流",
                "description": "测试 Workflow 项目识别",
                "min_agent_version": crate::VERSION,
                "steps": [{
                    "id": "START",
                    "title": "开始",
                    "kind": "manual",
                    "execution_mode": "sync"
                }],
                "artifacts": [],
                "ui": { "mode": "standard" }
            }))
            .unwrap(),
        )
        .unwrap();

        let plugin_record = project_record_from_path(&plugin, "test").unwrap();
        let skill_record = project_record_from_path(&skill, "test").unwrap();
        let workflow_record = project_record_from_path(&workflow, "test").unwrap();
        assert_eq!(plugin_record.id, "plugin:com.himind.project-test");
        assert_eq!(skill_record.id, "skill:com.himind.skill.project-test");
        assert_eq!(
            workflow_record.id,
            "workflow:com.himind.workflow.project-test"
        );
        assert_eq!(plugin_record.kind, ExtensionProjectKind::Plugin);
        assert_eq!(skill_record.kind, ExtensionProjectKind::Skill);
        assert_eq!(workflow_record.kind, ExtensionProjectKind::Workflow);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_skeleton_uses_canonical_layout_and_declarative_ui() {
        let root = env::temp_dir().join(format!("himind-workflow-skeleton-{}", now_stamp()));
        fs::create_dir_all(&root).unwrap();
        let input = CreateExtensionProjectInput {
            kind: ExtensionProjectKind::Workflow,
            slug: "canonical-workflow".to_string(),
            extension_id: "com.example.workflow.canonical".to_string(),
            name: "Canonical Workflow".to_string(),
            description: "Canonical layout test.".to_string(),
            category: "software-engineering".to_string(),
            template: "strict".to_string(),
        };
        let result = create_workflow_skeleton(&root, &input.slug, &input).unwrap();
        let workflow_root = Path::new(result["root"].as_str().unwrap());
        assert_eq!(
            workflow_root,
            root.join("workflows").join("canonical-workflow")
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(workflow_root.join("workflow.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["ui"]["mode"], "declarative");
        assert_eq!(manifest["ui"]["entry"], "ui/workflow-view.json");
        assert!(workflow_root.join("ui/workflow-view.json").is_file());
        assert!(workflow_root.join("schemas").is_dir());
        assert!(workflow_root.join("artifacts").is_dir());
        assert!(workflow_root.join("connectors").is_dir());
        assert!(workflow_root.join("tests/contract").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reconciles_projects_with_the_declared_source_catalog() {
        let root = env::temp_dir().join(format!("himind-project-reconcile-{}", now_stamp()));
        let live = root.join("plugins").join("live");
        fs::create_dir_all(&live).unwrap();
        fs::write(
            live.join("plugin.json"),
            r#"{"id":"com.himind.live","name":"在册插件","description":"仍在清单里","version":"1.0.0"}"#,
        )
        .unwrap();
        let workspaces = vec![LocalSourceWorkspace {
            kind: "plugin".to_string(),
            extension_id: "com.himind.live".to_string(),
            path: live.clone(),
            repository: "Owner/repo".to_string(),
            subdirectory: "plugins/live".to_string(),
            unit_key: "remote:owner/repo#stable#public".to_string(),
        }];
        let snapshot = crate::app::extension_source::LocalSourceSnapshot::for_test(
            &root,
            "Owner/repo",
            "main",
            workspaces.clone(),
        );
        let mut records = vec![
            ProjectRecord {
                id: "plugin:com.himind.retired".to_string(),
                kind: ExtensionProjectKind::Plugin,
                extension_id: "com.himind.retired".to_string(),
                name: "已改名插件".to_string(),
                description: String::new(),
                version: "0.9.0".to_string(),
                workspace_path: root.join("plugins").join("retired"),
                source: "extension_source".to_string(),
                source_repository: "Owner/repo".to_string(),
                source_default_branch: "main".to_string(),
                source_subdirectory: "plugins/retired".to_string(),
                source_commit: String::new(),
                distribution_targets: None,
                updated_at: now_stamp(),
                source_unit_key: String::new(),
                workspace_key: String::new(),
            },
            ProjectRecord {
                id: "plugin:com.himind.elsewhere".to_string(),
                kind: ExtensionProjectKind::Plugin,
                extension_id: "com.himind.elsewhere".to_string(),
                name: "别的源".to_string(),
                description: String::new(),
                version: "0.1.0".to_string(),
                workspace_path: root.join("plugins").join("elsewhere"),
                source: "extension_source".to_string(),
                source_repository: "Other/repo".to_string(),
                source_default_branch: "main".to_string(),
                source_subdirectory: "plugins/elsewhere".to_string(),
                source_commit: String::new(),
                distribution_targets: None,
                updated_at: now_stamp(),
                source_unit_key: String::new(),
                workspace_key: String::new(),
            },
        ];

        assert!(reconcile_source_declared_projects(
            &mut records,
            std::slice::from_ref(&snapshot),
            &workspaces,
        ));
        assert_eq!(
            records
                .iter()
                .map(|record| record.extension_id.as_str())
                .collect::<Vec<_>>(),
            vec!["com.himind.elsewhere", "com.himind.live"],
            "已退出清单的登记应移除，清单里的扩展应补登记，其他源的登记不受影响"
        );
        let added = records
            .iter()
            .find(|record| record.extension_id == "com.himind.live")
            .unwrap();
        assert_eq!(added.source, "extension_source");
        assert_eq!(added.source_subdirectory, "plugins/live");
        assert_eq!(added.source_default_branch, "main");
        assert!(
            !reconcile_source_declared_projects(&mut records, &[snapshot], &workspaces),
            "对账后必须收敛，不再重复改写"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rebinds_projects_stuck_on_agent_draft_directories_to_the_source_workspace() {
        let source = env::temp_dir().join(format!("himind-project-source-{}", now_stamp()));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            r#"{"id":"com.himind.rebind-test","name":"回绑插件","description":"测试回绑","version":"0.1.0"}"#,
        )
        .unwrap();
        let draft = draft_directory("rebind-test");
        let workspaces = vec![LocalSourceWorkspace {
            kind: "plugin".to_string(),
            extension_id: "com.himind.rebind-test".to_string(),
            path: source.clone(),
            repository: "Owner/repo".to_string(),
            subdirectory: "plugins/rebind-test".to_string(),
            unit_key: "remote:owner/repo".to_string(),
        }];
        let mut records = vec![
            project_record_from_path(&draft, "legacy_candidate").unwrap(),
            ProjectRecord {
                workspace_path: source.clone(),
                ..project_record_from_path(&source, "extension_source").unwrap()
            },
        ];
        let mut stale_label = vec![project_record_from_path(&source, "legacy_candidate").unwrap()];

        assert!(rebind_extension_source_workspaces(
            &mut records,
            &workspaces
        ));
        assert_eq!(records[0].workspace_path, source);
        assert_eq!(records[0].source, "extension_source");
        assert_eq!(records[0].source_repository, "Owner/repo");
        assert_eq!(records[0].source_subdirectory, "plugins/rebind-test");
        assert_eq!(records[1].source, "extension_source");
        assert!(
            rebind_extension_source_workspaces(&mut stale_label, &workspaces),
            "已指向源码工作区但来源标记陈旧的记录应被归一"
        );
        assert_eq!(stale_label[0].workspace_path, source);
        assert_eq!(stale_label[0].source, "extension_source");
        assert!(
            !rebind_extension_source_workspaces(&mut stale_label, &workspaces),
            "归一后必须收敛，不再重复改写"
        );
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(draft);
    }

    #[test]
    fn draft_project_record_prefers_the_declared_source_workspace_over_the_draft_path() {
        let source = env::temp_dir().join(format!("himind-project-declared-{}", now_stamp()));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            r#"{"id":"com.himind.declared-test","name":"声明插件","description":"测试声明目录","version":"0.1.0"}"#,
        )
        .unwrap();
        let draft = draft_directory("declared-test");
        let workspaces = vec![LocalSourceWorkspace {
            kind: "plugin".to_string(),
            extension_id: "com.himind.declared-test".to_string(),
            path: source.clone(),
            repository: String::new(),
            subdirectory: String::new(),
            unit_key: String::new(),
        }];

        let record = draft_project_record(
            ExtensionProjectKind::Plugin,
            "com.himind.declared-test",
            Some(&draft),
            &workspaces,
        )
        .unwrap();
        assert_eq!(record.workspace_path, source);
        assert_eq!(record.source, "extension_source");

        assert!(
            draft_project_record(
                ExtensionProjectKind::Plugin,
                "com.himind.declared-test",
                Some(&draft),
                &[],
            )
            .is_none(),
            "没有扩展源声明时不得绑定 Agent 草稿目录"
        );
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(draft);
    }

    /// `CARGO_MANIFEST_DIR` 下的目录会被判定为 Agent 自管路径，用它模拟草稿产物目录。
    fn draft_directory(tag: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("himind-agent-test-drafts")
            .join(tag);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("plugin.json"),
            r#"{"id":"com.himind.rebind-test","name":"草稿插件","description":"草稿","version":"0.1.0"}"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn rejects_ambiguous_project_directory() {
        let root = env::temp_dir().join(format!("himind-project-ambiguous-{}", now_stamp()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.json"), "{}").unwrap();
        fs::write(root.join("skill.json"), "{}").unwrap();
        assert!(project_record_from_path(&root, "test").is_err());
        let _ = fs::remove_dir_all(root);
    }

    fn target_settings(
        unit_key: &str,
        targets: Vec<DistributionTarget>,
    ) -> crate::app::extension_source::ExtensionSourceSettings {
        let mut settings = crate::app::extension_source::ExtensionSourceSettings::default();
        settings
            .distribution_targets
            .insert(unit_key.to_string(), targets);
        settings
    }

    #[test]
    fn project_override_wins_over_unit_and_system_defaults() {
        let settings = target_settings(
            "example#stable#public",
            vec![DistributionTarget::Workbench, DistributionTarget::Github],
        );
        let (targets, source) = resolve_distribution_targets(
            Some(&vec![DistributionTarget::Github]),
            "example#stable#public",
            &settings,
            &[DistributionTarget::Workbench, DistributionTarget::Github],
        );
        assert_eq!(targets, vec![DistributionTarget::Github]);
        assert_eq!(source, "project");
    }

    #[test]
    fn unit_default_applies_when_project_has_no_override() {
        let settings = target_settings(
            "example#stable#public",
            vec![DistributionTarget::Workbench, DistributionTarget::Github],
        );
        let (targets, source) = resolve_distribution_targets(
            None,
            "example#stable#public",
            &settings,
            &[DistributionTarget::Workbench, DistributionTarget::Github],
        );
        assert_eq!(
            targets,
            vec![DistributionTarget::Workbench, DistributionTarget::Github]
        );
        assert_eq!(source, "unit");
    }

    #[test]
    fn missing_unit_or_empty_override_falls_back_to_workbench_only() {
        let settings = crate::app::extension_source::ExtensionSourceSettings::default();
        let (targets, source) = resolve_distribution_targets(None, "", &settings, &[]);
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "default");

        // 空的覆盖集合按「未覆盖」处理，避免出现没有任何落点的项目。
        let (targets, source) = resolve_distribution_targets(Some(&Vec::new()), "", &settings, &[]);
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "default");
    }

    #[test]
    fn explicit_unit_default_narrows_manifest_even_when_it_equals_the_factory_default() {
        // `["workbench"]` 与出厂默认同值，但显式登记表示「这个单元只发工作台」，
        // 必须能压住清单里声明的 GitHub，否则约束形同虚设。
        let settings =
            target_settings("example#stable#public", vec![DistributionTarget::Workbench]);
        let (targets, source) = resolve_distribution_targets(
            None,
            "example#stable#public",
            &settings,
            &[DistributionTarget::Workbench, DistributionTarget::Github],
        );
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "unit");

        // 未登记的单元（继承）才按清单声明放行。
        let inherited = crate::app::extension_source::ExtensionSourceSettings::default();
        let (targets, source) = resolve_distribution_targets(
            None,
            "example#stable#public",
            &inherited,
            &[DistributionTarget::Workbench, DistributionTarget::Github],
        );
        assert_eq!(
            targets,
            vec![DistributionTarget::Workbench, DistributionTarget::Github]
        );
        assert_eq!(source, "manifest");
    }

    #[test]
    fn manifest_declaration_caps_local_settings() {
        let settings = crate::app::extension_source::ExtensionSourceSettings::default();
        let declared = vec![DistributionTarget::Workbench];

        // 未声明时保持既有继承链：本机默认仍然只看分发单元设置。
        let (targets, source) = resolve_distribution_targets(None, "", &settings, &[]);
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "default");

        // 有声明且本机没有单独设置：直接按声明走，并说明来源是清单。
        let (targets, source) = resolve_distribution_targets(None, "", &settings, &declared);
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "manifest");

        // 本机想加 GitHub，但清单只声明工作台：裁回声明范围并标记来源。
        let (targets, source) = resolve_distribution_targets(
            Some(&vec![
                DistributionTarget::Workbench,
                DistributionTarget::Github,
            ]),
            "",
            &settings,
            &declared,
        );
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "manifest");

        // 分发单元默认越界时同样被裁回声明范围。
        let settings = target_settings(
            "example#stable#public",
            vec![DistributionTarget::Workbench, DistributionTarget::Github],
        );
        let (targets, source) =
            resolve_distribution_targets(None, "example#stable#public", &settings, &declared);
        assert_eq!(targets, vec![DistributionTarget::Workbench]);
        assert_eq!(source, "manifest");
    }

    #[test]
    fn declared_targets_are_read_from_extension_manifest() {
        let root = env::temp_dir().join(format!("himind-declared-{}", now_stamp()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("skill.json"),
            r#"{"id":"com.himind.skill.declared","name":"声明技能","description":"","version":"1.0.0","categories":["software-engineering"],"author":"tester","release_notes":"首个版本","distribution_targets":["github","workbench","unknown-target"]}"#,
        )
        .unwrap();
        let mut record = record(
            ExtensionProjectKind::Skill,
            "com.himind.skill.declared".to_string(),
            "声明技能".to_string(),
            String::new(),
            "1.0.0".to_string(),
            &root,
            "local_workspace",
        );
        assert_eq!(
            read_declared_distribution_targets(&record),
            vec![DistributionTarget::Workbench, DistributionTarget::Github]
        );

        // 清掉声明后退回「未声明」，既有的继承链不受影响。
        fs::write(
            root.join("skill.json"),
            r#"{"id":"com.himind.skill.declared","name":"声明技能","description":"","version":"1.0.0"}"#,
        )
        .unwrap();
        assert!(read_declared_distribution_targets(&record).is_empty());
        record.workspace_path = root.join("missing");
        assert!(read_declared_distribution_targets(&record).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_record_omits_targets_until_explicitly_set() {
        let record = record(
            ExtensionProjectKind::Plugin,
            "com.himind.example".to_string(),
            "示例插件".to_string(),
            String::new(),
            "0.1.0".to_string(),
            Path::new("F:/example"),
            "local_workspace",
        );
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("distribution_targets"));

        // 旧配置反序列化必须继续可用。
        let legacy: ProjectRecord = serde_json::from_str(
            r#"{"id":"plugin:com.himind.legacy","kind":"plugin","extension_id":"com.himind.legacy","name":"旧插件","description":"","version":"0.1.0","workspace_path":"F:/legacy","source":"local_workspace","updated_at":"1"}"#,
        )
        .unwrap();
        assert!(legacy.distribution_targets.is_none());
        let view = ExtensionProject::from(legacy);
        assert_eq!(
            view.distribution_targets,
            vec![DistributionTarget::Workbench]
        );
        assert_eq!(view.distribution_targets_source, "default");
    }

    #[test]
    fn explicit_override_projects_as_project_sourced_targets() {
        let mut record = record(
            ExtensionProjectKind::Skill,
            "com.himind.skill.example".to_string(),
            "示例技能".to_string(),
            String::new(),
            "0.1.0".to_string(),
            Path::new("F:/example"),
            "local_workspace",
        );
        record.distribution_targets = Some(vec![DistributionTarget::Github]);
        let view = ExtensionProject::from(record);
        assert_eq!(view.distribution_targets, vec![DistributionTarget::Github]);
        assert_eq!(view.distribution_targets_source, "project");
    }

    /// 同一个扩展 ID 出现在两个工作区（两个分支 / 两份检出）时，两边都要能各自
    /// 登记、各自刷新，谁都不能把谁挤掉。这是「一个 Agent 同时服务多个工作区会话」
    /// 的核心不变量。
    #[test]
    fn same_extension_in_two_workspaces_keeps_both_records() {
        let registry = registry_file("two-workspaces");
        let first = plugin_workspace("two-workspaces-a", "com.himind.multiwindow");
        let second = plugin_workspace("two-workspaces-b", "com.himind.multiwindow");

        let registered_first = register_in(&registry, &first).unwrap();
        let registered_second = register_in(&registry, &second).unwrap();

        assert_eq!(registered_first.id, "plugin:com.himind.multiwindow");
        assert_ne!(registered_second.id, registered_first.id);
        assert!(registered_second
            .id
            .starts_with("plugin:com.himind.multiwindow@"));
        assert_eq!(registered_second.workspace_path, display_of(&second));
        assert_eq!(registered_first.workspace_path, display_of(&first));

        let records = read_records(&registry).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records
                .iter()
                .filter(|record| record.extension_id == "com.himind.multiwindow")
                .count(),
            2
        );

        // 再次打开第二个目录：只刷新它自己那条，第一条不动。
        let reopened = register_in(&registry, &second).unwrap();
        assert_eq!(reopened.id, registered_second.id);
        assert_eq!(read_records(&registry).unwrap().len(), 2);

        let _ = fs::remove_dir_all(first);
        let _ = fs::remove_dir_all(second);
        let _ = fs::remove_file(&registry);
    }

    /// 多会话并发登记：登记表是共享文件，读-改-写必须整体持锁，否则后写的那份
    /// 会把先写的那份的登记挤掉（表现为"另一个会话的项目在列表里偶尔消失"）。
    #[test]
    fn concurrent_registrations_lose_nothing() {
        let registry = registry_file("concurrent");
        let workspaces: Vec<_> = (0..6)
            .map(|index| {
                plugin_workspace(
                    &format!("concurrent-{index}"),
                    &format!("com.himind.concurrent-{index}"),
                )
            })
            .collect();

        std::thread::scope(|scope| {
            for workspace in &workspaces {
                let registry = registry.clone();
                scope.spawn(move || {
                    register_in(&registry, workspace).unwrap();
                });
            }
        });

        let records = read_records(&registry).unwrap();
        for index in 0..6 {
            assert!(
                records
                    .iter()
                    .any(|record| record.extension_id == format!("com.himind.concurrent-{index}")),
                "并发登记丢了第 {index} 个工作区"
            );
        }

        for workspace in workspaces {
            let _ = fs::remove_dir_all(workspace);
        }
        let _ = fs::remove_file(&registry);
    }

    /// 并发登记「同一个扩展」的两份检出：两条都在，id 必须互不相同。
    #[test]
    fn concurrent_registrations_of_one_extension_stay_distinct() {
        let registry = registry_file("concurrent-same-id");
        let workspaces: Vec<_> = (0..6)
            .map(|index| plugin_workspace(&format!("same-id-{index}"), "com.himind.racing"))
            .collect();

        std::thread::scope(|scope| {
            for workspace in &workspaces {
                let registry = registry.clone();
                scope.spawn(move || {
                    register_in(&registry, workspace).unwrap();
                });
            }
        });

        let records = read_records(&registry).unwrap();
        let mut ids: Vec<String> = records
            .iter()
            .filter(|record| record.extension_id == "com.himind.racing")
            .map(|record| record.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 6, "并发登记同一个扩展时登记 id 重复了: {ids:?}");

        for workspace in workspaces {
            let _ = fs::remove_dir_all(workspace);
        }
        let _ = fs::remove_file(&registry);
    }

    /// 规范 id 的归属是「粘性」的：先登记的那条一直拿着它，后来者加工作区摘要后缀，
    /// 两条交替刷新也不会让 id 横跳（否则界面上的选中项会跟着丢）。
    #[test]
    fn canonical_record_id_stays_with_the_first_workspace() {
        let mut records = vec![
            record(
                ExtensionProjectKind::Plugin,
                "com.himind.sticky".to_string(),
                "粘性插件".to_string(),
                String::new(),
                "0.1.0".to_string(),
                &env::temp_dir().join("himind-sticky-a"),
                "local_workspace",
            ),
            record(
                ExtensionProjectKind::Plugin,
                "com.himind.sticky".to_string(),
                "粘性插件".to_string(),
                String::new(),
                "0.1.0".to_string(),
                &env::temp_dir().join("himind-sticky-b"),
                "local_workspace",
            ),
            record(
                ExtensionProjectKind::Plugin,
                "com.himind.sticky".to_string(),
                "粘性插件".to_string(),
                String::new(),
                "0.1.0".to_string(),
                &env::temp_dir().join("himind-sticky-c"),
                "local_workspace",
            ),
        ];

        // 第一轮：A 已经占着规范 id，B/C 加后缀。
        records[0].id = "plugin:com.himind.sticky".to_string();
        assign_record_ids(&mut records);
        let id_of = |records: &Vec<ProjectRecord>, index: usize| records[index].id.clone();
        assert_eq!(id_of(&records, 0), "plugin:com.himind.sticky");
        assert!(id_of(&records, 1).starts_with("plugin:com.himind.sticky@"));
        assert!(id_of(&records, 2).starts_with("plugin:com.himind.sticky@"));
        let second_id = id_of(&records, 1);

        // 第二轮：顺序打乱、重新收敛，id 不能变。
        records.swap(0, 2);
        assign_record_ids(&mut records);
        assert!(
            records
                .iter()
                .any(|record| record.id == "plugin:com.himind.sticky"),
            "规范 id 必须继续有人占着"
        );
        assert!(
            records.iter().any(|record| record.id == second_id),
            "已经分配过的工作区后缀不能改名"
        );

        // 主登记被移除后，剩下的登记才回到规范 id。
        records.retain(|record| record.id != "plugin:com.himind.sticky");
        assign_record_ids(&mut records);
        assert_eq!(records.len(), 2);
        assert!(records
            .iter()
            .any(|record| record.id.starts_with("plugin:com.himind.sticky")));

        // 同一工作区的重复登记收敛成一条。
        let duplicate = records[0].clone();
        records.push(duplicate);
        assign_record_ids(&mut records);
        assert_eq!(records.len(), 2);
    }

    /// 同一个扩展在多个工作区登记后，调用方只拿扩展身份（`kind:extension_id`）来
    /// 引用它时，必须能解析到某个具体登记，而不是报"不存在"。
    #[test]
    fn record_id_for_resolves_identity_and_workspace_variants() {
        let mut records = vec![record(
            ExtensionProjectKind::Plugin,
            "com.himind.resolve".to_string(),
            "解析插件".to_string(),
            String::new(),
            "0.1.0".to_string(),
            &env::temp_dir().join("himind-resolve-a"),
            "local_workspace",
        )];
        assert_eq!(
            record_id_for(&records, ExtensionProjectKind::Plugin, "com.himind.resolve"),
            Some("plugin:com.himind.resolve".to_string())
        );

        let mut variant = records[0].clone();
        variant.workspace_path = env::temp_dir().join("himind-resolve-b");
        variant.workspace_key = String::new();
        records.push(variant);
        assign_record_ids(&mut records);
        assert_eq!(records[0].id, "plugin:com.himind.resolve");
        assert!(records[1].id.starts_with("plugin:com.himind.resolve@"));

        // 另一个扩展身份不会被误认。
        assert_eq!(
            record_id_for(&records, ExtensionProjectKind::Plugin, "com.himind.other"),
            None
        );
        assert_eq!(
            record_id_for(&records, ExtensionProjectKind::Skill, "com.himind.resolve"),
            None
        );
    }

    fn registry_file(tag: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("himind-registry-{tag}-{}.json", now_stamp()));
        let _ = fs::remove_file(&path);
        path
    }

    fn plugin_workspace(tag: &str, extension_id: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("himind-project-{tag}-{}", now_stamp()));
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("plugin.json"),
            format!(
                r#"{{"id":"{extension_id}","name":"多工作区插件","description":"测试多工作区登记","version":"0.1.0"}}"#
            ),
        )
        .unwrap();
        path
    }

    fn display_of(path: &Path) -> String {
        // `env::temp_dir()` 在 Windows 上可能是 8.3 短名（ADMINI~1），登记里存的是
        // 规范化之后的长名，比较前先对齐。
        crate::extension_workspace::display_path(&path.canonicalize().unwrap())
    }
}
