//! 用户数据位置：选择、校验与迁移。
//!
//! 目标只有一个：让生产 Agent 的用户数据可以放在任意本地磁盘上，而程序仍然
//! 留在 `%LOCALAPPDATA%\HiMindAgent`（升级器、协议注册、快捷方式都按安装根
//! 工作，跟着搬会把它们全部弄坏）。
//!
//! 迁移不在运行中做。当前进程有 SQLite、日志、WebView2 在写，热拷贝只能得到
//! 一份"看起来完整"的混合数据。所以流程是「排队 → 重启 → 在下一个进程启动、
//! 任何 store 打开之前执行」：
//!
//! 1. 复制到目标（robocopy，逐项校验文件数与字节数）；
//! 2. 在新数据根里把绝对路径改写掉（`media-registry.json` 等台账记的是旧位置）；
//! 3. 最后一步才写 `data-location.json` 指针——指针落盘之前，旧目录始终是
//!    唯一权威，失败只会留下一个可以重试的目标目录，不会让 Agent 半途换根。
//! 4. 旧目录保留，由用户在确认数据可用后自行删除。

use crate::store::paths::{self, DataLocation, DATA_LOCATION_VERSION};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub(crate) const PENDING_FILE: &str = "data-migration.json";
pub(crate) const RESULT_FILE: &str = "data-migration-result.json";
/// 目标目录标记：只允许写进空目录或上次迁移留下的目标目录，避免把数据倒进
/// 用户随手选的一个已有文件夹里。
const TARGET_MARKER: &str = ".himind-data-root";
/// 迁移时保留在安装根的程序条目。其余顶层条目一律视为用户数据。
const PROGRAM_ENTRIES: &[&str] = &[
    "versions",
    "runtimes",
    "resources",
    "previous",
    "staging",
    "trusted-keys",
    "profiles",
    "active-version",
    "himind-agent.ico",
    "uninstall.exe",
    "repair-storage.ps1",
    "data-location.json",
    "data-location.json.bak",
    PENDING_FILE,
    RESULT_FILE,
];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MigrationPlan {
    pub source: String,
    pub target: String,
    pub bytes: u64,
    pub entries: usize,
    pub blocker: Option<String>,
    pub warnings: Vec<String>,
}

