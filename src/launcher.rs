//! `himind-agent-launcher.exe` resolves the active Agent version and starts it.
//!
//! On Windows it is a GUI-subsystem binary even though it is also a support CLI
//! (`diagnostics`, `runtime`, `trust`) and the MCP entry point. A desktop
//! double-click starts it from Explorer, which has no console, so a
//! console-subsystem launcher gets a console window of its own — and Windows
//! shows that window before the process can do anything about it. Detaching the
//! console afterwards still leaves the flash the user sees between the
//! double-click and the Agent window. A GUI-subsystem binary never gets one.
//! Callers that need stdio are unaffected: they hand their own handles over, and
//! the launcher passes them on (see `agent_stdio`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod install_layout;

use serde_json::json;
use std::env;
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    if let Err(error) = run() {
        report_launcher_problem(&format!("agent launcher failed: {error}"));
        if let Ok(executable) = env::current_exe() {
            if let Some(root) = executable.parent() {
                record_launcher_event(root, "error", &format!("launcher 启动失败：{error}"));
            }
        }
        show_error("HiMind Agent 无法启动", &error.to_string());
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let root = env::current_exe()?
        .parent()
        .ok_or("Agent launcher directory is unavailable")?
        .to_path_buf();
    let executable = install_layout::resolve_agent_path(&root)?;
    let state_path = root.join("data").join("agent-state.json");
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let trusted_keys = root.join("trusted-keys");
    let arguments = validated_arguments(env::args().skip(1).collect())?;
    let mcp_mode = arguments.iter().any(|argument| argument == "--mcp");
    let cli_mode = is_cli_launch(&arguments);
    // Desktop launches are the entry point whose console nobody reads. The
    // launcher is GUI-subsystem so there is nothing to detach in the shipped
    // build; detaching stays as the fallback for a console-subsystem build, and
    // it has to run before anything else can slow the launch down.
    let desktop_launch = !mcp_mode && !cli_mode;
    if desktop_launch && launcher_owns_its_console() {
        detach_launcher_console();
    }
    if !is_protocol_launch(&arguments) {
        if let Err(error) = repair_protocol_registration(&root, &arguments) {
            let message = format!(
                "agent protocol registration repair failed: {error}；普通用户启动将继续，但协议唤起可能不可用。请运行 diagnostics check 检查 HKCU\\Software\\Classes 权限和 Low integrity 标记。"
            );
            record_launcher_event(&root, "error", &message);
            report_launcher_problem(&message);
        }
        if let Err(error) = repair_autostart_registration(&root, &arguments) {
            let message = format!(
                "agent autostart registration repair failed: {error}；普通用户启动将继续，但登录自启可能会弹出用户没有主动打开的窗口。请运行 diagnostics check 检查 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run 权限。"
            );
            record_launcher_event(&root, "error", &message);
            report_launcher_problem(&message);
        }
    }
    // Release Agent is a GUI-subsystem binary on Windows. MCP clients require
    // a console process with an attached stdio stream, so always select the
    // same-version companion when the launcher is used for MCP.
    let executable = if mcp_mode {
        install_layout::resolve_mcp_path(&root).map_err(|error| {
            format!(
                "MCP console companion is unavailable; reinstall or update HiMind Agent: {error}"
            )
        })?
    } else {
        executable
    };
    let mut command = Command::new(executable);
    let command_arguments = arguments_with_default_state(&arguments, state_path);
    command.args(&command_arguments).current_dir(&root);
    if trusted_keys.is_dir() && env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR").is_none() {
        command.env("HIMIND_TRUSTED_SIGNING_KEYS_DIR", trusted_keys);
    }
    if env::var_os("HIMIND_REQUIRE_SIGNED_UPDATES").is_none() {
        command.env("HIMIND_REQUIRE_SIGNED_UPDATES", "true");
    }
    // No console window for the child either: the launcher has no console, so a
    // console-subsystem child (the MCP companion, or a helper the Agent starts)
    // would allocate one of its own and put it on the desktop. Inherited std
    // handles — what MCP and CLI callers actually read — are untouched.
    configure_child_process(&mut command);
    if mcp_mode || cli_mode {
        // MCP clients own the stdio contract. The launcher must stay attached
        // until the selected Agent exits and must propagate its exit code.
        command.stdin(Stdio::inherit());
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let (stdin, stdout, stderr) = agent_stdio();
    command.stdin(stdin).stdout(stdout).stderr(stderr);
    command.spawn()?;
    Ok(())
}

