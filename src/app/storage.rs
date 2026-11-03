//! 存储占用盘点与可重建内容清理。
//!
//! 为什么先做清理而不是「迁移数据目录」：Agent 数据根里体积最大的一块是拓展
//! 开发草稿（每个版本一份解包产物），其次是可重新下载的程序版本与运行时。
//! 这些内容都能重建，删掉立刻见效，而且不碰任何用户配置、凭据和工作区。
//!
//! 三条不能妥协的规则：
//!
//! - **只删自己能证明来历的目录。** 草稿版本目录必须带 `draft.json` 才算数；
//!   程序版本目录必须能被 `install_layout::validate_version` 接受。
//! - **正在用的不动。** 已登记为开发插件的草稿、当前运行的程序版本、`active-version`
//!   指向的版本与它的上一版，一律跳过。
//! - **删除落在可信根内。** 每条删除都先经 [`TrustedRoot`] 解析真实路径，
//!   目录联接（junction）不能把删除带到数据根之外。

use crate::path_guard::TrustedRoot;
use crate::store::paths;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 每个插件默认保留的草稿版本数。
pub(crate) const DRAFT_KEEP_DEFAULT: usize = 2;
/// 保留数上限：再多就不是「清理历史」而是「什么都不删」。
pub(crate) const DRAFT_KEEP_MAX: usize = 5;
/// 缓存与临时文件只删这么久没动过的：安装、解包可能正在写。
const MIN_REBUILDABLE_AGE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StorageEntry {
    pub key: String,
    pub label: String,
    pub path: String,
    pub bytes: u64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CleanupOption {
    pub key: String,
    pub label: String,
    pub path: String,
    pub bytes: u64,
    pub entries: usize,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StorageReport {
    pub profile: String,
    pub root: String,
    pub install_root: String,
    /// 当前 profile 的数据目录 + 落在它之外的程序目录，不重复计数。
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub other_profiles_bytes: u64,
    pub entries: Vec<StorageEntry>,
    pub cleanup: Vec<CleanupOption>,
    pub draft_keep: usize,
    /// 数据位置（当前、默认、是否能改、排队中的迁移、上次结果）。
    pub location: crate::app::data_location::DataLocationStatus,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CleanupOutcome {
    pub removed: usize,
    pub freed_bytes: u64,
    pub failures: Vec<String>,
    pub report: StorageReport,
}

#[derive(Debug, Default)]
struct CleanupPlan {
    /// (真实路径, 会释放的字节数)
    items: Vec<(PathBuf, u64)>,
}

impl CleanupPlan {
    fn bytes(&self) -> u64 {
        self.items.iter().map(|(_, bytes)| *bytes).sum()
    }
}

/// 一条可清理项。计划失败时仍然给出条目（字数为 0），界面据此显示「没有可清理
/// 的内容」，而不是整块消失或留下一个空标题。
fn cleanup_option(
    key: &str,
    label: &str,
    path: &Path,
    note: &str,
    plan: Result<CleanupPlan, Box<dyn Error>>,
) -> CleanupOption {
    let plan = plan.unwrap_or_default();
    CleanupOption {
        key: key.to_string(),
        label: label.to_string(),
        path: path.to_string_lossy().to_string(),
        bytes: plan.bytes(),
        entries: plan.items.len(),
        note: note.to_string(),
    }
}

/// 当前 profile 的存储占用与可清理量。读取失败按 0 处理：这一屏是只读盘点，
/// 某个子目录不存在或暂时读不到不该让整页打不开。
pub(crate) fn report(keep: usize) -> Result<StorageReport, Box<dyn Error>> {
    let keep = clamp_keep(keep);
    let root = paths::agent_home();
    let install_root = paths::shared_agent_root();
    let profile = paths::profile_name();

    let program_dirs = ["versions", "runtimes", "resources", "previous", "staging"];
    let mut entries = Vec::new();
    let data_rows = [
        ("data", "配置与状态", "设置、凭据与运行状态"),
        ("local-runs.sqlite3", "运行历史", "任务与工作流运行记录"),
        ("media-inbox", "媒体收件箱", "生成并留在本机的图片与视频"),
        ("plugins", "已安装拓展", "已安装的拓展内容"),
        ("plugin-data", "拓展运行数据", "拓展自己的运行数据"),
        ("plugin-drafts", "拓展开发草稿", "每个版本一份解包产物"),
        ("skill-drafts", "技能开发草稿", "开发中的技能草稿"),
        ("workflow-drafts", "工作流开发草稿", "开发中的工作流草稿"),
        ("cache", "缓存", "可重建的解析与下载缓存"),
        ("tmp", "临时文件", "安装与解包临时目录"),
        ("logs", "日志", "运行日志，可导出诊断"),
    ];
    for (name, label, note) in data_rows {
        let path = root.join(name);
        entries.push(StorageEntry {
            key: name.to_string(),
            label: label.to_string(),
            bytes: path_size(&path),
            path: path.to_string_lossy().to_string(),
            note: note.to_string(),
        });
    }
    for (name, label, note) in [
        ("versions", "程序版本", "可回退的程序版本"),
        ("runtimes", "内置运行时", "内置 AI 运行时"),
        ("resources", "随包资源", "随安装包分发的资源"),
    ] {
        let path = install_root.join(name);
        entries.push(StorageEntry {
            key: format!("install:{name}"),
            label: label.to_string(),
            bytes: path_size(&path),
            path: path.to_string_lossy().to_string(),
            note: note.to_string(),
        });
    }

    let other_profiles_bytes = other_profiles_size(&install_root, &root);
    if other_profiles_bytes > 0 {
        let path = install_root.join("profiles");
        entries.push(StorageEntry {
            key: "profiles".to_string(),
            label: "其它 profile".to_string(),
            bytes: other_profiles_bytes,
            path: path.to_string_lossy().to_string(),
            note: "各自独立的数据目录".to_string(),
        });
    }

    let mut total_bytes = path_size(&root);
    for name in program_dirs {
        let path = install_root.join(name);
        if !path.starts_with(&root) {
            total_bytes = total_bytes.saturating_add(path_size(&path));
        }
    }
    if !install_root.join("profiles").starts_with(&root) {
        total_bytes = total_bytes.saturating_add(other_profiles_bytes);
    }

    let drafts_path = root.join("plugin-drafts");
    let versions_path = install_root.join("versions");

    Ok(StorageReport {
        profile,
        root: root.to_string_lossy().to_string(),
        install_root: install_root.to_string_lossy().to_string(),
        total_bytes,
        free_bytes: free_bytes(&root),
        other_profiles_bytes,
        entries,
        cleanup: vec![
            cleanup_option(
                "drafts",
                "拓展开发草稿",
                &drafts_path,
                &format!("每个插件保留最近 {keep} 个版本"),
                plan_drafts_in(&drafts_path, keep, &protected_draft_paths()),
            ),
            cleanup_option(
                "versions",
                "程序旧版本",
                &versions_path,
                "保留当前与上一版本",
                plan_versions_in(&install_root),
            ),
            cleanup_option(
                "cache",
                "缓存与临时文件",
                &root,
                "10 分钟内改动过的保留",
                plan_rebuildable_in(&root),
            ),
        ],
        draft_keep: keep,
        location: crate::app::data_location::status(),
    })
}

pub(crate) fn cleanup_drafts(keep: usize) -> Result<CleanupOutcome, Box<dyn Error>> {
    let keep = clamp_keep(keep);
    let root = paths::agent_home();
    let plan = plan_drafts_in(&root.join("plugin-drafts"), keep, &protected_draft_paths())?;
    run(plan, keep)
}

pub(crate) fn cleanup_versions() -> Result<CleanupOutcome, Box<dyn Error>> {
    let plan = plan_versions_in(&paths::shared_agent_root())?;
    run(plan, DRAFT_KEEP_DEFAULT)
}

pub(crate) fn cleanup_cache() -> Result<CleanupOutcome, Box<dyn Error>> {
    let plan = plan_rebuildable_in(&paths::agent_home())?;
    run(plan, DRAFT_KEEP_DEFAULT)
}

fn run(plan: CleanupPlan, keep: usize) -> Result<CleanupOutcome, Box<dyn Error>> {
    let mut removed = 0;
    let mut freed_bytes = 0_u64;
    let mut failures = Vec::new();
    for (path, bytes) in &plan.items {
        let result = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        match result {
            Ok(()) => {
                removed += 1;
                freed_bytes = freed_bytes.saturating_add(*bytes);
            }
            Err(error) => failures.push(format!("{}：{error}", path.display())),
        }
    }
    Ok(CleanupOutcome {
        removed,
        freed_bytes,
        failures,
        report: report(keep)?,
    })
}

fn clamp_keep(keep: usize) -> usize {
    keep.clamp(1, DRAFT_KEEP_MAX)
}

// ---------------------------------------------------------------------------
// 拓展开发草稿
// ---------------------------------------------------------------------------

/// 每个插件保留最近 `keep` 个草稿版本，其余进入清理计划。
///
/// 排序先看 `draft.json` 的 `updated_at`（作者最后一次保存的时间），再看版本号。
/// 只按版本号排会误删「回头修旧版本」得到的新草稿。
fn plan_drafts_in(
    drafts_root: &Path,
    keep: usize,
    protected: &[PathBuf],
) -> Result<CleanupPlan, Box<dyn Error>> {
    let mut plan = CleanupPlan::default();
    if !drafts_root.is_dir() {
        return Ok(plan);
    }
    let trusted = TrustedRoot::new(drafts_root)?;
    let keep = clamp_keep(keep);
    for plugin in child_dirs(drafts_root) {
        let mut versions: Vec<DraftVersion> = child_dirs(&plugin)
            .into_iter()
            .filter(|path| path.join("draft.json").is_file())
            .map(|path| DraftVersion {
                updated_at: draft_updated_at(&path.join("draft.json")),
                version: path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                bytes: path_size(&path),
                path,
            })
            .collect();
        versions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| version_key(&right.version).cmp(&version_key(&left.version)))
                .then_with(|| right.version.cmp(&left.version))
        });
        for candidate in versions.into_iter().skip(keep) {
            if is_protected(&candidate.path, protected) {
                continue;
            }
            let resolved = trusted.ensure_within(&candidate.path, "清理历史草稿")?;
            plan.items.push((resolved, candidate.bytes));
        }
    }
    Ok(plan)
}