impl MigrationPlan {
    pub(crate) fn ready(&self) -> bool {
        self.blocker.is_none() && self.entries > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingMigration {
    pub target: String,
    pub queued_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MigrationResult {
    /// `done` 或 `failed`。
    pub status: String,
    pub source: String,
    pub target: String,
    pub entries: usize,
    pub bytes: u64,
    pub rewrites: usize,
    pub finished_at: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// 当前数据位置；用于界面展示，不做任何写入。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DataLocationStatus {
    pub profile: String,
    pub data_root: String,
    pub install_root: String,
    /// 数据根是否由用户指定（迁移过）。
    pub custom: bool,
    /// 默认位置；「恢复默认」用它。
    pub default_root: String,
    pub pointer_file: String,
    pub can_move: bool,
    /// 不能迁移的原因；`can_move` 为真时为空。
    pub blocked_reason: String,
    pub pending: Option<PendingMigration>,
    pub last_result: Option<MigrationResult>,
}

pub(crate) fn status() -> DataLocationStatus {
    let profile = paths::profile_name();
    let data_root = paths::agent_home();
    let install_root = paths::shared_agent_root();
    let pointer = paths::data_location_file();
    let blocked_reason = move_blocker(&profile);
    DataLocationStatus {
        profile,
        data_root: data_root.to_string_lossy().to_string(),
        install_root: install_root.to_string_lossy().to_string(),
        custom: paths::configured_data_root().is_some(),
        default_root: install_root.to_string_lossy().to_string(),
        pointer_file: pointer.to_string_lossy().to_string(),
        can_move: blocked_reason.is_empty(),
        blocked_reason,
        pending: pending(),
        last_result: last_result(),
    }
}

fn move_blocker(profile: &str) -> String {
    if paths::explicit_agent_home().is_some() {
        return "当前实例使用 HIMIND_AGENT_HOME 指定数据根，不能再改位置。".to_string();
    }
    if !paths::is_production_profile(profile) {
        return "只有生产 profile 支持更改数据位置。".to_string();
    }
    String::new()
}

/// 目标位置的完整校验结果。任何一条不满足都会写成 `blocker`，界面原样展示。
pub(crate) fn plan(target: &Path) -> Result<MigrationPlan, Box<dyn Error>> {
    let source = paths::agent_home();
    plan_from(&source, target, move_blocker(&paths::profile_name()))
}

/// 校验主体。`policy` 是实例级限制（profile、显式数据根），与目标路径无关；
/// 拆出来是为了让目标路径的校验能在临时目录上测，不必改进程环境。
fn plan_from(
    source: &Path,
    target: &Path,
    policy: String,
) -> Result<MigrationPlan, Box<dyn Error>> {
    let mut plan = MigrationPlan {
        source: source.to_string_lossy().to_string(),
        target: target.to_string_lossy().to_string(),
        bytes: 0,
        entries: 0,
        blocker: None,
        warnings: Vec::new(),
    };
    if !policy.is_empty() {
        plan.blocker = Some(policy);
        return Ok(plan);
    }
    if let Some(reason) = path_blocker(source, target) {
        plan.blocker = Some(reason);
        return Ok(plan);
    }

    let mut bytes = 0_u64;
    let mut entries = 0;
    for entry in data_entries(source) {
        entries += 1;
        bytes = bytes.saturating_add(crate::app::storage::path_size(&entry));
    }
    plan.bytes = bytes;
    plan.entries = entries;
    if entries == 0 {
        plan.blocker = Some("没有需要迁移的数据。".to_string());
        return Ok(plan);
    }
    if let Some(reason) = writable_blocker(target) {
        plan.blocker = Some(reason);
        return Ok(plan);
    }
    let free = crate::app::storage::free_bytes(target);
    if free > 0 && free < bytes.saturating_add(256 * 1024 * 1024) {
        plan.blocker = Some(format!(
            "目标磁盘剩余 {}，不足以再放一份 {}。",
            human_bytes(free),
            human_bytes(bytes)
        ));
    }
    if !volume_is_ntfs(target) {
        plan.warnings
            .push("目标不是 NTFS：文件权限与完整性级别不会随数据保留。".to_string());
    }
    Ok(plan)
}

fn path_blocker(source: &Path, target: &Path) -> Option<String> {
    if target.as_os_str().is_empty() {
        return Some("请选择目标文件夹。".to_string());
    }
    if !target.is_absolute()
        || target
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Some("目标必须是绝对路径。".to_string());
    }
    let target_key = normalize(target);
    let source_key = normalize(source);
    if target_key == source_key {
        return Some("目标就是当前数据目录。".to_string());
    }
    if target_key.starts_with(&format!("{source_key}\\"))
        || source_key.starts_with(&format!("{target_key}\\"))
    {
        return Some("目标不能是当前数据目录的子目录或父目录。".to_string());
    }
    if target_key.starts_with(r"\\") {
        return Some("目标必须是本机磁盘，不能是网络位置。".to_string());
    }
    if let Some(reason) = existing_target_blocker(target) {
        return Some(reason);
    }
    None
}

/// 已有目录只接受两种：空的，或上次迁移留下的（带标记）。后者的内容会被清空
/// 重来——那里只可能有不完整的副本。
fn existing_target_blocker(target: &Path) -> Option<String> {
    if !target.exists() {
        return None;
    }
    if !target.is_dir() {
        return Some("目标位置必须是文件夹。".to_string());
    }
    let entries = fs::read_dir(target).ok().map(|items| {
        items
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>()
    })?;
    if entries.is_empty() || entries == vec![TARGET_MARKER.to_string()] {
        return None;
    }
    if entries.iter().any(|name| name == TARGET_MARKER) {
        return None;
    }
    Some("目标文件夹不是空的，请选择空文件夹。".to_string())
}

fn writable_blocker(target: &Path) -> Option<String> {
    if let Err(error) = fs::create_dir_all(target) {
        return Some(format!("无法创建目标文件夹：{error}"));
    }
    let probe = target.join(".himind-write-test");
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            None
        }
        Err(error) => Some(format!("目标文件夹不可写：{error}")),
    }
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut index = 0;
    while value >= 1024.0 && index < UNITS.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    format!("{value:.1} {}", UNITS[index])
}

