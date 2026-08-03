use std::env;
use std::error::Error;
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
    use super::validated_arguments;

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
}