struct DraftVersion {
    path: PathBuf,
    version: String,
    updated_at: u128,
    bytes: u64,
}

#[derive(Deserialize)]
struct DraftStamp {
    #[serde(default)]
    updated_at: String,
}

fn draft_updated_at(draft_json: &Path) -> u128 {
    let Ok(content) = fs::read_to_string(draft_json) else {
        return 0;
    };
    serde_json::from_str::<DraftStamp>(content.trim_start_matches('\u{feff}'))
        .ok()
        .and_then(|stamp| stamp.updated_at.parse::<u128>().ok())
        .unwrap_or(0)
}

/// 已登记为开发插件的目录：草稿被解包到 `<草稿版本>/package` 后登记，删掉就等于
/// 把正在调试的插件从磁盘上抽走。这里连同它的祖先版本目录一起保护。
fn protected_draft_paths() -> Vec<PathBuf> {
    crate::capability::plugin::development_plugin_entries()
        .into_iter()
        .map(|(_, path)| canonical_or_self(path))
        .collect()
}

fn is_protected(candidate: &Path, protected: &[PathBuf]) -> bool {
    let resolved = canonical_or_self(candidate.to_path_buf());
    protected.iter().any(|guarded| {
        let guarded = canonical_or_self(guarded.clone());
        guarded == resolved || guarded.starts_with(&resolved)
    })
}

