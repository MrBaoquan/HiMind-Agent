//! 可信根（trusted root）路径校验。
//!
//! 安装台账、项目锁文件、渲染收据、工作区登记表都可能出现在仓库里 —— 也就是
//! 可能被提交、被他人改写、被旧版本写坏。任何"删除、覆盖、改名"操作都不能直接
//! 使用这些记录里的路径字符串：调用方必须先从适配器配置（客户端目录常量、用户
//! 选择的目录、工作区根）重新推导一个可信根，再确认目标落在根内。
//!
//! 校验必须是 fail-closed 的：解析失败、路径含 `..`、目标解析到根之外，都要报错
//! 并中止操作，而不是"跳过这一条继续跑"。
//!
//! 关键点是**真实路径**而不是字面路径。Windows 的目录联接（junction）和符号链接
//! 会让一个看起来在根内的路径实际指向根外的位置；`fs::canonicalize` 会解析这些
//! 重解析点，所以比较之前先解析、比较之后再删除。对于还不存在的路径，先把最近
//! 的存在祖先解析掉，再把剩余片段接回去，避免"链接 + 未创建子目录"的逃逸。

use std::error::Error;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// 一个已经解析过、确认存在且确实是目录的可信根。
///
/// 类型本身没有额外的安全语义，价值在于把"可信根"这个概念固化成构造一次、
/// 反复校验的对象，避免调用方在删除现场临时拼字符串。
#[derive(Debug, Clone)]
pub(crate) struct TrustedRoot {
    root: PathBuf,
}

impl TrustedRoot {
    /// 由适配器配置推导出的根目录。必须是已存在的目录。
    pub(crate) fn new(path: &Path) -> Result<Self, Box<dyn Error>> {
        let root = path
            .canonicalize()
            .map_err(|error| format!("可信根不可访问: {} ({error})", path.display()))?;
        if !root.is_dir() {
            return Err(format!("可信根必须是目录: {}", path.display()).into());
        }
        Ok(Self { root })
    }

