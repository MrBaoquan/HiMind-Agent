use std::env;
use std::path::{Path, PathBuf};

const DEFAULT_AGENT_DIRECTORY: &str = "HiMindAgent";
/// The shipped profile. Anything that is not a named, non-production profile
/// resolves to this one, so the historical data root never moves.
pub(crate) const PRODUCTION_PROFILE: &str = "production";
/// The profile a binary that is *not* an installation runs under.
///
/// A `cargo run`, a `target/release/himind-agent.exe`, or a copy of the build
/// output must never open the data root of the Agent the user actually
/// installed: that is how a development session erased a production identity.
pub(crate) const DEVELOPMENT_PROFILE: &str = "development";
/// WebView2 用户数据目录：生产安装由 Tauri 按 `tauri.conf.json` 的
/// identifier 推导出来，升级时必须落在同一个目录，否则用户的登录态、
/// 本地存储会跟着换目录一起丢。
const PRODUCTION_WEBVIEW_DIRECTORY: &str = "com.himind.agent";
/// 非生产 profile 的 WebView2 目录名，放在各自的 `agent_home()` 下。
const PROFILE_WEBVIEW_DIRECTORY: &str = "ebwebview";

/// Returns the persistent root for the current Agent profile.
///
/// `HIMIND_AGENT_HOME` is an explicit, complete root and therefore takes
/// precedence over the profile selector. The installed production Agent
/// keeps the historical default path; development profiles are nested below
/// `profiles/<name>` without changing production data.
pub(crate) fn agent_home() -> PathBuf {
    if let Some(explicit) = explicit_agent_home() {
        return explicit;
    }

    let base = local_app_data_base().join(DEFAULT_AGENT_DIRECTORY);
    let profile = env::var("HIMIND_AGENT_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| is_safe_profile_name(value));
    match profile {
        Some(profile) if profile != "production" && profile != "default" => {
            base.join("profiles").join(profile)
        }
        _ => base,
    }
}

fn local_app_data_base() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(env::temp_dir)
}

/// WebView2 的用户数据目录。
///
/// WebView2 的用户数据目录是**进程级独占**的：同一台机器上第二个 Agent
/// 进程（开发 profile，或显式放行的并行实例）如果指向同一个目录，WebView2
/// 环境创建会失败，表现出来就是「托盘图标在、主窗口没了」，而且进程里没有
/// 任何报错。所以目录必须跟着 profile 走：
///
/// - 生产保持历史目录 `%LOCALAPPDATA%\com.himind.agent`，升级不掉登录态；
/// - 其它 profile 落到各自的 `agent_home()/ebwebview`，与生产完全隔离；
/// - `HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE=1` 时再按本地端口分一份，
///   同一个 profile 的多个实例也不会互相顶掉（端口本来就要求互不相同）。
pub(crate) fn webview_user_data_dir(local_port: u16) -> PathBuf {
    let mut directory = if profile_name() == "production" {
        local_app_data_base().join(PRODUCTION_WEBVIEW_DIRECTORY)
    } else {
        agent_home().join(PROFILE_WEBVIEW_DIRECTORY)
    };
    if env::var("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE").as_deref() == Ok("1") {
        directory = directory.join(format!("instance-{local_port}"));
    }
    directory
}

/// 测试与排查用的显式覆盖：命令行环境里给了 WebView2 目录就以它为准。
pub(crate) fn explicit_webview_user_data_dir() -> Option<PathBuf> {
    ["WEBVIEW2_USER_DATA_FOLDER", "HIMIND_AGENT_WEBVIEW_DATA_DIR"]
        .iter()
        .find_map(|key| {
            env::var_os(key)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        })
}

/// 把 WebView2 用户数据目录落到当前进程环境里，必须在 Tauri 建任何 WebView
/// 之前调用：wry 创建 WebView2 环境时读的就是这个变量，Tauri 自己按
/// identifier 推导的目录会被它覆盖掉。
///
/// 显式设置过 `WEBVIEW2_USER_DATA_FOLDER` 时不再改动，保留排查手段。
pub(crate) fn apply_webview_user_data_dir(local_port: u16) -> PathBuf {
    if let Some(explicit) = explicit_webview_user_data_dir() {
        return explicit;
    }
    let directory = webview_user_data_dir(local_port);
    env::set_var("WEBVIEW2_USER_DATA_FOLDER", &directory);
    directory
}

