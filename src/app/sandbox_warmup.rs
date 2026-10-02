//! HiMind AI（DSH）会话的沙箱写权限预热。
//!
//! Windows 上 DSH 用受限令牌沙箱跑命令：工作区根目录先拿到一条能力 SID 的写
//! 权限 ACE，再由继承把权限铺满整棵目录树。ACE 一旦落地就是常驻的（`standing`，
//! 跨会话复用），命中之后这条路只有一次 DACL 读，几十毫秒；但**第一次**落地要向
//! 每个子目录逐个传播，实测 135,440 个文件 / 4.0 GB 的工作区要 90 秒，14,355 个
//! 文件的工作区只要 2 秒。
//!
//! 这笔钱按设计是算在「第一次用到工作区的那一刻」——也就是用户的第一条命令里，
//! 表现出来就是 HiMind AI 卡在工具调用上几分钟。这里把它提前到会话启动时、放在
//! 后台线程里付掉，用户第一次用工具就只付那几十毫秒。做的动作和真实沙箱完全
//! 一致（同一个厂商模块、同一条 ACE、同一个 `standing` 语义），所以不会造出第二个
//! 真相：厂商模块自己的 `withPathLock` 是跨进程文件锁，预热和真实调用撞上也只是
//! 排队，不会写出两条互相覆盖的 DACL。
//!
//! 边界：预热失败不影响会话。缺少 `node.exe` 或 ACL 模块（例如
//! `HIMIND_DSH_EXECUTABLE` 指向的开发构建）就是「少了一次加速」，只记一行日志。

use std::path::Path;

/// 预热结果。三种情况要区分开：跳过不是失败，失败要留下证据。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WarmupOutcome {
    /// 写权限已就位（可能本来就已常驻）。
    Ready(String),
    /// 这台机器/这份运行时用不到预热。
    Skipped(String),
    /// 预热真的跑了但没成，真实调用还要自己付那笔钱。
    Failed(String),
}

impl WarmupOutcome {
    fn log_line(&self, workspace: &Path, elapsed_ms: u128) -> (bool, String) {
        let workspace = workspace.display();
        match self {
            WarmupOutcome::Ready(detail) => (
                false,
                format!("沙箱写权限预热完成 {workspace}（{elapsed_ms} ms）：{detail}"),
            ),
            WarmupOutcome::Skipped(reason) => {
                (false, format!("沙箱写权限预热跳过 {workspace}：{reason}"))
            }
            WarmupOutcome::Failed(reason) => (
                true,
                format!("沙箱写权限预热失败 {workspace}（{elapsed_ms} ms）：{reason}"),
            ),
        }
    }
}

/// 预热一个工作区。调用方拿到的是 `Result` 语义的结论，不抛异常：
/// 会话启动路径不允许因为预热而失败。
pub(crate) fn warm(executable: &Path, workspace: &Path) -> WarmupOutcome {
    platform::warm(executable, workspace)
}

/// 后台预热一个工作区，不在调用线程上等待。
///
/// 同一个工作区在预热途中被再次请求时直接返回：厂商模块按路径加了跨进程锁，
/// 排队两次没有意义，只会多起一个 node 进程。
pub(crate) fn spawn(executable: &Path, workspace: &Path) {
    let Some(key) = claim(workspace) else {
        return;
    };
    let executable = executable.to_path_buf();
    let workspace = workspace.to_path_buf();
    let thread_workspace = workspace.clone();
    let thread_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("himind-sandbox-warmup".to_string())
        .spawn(move || {
            let started = std::time::Instant::now();
            let outcome = warm(&executable, &thread_workspace);
            release(&thread_key);
            let (warning, line) =
                outcome.log_line(&thread_workspace, started.elapsed().as_millis());
            if warning {
                crate::approval::manager::append_event_log("warn", &line);
            } else {
                crate::approval::manager::append_event_log("info", &line);
            }
        });
    if spawned.is_err() {
        release(&key);
    }
}

/// 顺序预热一批工作区（Agent 启动时用：把已知的拓展工作区提前铺好）。
/// 顺序而不是并发，是为了不让一堆 node 进程同时啃磁盘。
pub(crate) fn spawn_batch(executable: &Path, workspaces: Vec<std::path::PathBuf>) {
    if workspaces.is_empty() {
        return;
    }
    let executable = executable.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("himind-sandbox-warmup-batch".to_string())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut warmed = 0usize;
            for workspace in workspaces {
                let Some(key) = claim(&workspace) else {
                    continue;
                };
                let item_started = std::time::Instant::now();
                let outcome = warm(&executable, &workspace);
                release(&key);
                let (warning, line) =
                    outcome.log_line(&workspace, item_started.elapsed().as_millis());
                if warning {
                    crate::approval::manager::append_event_log("warn", &line);
                } else {
                    crate::approval::manager::append_event_log("info", &line);
                }
                if matches!(outcome, WarmupOutcome::Skipped(_)) {
                    // 跳过说明这批工作区没有预热价值（运行时缺模块），继续也没用。
                    break;
                }
                warmed += 1;
            }
            crate::approval::manager::append_event_log(
                "info",
                &format!(
                    "沙箱写权限预热批次结束：{} 个工作区，总耗时 {} ms",
                    warmed,
                    started.elapsed().as_millis()
                ),
            );
        });
}