    /// 由适配器配置推导出的根目录，允许根本身还没被创建。
    ///
    /// 环境变量指向的自定义客户端目录在首次渲染前并不存在，这时向上取最近的
    /// 存在祖先作为根。祖先同样会被解析，所以"父目录是指向别处的联接"依旧会被
    /// 后面的包含性判断抓住。
    pub(crate) fn nearest_existing(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut cursor = resolve_real_path(path)?;
        loop {
            if cursor.is_dir() {
                return Ok(Self { root: cursor });
            }
            if is_reparse_point(&cursor) {
                return Err(format!("可信根不能被替换成链接: {}", cursor.display()).into());
            }
            match cursor.parent() {
                Some(parent) if parent != cursor => cursor = parent.to_path_buf(),
                _ => {
                    return Err(format!("可信根不存在: {}", path.display()).into());
                }
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    /// 把候选路径解析成真实路径，并确认它落在可信根内。
    ///
    /// 返回解析后的路径：调用方应当拿这个返回值去做文件系统操作，而不是拿原始
    /// 字符串 —— 否则重解析点会在"检查"和"使用"之间被重新引入。
    pub(crate) fn ensure_within(
        &self,
        candidate: &Path,
        action: &str,
    ) -> Result<PathBuf, Box<dyn Error>> {
        let resolved = resolve_real_path(candidate)?;
        if !path_is_within(&self.root, &resolved) {
            return Err(format!(
                "拒绝{action}：目标解析到可信根之外\n  目标: {}\n  解析为: {}\n  可信根: {}",
                candidate.display(),
                resolved.display(),
                self.root.display()
            )
            .into());
        }
        if resolved == self.root {
            return Err(format!("拒绝{action}：目标就是可信根自身 {}", self.root.display()).into());
        }
        Ok(resolved)
    }

    /// 校验并递归删除一个目录。
    ///
    /// 目标不存在时返回 `Ok(false)` —— 幂等的卸载需要这个语义。目录联接 / 符号
    /// 链接本身只删除链接，不删除链接指向的内容。
    pub(crate) fn remove_tree(
        &self,
        candidate: &Path,
        action: &str,
    ) -> Result<bool, Box<dyn Error>> {
        // 候选本身就是链接时，`canonicalize` 会跟到链接指向的位置（可能在根外），
        // 但我们要删的是根内这个目录项本身。所以这里校验"目录项位置"，删链接。
        if is_reparse_point(candidate) {
            self.ensure_entry_within(candidate, action)?;
            remove_link(candidate)?;
            return Ok(true);
        }
        let resolved = self.ensure_within(candidate, action)?;
        if !resolved.exists() {
            return Ok(false);
        }
        // 解析之后 resolved 里不再有重解析点，直接删是安全的。
        std::fs::remove_dir_all(&resolved)?;
        Ok(true)
    }

    /// 校验并删除一个文件（不跟随链接）。
    pub(crate) fn remove_file(
        &self,
        candidate: &Path,
        action: &str,
    ) -> Result<bool, Box<dyn Error>> {
        if is_reparse_point(candidate) {
            self.ensure_entry_within(candidate, action)?;
            remove_link(candidate)?;
            return Ok(true);
        }
        let resolved = self.ensure_within(candidate, action)?;
        if !resolved.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&resolved)?;
        Ok(true)
    }

    /// 校验并删除一个空目录（非空时失败，不递归）。
    pub(crate) fn remove_empty_dir(
        &self,
        candidate: &Path,
        action: &str,
    ) -> Result<bool, Box<dyn Error>> {
        let resolved = self.ensure_within(candidate, action)?;
        if !resolved.exists() {
            return Ok(false);
        }
        std::fs::remove_dir(&resolved)?;
        Ok(true)
    }

    /// 校验一个"目录项"本身的位置是否落在根内，不跟随该目录项自己的链接。
    ///
    /// 父目录链照常解析（中途的联接会暴露），最后一段名字按字面接回去。这样
    /// `root\linked -> D:\elsewhere` 这种"根内的链接"可以安全清理链接本身，
    /// 而 `root\skills\precious`（`skills` 是指向根外的链接）依旧会被拒绝。
    pub(crate) fn ensure_entry_within(
        &self,
        candidate: &Path,
        action: &str,
    ) -> Result<PathBuf, Box<dyn Error>> {
        let Some(name) = candidate.file_name() else {
            return Err(format!("拒绝{action}：路径没有末段名字 {}", candidate.display()).into());
        };
        let Some(parent) = candidate
            .parent()
            .filter(|item| !item.as_os_str().is_empty())
        else {
            return Err(format!("拒绝{action}：路径没有父目录 {}", candidate.display()).into());
        };
        let resolved_parent = resolve_real_path(parent)?;
        let location = resolved_parent.join(name);
        if !components_contain(&self.root, &location) {
            return Err(format!(
                "拒绝{action}：目录项落在可信根之外\n  目标: {}\n  位置: {}\n  可信根: {}",
                candidate.display(),
                location.display(),
                self.root.display()
            )
            .into());
        }
        Ok(location)
    }

    /// 校验源和目标都在根内，且目标是安全的重命名落点。
    ///
    /// 渲染失败回滚依赖这一步：只有确认备份目录落在同一个可信根内，才允许把
    /// 已存在的渲染结果搬到它下面。
    pub(crate) fn rename_within(
        &self,
        from: &Path,
        to: &Path,
        action: &str,
    ) -> Result<(), Box<dyn Error>> {
        let resolved_from = self.ensure_within(from, action)?;
        let resolved_to = self.ensure_within(to, action)?;
        if resolved_to.exists() {
            return Err(format!("拒绝{action}：目标已存在 {}", resolved_to.display()).into());
        }
        if let Some(parent) = resolved_to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&resolved_from, &resolved_to)?;
        Ok(())
    }
}

/// 把路径解析成"真实路径"，允许尾部还不存在。
///
/// 存在的路径直接 `canonicalize`。不存在的路径先向上找到最近的存在祖先，解析它，
/// 再把剩下的片段按字面接回去。含 `..` 的路径一律拒绝：合法调用方从不构造它们，
/// 而放过它们会让包含性判断形同虚设。
pub(crate) fn resolve_real_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if path.as_os_str().is_empty() {
        return Err("路径为空".into());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(format!("路径包含上级目录引用，拒绝解析: {}", path.display()).into());
    }
    if let Ok(canonical) = path.canonicalize() {
        return Ok(canonical);
    }