/// Standard streams for a desktop-launched Agent.
///
/// Explorer hands a GUI-subsystem process no standard handles at all, and the
/// Agent is GUI-subsystem too: Rust's `println!` panics on an invalid handle, so
/// passing NULL through would abort the start before a window appeared. Handing
/// the child `NUL` keeps the startup log out of a window nobody wants while
/// `logs/agent-events.jsonl` keeps every line that matters. A start from a
/// terminal does have handles, and there they stay inherited — that is how
/// `himind-agent-launcher.exe --api ... --local-app` can still be read while
/// debugging.
fn agent_stdio() -> (Stdio, Stdio, Stdio) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };

        fn present(which: u32) -> bool {
            let handle = unsafe { GetStdHandle(which) };
            !handle.is_null() && handle != INVALID_HANDLE_VALUE
        }

        let stdout = if present(STD_OUTPUT_HANDLE) {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
        let stderr = if present(STD_ERROR_HANDLE) {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
        (Stdio::null(), stdout, stderr)
    }
    #[cfg(not(windows))]
    {
        (Stdio::null(), Stdio::inherit(), Stdio::inherit())
    }
}

/// Start children without a console window of their own.
///
/// The launcher has no console, so a console-subsystem child would otherwise
/// allocate one and flash it over the desktop. `CREATE_NO_WINDOW` only suppresses
/// console allocation; inherited std handles, which is what MCP and CLI callers
/// use, are untouched.
#[cfg(windows)]
fn configure_child_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_child_process(_command: &mut Command) {}

/// The launcher is a Windows GUI-adjacent process, so stderr is invisible for
/// a desktop double-click. Keep protocol-repair failures in the same bounded
/// event log consumed by `diagnostics check` and `diagnostics export`.
fn record_launcher_event(root: &Path, level: &str, message: &str) {
    let path = root.join("logs").join("agent-events.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = json!({
        "time": format!("{}", seconds),
        "timestamp": seconds,
        "level": level,
        "message": message,
        "source": "launcher",
    });
    if let Ok(mut text) = serde_json::to_string(&line) {
        text.push('\n');
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
    }
}

/// Whether the console this process is attached to exists only because of it.
///
/// Only reachable in a console-subsystem build: the shipped launcher is
/// GUI-subsystem and never has a console. Kept as the fallback, because a build
/// without the subsystem attribute is exactly how the black startup window comes
/// back. A launch from a terminal or a script shares that caller's console, and
/// there the console output *is* the feedback, so it has to stay.
#[cfg(windows)]
fn launcher_owns_its_console() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    // One entry is this process alone. A larger answer includes the shell (or
    // overflows the buffer), and zero means there is no console at all — nothing
    // to preserve in either case.
    let mut attached = [0_u32; 2];
    let count = unsafe { GetConsoleProcessList(attached.as_mut_ptr(), attached.len() as u32) };
    count <= 1
}

#[cfg(not(windows))]
fn launcher_owns_its_console() -> bool {
    false
}

/// Drop the console window Windows created for a desktop launch.
///
/// Fallback for a console-subsystem build: the `diagnostics`, `runtime` and
/// `trust` subcommands — and the MCP tunnel — keep a usable stdio without it,
/// because a GUI-subsystem process still inherits the std handles of whoever
/// started it. `FreeConsole` only detaches this process: a launcher-owned console
/// is destroyed together with its window, a shared one is left alone.
/// Deliberately no `ShowWindow(SW_HIDE)` — on a shared console that would hide
/// the user's own terminal window.
#[cfg(windows)]
fn detach_launcher_console() {
    unsafe { windows_sys::Win32::System::Console::FreeConsole() };
}

