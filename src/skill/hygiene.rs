//! 客户端技能目录的卫生清理。
//!
//! 客户端按 `**/SKILL.md` 递归发现技能，所以目录里任何一份多余的
//! `SKILL.md` 都会被当成"另一个技能"读走。历史上有两类残留会造成这种
//! 结果：
//!
//! 1. 0.3.47 之前的渲染布局是 `<skill-id>/current`。升级后现代布局是
//!    `<slug>`，老目录却不会被自动清理，同一个技能在客户端里出现两次，
//!    而且 `previous/` 里的旧副本会被读成第二个技能（版本还更旧）。
//! 2. 渲染过程被中断（进程被杀、磁盘满）时，`.himind-<slug>-staging-*`
//!    与 `.himind-<slug>-backup-*` 会留在客户端根目录里。
//!
//! 这里只删两类可证明属于 HiMind 的目录：带 HiMind 收据的 legacy 树，
//! 以及带 HiMind 前缀的中断残留。用户自己放进客户端目录的技能不满足
//! 任何一个条件，不会被碰到。

use crate::skill::clients::DIRECTORY_CLIENTS;
use crate::skill::types::SkillReceipt;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const RECEIPT_NAME: &str = ".himind-render.json";
/// 中断残留统一用 `.himind-` 前缀 + 隐藏目录。合法的 Skill 目录名不允许
/// 以 `.` 开头（见 `codex::validate_skill_slug`），所以不会误伤。
const RESIDUE_PREFIX: &str = ".himind-";
/// 残留必须"足够旧"才清理：正在进行的安装也会短暂存在同名目录。
const RESIDUE_MIN_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HygieneReport {
    /// 已删除的 legacy 托管树（含 `previous/` 在内的整个技能目录）。
    pub removed_legacy: Vec<PathBuf>,
    /// 已删除的中断残留目录。
    pub removed_residue: Vec<PathBuf>,
    /// 仍是唯一副本、故意保留的 legacy 托管树。
    pub retained_legacy: Vec<PathBuf>,
}

impl HygieneReport {
    pub(crate) fn touched(&self) -> usize {
        self.removed_legacy.len() + self.removed_residue.len()
    }
}

/// 扫描所有已知客户端技能根目录并清理残留。
///
/// `active_codex_root` 是当前真正用于渲染 Codex 技能的根目录：只有当同一个
/// 技能在那里已经有现代布局副本时，才会删掉 legacy 树，避免把某个技能在
/// 客户端里唯一的副本删没。
pub(crate) fn run(active_codex_root: Option<&Path>) -> HygieneReport {
    let mut roots = candidate_roots();
    if let Some(active) = active_codex_root {
        push_unique(&mut roots, active.to_path_buf());
    }
    let mut report = HygieneReport::default();
    for root in roots {
        sweep_root(&root, active_codex_root, &mut report);
    }
    report
}

/// 所有客户端渲染收据里记录过的来源目录。
///
/// 软链接模式下客户端目录里是指回 `<版本目录>` 的链接，所以收版本目录之前
/// 必须先问一句"还有客户端指着它吗"。返回的是收据原文里的路径，调用方按
/// 自己的目录比较即可；读不出的收据不算数（保守方向由调用方兜底）。
pub(crate) fn referenced_render_sources() -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for root in candidate_roots() {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            push_source(&mut sources, &path.join(RECEIPT_NAME));
            for layout in ["current", "previous"] {
                push_source(&mut sources, &path.join(layout).join(RECEIPT_NAME));
            }
        }
    }
    sources
}

fn push_source(sources: &mut Vec<PathBuf>, receipt_path: &Path) {
    let Some(source) = read_receipt_source_root(receipt_path) else {
        return;
    };
    if !sources.iter().any(|existing| existing == &source) {
        sources.push(source);
    }
}