    let mut tail: Vec<OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match cursor.file_name() {
            Some(name) => tail.push(name.to_os_string()),
            None => return Err(format!("路径无法解析: {}", path.display()).into()),
        }
        let Some(parent) = cursor.parent() else {
            return Err(format!("路径无法解析: {}", path.display()).into());
        };
        if parent.as_os_str().is_empty() {
            return Err(format!("路径无法解析: {}", path.display()).into());
        }
        if let Ok(canonical) = parent.canonicalize() {
            let mut resolved = canonical;
            for part in tail.iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
        if cursor == parent {
            return Err(format!("路径无法解析: {}", path.display()).into());
        }
        cursor = parent.to_path_buf();
    }
}

/// 候选路径是否落在根内（含根自身）。两侧都会先解析成真实路径。
pub(crate) fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let Ok(resolved_root) = resolve_real_path(root) else {
        return false;
    };
    let Ok(resolved_candidate) = resolve_real_path(candidate) else {
        return false;
    };
    components_contain(&resolved_root, &resolved_candidate)
}

/// 便捷函数：直接用字面根校验候选路径。调用方只有在校验"根"这件事本身不涉及
/// 台账内容时才该用它；其余场景请先构造 [`TrustedRoot`]。
pub(crate) fn ensure_path_within(
    root: &Path,
    candidate: &Path,
    action: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let root = TrustedRoot::new(root)?;
    root.ensure_within(candidate, action)
}

fn components_contain(root: &Path, candidate: &Path) -> bool {
    let root_parts = fold_components(root);
    let candidate_parts = fold_components(candidate);
    candidate_parts.len() >= root_parts.len()
        && candidate_parts[..root_parts.len()] == root_parts[..]
}

/// 归一分量：Windows 上路径比较是大小写不敏感的，分隔符也要统一。
/// 逐分量比较（而不是字符串 `starts_with`）避免 `C:\a\b` 与 `C:\a\bc` 这类
/// 前缀混淆。
fn fold_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|part| {
            let text = part.as_os_str().to_string_lossy().replace('/', "\\");
            let text = text.trim_end_matches('\\');
            if cfg!(windows) {
                text.to_ascii_lowercase()
            } else {
                text.to_string()
            }
        })
        .collect()
}

