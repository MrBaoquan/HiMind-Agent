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
    let mut command = Command::new(executable);
    command
        .args(env::args().skip(1))
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