/// 安装根下哪些顶层条目属于用户数据：白名单之外的一切。
fn data_entries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut items = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            !is_program_entry(&name)
        })
        .collect::<Vec<_>>();
    items.sort();
    items
}

fn is_program_entry(name: &str) -> bool {
    PROGRAM_ENTRIES.contains(&name)
        || name.starts_with("himind-agent-launcher")
        || name.starts_with("himind-agent-updater")
        || name.starts_with("himind-agent.ico")
}

// ---------------------------------------------------------------------------
// 排队与执行
// ---------------------------------------------------------------------------

pub(crate) fn pending() -> Option<PendingMigration> {
    let path = pending_path();
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()
}

pub(crate) fn last_result() -> Option<MigrationResult> {
    let content = fs::read_to_string(result_path()).ok()?;
    serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()
}

pub(crate) fn queue(target: &Path) -> Result<MigrationPlan, Box<dyn Error>> {
    let plan = plan(target)?;
    if let Some(reason) = plan.blocker.clone() {
        return Err(reason.into());
    }
    let pending = PendingMigration {
        target: plan.target.clone(),
        queued_at: now_stamp(),
    };
    let content = serde_json::to_vec_pretty(&pending)?;
    crate::store::atomic_file::atomic_write(&pending_path(), &content)?;
    Ok(plan)
}

pub(crate) fn cancel() -> bool {
    fs::remove_file(pending_path()).is_ok()
}

/// 清掉上一次迁移的结果卡片。
pub(crate) fn dismiss_result() -> bool {
    fs::remove_file(result_path()).is_ok()
}