#[cfg(not(windows))]
fn detach_launcher_console() {}

/// `eprintln!` panics when the standard error handle is gone, which is exactly
/// the state after a desktop launch detaches its console — and a panic here would
/// abort the launch before the user sees anything. The same message is already in
/// `logs/agent-events.jsonl`, so a failed write needs no report of its own.
fn report_launcher_problem(message: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{message}");
}

fn is_cli_launch(arguments: &[String]) -> bool {
    let value_flags = ["--api", "--profile", "--state", "--local-port", "--mode"];
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument.starts_with('-') {
            index += if value_flags.contains(&argument) {
                2
            } else {
                1
            };
            continue;
        }
        return matches!(argument, "diagnostics" | "runtime" | "trust");
    }
    false
}

/// Global options must precede the CLI subcommand. Appending `--state` after
/// `diagnostics check` made the Agent reject its own documented support command
/// with “diagnostics check 不接受其它参数”. GUI launches have no subcommand,
/// so they continue to receive the default at the end of their argument list.
fn arguments_with_default_state(
    arguments: &[String],
    state_path: std::path::PathBuf,
) -> Vec<String> {
    if arguments.iter().any(|argument| argument == "--state") {
        return arguments.to_vec();
    }
    let value_flags = ["--api", "--profile", "--local-port", "--mode"];
    let mut command_index = arguments.len();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument.starts_with('-') {
            index += if value_flags.contains(&argument) {
                2
            } else {
                1
            };
            continue;
        }
        command_index = index;
        break;
    }
    let mut updated = arguments.to_vec();
    updated.splice(
        command_index..command_index,
        [
            "--state".to_string(),
            state_path.to_string_lossy().to_string(),
        ],
    );
    updated
}

fn show_error(title: &str, message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |value: &str| {
            value
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>()
        };
        let title = wide(title);
        let message = wide(message);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = (title, message);
}

fn is_protocol_launch(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == "--protocol-url")
}

/// 本机安装版需要维护的两条 HKCU 注册项内容。
///
/// `command` 与 `autostart_command` 必须与安装器写入的形式逐字一致
/// （`PRODUCT_LAUNCH_ARGS` / `PRODUCT_AUTOSTART_ARGS` 展开后的整串）：启动器只在
/// 注册项已经指向本次安装时才跳过写入，拼写不同会让每次双击都去改注册表；在注册
/// 表项 ACL 只允许高完整性令牌写入的机器上，那次写入会失败并报
/// "拒绝访问 (os error 5)"。
struct LaunchRegistration {
    command: String,
    autostart_command: String,
    launcher: PathBuf,
    api_base: String,
    local_port: u16,
}

fn protocol_registration_command(
    root: &Path,
    arguments: &[String],
) -> Result<Option<LaunchRegistration>, Box<dyn Error>> {
    if is_protocol_launch(arguments) || !installed_layout_is_valid(root) {
        return Ok(None);
    }
    let Some(api_base) = unique_argument_value(arguments, "--api")? else {
        return Ok(None);
    };
    let Some(local_port) = unique_argument_value(arguments, "--local-port")? else {
        return Ok(None);
    };
    let mode = unique_argument_value(arguments, "--mode")?;
    if mode.is_some_and(|value| !matches!(value, "connected" | "independent")) {
        return Err("invalid Agent mode for protocol registration".into());
    }
    if !arguments.iter().any(|argument| argument == "--local-app") {
        return Ok(None);
    }
    let api_url = url::Url::parse(api_base)?;
    if !matches!(api_url.scheme(), "http" | "https")
        || api_url.host_str().is_none()
        || !api_url.username().is_empty()
        || api_url.password().is_some()
        || api_url.query().is_some()
        || api_url.fragment().is_some()
        || api_base.contains('"')
    {
        return Err("invalid Dashboard API URL for protocol registration".into());
    }
    let port = local_port.parse::<u16>()?;
    if port == 0 {
        return Err("invalid local Agent port for protocol registration".into());
    }
    let launcher = root.join("himind-agent-launcher.exe");
    let mode_argument = mode
        .map(|value| format!(" --mode {value}"))
        .unwrap_or_default();
    let executable = launcher.display();
    Ok(Some(LaunchRegistration {
        command: format!(
            "\"{executable}\" --api {api_base}{mode_argument} --local-app --local-port {port} --show --protocol-url \"%1\""
        ),
        autostart_command: format!(
            "\"{executable}\" --api {api_base}{mode_argument} --local-app --local-port {port} --hidden"
        ),
        launcher,
        api_base: api_base.to_string(),
        local_port: port,
    }))
}

