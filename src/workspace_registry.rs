//! 设备级「路径 → 项目」登记表。
//!
//! 这张表回答一个问题：**这个目录属于哪个项目、它的上级是谁、该用什么命令**。
//! 它不记录工时与进度（那是 work-log 的事），也不改变任何会话权限。
//!
//! ## 为什么在内核而不是插件
//!
//! 数据落在 `data/workspace-registry.json`，因为 `plugin-data/` 不进备份包
//! （`app::backup` 的排除表写着「随插件重装重建」），而 `data/` 才进
//! （ADR 0002）。「一个路径属于哪个项目」是用户的认知而非可重建缓存，
//! 丢了就是丢了。同一份表还要被 `engineering.workspace.lease.*` 的项目身份
//! 与工作流的 `workspace_root` 解析消费，跨进程读一份插件私有目录并不现实。
//!
//! ## 只做确定性的事
//!
//! 归一化路径、按最长前缀解析、维护项目树与候选、原子写盘。它不判断
//! 「该不该登记」——那属于配套技能 `workspace-context`。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::atomic_file;

/// 登记表文件版本号；读到旧版本时按需迁移。
pub(crate) const SCHEMA_VERSION: &str = "workspace_registry.v1";
/// 能力契约版本。与内置插件条目共用同一个值，避免注册表版本漂移。
pub(crate) const CAPABILITY_VERSION: &str = "0.1.0";
/// 登记表在内核数据目录下的文件名。
const REGISTRY_FILE: &str = "workspace-registry.json";

/// 项目形态。只用于展示与筛选，不驱动解析逻辑。
pub(crate) const KIND_AGGREGATE: &str = "aggregate";
pub(crate) const KIND_PRODUCT: &str = "product";
pub(crate) const KIND_SUBPROJECT: &str = "subproject";
pub(crate) const KIND_DELIVERY: &str = "delivery";
pub(crate) const KIND_LIBRARY: &str = "library";
pub(crate) const KIND_DOCS: &str = "docs";
pub(crate) const KIND_SANDBOX: &str = "sandbox";
pub(crate) const KIND_OTHER: &str = "other";

/// 路径角色。`worktree` / `release` 这类目录不该被当成新项目。
pub(crate) const ROLE_ROOT: &str = "root";
const ROLE_WORKTREE: &str = "worktree";
const ROLE_MIRROR: &str = "mirror";
const ROLE_RELEASE: &str = "release";
const ROLE_SOURCE: &str = "source";
const ROLE_BUILD: &str = "build";

/// 绑定来源。手工登记的置信度最高。
pub(crate) const SOURCE_MANUAL: &str = "manual";
const SOURCE_IMPORT: &str = "import";
const SOURCE_MANIFEST: &str = "manifest";
const SOURCE_GIT: &str = "git";
pub(crate) const SOURCE_SESSION: &str = "session";
const SOURCE_SCAN: &str = "scan";

/// 置信度：只有 `exact` 才允许技能侧自动落库。
pub(crate) const CONFIDENCE_EXACT: &str = "exact";
pub(crate) const CONFIDENCE_INFERRED: &str = "inferred";

/// 结构化错误码。技能与界面按 code 分支，不解析中文句子。
pub(crate) const ERR_PATH_NOT_ABSOLUTE: &str = "PATH_NOT_ABSOLUTE";
pub(crate) const ERR_PATH_INVALID: &str = "PATH_INVALID";
pub(crate) const ERR_PATH_CONFLICT: &str = "PATH_CONFLICT";
pub(crate) const ERR_PROJECT_NOT_FOUND: &str = "PROJECT_NOT_FOUND";
pub(crate) const ERR_PROJECT_HAS_CHILDREN: &str = "PROJECT_HAS_CHILDREN";
pub(crate) const ERR_PROJECT_HAS_WORKSPACES: &str = "PROJECT_HAS_WORKSPACES";
pub(crate) const ERR_PARENT_CYCLE: &str = "PARENT_CYCLE";
pub(crate) const ERR_ALIAS_CONFLICT: &str = "ALIAS_CONFLICT";
pub(crate) const ERR_SCHEMA_INVALID: &str = "SCHEMA_INVALID";

fn valid_kinds() -> &'static [&'static str] {
    &[
        KIND_AGGREGATE,
        KIND_PRODUCT,
        KIND_SUBPROJECT,
        KIND_DELIVERY,
        KIND_LIBRARY,
        KIND_DOCS,
        KIND_SANDBOX,
        KIND_OTHER,
    ]
}

fn valid_roles() -> &'static [&'static str] {
    &[
        ROLE_ROOT,
        ROLE_WORKTREE,
        ROLE_MIRROR,
        ROLE_RELEASE,
        ROLE_SOURCE,
        ROLE_BUILD,
    ]
}

fn valid_sources() -> &'static [&'static str] {
    &[
        SOURCE_MANUAL,
        SOURCE_IMPORT,
        SOURCE_MANIFEST,
        SOURCE_GIT,
        SOURCE_SESSION,
        SOURCE_SCAN,
    ]
}

/// 一条建议命令；`cwd` 相对于项目根。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub cwd: String,
}

/// 文档或规则文件的入口。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryLink {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub path: String,
}

/// 项目上「该用什么命令、看哪个文档」的短索引。
///
/// 它不是知识库：技能读到后仍应打开真实文件。四个列表都带 `skip_serializing_if`，
/// 空列表序列化成「没有」而不是 `null`，免得调用方每次都判空。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryContext {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<RegistryCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<RegistryLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RegistryLink>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// 跨系统的稳定 ID。这里只存链接，不复制工时、优先级、下一步。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryLinks {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub work_log_project_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub work_log_project_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workbench_project_id: String,
}

/// 逻辑节点：项目、子项目、聚合节点都用它表达。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryProject {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub context: RegistryContext,
    #[serde(default)]
    pub links: RegistryLinks,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// 物理路径绑定；一个项目可以有多条。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryWorkspace {
    pub id: String,
    pub project_id: String,
    /// 归一化后的绝对路径。
    pub path: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub created_at: String,
}

/// 待确认的路径候选。发现是建议，登记是决定。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryCandidate {
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suggested_project: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suggested_parent: String,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub seen_at: String,
    #[serde(default)]
    pub seen_count: i64,
    #[serde(default)]
    pub status: String,
}