/// 启动早期执行排队中的迁移。返回本次的结果（成功或失败），没有排队时 `None`。
///
/// 调用点必须在任何 store 打开之前：`main` 里紧跟在 profile 确定之后。
pub(crate) fn run_pending() -> Option<MigrationResult> {
    let pending = pending()?;
    let target = PathBuf::from(pending.target.trim());
    let source = paths::agent_home();
    let result = match plan(&target) {
        Ok(plan) if plan.blocker.is_some() => Err(plan.blocker.unwrap_or_default()),
        Ok(plan) => {
            let entries = plan.entries;
            migrate(&source, &target)
                .map(|mut outcome| {
                    outcome.entries = entries;
                    outcome
                })
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    };
    let result = match result {
        Ok(outcome) => {
            let location = DataLocation {
                version: DATA_LOCATION_VERSION,
                data_root: target.to_string_lossy().to_string(),
                previous_root: Some(source.to_string_lossy().to_string()),
                updated_at: now_stamp(),
            };
            match paths::write_data_location(&location) {
                Ok(_) => MigrationResult {
                    status: "done".to_string(),
                    source: source.to_string_lossy().to_string(),
                    target: target.to_string_lossy().to_string(),
                    entries: outcome.entries,
                    bytes: outcome.bytes,
                    rewrites: outcome.rewrites,
                    finished_at: now_stamp(),
                    error: None,
                },
                Err(error) => MigrationResult {
                    status: "failed".to_string(),
                    source: source.to_string_lossy().to_string(),
                    target: target.to_string_lossy().to_string(),
                    entries: outcome.entries,
                    bytes: outcome.bytes,
                    rewrites: outcome.rewrites,
                    finished_at: now_stamp(),
                    error: Some(format!("复制完成，但写入数据位置指针失败：{error}")),
                },
            }
        }
        Err(error) => MigrationResult {
            status: "failed".to_string(),
            source: source.to_string_lossy().to_string(),
            target: target.to_string_lossy().to_string(),
            entries: 0,
            bytes: 0,
            rewrites: 0,
            finished_at: now_stamp(),
            error: Some(error),
        },
    };
    if let Ok(content) = serde_json::to_vec_pretty(&result) {
        let _ = crate::store::atomic_file::atomic_write(&result_path(), &content);
    }
    let _ = fs::remove_file(pending_path());
    Some(result)
}

struct MigrationOutcome {
    entries: usize,
    bytes: u64,
    rewrites: usize,
}

/// 复制 → 校验 → 改写旧绝对路径。指针留给调用方在成功之后写。
fn migrate(source: &Path, target: &Path) -> Result<MigrationOutcome, Box<dyn Error>> {
    fs::create_dir_all(target)?;
    fs::write(target.join(TARGET_MARKER), b"himind-agent data root\n")?;
    // 重试时先清掉目标里上次留下的内容：那里只可能是不完整的副本。
    for entry in fs::read_dir(target)?.flatten() {
        if entry.file_name().to_string_lossy() == TARGET_MARKER {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }

    let mut outcome = MigrationOutcome {
        entries: 0,
        bytes: 0,
        rewrites: 0,
    };
    for entry in data_entries(source) {
        let name = entry
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        let destination = target.join(&name);
        let (files, bytes) = tree_stats(&entry)?;
        if entry.is_dir() {
            copy_tree(&entry, &destination)?;
        } else {
            fs::copy(&entry, &destination)?;
        }
        let (copied_files, copied_bytes) = tree_stats(&destination)?;
        if copied_files != files || copied_bytes != bytes {
            return Err(format!(
                "复制校验失败：{name} 源 {files} 个文件 {bytes} 字节，目标 {copied_files} 个文件 {copied_bytes} 字节"
            )
            .into());
        }
        outcome.entries += 1;
        outcome.bytes = outcome.bytes.saturating_add(bytes);
    }
    outcome.rewrites = rewrite_absolute_paths(target, source)?;
    Ok(outcome)
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("robocopy");
    command.arg(source).arg(target).args([
        "/E",
        "/COPY:DAT",
        "/R:1",
        "/W:1",
        "/NFL",
        "/NDL",
        "/NJH",
        "/NJS",
        "/NP",
        "/XJ",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let status = command
        .status()
        .map_err(|error| format!("无法运行 robocopy（{}）：{error}", source.display()))?;
    // robocopy 用 0–7 表示成功，8 以上才是失败。
    match status.code() {
        Some(code) if code < 8 => Ok(()),
        other => Err(format!(
            "复制 {} 失败（robocopy 退出码 {:?}）",
            source.display(),
            other
        )
        .into()),
    }
}

fn tree_stats(path: &Path) -> Result<(u64, u64), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok((0, 0));
    }
    if metadata.is_file() {
        return Ok((1, metadata.len()));
    }
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)?.flatten() {
            let metadata = entry.metadata()?;
            if entry.file_type()?.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                files += 1;
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok((files, bytes))
}

/// 台账里的绝对路径要跟着搬：`media-registry.json` 记的是图片在旧数据根下的
/// 位置，不改写就等于迁移后所有历史图片都打不开。
///
/// 只改顶层 `.json` 与 `data/*.json`：这两个范围覆盖了 Agent 自己写的台账，
/// 又不会去动插件内容或用户文件。
fn rewrite_absolute_paths(root: &Path, previous: &Path) -> Result<usize, Box<dyn Error>> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") || name.ends_with(".json.bak") {
                files.push(entry.path());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(root.join("data")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") || name.ends_with(".json.bak") {
                files.push(entry.path());
            }
        }
    }
    let spare = [
        previous.to_string_lossy().to_string(),
        previous.to_string_lossy().replace('\\', "\\\\"),
        previous.to_string_lossy().replace('\\', "/"),
    ];
    let replacement = [
        root.to_string_lossy().to_string(),
        root.to_string_lossy().replace('\\', "\\\\"),
        root.to_string_lossy().replace('\\', "/"),
    ];
    let mut rewrites = 0;
    for file in files {
        let Ok(metadata) = fs::metadata(&file) else {
            continue;
        };
        if metadata.len() > 8 * 1024 * 1024 {
            continue;
        }
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let mut updated = content.clone();
        for (from, to) in spare.iter().zip(replacement.iter()) {
            updated = updated.replace(from, to);
        }
        if updated != content {
            crate::store::atomic_file::atomic_write(&file, updated.as_bytes())?;
            rewrites += 1;
        }
    }
    Ok(rewrites)
}