fn unique_argument_value<'a>(
    arguments: &'a [String],
    name: &str,
) -> Result<Option<&'a str>, Box<dyn Error>> {
    let indexes = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == name).then_some(index))
        .collect::<Vec<_>>();
    if indexes.is_empty() {
        return Ok(None);
    }
    if indexes.len() != 1 || indexes[0] + 1 >= arguments.len() {
        return Err(format!("invalid {name} launcher argument").into());
    }
    Ok(Some(arguments[indexes[0] + 1].as_str()))
}

fn installed_layout_is_valid(root: &Path) -> bool {
    install_layout::resolve_agent_path(root).is_ok()
        && root.join("himind-agent-launcher.exe").is_file()
        && root.join("himind-agent-updater.exe").is_file()
        && root.join("himind-agent.ico").is_file()
}

#[cfg(windows)]
fn repair_protocol_registration(root: &Path, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let Some(registration) = protocol_registration_command(root, arguments)? else {
        return Ok(());
    };
    let icon_path = root.join("himind-agent.ico");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // The installer writes this per-user registration while it may be running
    // elevated. On some Windows configurations that leaves the key writable
    // only from a high-integrity token. A normal double-click does not need to
    // rewrite an already-correct registration, so read it first and leave the
    // user's ACL alone when every value already points at this installation.
    if let Ok(protocol) = hkcu.open_subkey(r"Software\Classes\himind-agent") {
        let current_description = protocol.get_value::<String, _>("").unwrap_or_default();
        let current_url_protocol = protocol
            .get_value::<String, _>("URL Protocol")
            .unwrap_or_default();
        let current_icon = protocol
            .open_subkey("DefaultIcon")
            .ok()
            .and_then(|key| key.get_value::<String, _>("").ok())
            .unwrap_or_default();
        let current_command = protocol
            .open_subkey(r"shell\open\command")
            .ok()
            .and_then(|key| key.get_value::<String, _>("").ok())
            .unwrap_or_default();
        if current_description == "URL:HiMind Agent Protocol"
            && current_url_protocol.is_empty()
            && current_icon.eq_ignore_ascii_case(&icon_path.to_string_lossy())
            && launch_registration_matches(&current_command, &registration)
            && registration_tokens(&current_command).contains(&"--protocol-url")
        {
            return Ok(());
        }
    }

    let (protocol, _) = hkcu.create_subkey(r"Software\Classes\himind-agent")?;
    protocol.set_value("", &"URL:HiMind Agent Protocol")?;
    protocol.set_value("URL Protocol", &"")?;
    let (icon, _) = hkcu.create_subkey(r"Software\Classes\himind-agent\DefaultIcon")?;
    icon.set_value("", &icon_path.to_string_lossy().as_ref())?;
    let (open, _) = hkcu.create_subkey(r"Software\Classes\himind-agent\shell\open\command")?;
    open.set_value("", &registration.command)?;
    Ok(())
}

/// 读写 HKCU 的登录自启项，让升级到「默认显示主窗口」的版本后不会每次登录都
/// 弹出一个用户没有主动打开的窗口。
///
/// 只在自启项**已经存在**时才维护：用户没勾选「开机启动」时安装器不写这一项，
/// 启动器也不替用户创建。
#[cfg(windows)]
fn repair_autostart_registration(root: &Path, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let Some(registration) = protocol_registration_command(root, arguments)? else {
        return Ok(());
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run) = hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") else {
        return Ok(());
    };
    let Ok(current) = run.get_value::<String, _>("HiMindAgent") else {
        return Ok(());
    };
    if launch_registration_matches(&current, &registration) && command_requests_background(&current)
    {
        return Ok(());
    }
    let (run, _) = hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")?;
    run.set_value("HiMindAgent", &registration.autostart_command)?;
    Ok(())
}

