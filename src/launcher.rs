use std::env;
use std::error::Error;
use std::path::Path;
use std::process::Command;

fn main() {
    if let Err(error) = run() {
        eprintln!("agent launcher failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let root = env::current_exe()?
        .parent()
        .ok_or("Agent launcher directory is unavailable")?
        .to_path_buf();
    let executable = root.join("current").join("himind-agent.exe");
    if !executable.is_file() {
        return Err(format!("installed Agent is missing: {}", executable.display()).into());
    }
    let state_path = root.join("data").join("agent-state.json");
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let trusted_keys = root.join("trusted-keys");
    let arguments = validated_arguments(env::args().skip(1).collect())?;
    if !is_protocol_launch(&arguments) {
        if let Err(error) = repair_protocol_registration(&root, &arguments) {
            eprintln!("agent protocol registration repair failed: {error}");
        }
    }
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .arg("--state")
        .arg(state_path)
        .current_dir(&root);
    if trusted_keys.is_dir() && env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR").is_none() {
        command.env("HIMIND_TRUSTED_SIGNING_KEYS_DIR", trusted_keys);
    }
    if env::var_os("HIMIND_REQUIRE_SIGNED_UPDATES").is_none() {
        command.env("HIMIND_REQUIRE_SIGNED_UPDATES", "true");
    }
    command.spawn()?;
    Ok(())
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
    Ok(Some(format!(
        "\"{}\" --api \"{}\" --local-app --local-port {} --protocol-url \"%1\"",
        launcher.display(),
        api_base,
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
    root.join("current").join("himind-agent.exe").is_file()
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
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (protocol, _) = hkcu.create_subkey(r"Software\Classes\himind-agent")?;
    protocol.set_value("", &"URL:HiMind Agent Protocol")?;
    protocol.set_value("URL Protocol", &"")?;
    let (icon, _) = hkcu.create_subkey(r"Software\Classes\himind-agent\DefaultIcon")?;
    icon.set_value(
        "",
        &root.join("himind-agent.ico").to_string_lossy().as_ref(),
    )?;
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
    use super::{protocol_registration_command, validated_arguments};
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