/// 状态取值：待确认 / 已忽略 / 已接受。
pub(crate) const CANDIDATE_PENDING: &str = "pending";
pub(crate) const CANDIDATE_DISMISSED: &str = "dismissed";
pub(crate) const CANDIDATE_ACCEPTED: &str = "accepted";

/// 整张登记表，也是落盘结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Registry {
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub projects: Vec<RegistryProject>,
    #[serde(default)]
    pub workspaces: Vec<RegistryWorkspace>,
    #[serde(default)]
    pub candidates: Vec<RegistryCandidate>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            updated_at: String::new(),
            projects: Vec::new(),
            workspaces: Vec::new(),
            candidates: Vec::new(),
        }
    }
}

/// 登记表文件位置；`HIMIND_WORKSPACE_REGISTRY_FILE` 只留给测试。
pub(crate) fn registry_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HIMIND_WORKSPACE_REGISTRY_FILE") {
        return PathBuf::from(path);
    }
    crate::store::paths::agent_home()
        .join("data")
        .join(REGISTRY_FILE)
}

impl Registry {
    pub(crate) fn project(&self, id: &str) -> Option<&RegistryProject> {
        self.projects.iter().find(|project| project.id == id)
    }

    fn project_mut(&mut self, id: &str) -> Option<&mut RegistryProject> {
        self.projects.iter_mut().find(|project| project.id == id)
    }

    /// 按 ID、名称或别名查找，名称与别名大小写不敏感。
    pub(crate) fn find_project(&self, reference: &str) -> Option<&RegistryProject> {
        let reference = reference.trim();
        if reference.is_empty() {
            return None;
        }
        if let Some(project) = self.project(reference) {
            return Some(project);
        }
        let lower = reference.to_lowercase();
        self.projects.iter().find(|project| {
            project.name.to_lowercase() == lower
                || project
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == lower)
        })
    }

    /// 直接子项目，按名称排序。
    pub(crate) fn children(&self, parent_id: &str) -> Vec<RegistryProject> {
        let mut children: Vec<RegistryProject> = self
            .projects
            .iter()
            .filter(|project| project.parent_id == parent_id)
            .cloned()
            .collect();
        children.sort_by(|left, right| left.name.cmp(&right.name));
        children
    }

    /// 从直接父级到根的链。遇到环就停，不无限循环。
    pub(crate) fn ancestors(&self, project_id: &str) -> Vec<RegistryProject> {
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(project_id.to_string());
        let mut current = self.project(project_id);
        while let Some(project) = current {
            if project.parent_id.is_empty() {
                break;
            }
            if !seen.insert(project.parent_id.clone()) {
                break;
            }
            let Some(parent) = self.project(&project.parent_id) else {
                break;
            };
            chain.push(parent.clone());
            current = Some(parent);
        }
        chain
    }

    /// `project_id` 是否是 `candidate` 的后代；用于父级环检测。
    fn has_descendant(&self, project_id: &str, candidate: &str) -> bool {
        if project_id.is_empty() || candidate.is_empty() {
            return false;
        }
        let mut current = self.project(project_id);
        let mut guard = 0usize;
        while let Some(project) = current {
            if project.parent_id.is_empty() || guard > self.projects.len() {
                break;
            }
            if project.parent_id == candidate {
                return true;
            }
            current = self.project(&project.parent_id);
            guard += 1;
        }
        false
    }

    /// 某项目的全部路径绑定。
    pub(crate) fn workspaces_of(&self, project_id: &str) -> Vec<RegistryWorkspace> {
        self.workspaces
            .iter()
            .filter(|workspace| workspace.project_id == project_id)
            .cloned()
            .collect()
    }

    /// 某条归一化路径当前绑定的项目 ID；未绑定返回空串。
    pub(crate) fn path_owner(&self, normalized: &str) -> String {
        for workspace in &self.workspaces {
            let Ok(existing) = normalize_path(&workspace.path) else {
                continue;
            };
            if paths_equal(&existing, normalized) {
                return workspace.project_id.clone();
            }
        }
        String::new()
    }

    /// 结构与关系校验，不碰文件系统。
    pub(crate) fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut project_ids = std::collections::HashSet::new();
        let mut aliases: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for project in &self.projects {
            if project.id.trim().is_empty() {
                problems.push("项目缺少 id".to_string());
                continue;
            }
            if !project_ids.insert(project.id.clone()) {
                problems.push(format!("项目 ID 重复：{}", project.id));
            }
            if project.name.trim().is_empty() {
                problems.push(format!("{} 缺少名称", project.id));
            }
            if !project.kind.is_empty() && !valid_kinds().contains(&project.kind.as_str()) {
                problems.push(format!("{} 的 kind 无效：{}", project.id, project.kind));
            }
            for alias in &project.aliases {
                let key = alias.trim().to_lowercase();
                if key.is_empty() {
                    continue;
                }
                if let Some(owner) = aliases.get(&key) {
                    if owner != &project.id {
                        problems.push(format!("别名 {alias:?} 同时属于 {owner} 与 {}", project.id));
                    }
                }
                aliases.insert(key, project.id.clone());
            }
        }

        for project in &self.projects {
            if project.parent_id.is_empty() {
                continue;
            }
            if !project_ids.contains(&project.parent_id) {
                problems.push(format!(
                    "{} 的父级不存在：{}",
                    project.id, project.parent_id
                ));
                continue;
            }
            if project.parent_id == project.id {
                problems.push(format!("{} 的父级是自己", project.id));
                continue;
            }
            if self.has_descendant(&project.parent_id, &project.id) {
                problems.push(format!(
                    "{} 与 {} 之间存在父子环",
                    project.id, project.parent_id
                ));
            }
        }

        let mut paths: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for workspace in &self.workspaces {
            if !project_ids.contains(&workspace.project_id) {
                problems.push(format!(
                    "路径 {} 指向不存在的项目 {}",
                    workspace.path, workspace.project_id
                ));
            }
            if !valid_roles().contains(&workspace.role.as_str()) {
                problems.push(format!(
                    "路径 {} 的 role 无效：{}",
                    workspace.path, workspace.role
                ));
            }
            if !valid_sources().contains(&workspace.source.as_str()) {
                problems.push(format!(
                    "路径 {} 的 source 无效：{}",
                    workspace.path, workspace.source
                ));
            }
            let key = path_key(&workspace.path);
            if let Some(owner) = paths.get(&key) {
                problems.push(format!(
                    "路径 {} 同时绑定到 {} 与 {}",
                    workspace.path, owner, workspace.project_id
                ));
            }
            paths.insert(key, workspace.project_id.clone());
        }
        problems.sort();
        problems
    }

    /// 读盘。文件不存在按空表处理——首次使用就是这样，不是错误。
    pub(crate) fn load() -> Result<Self, Box<dyn Error>> {
        Self::load_from(&registry_path())
    }

    /// 从指定文件读。主文件坏掉时回落到 `.bak`：一张登记表读不出来
    /// 不该让整个能力面瘫痪。
    pub(crate) fn load_from(path: &Path) -> Result<Self, Box<dyn Error>> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        match decode(&data) {
            Ok(registry) => Ok(registry),
            Err(primary) => {
                let backup = atomic_file::backup_path(path);
                if let Ok(fallback) = fs::read(&backup) {
                    if let Ok(registry) = decode(&fallback) {
                        return Ok(registry);
                    }
                }
                Err(primary)
            }
        }
    }

    /// 在跨进程锁内读改写。`mutate` 返回错误时不写盘。
    pub(crate) fn update(
        mutate: impl FnOnce(&mut Self) -> Result<(), Box<dyn Error>>,
    ) -> Result<Self, Box<dyn Error>> {
        Self::update_at(&registry_path(), mutate)
    }

    pub(crate) fn update_at(
        path: &Path,
        mutate: impl FnOnce(&mut Self) -> Result<(), Box<dyn Error>>,
    ) -> Result<Self, Box<dyn Error>> {
        let _lock = atomic_file::lock(path)?;
        let mut registry = Self::load_from(path)?;
        mutate(&mut registry)?;
        registry.schema_version = SCHEMA_VERSION.to_string();
        registry.updated_at = now_iso();
        let payload = serde_json::to_vec_pretty(&registry)?;
        let mut payload = payload;
        payload.push(b'\n');
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_file::atomic_write(path, &payload)?;
        Ok(registry)
    }
}