fn canonical_or_self(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

// ---------------------------------------------------------------------------
// 程序版本
// ---------------------------------------------------------------------------

/// `versions/<version>` 只保留 `active-version`、它的上一版，以及当前进程正在
/// 运行的那一版；更早的版本只能在更新界面重新下载。
fn plan_versions_in(install_root: &Path) -> Result<CleanupPlan, Box<dyn Error>> {
    let versions_root = install_root.join("versions");
    let mut plan = CleanupPlan::default();
    if !versions_root.is_dir() {
        return Ok(plan);
    }
    let trusted = TrustedRoot::new(&versions_root)?;
    let active = crate::install_layout::read_active_version(install_root)
        .ok()
        .flatten();
    let running = running_version_directory();

    let mut versions = child_dirs(&versions_root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                // 安装中断留下的暂存目录以 `.` 或安装期前缀开头：它们不是版本，
                // 有自己的失效判断，不能被这里顺手删掉。
                .is_some_and(|name| {
                    !name.starts_with('.')
                        && !name.starts_with("staging-")
                        && !name.starts_with("current-")
                        && !name.starts_with("swap-")
                        && !name.starts_with("restore-")
                        && crate::install_layout::validate_version(name).is_ok()
                })
        })
        .collect::<Vec<_>>();
    versions
        .sort_by(|left, right| version_key(&file_name(right)).cmp(&version_key(&file_name(left))));

    let mut keep: Vec<String> = Vec::new();
    if let Some(active) = active.clone() {
        keep.push(active);
        // 上一版：比 active 小的最高的那一版。
        if let Some(previous) = versions
            .iter()
            .map(|path| file_name(path))
            .find(|name| keep.iter().all(|kept| kept != name))
        {
            keep.push(previous);
        }
    } else {
        // 没有版本指针（半成品安装或开发机上的残留）时保留最高的两版：
        // 删到只剩一版并不能换来多少空间，却会少一次回退机会。
        for name in versions.iter().map(|path| file_name(path)).take(2) {
            keep.push(name);
        }
    }
    if let Some(running) = running {
        if !keep.contains(&running) {
            keep.push(running);
        }
    }

    for candidate in versions {
        let name = file_name(&candidate);
        if keep.iter().any(|kept| kept == &name) {
            continue;
        }
        let bytes = path_size(&candidate);
        let resolved = trusted.ensure_within(&candidate, "清理旧版本")?;
        plan.items.push((resolved, bytes));
    }
    Ok(plan)
}