fn claim(workspace: &Path) -> Option<String> {
    let key = dedupe_key(workspace);
    let mut in_flight = in_flight().lock().ok()?;
    in_flight.insert(key.clone()).then_some(key)
}

fn release(key: &str) {
    if let Ok(mut in_flight) = in_flight().lock() {
        in_flight.remove(key);
    }
}

/// 去重键：同一个工作区可能以两种写法到达——工作区绑定里存的是规范前缀形式
/// `\\?\F:\dir`（长路径安全），最近会话里存的是普通形式 `F:\dir`。两者指向同一
/// 棵目录树，厂商的能力 SID 也按同一条规范路径派生（实测两种写法得到的 SID 完全
/// 相同），所以必须归一到同一把键，否则同一个工作区会被两条线程同时铺一遍 ACL：
/// 实测那种并发会把 21 秒的批量预热拖成 24.9 秒，并且几倍地放大磁盘写入。
///
/// 只归一「键」，不归一「交给 node 的路径」——保留 `\\?\` 前缀是为了不破坏
/// 超过 MAX_PATH 的工作区。
fn dedupe_key(workspace: &Path) -> String {
    let text = workspace.to_string_lossy();
    let trimmed = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\\.\"))
        .unwrap_or(text.as_ref());
    let trimmed = trimmed.trim_end_matches(['\\', '/']);
    // 纯盘符（`F:`）会丢掉根分隔符语义，退回到原写法。
    let normalized = if trimmed.len() <= 2 && trimmed.ends_with(':') {
        text.as_ref()
    } else {
        trimmed
    };
    normalized.to_ascii_lowercase()
}

fn in_flight() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(windows)]
mod platform {
    use super::WarmupOutcome;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    /// 预热脚本：和厂商沙箱对同一件事的描述一模一样——用工作区规范路径派生的
    /// 能力 SID，把写权限 ACE 常驻（`standing = true`）地铺到工作区根，让继承
    /// 覆盖整棵树。用厂商模块而不是自己调 Win32，是为了不复制那份 SID 派生与
    /// ACE 形状（DDL、mask、OI/CI 标志）的约定。
    const WARMUP_SCRIPT: &str = r#"
const { realpathSync } = require("node:fs");
const { pathToFileURL } = require("node:url");
(async () => {
  const root = realpathSync.native(process.argv[2]);
  const acl = await import(pathToFileURL(process.argv[1]).href);
  const writeSid = acl.workspaceWriteSid(root);
  acl.AclWriteGrant.create(writeSid).add(root, true);
  console.log("ready " + writeSid + " " + root);
})().catch((error) => {
  const message = error && error.message ? error.message : String(error);
  console.error("failed: " + message);
  process.exitCode = 1;
});
"#;

    /// 从运行时目录里定位 `node.exe` 与厂商 ACL 模块入口。两者都是运行时自带物：
    /// `<version>\node.exe` 和
    /// `<version>\node_modules\@deepseek-ai\dsh-sandbox-windows-acl\lib\index.js`。
    /// `executable` 是 `<version>\bin\dsh.cmd`，所以版本目录是它的上上层。
    pub(super) fn acl_module_paths(executable: &Path) -> Option<(PathBuf, PathBuf)> {
        let version_root = executable.parent()?.parent()?;
        let node = version_root.join("node.exe");
        let module = version_root
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-sandbox-windows-acl")
            .join("lib")
            .join("index.js");
        (node.is_file() && module.is_file()).then_some((node, module))
    }