fn decode(data: &[u8]) -> Result<Registry, Box<dyn Error>> {
    let text = String::from_utf8_lossy(data);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Registry::default());
    }
    let mut registry: Registry =
        serde_json::from_str(trimmed).map_err(|error| format!("{ERR_SCHEMA_INVALID}: {error}"))?;
    if registry.schema_version.is_empty() {
        registry.schema_version = SCHEMA_VERSION.to_string();
    }
    Ok(registry)
}

// ---------------------------------------------------------------------------
// 路径归一化与解析
// ---------------------------------------------------------------------------

/// 平台是否大小写不敏感。只影响比较用的 key，不改落盘拼写。
fn case_insensitive() -> bool {
    cfg!(windows)
}

/// 把用户给的路径收敛成唯一写法。
///
/// 处理反斜杠、`\\?\` 长路径前缀、UNC（`\\server\share`）、盘符、尾斜杠、
/// `.` 与 `..`。**不解析符号链接**——登记表保存的是用户以为的路径。
///
/// 刻意不用 `Path::canonicalize`：那会解析符号链接、要求路径存在，
/// 而登记表必须能保存一个当前不可达的目录（拔掉的移动硬盘、被删的临时目录）。
pub(crate) fn normalize_path(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{ERR_PATH_NOT_ABSOLUTE}: 路径为空"));
    }
    let mut value = value.replace('\\', "/");

    // 长路径前缀：`//?/UNC/server/share`、`//?/C:/dir`、`//./C:/dir`。
    if value.starts_with("//?/UNC/") || value.to_lowercase().starts_with("//?/unc/") {
        value = format!("//{}", &value["//?/UNC/".len()..]);
    } else if let Some(rest) = value.strip_prefix("//?/") {
        value = rest.to_string();
    } else if let Some(rest) = value.strip_prefix("//./") {
        value = rest.to_string();
    }

    let (prefix, rest) = if let Some(rest) = value.strip_prefix("//") {
        // UNC：前两段是主机与共享名，必须原样保留。`path::clean` 会把
        // `//server/share` 压成 `/server/share`，UNC 当场失效。
        let mut host = String::new();
        let mut share = String::new();
        let mut tail: Vec<&str> = Vec::new();
        for part in rest.split('/') {
            match part {
                "" | "." => continue,
                ".." => return Err(format!("{ERR_PATH_INVALID}: UNC 路径不能越过共享根: {raw}")),
                _ if host.is_empty() => host = part.to_string(),
                _ if share.is_empty() => share = part.to_string(),
                other => tail.push(other),
            }
        }
        if host.is_empty() || share.is_empty() {
            return Err(format!(
                "{ERR_PATH_INVALID}: UNC 路径缺少主机或共享名: {raw}"
            ));
        }
        (format!("//{host}/{share}"), tail.join("/"))
    } else {
        let bytes = value.as_bytes();
        let drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
        if !drive {
            return Err(format!(
                "{ERR_PATH_NOT_ABSOLUTE}: 需要绝对路径（盘符或 UNC）: {raw}"
            ));
        }
        // 盘符必须带上冒号：只留字母会把 `F:/a` 拼成 `F/a`，UNC 判定和
        // 边界比较都会跟着错。
        let prefix = format!("{}:", value[..1].to_uppercase());
        let rest = &value[2..];
        if !rest.is_empty() && !rest.starts_with('/') {
            return Err(format!("{ERR_PATH_INVALID}: 盘符后缺少分隔符: {raw}"));
        }
        (prefix, rest.to_string())
    };

    let mut segments: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if segments.pop().is_none() {
                    return Err(format!("{ERR_PATH_INVALID}: 路径越过了根: {raw}"));
                }
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        // 盘符根写成 `C:/` 而不是 `C:`：后者在 Windows 上表示「该盘当前目录」。
        return Ok(if prefix.starts_with("//") {
            prefix
        } else {
            format!("{prefix}/")
        });
    }
    Ok(format!("{prefix}/{}", segments.join("/")))
}