fn running_version_directory() -> Option<String> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let directory = executable.parent()?;
    if directory
        .parent()?
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("versions"))
    {
        return Some(file_name(directory));
    }
    None
}

fn version_key(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.').map(|part| {
        part.chars()
            .take_while(|char| char.is_ascii_digit())
            .collect::<String>()
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

// ---------------------------------------------------------------------------
// 缓存与临时文件
// ---------------------------------------------------------------------------

/// `cache/` 与 `tmp/` 的**内容**可重建，目录本身保留。
fn plan_rebuildable_in(root: &Path) -> Result<CleanupPlan, Box<dyn Error>> {
    let mut plan = CleanupPlan::default();
    let Ok(trusted) = TrustedRoot::new(root) else {
        return Ok(plan);
    };
    for name in ["cache", "tmp"] {
        let directory = root.join(name);
        if !directory.is_dir() {
            continue;
        }
        for entry in child_entries(&directory) {
            let Ok(metadata) = fs::symlink_metadata(&entry) else {
                continue;
            };
            // 链接指向的位置可能不在数据根内，跟着删就越界了；跳过它。
            if is_link_like(&metadata) {
                continue;
            }
            if !is_old_enough(&entry) {
                continue;
            }
            let bytes = path_size(&entry);
            let resolved = trusted.ensure_within(&entry, "清理缓存")?;
            plan.items.push((resolved, bytes));
        }
    }
    Ok(plan)
}

fn is_old_enough(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age >= MIN_REBUILDABLE_AGE)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 文件系统小工具
// ---------------------------------------------------------------------------

fn child_dirs(directory: &Path) -> Vec<PathBuf> {
    child_entries(directory)
        .into_iter()
        .filter(|path| {
            fs::symlink_metadata(path)
                .map(|meta| meta.is_dir() && !is_link_like(&meta))
                .unwrap_or(false)
        })
        .collect()
}

fn child_entries(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries.flatten().map(|entry| entry.path()).collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 目录占用。联接与符号链接不跟随：否则一个联到别处的目录会把别人的体积算进来，
/// 也会让删除跑到数据根之外。
pub(crate) fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if is_link_like(&metadata) {
        return 0;
    }
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    let mut total = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            // `DirEntry::metadata` 会跟随链接；再用 symlink_metadata 复核一次。
            if fs::symlink_metadata(entry.path())
                .map(|meta| is_link_like(&meta))
                .unwrap_or(true)
            {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn other_profiles_size(install_root: &Path, current: &Path) -> u64 {
    let profiles = install_root.join("profiles");
    if !profiles.is_dir() {
        return 0;
    }
    let current = canonical_or_self(current.to_path_buf());
    child_dirs(&profiles)
        .into_iter()
        .filter(|path| canonical_or_self(path.clone()) != current)
        .map(|path| path_size(&path))
        .sum()
}

/// 数据根所在磁盘的剩余空间。取不到就返回 0，界面只显示「未知」。
pub(crate) fn free_bytes(path: &Path) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut available = 0_u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            return available;
        }
    }
    let _ = path;
    0
}