fn read_receipt_source_root(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    let receipt: SkillReceipt =
        serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()?;
    let source = receipt.source_root.trim();
    (!source.is_empty()).then(|| PathBuf::from(source))
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in crate::skill::codex::global_candidate_roots() {
        push_unique(&mut roots, path);
    }
    for key in ["HIMIND_CODEX_SKILL_DIR", "CODEX_SKILL_DIR"] {
        if let Some(value) = env::var_os(key) {
            push_unique(&mut roots, PathBuf::from(value));
        }
    }
    for definition in DIRECTORY_CLIENTS {
        if let Some(env_key) = definition.skill_env_key() {
            if let Some(value) = env::var_os(env_key) {
                push_unique(&mut roots, PathBuf::from(value));
            }
        }
    }
    if let Some(home) = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
    {
        for definition in DIRECTORY_CLIENTS {
            if let Some(user_dir) = definition.skill_user_dir() {
                push_unique(&mut roots, home.join(user_dir));
            }
        }
    }
    roots
}

fn push_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn sweep_root(root: &Path, active_codex_root: Option<&Path>, report: &mut HygieneReport) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    // 删除只允许发生在"扫到的这个根"里：目录项自身是链接时只删链接，父级被换成
    // 指向别处的联接时直接拒绝，避免把根外目录当成残留删掉。
    let Ok(guard) = crate::path_guard::TrustedRoot::new(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if is_residual_name(&name) {
            if is_stale_residue(&path, &name)
                && guard.remove_tree(&path, "清理中断残留").unwrap_or(false)
            {
                report.removed_residue.push(path);
            }
            continue;
        }
        if !is_legacy_managed_tree(&path) {
            continue;
        }
        if !superseded_by_modern_layout(&name, active_codex_root) {
            report.retained_legacy.push(path);
            continue;
        }
        if guard
            .remove_tree(&path, "清理旧布局技能目录")
            .unwrap_or(false)
        {
            report.removed_legacy.push(path);
        }
    }
}

fn is_residual_name(name: &str) -> bool {
    name.starts_with(RESIDUE_PREFIX) && (name.contains("-staging-") || name.contains("-backup-"))
}

/// 残留是否已经"足够旧"。
///
/// 目录名里的时间戳是渲染时的毫秒数（见 `codex::unique_stamp`），比文件系统
/// 修改时间更可靠：复制、备份、恢复都可能把 mtime 刷新成"刚刚"。名字里读不出
/// 时间戳时（历史命名），退回 mtime 判断。
pub(crate) fn is_stale_residue(path: &Path, name: &str) -> bool {
    let stamp = name
        .rsplit_once("-staging-")
        .or_else(|| name.rsplit_once("-backup-"))
        .map(|(_, stamp)| stamp);
    if let Some(stamp) = stamp {
        if let Some(millis) = stamp
            .split('-')
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            let created = SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
            return SystemTime::now()
                .duration_since(created)
                .map(|age| age >= RESIDUE_MIN_AGE)
                .unwrap_or(false);
        }
    }
    is_older_than(path, RESIDUE_MIN_AGE)
}

/// 只有 `<skill-id>/current` 或 `<skill-id>/previous` 带 HiMind 收据的目录，
/// 才能证明是 HiMind 写出来的 legacy 托管树。
fn is_legacy_managed_tree(dir: &Path) -> bool {
    ["current", "previous"]
        .iter()
        .any(|layout| read_receipt_skill_id(&dir.join(layout).join(RECEIPT_NAME)).is_some())
}

fn read_receipt_skill_id(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    let receipt: SkillReceipt =
        serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()?;
    (!receipt.skill_id.trim().is_empty()).then_some(receipt.skill_id)
}

fn superseded_by_modern_layout(legacy_dir_name: &str, active_codex_root: Option<&Path>) -> bool {
    let Some(active_root) = active_codex_root else {
        return false;
    };
    let Some(slug) = legacy_dir_name.rsplit('.').next() else {
        return false;
    };
    if slug.is_empty() || slug == legacy_dir_name {
        // 没有点分 ID 就不是 legacy 布局，交给收据判断即可。
        return false;
    }
    active_root.join(slug).join(RECEIPT_NAME).is_file()
}