fn path_key(path: &str) -> String {
    if case_insensitive() {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// 两个已归一化路径是否同一条。
pub(crate) fn paths_equal(left: &str, right: &str) -> bool {
    path_key(left) == path_key(right)
}

/// `child` 是否等于 `parent` 或位于其下，且落在路径边界上
/// （`A/SUB` 不能匹配 `A/SUBWAY`）。
pub(crate) fn is_under(child: &str, parent: &str) -> bool {
    let child_key = path_key(child);
    let mut parent_key = path_key(parent);
    if child_key == parent_key {
        return true;
    }
    if !parent_key.ends_with('/') {
        parent_key.push('/');
    }
    child_key.starts_with(&parent_key)
}

fn role_priority(role: &str) -> u8 {
    match role {
        ROLE_ROOT => 0,
        ROLE_SOURCE | ROLE_BUILD => 1,
        ROLE_WORKTREE | ROLE_MIRROR | ROLE_RELEASE => 2,
        _ => 3,
    }
}

fn source_priority(source: &str) -> u8 {
    match source {
        SOURCE_MANUAL => 0,
        SOURCE_IMPORT => 1,
        SOURCE_MANIFEST => 2,
        SOURCE_GIT => 3,
        SOURCE_SESSION => 4,
        _ => 5,
    }
}

/// 一次路径解析的命中结果。
#[derive(Debug, Clone)]
pub(crate) struct ResolvedHit {
    pub workspace: RegistryWorkspace,
    pub project: RegistryProject,
}

/// 用最长前缀把路径映射到某个已登记项目。
///
/// 命中多个同长度绑定时，按 role 优先级再按 source 优先级收敛；仍然并列
/// 说明数据本身冲突，返回 `Err(冲突描述)`，不静默挑一个。
pub(crate) fn resolve_path(
    registry: &Registry,
    normalized: &str,
) -> Result<Option<ResolvedHit>, String> {
    let mut hits: Vec<(usize, u8, u8, RegistryWorkspace, RegistryProject)> = Vec::new();
    for workspace in &registry.workspaces {
        let Ok(workspace_path) = normalize_path(&workspace.path) else {
            continue;
        };
        if !is_under(normalized, &workspace_path) {
            continue;
        }
        let Some(project) = registry.project(&workspace.project_id) else {
            continue;
        };
        hits.push((
            path_key(&workspace_path).len(),
            role_priority(&workspace.role),
            source_priority(&workspace.source),
            workspace.clone(),
            project.clone(),
        ));
    }
    if hits.is_empty() {
        return Ok(None);
    }
    hits.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    let best = &hits[0];
    for other in hits.iter().skip(1) {
        if other.0 != best.0 || other.1 != best.1 {
            break;
        }
        if other.4.id != best.4.id {
            return Err(format!(
                "{ERR_PATH_CONFLICT}: {normalized} 同时命中 {} 与 {}",
                best.4.id, other.4.id
            ));
        }
    }
    let (_, _, _, workspace, project) = &hits[0];
    Ok(Some(ResolvedHit {
        workspace: workspace.clone(),
        project: project.clone(),
    }))
}

/// 用目录名的常见后缀推断该路径是不是 worktree / 发布目录。
/// 只给候选打标签，不参与解析判定。
pub(crate) fn worktree_hint(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("-release-") || lower.ends_with("-release") {
        ROLE_RELEASE
    } else if lower.contains("-worktree-") || lower.ends_with("-worktree") {
        ROLE_WORKTREE
    } else if lower.ends_with("-mirror") || lower.contains("-mirror-") {
        ROLE_MIRROR
    } else {
        ""
    }
}

fn now_iso() -> String {
    // 与仓里其它时间戳一致：秒级精度 + `Z`，不写毫秒与偏移量。
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// 写入：项目、绑定、候选
// ---------------------------------------------------------------------------

/// 父级字段的三态：没传（保持原值）、显式设成某个父级、显式清空成根。
///
/// 用 `Option<String>` 做不到这件事：JSON 里的 `null` 和字段缺失都会解成
/// `None`，于是「把项目提升为根」和「什么都不改」变成同一个请求。
/// 父级这种可清空的字段必须能表达两种意图。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum ParentField {
    /// 字段没出现：保持原值。
    #[default]
    Missing,
    /// 字段出现：`null` 与 `""` 都表示「提升为根」，其余是父级 ID。
    Set(String),
}

impl<'de> Deserialize<'de> for ParentField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // 字段一旦进入这里就说明它出现过，`null` 解成空串。
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(Self::Set(value.unwrap_or_default()))
    }
}

impl Serialize for ParentField {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Missing => serializer.serialize_none(),
            Self::Set(value) => serializer.serialize_str(value),
        }
    }
}

/// `project.upsert` 的输入。零值字段表示「不改」。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectInput {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub kind: String,
    pub status: String,
    pub parent_id: ParentField,
    pub color: String,
    pub description: String,
    pub stack: Vec<String>,
    pub commands: Vec<RegistryCommand>,
    pub docs: Vec<RegistryLink>,
    pub rules: Vec<RegistryLink>,
    pub notes: String,
    pub work_log_project_id: String,
    pub work_log_project_name: String,
    pub workbench_project_id: String,
}

/// `path.bind` 的输入。`unbind=true` 时只按 `path` 移除。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct BindInput {
    pub project_ref: String,
    pub path: String,
    pub role: String,
    pub source: String,
    pub confidence: String,
    pub unbind: bool,
}

fn next_id(prefix: &str) -> String {
    // 时间戳只到毫秒不够：Windows 的时钟按定时器周期推进，同一毫秒内连续
    // 两次调用会拿到同一个值。加随机后缀后同毫秒内也能区分，且跨进程安全。
    let mut bytes = [0u8; 4];
    let _ = rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{prefix}-{}-{suffix}",
        chrono::Utc::now().timestamp_millis()
    )
}

fn normalize_list(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        out.push(value);
    }
    out
}