#[cfg(not(windows))]
fn repair_autostart_registration(root: &Path, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let _ = protocol_registration_command(root, arguments)?;
    Ok(())
}

/// 把注册表里的命令行切成 token，去掉包裹的引号。
fn registration_tokens(command: &str) -> Vec<&str> {
    command
        .split_whitespace()
        .map(|token| token.trim_matches('"'))
        .collect()
}

/// 已有的注册命令是否已经指向本次安装（同一个启动器、同一个 API 地址与端口）。
///
/// 用语义比对而不是逐字比对：安装器写的是宏展开后的整串，启动器自己拼的串可能在
/// 引号、参数顺序或 `--mode` 上有差别。逐字比对会把两份等价的注册判成不一致，于是
/// 每次双击都去写注册表——在 ACL 只允许高完整性令牌写入的机器上，那正是用户看到的
/// `agent protocol registration repair failed: 拒绝访问。 (os error 5)`。
fn launch_registration_matches(command: &str, expected: &LaunchRegistration) -> bool {
    let tokens = registration_tokens(command);
    let Some(registered_launcher) = tokens.first() else {
        return false;
    };
    if !registered_launcher.eq_ignore_ascii_case(&expected.launcher.to_string_lossy()) {
        return false;
    }
    if !tokens.iter().any(|token| *token == "--local-app") {
        return false;
    }
    let value_of = |name: &str| -> Option<&str> {
        tokens
            .iter()
            .position(|token| *token == name)
            .and_then(|index| tokens.get(index + 1).copied())
    };
    if value_of("--api") != Some(expected.api_base.as_str()) {
        return false;
    }
    if value_of("--local-port") != Some(expected.local_port.to_string().as_str()) {
        return false;
    }
    true
}

/// 已有命令是否明确要求后台启动（登录自启项必须带 `--hidden`）。
fn command_requests_background(command: &str) -> bool {
    registration_tokens(command)
        .iter()
        .any(|token| *token == "--hidden" || *token == "--background")
}

#[cfg(not(windows))]
fn repair_protocol_registration(root: &Path, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let _ = protocol_registration_command(root, arguments)?;
    Ok(())
}

fn validated_arguments(arguments: Vec<String>) -> Result<Vec<String>, Box<dyn Error>> {
    let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--protocol-url")
    else {
        return Ok(arguments);
    };
    if index + 2 != arguments.len() || !is_safe_open_url(&arguments[index + 1]) {
        return Err("invalid HiMind Agent protocol URL".into());
    }
    Ok(arguments)
}

fn is_safe_open_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "himind-agent"
        && url.host_str() == Some("open")
        && (url.path().is_empty() || url.path() == "/")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

#[cfg(test)]
mod tests {
    use super::{
        arguments_with_default_state, command_requests_background, launch_registration_matches,
        protocol_registration_command, validated_arguments,
    };
    use std::fs;
    use std::path::PathBuf;