/// 该路径本身是不是重解析点（符号链接 / 目录联接）。
///
/// 用 `symlink_metadata` 而不是 `metadata`：后者会跟随链接，拿不到链接本身。
pub(crate) fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// 这个字符串能不能当成"单个目录名"去拼路径。
///
/// 光看字符集是不够的：点号本身是合法字符，于是 `..` 会大摇大摆地通过只查字符集的
/// 校验，`<root>/versions/..` 就落到了 `<root>`，等于把版本目录挪出它该在的地方；
/// Windows 还会把结尾的点号和空格规范化掉，`..` 之外的 `...`、`name.` 同样会指向
/// 别的目录。所以这里在字符集之外再要求：不以点号结尾、且至少有一个字母数字。
///
/// 调用方仍然要自己决定字符集（插件 ID 不许有 `+`，技能版本号可以有），这个函数
/// 只回答"它会不会被文件系统解释成别的目录"。
pub(crate) fn is_safe_dir_name(value: &str) -> bool {
    !value.is_empty()
        && !value.ends_with(['.', ' '])
        && value.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

fn remove_link(path: &Path) -> Result<(), Box<dyn Error>> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(directory_error) => std::fs::remove_file(path).map_err(|file_error| {
            format!(
                "无法删除链接 {}: 目录方式失败({directory_error})，文件方式失败({file_error})",
                path.display()
            )
            .into()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "himind-path-guard-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn resolves_a_tail_that_does_not_exist_yet() {
        let root = temp_dir("tail");
        let candidate = root.join("client").join("skills").join("demo");
        let resolved = resolve_real_path(&candidate).unwrap();
        assert!(path_is_within(&root, &resolved));
        assert!(resolved.ends_with("skills/demo".replace('/', std::path::MAIN_SEPARATOR_STR)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_parent_dir_components() {
        let root = temp_dir("parent");
        // 注意：`PathBuf::push("..")` 会就地折叠上一段，所以必须按字面拼字符串，
        // 才能构造出"故意带 `..` 的路径"。台账里的路径正是这种字面字符串。
        let escape = PathBuf::from(format!("{}\\..\\elsewhere", root.display()));
        assert!(escape.components().count() > root.components().count());
        assert!(resolve_real_path(&escape).is_err());
        assert!(!path_is_within(&root, &escape));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_confuse_sibling_prefixes() {
        let base = temp_dir("prefix");
        let root = base.join("skills");
        fs::create_dir_all(&root).unwrap();
        let sibling = base.join("skills-backup").join("demo");
        fs::create_dir_all(&sibling).unwrap();
        assert!(!path_is_within(&root, &sibling));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn keeps_dot_only_names_out_of_path_segments() {
        for unsafe_name in [".", "..", "...", "1.0.0.", "name.", " ", ""] {
            assert!(
                !is_safe_dir_name(unsafe_name),
                "{unsafe_name:?} 不能被当成目录名"
            );
        }
        for safe_name in ["1.0.0", "com.himind.plugin", "0.0.0+sha.abcdef", "v1"] {
            assert!(
                is_safe_dir_name(safe_name),
                "{safe_name:?} 应该是合法目录名"
            );
        }
    }

    #[test]
    fn remove_tree_is_idempotent_and_refuses_the_root() {
        let base = temp_dir("remove");
        let root = TrustedRoot::new(&base).unwrap();
        assert!(!root.remove_tree(&base.join("missing"), "卸载").unwrap());

        let victim = base.join("skill");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("SKILL.md"), "demo").unwrap();
        assert!(root.remove_tree(&victim, "卸载").unwrap());
        assert!(!victim.exists());

        assert!(root.remove_tree(&base, "卸载").is_err());
        assert!(base.exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn refuses_a_target_outside_the_trusted_root() {
        let base = temp_dir("outside");
        assert!(
            TrustedRoot::new(&base.join("inside")).is_err(),
            "不存在的根必须构造失败"
        );
        fs::create_dir_all(base.join("inside")).unwrap();
        let root = TrustedRoot::new(&base.join("inside")).unwrap();
        let outside = base.join("outside").join("important");
        fs::create_dir_all(&outside).unwrap();
        assert!(root.remove_tree(&outside, "卸载").is_err());
        assert!(outside.exists());
        let _ = fs::remove_dir_all(base);
    }

    /// 重解析点逃逸：`root/link` 是指向根外目录的符号链接。
    /// 没有创建链接的权限时（非管理员且未开开发者模式）跳过，不把环境问题当失败。
    #[cfg(windows)]
    #[test]
    fn refuses_to_delete_through_a_symlinked_directory() {
        use std::os::windows::fs::symlink_dir;

        let base = temp_dir("symlink");
        let root_dir = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root_dir).unwrap();
        fs::create_dir_all(outside.join("precious")).unwrap();
        fs::write(outside.join("precious").join("SKILL.md"), "keep me").unwrap();

        let link = root_dir.join("skills");
        if symlink_dir(&outside, &link).is_err() {
            let _ = fs::remove_dir_all(base);
            return;
        }

        let root = TrustedRoot::new(&root_dir).unwrap();
        let escape = link.join("precious");
        assert!(root.remove_tree(&escape, "卸载").is_err());
        assert!(
            outside.join("precious").join("SKILL.md").is_file(),
            "符号链接指向的内容不能被删除"
        );
        let _ = fs::remove_dir_all(base);
    }

    /// 链接本身属于根内时，删除的是链接而不是链接指向的内容。
    #[cfg(windows)]
    #[test]
    fn removing_a_link_inside_the_root_keeps_the_target() {
        use std::os::windows::fs::symlink_dir;

        let base = temp_dir("link-keep");
        let root_dir = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("data.txt"), "keep").unwrap();

        let link = root_dir.join("linked");
        if symlink_dir(&outside, &link).is_err() {
            let _ = fs::remove_dir_all(base);
            return;
        }

        let root = TrustedRoot::new(&root_dir).unwrap();
        assert!(root.remove_tree(&link, "清理").unwrap());
        assert!(!is_reparse_point(&link));
        assert!(outside.join("data.txt").is_file());
        let _ = fs::remove_dir_all(base);
    }
}