/// 新建或修改项目。`parent_id` 用 [`ParentField`] 区分「没传」与「显式设为根」。
pub(crate) fn upsert_project(
    registry: &mut Registry,
    input: ProjectInput,
) -> Result<RegistryProject, Box<dyn Error>> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(format!("{ERR_SCHEMA_INVALID}: name 必填").into());
    }
    let kind = if input.kind.trim().is_empty() {
        KIND_OTHER.to_string()
    } else {
        input.kind.trim().to_string()
    };
    if !valid_kinds().contains(&kind.as_str()) {
        return Err(format!("{ERR_SCHEMA_INVALID}: 不支持的 kind {kind:?}").into());
    }

    let existing_index = {
        let by_id = input.id.trim();
        if !by_id.is_empty() {
            match registry.projects.iter().position(|item| item.id == by_id) {
                Some(index) => Some(index),
                None => return Err(format!("{ERR_PROJECT_NOT_FOUND}: {by_id}").into()),
            }
        } else {
            let lower = name.to_lowercase();
            registry.projects.iter().position(|item| {
                item.name.to_lowercase() == lower
                    || item
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase() == lower)
            })
        }
    };

    let project_id = match existing_index {
        Some(index) => registry.projects[index].id.clone(),
        None => {
            let id = next_id("prj");
            registry.projects.push(RegistryProject {
                id: id.clone(),
                status: "active".to_string(),
                created_at: now_iso(),
                ..Default::default()
            });
            id
        }
    };

    // 别名冲突要在写入前拦住：两条记录抢同一个别名，解析结果就取决于顺序。
    let mut cleaned_aliases = Vec::new();
    let mut seen_alias = std::collections::HashSet::new();
    seen_alias.insert(name.to_lowercase());
    for alias in &input.aliases {
        let alias = alias.trim().to_string();
        if alias.is_empty() {
            continue;
        }
        let key = alias.to_lowercase();
        if !seen_alias.insert(key) {
            continue;
        }
        if let Some(owner) = registry.find_project(&alias) {
            if owner.id != project_id {
                return Err(
                    format!("{ERR_ALIAS_CONFLICT}: 别名 {alias:?} 已属于 {}", owner.name).into(),
                );
            }
        }
        cleaned_aliases.push(alias);
    }

    let parent = match &input.parent_id {
        ParentField::Missing => None,
        ParentField::Set(value) => {
            let parent_id = value.trim().to_string();
            if !parent_id.is_empty() {
                if parent_id == project_id {
                    return Err(format!("{ERR_PARENT_CYCLE}: 项目不能以自己为父级").into());
                }
                if registry.project(&parent_id).is_none() {
                    return Err(format!("{ERR_PROJECT_NOT_FOUND}: {parent_id}").into());
                }
                if registry.has_descendant(&parent_id, &project_id) {
                    return Err(
                        format!("{ERR_PARENT_CYCLE}: {parent_id} 是 {project_id} 的后代").into(),
                    );
                }
            }
            Some(parent_id)
        }
    };

    let updated_at = now_iso();
    let project = registry
        .project_mut(&project_id)
        .ok_or("项目刚创建就消失了")?;
    if let Some(parent_id) = parent {
        project.parent_id = parent_id;
    }
    project.name = name;
    project.aliases = cleaned_aliases;
    project.kind = kind;
    if !input.status.trim().is_empty() {
        project.status = input.status.trim().to_string();
    } else if project.status.is_empty() {
        project.status = "active".to_string();
    }
    if !input.color.is_empty() {
        project.color = input.color.clone();
    }
    if !input.description.is_empty() {
        project.description = input.description.clone();
    }
    if !input.stack.is_empty() {
        project.context.stack = normalize_list(&input.stack);
    }
    if !input.commands.is_empty() {
        project.context.commands = input.commands.clone();
    }
    if !input.docs.is_empty() {
        project.context.docs = input.docs.clone();
    }
    if !input.rules.is_empty() {
        project.context.rules = input.rules.clone();
    }
    if !input.notes.is_empty() {
        project.context.notes = input.notes.clone();
    }
    if !input.work_log_project_id.is_empty() {
        project.links.work_log_project_id = input.work_log_project_id.clone();
    }
    if !input.work_log_project_name.is_empty() {
        project.links.work_log_project_name = input.work_log_project_name.clone();
    }
    if !input.workbench_project_id.is_empty() {
        project.links.workbench_project_id = input.workbench_project_id.clone();
    }
    project.updated_at = updated_at;
    Ok(project.clone())
}

/// 删除项目。有子项目或路径绑定时默认拒绝——静默级联删除会让
/// 「一个路径对应什么项目」这种基础事实悄悄消失，比报错严重得多。
pub(crate) fn delete_project(
    registry: &mut Registry,
    project_id: &str,
    force: bool,
) -> Result<(), Box<dyn Error>> {
    if registry.project(project_id).is_none() {
        return Err(format!("{ERR_PROJECT_NOT_FOUND}: {project_id}").into());
    }
    let children = registry.children(project_id);
    if !children.is_empty() && !force {
        let names: Vec<&str> = children.iter().map(|child| child.name.as_str()).collect();
        return Err(format!(
            "{ERR_PROJECT_HAS_CHILDREN}: 仍有子项目 {}",
            names.join("、")
        )
        .into());
    }
    let bound = registry.workspaces_of(project_id);
    if !bound.is_empty() && !force {
        return Err(format!(
            "{ERR_PROJECT_HAS_WORKSPACES}: 仍有 {} 条路径绑定",
            bound.len()
        )
        .into());
    }
    // 强制删除时把子项目提升为根，不做级联删除：用户可能只是想拆掉这层。
    let updated_at = now_iso();
    for child in children {
        if let Some(target) = registry.project_mut(&child.id) {
            target.parent_id.clear();
            target.updated_at = updated_at.clone();
        }
    }
    registry.projects.retain(|item| item.id != project_id);
    registry
        .workspaces
        .retain(|item| item.project_id != project_id);
    Ok(())
}