    pub(super) fn warm(executable: &Path, workspace: &Path) -> WarmupOutcome {
        let Some((node, module)) = acl_module_paths(executable) else {
            return WarmupOutcome::Skipped(
                "当前运行时不含 Windows ACL 沙箱模块（node.exe / dsh-sandbox-windows-acl 缺失）"
                    .to_string(),
            );
        };
        if !workspace.is_dir() {
            return WarmupOutcome::Skipped("工作区目录不可用".to_string());
        }
        let mut command = crate::runtime::process::hidden_command(&node);
        command
            .arg("-e")
            .arg(WARMUP_SCRIPT)
            .arg(&module)
            .arg(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::runtime::process::remove_himind_secret_environment(&mut command);
        let output = match command.output() {
            Ok(output) => output,
            Err(error) => return WarmupOutcome::Failed(format!("无法执行 node：{error}")),
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if output.status.success() {
            WarmupOutcome::Ready(if stdout.is_empty() {
                "ready".to_string()
            } else {
                stdout
            })
        } else {
            WarmupOutcome::Failed(if stderr.is_empty() { stdout } else { stderr })
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::WarmupOutcome;
    use std::path::Path;

    /// 非 Windows 上沙箱靠 bwrap / Landlock / Seatbelt，没有「铺满整棵树」的
    /// 一次性成本，因此不需要预热。
    pub(super) fn warm(_executable: &Path, _workspace: &Path) -> WarmupOutcome {
        WarmupOutcome::Skipped("当前平台不使用 Windows ACL 沙箱".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-sandbox-warmup-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// 运行时目录布局是"约定"而不是"配置"：版本目录由可执行文件的位置反推。
    #[cfg(windows)]
    #[test]
    fn acl_module_paths_follow_the_runtime_layout() {
        let root = temp_root("layout");
        let version = root.join("versions").join("0.1.5-rc.2-test");
        let bin = version.join("bin");
        let module = version
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-sandbox-windows-acl")
            .join("lib");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&module).unwrap();
        fs::write(version.join("node.exe"), b"node").unwrap();
        fs::write(module.join("index.js"), b"module").unwrap();
        let executable = bin.join("dsh.cmd");
        fs::write(&executable, b"cmd").unwrap();

        let (node, resolved_module) = platform::acl_module_paths(&executable).unwrap();
        assert_eq!(node, version.join("node.exe"));
        assert_eq!(resolved_module, module.join("index.js"));
        let _ = fs::remove_dir_all(&root);
    }

    /// 模块缺失必须退化成"跳过"，而不是让会话启动挂上一个不存在的 node。
    #[cfg(windows)]
    #[test]
    fn missing_acl_module_is_a_skip_not_a_failure() {
        let root = temp_root("missing");
        let bin = root.join("versions").join("0.1.5").join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("dsh.cmd");
        fs::write(&executable, b"cmd").unwrap();

        assert!(platform::acl_module_paths(&executable).is_none());
        assert!(matches!(
            warm(&executable, &root),
            WarmupOutcome::Skipped(_)
        ));
        let _ = fs::remove_dir_all(&root);
    }

    /// 工作区不存在时也不允许 panic：预热是加速，不是功能前置条件。
    #[cfg(windows)]
    #[test]
    fn warm_on_a_missing_workspace_does_not_panic() {
        let root = temp_root("no-workspace");
        let outcome = warm(
            &root.join("nonexistent").join("dsh.cmd"),
            &root.join("nonexistent-workspace"),
        );
        assert!(matches!(outcome, WarmupOutcome::Skipped(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn in_flight_claims_are_exclusive() {
        let workspace = std::env::temp_dir().join("himind-warmup-claim");
        let first = claim(&workspace).unwrap();
        assert!(claim(&workspace).is_none());
        release(&first);
        assert!(claim(&workspace).is_some());
        release(&workspace.to_string_lossy().to_ascii_lowercase());
    }

    /// 同一个工作区的两种写法必须归一到同一把键：真实日志里工作区绑定给的是
    /// `\\?\F:\dir`，最近会话给的是 `F:\dir`，不归一就会并发铺两遍 ACL。
    #[test]
    fn dedupe_key_normalizes_path_spellings() {
        assert_eq!(
            dedupe_key(Path::new(r"\\?\F:\WebProjects\demo")),
            dedupe_key(Path::new(r"F:\WebProjects\demo"))
        );
        assert_eq!(
            dedupe_key(Path::new(r"F:\WebProjects\demo\")),
            dedupe_key(Path::new(r"F:\WebProjects\demo"))
        );
        assert_eq!(
            dedupe_key(Path::new(r"\\?\F:\WebProjects\demo")),
            r"f:\webprojects\demo"
        );
        // 纯盘符不能因为没有分隔符而退化成另一个工作区。
        assert_eq!(dedupe_key(Path::new("F:")), "f:");
    }

    #[test]
    fn claims_collapse_the_two_spellings_of_one_workspace() {
        let plain = Path::new(r"\\?\C:\himind-warmup-spelling\demo");
        let prefixed = Path::new(r"C:\himind-warmup-spelling\demo");
        let first = claim(plain).unwrap();
        assert!(claim(prefixed).is_none());
        release(&first);
        assert!(claim(prefixed).is_some());
        release(&dedupe_key(prefixed));
    }
}
