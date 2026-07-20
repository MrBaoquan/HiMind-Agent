use reqwest::blocking::Client;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    current_executable: PathBuf,
    staged_executable: PathBuf,
    api_base: String,
    target_version: String,
    local_port: u16,
    state_path: PathBuf,
    old_pid: u32,
    arguments: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("agent updater failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = env::args().nth(1).ok_or("update arguments are required")?;
    let args = serde_json::from_str::<UpdateArgs>(&raw)?;
    let current = fs::canonicalize(&args.current_executable)?;
    let staged = fs::canonicalize(&args.staged_executable)?;
    if !staged.is_file()
        || current == staged
        || current.file_name().and_then(|value| value.to_str()) != Some("himind-agent.exe")
    {
        return Err("invalid staged Agent executable".into());
    }
    let root = installation_root(&current);
    if !staged.starts_with(&root) && staged.parent() != Some(env::temp_dir().as_path()) {
        return Err("staged Agent executable is outside the installation root".into());
    }
    if !wait_for_exit(args.old_pid) {
        return Err("running Agent did not exit before update timeout".into());
    }
    let current_dir = root.join("current");
    let previous_dir = root.join("previous");
    fs::create_dir_all(&current_dir)?;
    fs::create_dir_all(&previous_dir)?;
    let current_target = current_dir.join("himind-agent.exe");
    let previous_target = previous_dir.join("himind-agent.exe");
    rotate_version(&current_target, &previous_target, &staged)?;
    let _ = fs::remove_file(&staged);
    thread::sleep(Duration::from_millis(1200));
    if launch_and_confirm(&args, &current_target) {
        report_update_result(&args, "update_success", "")?;
        return Ok(());
    }
    if previous_target.exists() {
        restore_previous(&current_target, &previous_target)?;
        if launch_and_confirm(&args, &current_target) {
            let _ = report_update_result(
                &args,
                "update_failed",
                "new Agent failed health check; previous restored",
            );
            return Err("new Agent failed health check and was rolled back".into());
        }
    }
    let _ = report_update_result(
        &args,
        "update_failed",
        "new Agent failed health check and rollback health check failed",
    );
    Err("Agent update failed and previous version could not be confirmed".into())
}

fn rotate_version(current: &Path, previous: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let backup = previous.with_extension("staging");
    if current.exists() {
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(previous);
        fs::copy(current, &backup)?;
        fs::rename(&backup, previous)?;
    }
    fs::copy(staged, current)?;
    Ok(())
}

fn restore_previous(current: &Path, previous: &Path) -> Result<(), Box<dyn Error>> {
    fs::copy(previous, current)?;
    Ok(())
}

fn launch_and_confirm(args: &UpdateArgs, executable: &Path) -> bool {
    let child = Command::new(executable)
        .args(&args.arguments)
        .current_dir(executable.parent().unwrap_or(Path::new(".")))
        .spawn();
    let Ok(child) = child else {
        return false;
    };
    if wait_for_health(
        args.local_port,
        &args.api_base,
        &args.state_path,
        executable,
        child.id(),
    ) {
        return true;
    }
    let _ = terminate(child.id());
    false
}

fn report_update_result(
    args: &UpdateArgs,
    report_type: &str,
    detail: &str,
) -> Result<(), Box<dyn Error>> {
    #[derive(Deserialize)]
    struct DistributionState {
        token: String,
    }
    let state_path = args
        .state_path
        .with_file_name("agent-state.distribution.json");
    if !state_path.is_file() {
        return Ok(());
    }
    let state = serde_json::from_str::<DistributionState>(&fs::read_to_string(state_path)?)?;
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .post(format!(
            "{}/api/distribution/client/update-result",
            args.api_base.trim_end_matches('/')
        ))
        .bearer_auth(state.token)
        .json(&serde_json::json!({
            "report_type": report_type,
            "from_version": "",
            "to_version": args.target_version,
            "detail": detail,
        }))
        .send()?
        .error_for_status()?;
    Ok(())
}

fn installation_root(executable: &Path) -> PathBuf {
    if executable
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("current")
    {
        executable
            .parent()
            .and_then(Path::parent)
            .unwrap_or(executable)
            .to_path_buf()
    } else {
        executable.parent().unwrap_or(executable).to_path_buf()
    }
}

fn wait_for_health(
    port: u16,
    api_base: &str,
    state_path: &Path,
    executable: &Path,
    pid: u32,
) -> bool {
    let client = match Client::builder().timeout(Duration::from_secs(2)).build() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(response) = client.get(format!("http://127.0.0.1:{port}/health")).send() {
            if response.status().is_success() {
                if let Ok(payload) = response.json::<serde_json::Value>() {
                    if payload.get("status").and_then(|value| value.as_str()) == Some("online")
                        && payload
                            .get("dashboard_worker_online")
                            .and_then(|value| value.as_bool())
                            == Some(true)
                        && state_path.is_file()
                    {
                        return true;
                    }
                }
            }
        }
        if !executable.exists() {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
    let _ = api_base;
    let _ = pid;
    false
}

fn wait_for_exit(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let running = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false);
        if !running {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

fn terminate(pid: u32) -> Result<(), Box<dyn Error>> {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{restore_previous, rotate_version};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rotates_and_restores_agent_versions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let current = root.join("current.exe");
        let previous = root.join("previous.exe");
        let staged = root.join("staged.exe");
        fs::write(&current, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        rotate_version(&current, &previous, &staged).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"new");
        assert_eq!(fs::read(&previous).unwrap(), b"old");

        restore_previous(&current, &previous).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"old");
        let _ = fs::remove_dir_all(root);
    }
}