/// 绑定或解绑一条路径。
pub(crate) fn bind_path(
    registry: &mut Registry,
    input: BindInput,
) -> Result<RegistryWorkspace, Box<dyn Error>> {
    let normalized = normalize_path(&input.path)?;

    if input.unbind {
        let mut removed = None;
        registry.workspaces.retain(|item| {
            let Ok(existing) = normalize_path(&item.path) else {
                return true;
            };
            if paths_equal(&existing, &normalized) {
                removed = Some(item.clone());
                return false;
            }
            true
        });
        return removed
            .ok_or_else(|| format!("{ERR_PROJECT_NOT_FOUND}: {normalized} 未绑定").into());
    }

    let project = registry
        .find_project(&input.project_ref)
        .ok_or_else(|| format!("{ERR_PROJECT_NOT_FOUND}: {}", input.project_ref))?
        .clone();

    let owner = registry.path_owner(&normalized);
    if !owner.is_empty() && owner != project.id {
        return Err(format!("{ERR_PATH_CONFLICT}: {normalized} 已绑定到 {owner}").into());
    }

    let role = if input.role.trim().is_empty() {
        ROLE_ROOT.to_string()
    } else {
        input.role.trim().to_string()
    };
    if !valid_roles().contains(&role.as_str()) {
        return Err(format!("{ERR_SCHEMA_INVALID}: 不支持的 role {role:?}").into());
    }
    let source = if input.source.trim().is_empty() {
        SOURCE_MANUAL.to_string()
    } else {
        input.source.trim().to_string()
    };
    if !valid_sources().contains(&source.as_str()) {
        return Err(format!("{ERR_SCHEMA_INVALID}: 不支持的 source {source:?}").into());
    }
    let confidence = if input.confidence.trim().is_empty() {
        CONFIDENCE_EXACT.to_string()
    } else {
        input.confidence.trim().to_string()
    };
    if confidence != CONFIDENCE_EXACT && confidence != CONFIDENCE_INFERRED {
        return Err(format!("{ERR_SCHEMA_INVALID}: 不支持的 confidence {confidence:?}").into());
    }

    // 同一路径重复绑定视为「改绑」，不是新增。
    for workspace in registry.workspaces.iter_mut() {
        let Ok(existing) = normalize_path(&workspace.path) else {
            continue;
        };
        if !paths_equal(&existing, &normalized) {
            continue;
        }
        workspace.project_id = project.id.clone();
        workspace.role = role;
        workspace.source = source;
        workspace.confidence = confidence;
        return Ok(workspace.clone());
    }

    let workspace = RegistryWorkspace {
        id: next_id("wsp"),
        project_id: project.id,
        path: normalized,
        role,
        source,
        confidence,
        created_at: now_iso(),
    };
    registry.workspaces.push(workspace.clone());
    Ok(workspace)
}

/// 未命中时给出「可能是谁」的建议。
///
/// 只读少量已知标记（`.git`、`.svn`、`.himind/project.json`、`extensions.json`），
/// 不递归扫描目录树。
pub(crate) fn suggest_candidates(registry: &Registry, normalized: &str) -> Vec<RegistryCandidate> {
    let mut candidates: Vec<RegistryCandidate> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut push = |path: &str, project: &str, parent: &str, confidence: &str, source: &str| {
        let key = format!("{}|{project}", path_key(path));
        if !seen.insert(key) {
            return;
        }
        candidates.push(RegistryCandidate {
            path: path.to_string(),
            suggested_project: project.to_string(),
            suggested_parent: parent.to_string(),
            confidence: confidence.to_string(),
            source: source.to_string(),
            seen_at: now_iso(),
            seen_count: 1,
            status: CANDIDATE_PENDING.to_string(),
        });
    };

    // ① 向上找工程标记，取最近的一个作为项目根。
    let mut current = normalized.to_string();
    for _ in 0..8 {
        if has_workspace_marker(&current) {
            let name = file_name_of(&current);
            let parent = parent_project_of(registry, &current);
            push(&current, &name, &parent, CONFIDENCE_INFERRED, SOURCE_GIT);
            break;
        }
        let parent = parent_path_of(&current);
        if parent == current || parent.is_empty() {
            break;
        }
        current = parent;
    }

    // ② 路径末段与已登记项目名/别名精确一致，且该路径尚未被占用。
    let base = file_name_of(normalized).to_lowercase();
    if !base.is_empty() && registry.path_owner(normalized).is_empty() {
        if let Some(project) = registry.projects.iter().find(|project| {
            project.name.to_lowercase() == base
                || project
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == base)
        }) {
            push(
                normalized,
                &project.name.clone(),
                &project.parent_id.clone(),
                CONFIDENCE_EXACT,
                SOURCE_SESSION,
            );
        }
    }
    candidates
}

fn has_workspace_marker(path: &str) -> bool {
    let local = Path::new(path);
    [
        ".git",
        ".svn",
        ".hg",
        ".himind/project.json",
        "extensions.json",
    ]
    .iter()
    .any(|name| local.join(name).exists())
}

fn file_name_of(normalized: &str) -> String {
    normalized
        .rsplit('/')
        .next()
        .unwrap_or(normalized)
        .to_string()
}

fn parent_path_of(normalized: &str) -> String {
    let Some(index) = normalized.rfind('/') else {
        return String::new();
    };
    // UNC 共享根与盘符根都要保留原形态。
    if let Some(rest) = normalized.strip_prefix("//") {
        if rest.matches('/').count() < 2 {
            return normalized.to_string();
        }
    }
    if index == 2 && normalized.as_bytes().get(1) == Some(&b':') {
        return normalized[..3].to_string();
    }
    let parent = &normalized[..index];
    if parent.is_empty() {
        "/".to_string()
    } else {
        parent.to_string()
    }
}

fn parent_project_of(registry: &Registry, normalized: &str) -> String {
    let mut parent = parent_path_of(normalized);
    while !parent.is_empty() && parent != normalized {
        if let Ok(Some(hit)) = resolve_path(registry, &parent) {
            return hit.project.id;
        }
        let next = parent_path_of(&parent);
        if next == parent {
            break;
        }
        parent = next;
    }
    String::new()
}

/// 把候选并入登记表；同一路径重复出现只累加次数。
pub(crate) fn record_candidates(registry: &mut Registry, incoming: Vec<RegistryCandidate>) {
    for candidate in incoming {
        let Ok(normalized) = normalize_path(&candidate.path) else {
            continue;
        };
        let mut merged = false;
        for existing in registry.candidates.iter_mut() {
            let Ok(existing_path) = normalize_path(&existing.path) else {
                continue;
            };
            if !paths_equal(&existing_path, &normalized) {
                continue;
            }
            merged = true;
            if existing.status == CANDIDATE_PENDING {
                existing.seen_count += 1;
                existing.seen_at = candidate.seen_at.clone();
                if !candidate.suggested_project.is_empty() {
                    existing.suggested_project = candidate.suggested_project.clone();
                }
            }
            break;
        }
        if !merged {
            registry.candidates.push(RegistryCandidate {
                path: normalized,
                ..candidate
            });
        }
    }
}

fn mark_candidate(registry: &mut Registry, normalized: &str, status: &str) {
    let seen_at = now_iso();
    for candidate in registry.candidates.iter_mut() {
        let Ok(existing) = normalize_path(&candidate.path) else {
            continue;
        };
        if paths_equal(&existing, normalized) {
            candidate.status = status.to_string();
            candidate.seen_at = seen_at.clone();
        }
    }
}

fn pending_candidate(registry: &Registry, normalized: &str) -> Option<RegistryCandidate> {
    registry.candidates.iter().find_map(|candidate| {
        let existing = normalize_path(&candidate.path).ok()?;
        (paths_equal(&existing, normalized) && candidate.status == CANDIDATE_PENDING)
            .then(|| candidate.clone())
    })
}

