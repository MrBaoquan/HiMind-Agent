mod install_layout;

use serde_json::json;
use std::env;
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn main() {
    if let Err(error) = run() {
        eprintln!("agent launcher failed: {error}");
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
    if !is_protocol_launch(&arguments) {
        if let Err(error) = repair_protocol_registration(&root, &arguments) {
            let message = format!(
                "agent protocol registration repair failed: {error}；普通用户启动将继续，但协议唤起可能不可用。请运行 diagnostics check 检查 HKCU\\Software\\Classes 权限和 Low integrity 标记。"
            );
            record_launcher_event(&root, "error", &message);
            eprintln!("{message}");
        }
    }
    let mcp_mode = arguments.iter().any(|argument| argument == "--mcp");
    let cli_mode = is_cli_launch(&arguments);
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
    command.spawn()?;
    Ok(())
}

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

fn protocol_registration_command(
    root: &Path,
    arguments: &[String],
) -> Result<Option<String>, Box<dyn Error>> {
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
    Ok(Some(format!(
        "\"{}\" --api \"{}\"{} --local-app --local-port {} --protocol-url \"%1\"",
        launcher.display(),
        api_base,
        mode_argument,
        port
    )))
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

    let Some(command) = protocol_registration_command(root, arguments)? else {
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
            && current_command == command
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
    open.set_value("", &command)?;
    Ok(())
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
    use super::{arguments_with_default_state, protocol_registration_command, validated_arguments};
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
    fn normal_installed_launch_builds_current_protocol_command() {
        let root = installed_root("repair");
        let arguments = vec![
            "--api".into(),
            "https://himind.example".into(),
            "--local-app".into(),
            "--local-port".into(),
            "18181".into(),
        ];
        let command = protocol_registration_command(&root, &arguments)
            .unwrap()
            .unwrap();
        assert_eq!(
            command,
            format!(
                "\"{}\" --api \"https://himind.example\" --local-app --local-port 18181 --protocol-url \"%1\"",
                root.join("himind-agent-launcher.exe").display()
            )
        );
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
        let command = protocol_registration_command(&root, &arguments)
            .unwrap()
            .unwrap();
        assert!(command.contains("--mode connected --local-app"));
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