fn is_older_than(path: &Path, min_age: Duration) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age >= min_age)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn receipt(skill_id: &str) -> String {
        serde_json::to_string_pretty(&SkillReceipt {
            skill_id: skill_id.to_string(),
            version: "1.0.0".to_string(),
            client: "codex".to_string(),
            agent_profile: "production".to_string(),
            source_root: "source".to_string(),
            rendered_root: "target".to_string(),
            rendered_at: "now".to_string(),
            render_mode: "copy".to_string(),
            target_kind: crate::skill::target::TARGET_KIND_GLOBAL.to_string(),
            workspace_root: None,
            workspace_id: None,
            files: vec!["SKILL.md".to_string()],
            checksums: BTreeMap::new(),
        })
        .unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "himind-skill-hygiene-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_legacy_tree(root: &Path, skill_id: &str) {
        for layout in ["current", "previous"] {
            let dir = root.join(skill_id).join(layout);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), "# legacy").unwrap();
            fs::write(dir.join(RECEIPT_NAME), receipt(skill_id)).unwrap();
        }
        fs::write(root.join(skill_id).join("current.json"), "{}").unwrap();
    }

    #[test]
    fn removes_legacy_tree_once_modern_layout_exists() {
        let root = temp_root("legacy");
        let active = temp_root("active");
        write_legacy_tree(&root, "com.example.skill");
        let modern = active.join("skill");
        fs::create_dir_all(&modern).unwrap();
        fs::write(modern.join(RECEIPT_NAME), receipt("com.example.skill")).unwrap();

        let mut report = HygieneReport::default();
        sweep_root(&root, Some(&active), &mut report);

        assert_eq!(report.removed_legacy, vec![root.join("com.example.skill")]);
        assert!(report.retained_legacy.is_empty());
        assert!(!root.join("com.example.skill").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&active);
    }

    #[test]
    fn keeps_legacy_tree_when_it_is_the_only_copy() {
        let root = temp_root("only-copy");
        write_legacy_tree(&root, "com.example.skill");

        let mut report = HygieneReport::default();
        sweep_root(&root, None, &mut report);

        assert!(report.removed_legacy.is_empty());
        assert_eq!(report.retained_legacy, vec![root.join("com.example.skill")]);
        assert!(root.join("com.example.skill").join("current").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn leaves_user_owned_skill_directories_alone() {
        let root = temp_root("user");
        let own = root.join("my-own-skill");
        fs::create_dir_all(own.join("references")).unwrap();
        fs::write(own.join("SKILL.md"), "# mine").unwrap();
        // 没有收据的同名 legacy 形状同样不动。
        let lookalike = root.join("com.example.skill").join("current");
        fs::create_dir_all(&lookalike).unwrap();
        fs::write(lookalike.join("SKILL.md"), "# mine").unwrap();

        let mut report = HygieneReport::default();
        sweep_root(&root, None, &mut report);

        assert!(report.removed_legacy.is_empty());
        assert!(own.join("SKILL.md").exists());
        assert!(lookalike.join("SKILL.md").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn removes_stale_residue_and_keeps_fresh_ones() {
        let root = temp_root("residue");
        let stale = root.join(".himind-demo-staging-12-1");
        let fresh_stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let fresh = root.join(format!(".himind-demo-user-backup-{fresh_stamp}-2"));
        fs::create_dir_all(&stale).unwrap();
        fs::create_dir_all(&fresh).unwrap();

        let mut report = HygieneReport::default();
        sweep_root(&root, None, &mut report);

        assert_eq!(report.removed_residue, vec![stale.clone()]);
        assert!(!stale.exists());
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_residue_names_only_for_himind_prefixes() {
        assert!(is_residual_name(".himind-demo-staging-1-1"));
        assert!(is_residual_name(".himind-demo-backup-1-1"));
        assert!(is_residual_name(".himind-demo-user-backup-1-1"));
        assert!(!is_residual_name(".system"));
        assert!(!is_residual_name("demo-staging-1"));
    }
}