/// 把候选落成项目或路径绑定。
///
/// `mode=bind` 要求候选能匹配到已有项目；`mode=create` 会新建项目再绑定。
pub(crate) fn accept_candidate(
    registry: &mut Registry,
    path: &str,
    mode: &str,
    project_ref: &str,
    parent_ref: &str,
) -> Result<(RegistryProject, RegistryWorkspace), Box<dyn Error>> {
    let normalized = normalize_path(path)?;
    let candidate = pending_candidate(registry, &normalized);
    let mode = if mode.trim().is_empty() {
        "create"
    } else {
        mode.trim()
    };

    let (project, workspace) = match mode {
        "bind" => {
            let target = if !project_ref.trim().is_empty() {
                registry.find_project(project_ref).cloned()
            } else {
                candidate
                    .as_ref()
                    .and_then(|item| registry.find_project(&item.suggested_project).cloned())
            };
            let target = target.ok_or_else(|| format!("{ERR_PROJECT_NOT_FOUND}: {project_ref}"))?;
            let workspace = bind_path(
                registry,
                BindInput {
                    project_ref: target.id.clone(),
                    path: normalized.clone(),
                    source: SOURCE_SESSION.to_string(),
                    confidence: CONFIDENCE_EXACT.to_string(),
                    ..Default::default()
                },
            )?;
            (target, workspace)
        }
        "create" => {
            let mut name = project_ref.trim().to_string();
            if name.is_empty() {
                if let Some(item) = candidate.as_ref() {
                    name = item.suggested_project.clone();
                }
            }
            if name.is_empty() {
                name = file_name_of(&normalized);
            }
            let mut parent = parent_ref.trim().to_string();
            if parent.is_empty() {
                if let Some(item) = candidate.as_ref() {
                    parent = item.suggested_parent.clone();
                }
            }
            let created = upsert_project(
                registry,
                ProjectInput {
                    name,
                    kind: if worktree_hint(&file_name_of(&normalized)).is_empty() {
                        KIND_PRODUCT.to_string()
                    } else {
                        KIND_DELIVERY.to_string()
                    },
                    parent_id: ParentField::Set(parent),
                    ..Default::default()
                },
            )?;
            let workspace = bind_path(
                registry,
                BindInput {
                    project_ref: created.id.clone(),
                    path: normalized.clone(),
                    source: SOURCE_SESSION.to_string(),
                    confidence: CONFIDENCE_EXACT.to_string(),
                    ..Default::default()
                },
            )?;
            (created, workspace)
        }
        other => {
            return Err(
                format!("{ERR_SCHEMA_INVALID}: mode 只能是 create 或 bind，收到 {other}").into(),
            );
        }
    };

    mark_candidate(registry, &normalized, CANDIDATE_ACCEPTED);
    Ok((project, workspace))
}

/// 只打标不删数据：用户否掉过一次，不必每轮再提示。
pub(crate) fn dismiss_candidate(registry: &mut Registry, path: &str) -> Result<(), Box<dyn Error>> {
    let normalized = normalize_path(path)?;
    if pending_candidate(registry, &normalized).is_none() {
        return Err(format!("{ERR_PROJECT_NOT_FOUND}: {normalized} 不在候选里").into());
    }
    mark_candidate(registry, &normalized, CANDIDATE_DISMISSED);
    Ok(())
}

// ---------------------------------------------------------------------------
// 读取：快照与 brief
// ---------------------------------------------------------------------------

/// `resolve` / `context.get` 的返回结构。
///
/// 把「命中了什么」与「为什么这么判」分开：技能要能区分精确命中与前缀匹配，
/// 再决定要不要打扰用户。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RegistrySnapshot {
    pub matched: bool,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<RegistryWorkspace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<RegistryProject>,
    pub ancestors: Vec<RegistryProject>,
    pub children: Vec<RegistryProject>,
    pub siblings: Vec<RegistryProject>,
    pub resolution: RegistryResolution,
    pub candidates: Vec<RegistryCandidate>,
    /// 恒为 false：登记只影响命名与上下文，不改变会话写权限。
    pub writable: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RegistryResolution {
    pub strategy: String,
    pub confidence: String,
    pub warnings: Vec<String>,
}

/// 解析一条路径。未命中时返回候选而不是报错——「没登记」是正常状态。
pub(crate) fn resolve_snapshot(
    registry: &Registry,
    raw_path: &str,
) -> Result<RegistrySnapshot, Box<dyn Error>> {
    let normalized = normalize_path(raw_path)?;
    let mut snapshot = RegistrySnapshot {
        matched: false,
        query: normalized.clone(),
        workspace: None,
        project: None,
        ancestors: Vec::new(),
        children: Vec::new(),
        siblings: Vec::new(),
        resolution: RegistryResolution {
            strategy: "none".to_string(),
            confidence: CONFIDENCE_INFERRED.to_string(),
            warnings: Vec::new(),
        },
        candidates: Vec::new(),
        writable: false,
        hint: String::new(),
    };

    match resolve_path(registry, &normalized)? {
        Some(hit) => {
            snapshot.matched = true;
            snapshot.resolution.strategy = "longest_prefix".to_string();
            snapshot.resolution.confidence = hit.workspace.confidence.clone();
            if hit.workspace.confidence == CONFIDENCE_INFERRED {
                snapshot.resolution.warnings.push(
                    "该路径绑定来自自动发现，名称与父级可能不准，可在工作区页面确认".to_string(),
                );
            }
            snapshot.ancestors = registry.ancestors(&hit.project.id);
            snapshot.children = registry.children(&hit.project.id);
            snapshot.siblings = if hit.project.parent_id.is_empty() {
                Vec::new()
            } else {
                registry
                    .children(&hit.project.parent_id)
                    .into_iter()
                    .filter(|item| item.id != hit.project.id)
                    .collect()
            };
            snapshot.workspace = Some(hit.workspace);
            snapshot.project = Some(hit.project);
        }
        None => {
            snapshot.candidates = suggest_candidates(registry, &normalized);
            snapshot.hint =
                "未命中登记表。这就是全部信息——不要为了有结果而临时造一个项目。".to_string();
        }
    }
    Ok(snapshot)
}