pub(crate) fn profile_name() -> String {
    env::var("HIMIND_AGENT_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| is_safe_profile_name(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| PRODUCTION_PROFILE.to_string())
}

/// Which selector decided the effective profile.
///
/// Only [`ProfileSource::Argument`] and [`ProfileSource::Environment`] mean the
/// user asked for that profile by name; the other two are inference, and the
/// caller may report them differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileSource {
    /// `--profile <name>` on the command line.
    Argument,
    /// `HIMIND_AGENT_PROFILE` in the process environment.
    Environment,
    /// No selector: the executable sits in an installation layout.
    InstalledDefault,
    /// No selector and the executable is a build output.
    DevelopmentDefault,
}

impl ProfileSource {
    /// True when the profile was inferred rather than requested.
    pub(crate) fn inferred(self) -> bool {
        matches!(self, Self::InstalledDefault | Self::DevelopmentDefault)
    }
}

/// `true` for every name that means "the user's installed Agent".
pub(crate) fn is_production_profile(profile: &str) -> bool {
    let profile = profile.trim();
    profile.is_empty() || profile == PRODUCTION_PROFILE || profile == "default"
}

/// Normalize an explicit profile selector, rejecting unsafe names.
pub(crate) fn normalize_profile(value: &str) -> Option<String> {
    let value = value.trim();
    is_safe_profile_name(value).then(|| value.to_string())
}

/// Decide the profile this process runs under.
///
/// Precedence is deliberate: an explicit selector first (`--profile`, then
/// `HIMIND_AGENT_PROFILE`), which may name `production`; otherwise the
/// executable decides. A binary inside an installation (`<root>/versions/<v>/`
/// or `<root>/current/`, with the launcher next to it) is the installed Agent
/// and keeps the production data root. Anything else is a build output and gets
/// its own profile.
///
/// That second step is what makes "run the development Agent next to the
/// installed one" safe without asking the user to configure anything.
pub(crate) fn resolve_profile(
    explicit: Option<&str>,
    executable: &Path,
) -> (String, ProfileSource) {
    if let Some(profile) = explicit.and_then(normalize_profile) {
        return (profile, ProfileSource::Argument);
    }
    if let Some(profile) = env::var("HIMIND_AGENT_PROFILE")
        .ok()
        .and_then(|value| normalize_profile(&value))
    {
        return (profile, ProfileSource::Environment);
    }
    if crate::install_layout::executable_is_installed(executable) {
        (
            PRODUCTION_PROFILE.to_string(),
            ProfileSource::InstalledDefault,
        )
    } else {
        (
            DEVELOPMENT_PROFILE.to_string(),
            ProfileSource::DevelopmentDefault,
        )
    }
}

/// Pin the resolved profile into the process environment.
///
/// Every existing call site reads `HIMIND_AGENT_PROFILE` (directly or through
/// [`profile_name`]), so writing it here is what makes the whole code base —
/// the stdio MCP companion and the WebView2 data directory included — agree on
/// one profile without threading a new parameter through dozens of call sites.
pub(crate) fn apply_profile(profile: &str) {
    env::set_var("HIMIND_AGENT_PROFILE", profile);
}