#[cfg(test)]
mod tests {
    use super::{clamp_keep, plan_drafts_in, plan_versions_in, DRAFT_KEEP_MAX};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "himind-storage-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_draft(root: &Path, plugin: &str, version: &str, updated_at: u128) {
        let directory = root.join(plugin).join(version);
        fs::create_dir_all(directory.join("package")).unwrap();
        fs::write(directory.join("package/plugin.json"), "{}").unwrap();
        fs::write(
            directory.join("draft.json"),
            format!("{{\"updated_at\":\"{updated_at}\"}}"),
        )
        .unwrap();
    }

    /// 只保留最近 N 个：按最后保存时间排序，而不是版本号。回头修旧版本得到的
    /// 新草稿必须留住，否则「刚改完就被清理」。
    #[test]
    fn draft_plan_keeps_the_most_recent_versions() {
        let root = scratch("drafts-recent");
        write_draft(&root, "com.example.plugin", "1.0.0", 100);
        write_draft(&root, "com.example.plugin", "1.0.1", 200);
        write_draft(&root, "com.example.plugin", "0.9.0", 300);
        let plan = plan_drafts_in(&root, 2, &[]).unwrap();
        assert_eq!(plan.items.len(), 1, "只该删掉最早保存的那一版");
        assert!(plan.items[0].0.ends_with(Path::new("1.0.0")));
        let _ = fs::remove_dir_all(&root);
    }

    /// 已登记为开发插件的草稿即使排在保留名单之外也不能删：它可能就是把当前
    /// 正在调试的那个版本。
    #[test]
    fn draft_plan_protects_registered_development_plugins() {
        let root = scratch("drafts-protected");
        write_draft(&root, "com.example.plugin", "1.0.0", 100);
        write_draft(&root, "com.example.plugin", "1.0.1", 200);
        write_draft(&root, "com.example.plugin", "1.0.2", 300);
        let protected = vec![root.join("com.example.plugin/1.0.0/package")];
        let plan = plan_drafts_in(&root, 1, &protected).unwrap();
        assert!(
            plan.items.iter().all(|(path, _)| !path.ends_with("1.0.0")),
            "正在开发中的草稿不能被清理"
        );
        assert_eq!(plan.items.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// 没有 `draft.json` 的目录不是草稿流程产出的，一律不动。
    #[test]
    fn draft_plan_ignores_directories_it_did_not_create() {
        let root = scratch("drafts-foreign");
        write_draft(&root, "com.example.plugin", "1.0.0", 100);
        write_draft(&root, "com.example.plugin", "1.0.1", 200);
        fs::create_dir_all(root.join("com.example.plugin/notes")).unwrap();
        fs::write(root.join("com.example.plugin/notes/todo.md"), "keep").unwrap();
        let plan = plan_drafts_in(&root, 1, &[]).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert!(plan.items[0].0.ends_with(Path::new("1.0.0")));
        let _ = fs::remove_dir_all(&root);
    }

    /// 程序版本：保留 `active-version` 与它的上一版，其余删除；`active-version`
    /// 缺失时至少保留最高的一版。
    #[test]
    fn version_plan_keeps_active_and_previous() {
        let root = scratch("versions");
        for version in ["0.3.88", "0.3.89", "0.3.90", "0.3.91"] {
            fs::create_dir_all(root.join("versions").join(version)).unwrap();
        }
        fs::write(root.join("active-version"), "0.3.91\n").unwrap();
        let plan = plan_versions_in(&root).unwrap();
        let removed = plan
            .items
            .iter()
            .map(|(path, _)| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(removed, vec!["0.3.89".to_string(), "0.3.88".to_string()]);

        fs::remove_file(root.join("active-version")).unwrap();
        let plan = plan_versions_in(&root).unwrap();
        assert_eq!(plan.items.len(), 2, "没有指针时只保留最高两版");
        let _ = fs::remove_dir_all(&root);
    }

    /// 安装暂存目录（`.` 开头或安装期前缀）不是版本目录，不该被版本清理顺手删掉：
    /// 那条路径有各自的失效判断。
    #[test]
    fn version_plan_ignores_staging_directories() {
        let root = scratch("versions-staging");
        fs::create_dir_all(root.join("versions/.0.4.0.installing-1")).unwrap();
        fs::create_dir_all(root.join("versions/0.4.0")).unwrap();
        let plan = plan_versions_in(&root).unwrap();
        assert!(plan.items.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keep_is_clamped_to_a_meaningful_range() {
        assert_eq!(clamp_keep(0), 1);
        assert_eq!(clamp_keep(1), 1);
        assert_eq!(clamp_keep(9), DRAFT_KEEP_MAX);
    }

    /// 序列化契约：前端按 snake_case 读字段，改名会让面板静默显示 0。
    #[test]
    fn cleanup_options_serialize_with_snake_case_fields() {
        let option = super::cleanup_option(
            "drafts",
            "拓展开发草稿",
            Path::new(r"C:\root"),
            "每个插件保留最近 2 个版本",
            Ok(super::CleanupPlan::default()),
        );
        let value = serde_json::to_value(option).unwrap();
        for key in ["key", "label", "path", "bytes", "entries", "note"] {
            assert!(value.get(key).is_some(), "缺少字段 {key}");
        }
    }
}