fn pending_path() -> PathBuf {
    paths::shared_agent_root().join(PENDING_FILE)
}

fn result_path() -> PathBuf {
    paths::shared_agent_root().join(RESULT_FILE)
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn volume_is_ntfs(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut volume = vec![0_u16; 261];
        let ok =
            unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
        if ok == 0 {
            return false;
        }
        let mut file_system = vec![0_u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                volume.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                file_system.as_mut_ptr(),
                file_system.len() as u32,
            )
        };
        if ok == 0 {
            return false;
        }
        let end = file_system
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(file_system.len());
        return String::from_utf16_lossy(&file_system[..end]).eq_ignore_ascii_case("NTFS");
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(test)]
mod tests {
    use super::{
        data_entries, is_program_entry, migrate, path_blocker, plan_from, rewrite_absolute_paths,
        tree_stats,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "himind-location-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// 程序目录不能被当成用户数据搬走：搬走之后升级器和快捷方式都会指向空目录。
    #[test]
    fn program_entries_stay_in_the_install_root() {
        for name in [
            "versions",
            "runtimes",
            "resources",
            "trusted-keys",
            "profiles",
            "active-version",
            "himind-agent-launcher.exe",
            "himind-agent-launcher.exe.backup-20261021",
            "himind-agent-updater.exe",
            "data-location.json",
            super::PENDING_FILE,
        ] {
            assert!(is_program_entry(name), "{name} 应留在安装根");
        }
        for name in [
            "data",
            "plugins",
            "plugin-drafts",
            "media-inbox",
            "local-runs.sqlite3",
            "logs",
            "ai-services.json",
        ] {
            assert!(!is_program_entry(name), "{name} 应随数据迁移");
        }
    }

    #[test]
    fn data_entries_lists_only_user_data() {
        let root = scratch("entries");
        fs::create_dir_all(root.join("versions")).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("active-version"), "0.4.0").unwrap();
        fs::write(root.join("ai-services.json"), "{}").unwrap();
        let names = data_entries(&root)
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["ai-services.json", "data"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn target_must_not_nest_with_the_current_root() {
        let root = scratch("nest");
        let source = root.join("HiMindAgent");
        fs::create_dir_all(&source).unwrap();
        assert!(path_blocker(&source, &source.join("nested")).is_some());
        assert!(path_blocker(&source, &root).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    /// 迁移后旧路径要按 JSON 里的两种转义写法一起改写，只改一种等于没改。
    #[test]
    fn rewrites_escaped_and_plain_absolute_paths() {
        let root = scratch("rewrite");
        let previous = root.join("OldHome");
        let target = root.join("NewHome");
        fs::create_dir_all(target.join("data")).unwrap();
        fs::write(
            target.join("media-registry.json"),
            format!(
                r#"{{"plain":"{}","escaped":"{}","slash":"{}"}}"#,
                previous.display(),
                previous.display().to_string().replace('\\', "\\\\"),
                previous.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        let rewrites = rewrite_absolute_paths(&target, &previous).unwrap();
        assert_eq!(rewrites, 1);
        let content = fs::read_to_string(target.join("media-registry.json")).unwrap();
        assert!(
            !content.contains("OldHome"),
            "旧路径必须全部改掉：{content}"
        );
        assert_eq!(content.matches("NewHome").count(), 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tree_stats_counts_files_and_bytes() {
        let root = scratch("stats");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("a.txt"), b"12345").unwrap();
        fs::write(root.join("nested/b.txt"), b"123").unwrap();
        assert_eq!(tree_stats(&root).unwrap(), (2, 8));
        let _ = fs::remove_dir_all(&root);
    }

    /// 迁移的准入条件：开发 profile 和显式数据根都不允许改位置。这两条是
    /// 「别把开发实例的数据搬走」和「隔离实例不能被迁移」的唯一防线。
    #[test]
    fn moving_is_blocked_for_development_profiles_and_explicit_homes() {
        let _guard = crate::store::paths::test_env_lock();
        let previous_profile = std::env::var_os("HIMIND_AGENT_PROFILE");
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::remove_var("HIMIND_AGENT_HOME");

        std::env::set_var("HIMIND_AGENT_PROFILE", "development");
        assert!(super::move_blocker("development").contains("生产"));

        std::env::set_var("HIMIND_AGENT_PROFILE", "production");
        assert!(super::move_blocker("production").is_empty());
        std::env::set_var(
            "HIMIND_AGENT_HOME",
            std::env::temp_dir().join("himind-explicit"),
        );
        assert!(super::move_blocker("production").contains("HIMIND_AGENT_HOME"));

        match previous_profile {
            Some(value) => std::env::set_var("HIMIND_AGENT_PROFILE", value),
            None => std::env::remove_var("HIMIND_AGENT_PROFILE"),
        }
        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
    }

    /// 迁移量只统计用户数据；目标里已经有别人的文件时直接拒绝，不能往用户
    /// 随手选的文件夹里倒数据。
    #[test]
    fn plan_counts_only_user_data_and_blocks_non_empty_targets() {
        let root = scratch("plan");
        let source = root.join("install");
        fs::create_dir_all(source.join("data")).unwrap();
        fs::create_dir_all(source.join("versions").join("0.4.0")).unwrap();
        fs::write(source.join("data/agent-state.json"), "{}").unwrap();
        fs::write(source.join("ai-services.json"), "{}").unwrap();
        let target = root.join("target");

        let plan = plan_from(&source, &target, String::new()).unwrap();
        assert_eq!(plan.entries, 2, "只该统计 data 与顶层配置，不含 versions");
        assert!(plan.blocker.is_none(), "{:?}", plan.blocker);
        assert!(plan.bytes > 0);

        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("unrelated.txt"), "x").unwrap();
        let blocked = plan_from(&source, &target, String::new()).unwrap();
        assert!(blocked.blocker.is_some(), "非空目标必须被拒绝");

        let _ = fs::remove_dir_all(&root);
    }

    /// 复制只搬用户数据、不搬程序目录，并在新位置把旧绝对路径改写掉。
    #[test]
    fn migrate_copies_user_data_and_rewrites_paths() {
        let root = scratch("migrate");
        let source = root.join("install");
        let target = root.join("data");
        fs::create_dir_all(source.join("data")).unwrap();
        fs::create_dir_all(source.join("plugin-drafts").join("x").join("1.0.0")).unwrap();
        fs::create_dir_all(source.join("versions").join("0.4.0")).unwrap();
        fs::write(source.join("data/agent-state.json"), "{}").unwrap();
        fs::write(source.join("plugin-drafts/x/1.0.0/draft.json"), "{}").unwrap();
        let registry = format!(
            r#"{{"path":"{}"}}"#,
            source.join("media-inbox").join("a.png").display()
        );
        fs::write(source.join("media-registry.json"), &registry).unwrap();

        let outcome = migrate(&source, &target).unwrap();
        assert!(target.join("data/agent-state.json").is_file());
        assert!(target.join("plugin-drafts/x/1.0.0/draft.json").is_file());
        assert!(!target.join("versions").exists(), "程序目录不随数据迁移");
        assert!(target.join(super::TARGET_MARKER).is_file());
        assert!(outcome.rewrites >= 1);

        let content = fs::read_to_string(target.join("media-registry.json")).unwrap();
        assert!(
            !content.contains(&source.to_string_lossy().to_string()),
            "旧数据根路径必须改写：{content}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