/// An explicit, complete data root was requested for this process.
pub(crate) fn explicit_agent_home() -> Option<PathBuf> {
    env::var_os("HIMIND_AGENT_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// `true` when `path` sits inside the installed Agent's data root.
fn is_inside_production_root(path: &Path) -> bool {
    if explicit_agent_home().is_some() {
        return false;
    }
    let root = local_app_data_base().join(DEFAULT_AGENT_DIRECTORY);
    if !path.starts_with(&root) {
        return false;
    }
    // `<root>/profiles/<name>` 是别的 profile 自己的地盘，不属于生产。
    !path.starts_with(root.join("profiles"))
}

/// `true` when an explicit `--state` would cross from this profile into the
/// installed Agent's data root.
///
/// Every AI client that the Agent registers itself into keeps a copy of the
/// launch line, `--state <data root>\agent-state.json` included. Those lines
/// outlive the build that wrote them, so an entry recorded by an earlier Agent
/// still names the *production* state file. Honouring it would hand the
/// installed Agent's identity to a development process — the exact crossover
/// the profile split exists to prevent. The caller ignores the flag instead,
/// and the profile-derived path wins.
///
/// `HIMIND_AGENT_HOME` stays the deliberate way to point a foreign binary at a
/// data root of your choosing, so it disables this guard.
pub(crate) fn explicit_state_crosses_profiles(path: &Path) -> bool {
    !is_production_profile(&profile_name()) && is_inside_production_root(path)
}

fn is_safe_profile_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Serializes tests that flip the process-global Agent home variables.
///
/// `HIMIND_AGENT_HOME` and `HIMIND_AGENT_PROFILE` are read on every call to
/// [`agent_home`], so a test that repoints them for isolation silently moves
/// every other parallel test's paths mid-run. Every test that sets or restores
/// them must hold this lock, and tests that need a stable home across several
/// calls should hold it as well.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_profile_name, resolve_profile, test_env_lock, webview_user_data_dir, ProfileSource,
    };
    use std::env;
    use std::fs;
    use std::path::Path;

    #[test]
    fn profile_names_are_path_safe() {
        assert!(is_safe_profile_name("development"));
        assert!(is_safe_profile_name("ecs-staging_01"));
        assert!(!is_safe_profile_name("../production"));
        assert!(!is_safe_profile_name("开发"));
        assert!(!is_safe_profile_name(""));
    }

    /// WebView2 只允许一个进程独占用户数据目录，同一台机器上的第二个 Agent
    /// 进程如果和别的 profile 共用一个目录，就会静默建不出主窗口。这条用例
    /// 把「目录跟着 profile 走」钉住，避免以后又退回到共享目录。
    #[test]
    fn webview_data_directory_follows_profile() {
        let _guard = test_env_lock();
        let previous_profile = env::var_os("HIMIND_AGENT_PROFILE");
        let previous_home = env::var_os("HIMIND_AGENT_HOME");
        let previous_parallel = env::var_os("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE");
        env::remove_var("HIMIND_AGENT_HOME");
        env::remove_var("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE");

        env::set_var("HIMIND_AGENT_PROFILE", "development");
        let development = webview_user_data_dir(18082);
        assert!(development.ends_with(Path::new("profiles").join("development").join("ebwebview")));

        env::set_var("HIMIND_AGENT_PROFILE", "production");
        let production = webview_user_data_dir(18181);
        assert!(production.ends_with("com.himind.agent"));
        assert_ne!(production, development);

        env::set_var("HIMIND_AGENT_PROFILE", "development");
        env::set_var("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE", "1");
        assert_ne!(
            webview_user_data_dir(18082),
            webview_user_data_dir(18083),
            "并行实例必须各自拿到独立的 WebView2 目录"
        );

        restore("HIMIND_AGENT_PROFILE", previous_profile);
        restore("HIMIND_AGENT_HOME", previous_home);
        restore("HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE", previous_parallel);
    }

    fn restore(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    /// 没有选择器时由可执行文件决定 profile：安装版继续用 production 数据根，
    /// 构建产物（`target/release/...`）落到自己的 profile，而不是打开用户装好的
    /// Agent 的数据目录。这条用例把「开发构建不会碰到生产数据」钉住。
    #[test]
    fn profile_selection_prefers_a_selector_and_otherwise_infers_from_the_executable() {
        let _guard = test_env_lock();
        let previous_profile = env::var_os("HIMIND_AGENT_PROFILE");
        env::remove_var("HIMIND_AGENT_PROFILE");

        let build_output = Path::new(r"F:\build\himind-agent\target\release\himind-agent.exe");
        assert_eq!(
            resolve_profile(None, build_output),
            ("development".to_string(), ProfileSource::DevelopmentDefault)
        );
        assert_eq!(
            resolve_profile(Some("ecs-staging"), build_output),
            ("ecs-staging".to_string(), ProfileSource::Argument)
        );
        // 非法名字按「没有选择器」处理，绝不拼进路径。
        assert_eq!(
            resolve_profile(Some("../production"), build_output).0,
            "development"
        );

        let root = env::temp_dir().join(format!(
            "himind-paths-installed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("versions/0.3.48")).unwrap();
        fs::write(root.join("versions/0.3.48/himind-agent.exe"), b"agent").unwrap();
        fs::write(root.join("himind-agent-launcher.exe"), b"launcher").unwrap();
        fs::write(root.join("himind-agent-updater.exe"), b"updater").unwrap();
        // 安装版会在根目录留下 active-version 指针；只有版本目录、launcher、
        // updater 而缺指针的半成品布局不算安装版（见 install_layout 的用例）。
        fs::write(root.join("active-version"), b"0.3.48\n").unwrap();
        assert_eq!(
            resolve_profile(None, &root.join("versions/0.3.48/himind-agent.exe")),
            ("production".to_string(), ProfileSource::InstalledDefault)
        );
        let _ = fs::remove_dir_all(&root);

        // 回归：`cargo build --release` 会把 agent、launcher、updater 一起放进
        // 同一个扁平目录。构建产物必须在磁盘上也被判成开发版，否则开发进程会
        // 打开并改写已安装 Agent 的数据目录。
        let build_root = env::temp_dir().join(format!(
            "himind-paths-build-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let build_dir = build_root.join("target").join("release");
        fs::create_dir_all(&build_dir).unwrap();
        fs::write(build_dir.join("himind-agent.exe"), b"agent").unwrap();
        fs::write(build_dir.join("himind-agent-launcher.exe"), b"launcher").unwrap();
        fs::write(build_dir.join("himind-agent-updater.exe"), b"updater").unwrap();
        assert_eq!(
            resolve_profile(None, &build_dir.join("himind-agent.exe")),
            ("development".to_string(), ProfileSource::DevelopmentDefault)
        );
        let _ = fs::remove_dir_all(&build_root);

        restore("HIMIND_AGENT_PROFILE", previous_profile);
    }

    /// AI 客户端里记录的启动行会活过写出它的那次构建。旧记录仍然写着生产
    /// `--state`，照做就等于把装好的 Agent 身份交给开发进程，所以跨 profile 的
    /// 显式 `--state` 必须被忽略；`HIMIND_AGENT_HOME` 是刻意的例外。
    #[test]
    fn explicit_state_may_not_cross_into_the_production_root() {
        let _guard = test_env_lock();
        let previous_profile = env::var_os("HIMIND_AGENT_PROFILE");
        let previous_home = env::var_os("HIMIND_AGENT_HOME");
        env::remove_var("HIMIND_AGENT_HOME");

        let production_state = super::local_app_data_base()
            .join("HiMindAgent")
            .join("data")
            .join("agent-state.json");

        env::set_var("HIMIND_AGENT_PROFILE", "development");
        assert!(super::explicit_state_crosses_profiles(&production_state));
        assert!(!super::explicit_state_crosses_profiles(
            &super::agent_home().join("data").join("agent-state.json")
        ));

        env::set_var("HIMIND_AGENT_PROFILE", "production");
        assert!(!super::explicit_state_crosses_profiles(&production_state));

        // 显式数据根是刻意的做法，护栏让路。
        env::set_var("HIMIND_AGENT_PROFILE", "development");
        env::set_var(
            "HIMIND_AGENT_HOME",
            production_state.parent().unwrap().as_os_str(),
        );
        assert!(!super::explicit_state_crosses_profiles(&production_state));

        restore("HIMIND_AGENT_PROFILE", previous_profile);
        restore("HIMIND_AGENT_HOME", previous_home);
    }
}
