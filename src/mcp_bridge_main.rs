#[path = "mcp_bridge.rs"]
mod mcp_bridge;
#[path = "mcp_ipc.rs"]
mod mcp_ipc;
#[path = "mcp_pipe.rs"]
mod mcp_pipe;

use std::env;
use std::error::Error;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("HiMind MCP bridge failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let profile = argument_value(&arguments, "--profile")
        .or_else(|| env::var("HIMIND_AGENT_PROFILE").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "production".to_string());
    let agent_home = agent_home(&profile);
    let state_path = argument_value(&arguments, "--state")
        .map(PathBuf::from)
        .unwrap_or_else(|| agent_home.join("data").join("agent-state.json"));
    let mode = argument_value(&arguments, "--mode");
    let api_base = argument_value(&arguments, "--api");
    let service_executable = service_executable()?;
    mcp_ipc::ensure_service_running(
        &profile,
        &state_path,
        &agent_home,
        &service_executable,
        mode.as_deref(),
        api_base.as_deref(),
    )?;
    mcp_bridge::run(&profile, &state_path, &agent_home)
}

fn service_executable() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(value) = env::var_os("HIMIND_AGENT_SERVICE_EXE") {
        return Ok(PathBuf::from(value));
    }
    let current = env::current_exe()?;
    let candidate = current
        .parent()
        .map(|parent| parent.join("himind-agent.exe"))
        .ok_or("MCP bridge executable directory is unavailable")?;
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "HiMind Agent service executable is missing beside the bridge: {}",
            candidate.display()
        )
        .into())
    }
}

fn argument_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn agent_home(profile: &str) -> PathBuf {
    if let Some(explicit) = env::var_os("HIMIND_AGENT_HOME") {
        return PathBuf::from(explicit);
    }
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("HiMindAgent");
    let profile = profile.trim();
    if profile.is_empty() || matches!(profile, "production" | "default") {
        base
    } else {
        base.join("profiles").join(safe_profile(profile))
    }
}

fn safe_profile(profile: &str) -> String {
    profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