/// 生成 ≤500 字的 agent 可读摘要。
///
/// 它是索引，不是知识库：读到命令与文档路径后仍应打开真实文件。
pub(crate) fn brief(snapshot: &RegistrySnapshot) -> String {
    let Some(project) = snapshot.project.as_ref() else {
        return format!(
            "[工作区] {}\n[项目] 未登记\n[提示] 不要猜测项目名；需要登记请先与用户确认。",
            snapshot.query
        );
    };
    let mut lines = vec![
        format!("[工作区] {}", snapshot.query),
        format_project_line(project),
    ];
    if !snapshot.ancestors.is_empty() {
        let names: Vec<&str> = snapshot
            .ancestors
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        lines.push(format!("[上级] {}", names.join(" ← ")));
    }
    if !snapshot.siblings.is_empty() {
        let names: Vec<&str> = snapshot
            .siblings
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        lines.push(format!("[同级] {}", names.join("、")));
    }
    if !project.context.stack.is_empty() {
        lines.push(format!("[技术栈] {}", project.context.stack.join(" / ")));
    }
    if !project.context.docs.is_empty() {
        let paths: Vec<&str> = project
            .context
            .docs
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        lines.push(format!("[文档] {}", paths.join("、")));
    }
    if !project.context.rules.is_empty() {
        let paths: Vec<&str> = project
            .context
            .rules
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        lines.push(format!("[规则] {}", paths.join("、")));
    }
    if !project.links.work_log_project_name.is_empty() {
        lines.push(format!(
            "[日志项目] {}",
            project.links.work_log_project_name
        ));
    }
    if !project.context.notes.is_empty() {
        lines.push(format!("[备注] {}", project.context.notes));
    }
    lines.push("[提示] 这是索引；执行命令前请读实际文件。".to_string());
    truncate_chars(&lines.join("\n"), 500)
}

fn format_project_line(project: &RegistryProject) -> String {
    let mut line = format!("[项目] {}", project.name);
    if !project.aliases.is_empty() {
        line.push_str(&format!(" ({})", project.aliases.join(" / ")));
    }
    let kind = if project.kind.is_empty() {
        KIND_OTHER
    } else {
        project.kind.as_str()
    };
    line.push_str(&format!(" · {kind}"));
    if !project.status.is_empty() {
        line.push_str(&format!(" · {}", project.status));
    }
    line
}

/// 按字符截断：中文按字节切会切坏。
fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut out: String = value.chars().take(limit).collect();
    out.push('…');
    out
}

/// 展示用的项目树。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RegistryTreeNode {
    pub project: RegistryProject,
    pub paths: Vec<RegistryWorkspace>,
    pub children: Vec<RegistryTreeNode>,
}

pub(crate) fn build_tree(registry: &Registry) -> Vec<RegistryTreeNode> {
    let ids: std::collections::HashSet<&str> = registry
        .projects
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    let mut roots: Vec<&RegistryProject> = registry
        .projects
        .iter()
        .filter(|project| project.parent_id.is_empty() || !ids.contains(project.parent_id.as_str()))
        .collect();
    roots.sort_by(|left, right| left.name.cmp(&right.name));
    roots
        .into_iter()
        .map(|project| build_node(registry, project, 0))
        .collect()
}

fn build_node(registry: &Registry, project: &RegistryProject, depth: usize) -> RegistryTreeNode {
    let children = if depth > registry.projects.len() {
        Vec::new()
    } else {
        registry
            .children(&project.id)
            .iter()
            .map(|child| build_node(registry, child, depth + 1))
            .collect()
    };
    RegistryTreeNode {
        project: project.clone(),
        paths: registry.workspaces_of(&project.id),
        children,
    }
}

/// `validate` 的返回：结构问题 + 文件系统里已失效的路径。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RegistryValidation {
    pub ok: bool,
    pub problems: Vec<String>,
    pub missing_paths: Vec<String>,
    pub project_count: usize,
    pub workspace_count: usize,
}

pub(crate) fn validate_registry(registry: &Registry) -> RegistryValidation {
    let problems = registry.validate();
    let missing_paths: Vec<String> = registry
        .workspaces
        .iter()
        .filter(|workspace| !Path::new(&workspace.path).is_dir())
        .map(|workspace| workspace.path.clone())
        .collect();
    RegistryValidation {
        ok: problems.is_empty() && missing_paths.is_empty(),
        problems,
        missing_paths,
        project_count: registry.projects.len(),
        workspace_count: registry.workspaces.len(),
    }
}

/// 列表视图的返回：树 + 平铺项目 + 绑定 + 更新时间。
pub(crate) fn list_view(registry: &Registry) -> Value {
    serde_json::json!({
        "tree": build_tree(registry),
        "projects": registry.projects,
        "workspaces": registry.workspaces,
        "updated_at": registry.updated_at,
    })
}

/// `workspace.registry.resolve` 的实现。
///
/// `record` 为真时把未命中产生的候选并入登记表——这是「发现」，
/// 与「决定」（建项目、绑路径）是两件事，调用方可以分开触发。
pub(crate) fn capability_resolve(
    input: &Value,
    record: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let workspace_root = workspace_root_arg(input)?;
    let registry = Registry::load()?;
    let snapshot = resolve_snapshot(&registry, &workspace_root)?;
    if record && !snapshot.candidates.is_empty() {
        // 候选写盘失败不该让解析整体失败：解析结果本身已经拿到了。
        let candidates = snapshot.candidates.clone();
        let _ = Registry::update(|registry| {
            record_candidates(registry, candidates);
            Ok(())
        });
    }
    Ok(serde_json::to_value(snapshot)?)
}

/// `workspace.registry.context.get` 的实现：解析 + 生成 brief。
pub(crate) fn capability_context(
    input: &Value,
    record: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let workspace_root = workspace_root_arg(input)?;
    let registry = Registry::load()?;
    let snapshot = resolve_snapshot(&registry, &workspace_root)?;
    if record && !snapshot.candidates.is_empty() {
        let candidates = snapshot.candidates.clone();
        let _ = Registry::update(|registry| {
            record_candidates(registry, candidates);
            Ok(())
        });
    }
    let text = brief(&snapshot);
    Ok(serde_json::json!({
        "matched": snapshot.matched,
        "brief": text,
        "snapshot": snapshot,
    }))
}

fn workspace_root_arg(input: &Value) -> Result<String, Box<dyn std::error::Error>> {
    input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "workspace_root 必填：由调用方显式传入，内核不从环境变量猜".into())
}

#[cfg(test)]
mod tests;