    fn installed_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-agent-launcher-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("current")).unwrap();
        for path in [
            root.join("current").join("himind-agent.exe"),
            root.join("himind-agent-launcher.exe"),
            root.join("himind-agent-updater.exe"),
            root.join("himind-agent.ico"),
        ] {
            fs::write(path, b"test").unwrap();
        }
        root
    }

    #[test]
    fn protocol_url_must_be_safe_and_final() {
        assert!(validated_arguments(vec![
            "--local-app".into(),
            "--protocol-url".into(),
            "himind-agent://open".into(),
        ])
        .is_ok());
        assert!(validated_arguments(vec![
            "--protocol-url".into(),
            "himind-agent://open".into(),
            "--state".into(),
            "attacker.json".into(),
        ])
        .is_err());
        assert!(validated_arguments(vec![
            "--protocol-url".into(),
            "himind-agent://open?command=exec".into(),
        ])
        .is_err());
    }

    #[test]
    fn default_state_precedes_cli_subcommand() {
        let args = vec!["diagnostics".to_string(), "check".to_string()];
        assert_eq!(
            arguments_with_default_state(&args, PathBuf::from(r"C:\state.json")),
            vec![
                "--state".to_string(),
                r"C:\state.json".to_string(),
                "diagnostics".to_string(),
                "check".to_string()
            ]
        );
    }

    #[test]
    fn normal_installed_launch_builds_installer_compatible_registrations() {
        let root = installed_root("repair");
        let arguments = vec![
            "--api".into(),
            "https://himind.example".into(),
            "--local-app".into(),
            "--local-port".into(),
            "18181".into(),
        ];
        let registration = protocol_registration_command(&root, &arguments)
            .unwrap()
            .unwrap();
        let launcher = root.join("himind-agent-launcher.exe");
        // 必须与安装器 `PRODUCT_LAUNCH_ARGS` / `PRODUCT_AUTOSTART_ARGS` 的展开结果
        // 逐字一致，否则每次双击都会重写注册项——在注册项 ACL 只允许高完整性令牌
        // 写入的机器上，那正是 "拒绝访问 (os error 5)" 的来源。
        assert_eq!(
            registration.command,
            format!(
                "\"{}\" --api https://himind.example --local-app --local-port 18181 --show --protocol-url \"%1\"",
                launcher.display()
            )
        );
        assert_eq!(
            registration.autostart_command,
            format!(
                "\"{}\" --api https://himind.example --local-app --local-port 18181 --hidden",
                launcher.display()
            )
        );
        assert!(launch_registration_matches(
            &registration.command,
            &registration
        ));
        // 0.3.69 的启动器写的是带引号的 API 地址：语义相等就不要重写。
        assert!(launch_registration_matches(
            &format!(
                "\"{}\" --api \"https://himind.example\" --local-app --local-port 18181 --show --protocol-url \"%1\"",
                launcher.display()
            ),
            &registration
        ));
        // 0.3.68 及更早写入的登录自启项没有 --hidden：判成匹配（同一个启动器/API/
        // 端口），但需要补后台标记。
        let legacy_autostart = format!(
            "\"{}\" --api https://himind.example --local-app --local-port 18181",
            launcher.display()
        );
        assert!(launch_registration_matches(
            &legacy_autostart,
            &registration
        ));
        assert!(!command_requests_background(&legacy_autostart));
        assert!(command_requests_background(&registration.autostart_command));
        // 指向别的安装或别的 API 时不算匹配。
        assert!(!launch_registration_matches(
            &format!(
                "\"{}\" --api https://himind.example --local-app --local-port 18181",
                root.join("other-launcher.exe").display()
            ),
            &registration
        ));
        assert!(!launch_registration_matches(
            &format!(
                "\"{}\" --api https://other.example --local-app --local-port 18181",
                launcher.display()
            ),
            &registration
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn protocol_registration_preserves_an_explicit_install_mode() {
        let root = installed_root("mode");
        let arguments = vec![
            "--api".into(),
            "https://himind.example".into(),
            "--mode".into(),
            "connected".into(),
            "--local-app".into(),
            "--local-port".into(),
            "18181".into(),
        ];
        let registration = protocol_registration_command(&root, &arguments)
            .unwrap()
            .unwrap();
        assert!(registration
            .command
            .contains("--mode connected --local-app"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn protocol_or_portable_launch_does_not_repair_registration() {
        let root = installed_root("skip");
        assert!(protocol_registration_command(
            &root,
            &[
                "--api".into(),
                "https://wrong.example".into(),
                "--local-app".into(),
                "--local-port".into(),
                "18181".into(),
                "--protocol-url".into(),
                "himind-agent://open".into(),
            ],
        )
        .unwrap()
        .is_none());
        fs::remove_file(root.join("himind-agent.ico")).unwrap();
        assert!(protocol_registration_command(
            &root,
            &[
                "--api".into(),
                "https://himind.example".into(),
                "--local-app".into(),
                "--local-port".into(),
                "18181".into(),
            ],
        )
        .unwrap()
        .is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
