mod install_layout;

use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    current_executable: PathBuf,
    staged_executable: PathBuf,
    staged_mcp: PathBuf,
    staged_package: PathBuf,
    staged_updater: PathBuf,
    staged_launcher: PathBuf,
    #[serde(default)]
    staged_vscode_extension: Option<PathBuf>,
    staged_runtime_manifest: PathBuf,
    staged_runtime_package: PathBuf,
    staged_runtime_signature: PathBuf,
    api_base: String,
    from_version: String,
    target_version: String,
    local_port: u16,
    state_path: PathBuf,
    #[serde(default)]
    old_pid: u32,
    #[serde(default)]
    wait_pid: u32,
    arguments: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UpdaterReplacementArgs {
    operation: String,
    parent_pid: u32,
    staged_package: PathBuf,
    staged_directory: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("agent updater failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = env::args().nth(1).ok_or("update arguments are required")?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)?;
    if value.get("operation").and_then(|value| value.as_str()) == Some("replace_updater") {
        return replace_running_updater(serde_json::from_value(value)?);
    }
    let args = serde_json::from_value::<UpdateArgs>(value)?;
    let result = run_update(&args);
    if let Err(error) = &result {
        let _ = mark_install_failed_if_pending(&args, &error.to_string());
    }
    result
}

fn run_update(args: &UpdateArgs) -> Result<(), Box<dyn Error>> {
    log_update(
        args,
        &format!(
            "开始更新：{} -> {}，current={}，staged={}",
            args.from_version,
            args.target_version,
            args.current_executable.display(),
            args.staged_executable.display()
        ),
    );
    let current = plain_path(&fs::canonicalize(&args.current_executable)?);
    let staged = plain_path(&fs::canonicalize(&args.staged_executable)?);
    if !staged.is_file()
        || current == staged
        || current.file_name().and_then(|value| value.to_str()) != Some(install_layout::AGENT_FILE)
    {
        return Err("invalid staged Agent executable".into());
    }
    let root = install_layout::installation_root_from_executable(&current);
    if !staged_path_allowed(&staged, &root) {
        return Err("staged Agent executable is outside the installation root".into());
    }
    let staged_mcp = canonical_staged_helper(&args.staged_mcp, install_layout::MCP_FILE, &root)?;
    let staged_updater =
        canonical_staged_helper(&args.staged_updater, install_layout::UPDATER_FILE, &root)?;
    let staged_launcher =
        canonical_staged_helper(&args.staged_launcher, install_layout::LAUNCHER_FILE, &root)?;
    let staged_vscode_extension = args
        .staged_vscode_extension
        .as_ref()
        .map(|path| -> Result<PathBuf, Box<dyn Error>> {
            if path.file_name().and_then(|value| value.to_str()) != Some("himind-ai.vsix") {
                return Err("invalid staged VS Code extension".into());
            }
            let canonical = canonical_staged_path(path, &root)?;
            let size = fs::metadata(&canonical)?.len();
            if size == 0 || size > 100 * 1024 * 1024 {
                return Err("staged VS Code extension has an invalid size".into());
            }
            Ok(canonical)
        })
        .transpose()?;
    let staged_runtime_manifest = canonical_staged_path(&args.staged_runtime_manifest, &root)?;
    let staged_runtime_package = canonical_staged_path(&args.staged_runtime_package, &root)?;
    let staged_runtime_signature = canonical_staged_path(&args.staged_runtime_signature, &root)?;
    let staged_package = canonical_staged_path(&args.staged_package, &root)?;
    wait_for_old_agent_exit(args)?;
    let runtime_bundle = match install_runtime_bundle(
        &root,
        &staged_runtime_manifest,
        &staged_runtime_package,
        &staged_runtime_signature,
    ) {
        Ok(transaction) => transaction,
        Err(error) => {
            let detail = format!("内置 Runtime 更新失败，已回退到旧 Agent：{error}");
            let _ = launch_for_recovery(args, &current, &args.from_version);
            let _ = report_update_result(args, "rolled_back", &detail);
            let _ = update_local_status(args, "rolled_back", &detail);
            return Err(detail.into());
        }
    };
    let had_active_version = install_layout::read_active_version(&root)?.is_some();
    let target_dir = install_layout::prepare_version_directory(
        &root,
        &args.target_version,
        &staged,
        &staged_mcp,
    )?;
    let target = target_dir.join(install_layout::AGENT_FILE);
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_file(&staged_mcp);
    thread::sleep(Duration::from_millis(300));
    let mut warning = String::new();
    match launch_and_confirm(&args, &target, &args.target_version) {
        LaunchOutcome::Confirmed => {}
        LaunchOutcome::UnconfirmedButAlive(reason) => {
            // The new process is up and its image is the version just written:
            // an inconclusive /health is not a reason to throw that away.
            append_warning(
                &mut warning,
                &format!("新版本已启动，但健康检查未确认：{reason}"),
            );
            log_update(args, &format!("保留新版本继续运行：{warning}"));
        }
        LaunchOutcome::Failed(reason) => {
            log_update(
                args,
                "新 Agent 启动检查失败，开始保留当前版本并恢复旧 Agent",
            );
            let _ = fs::remove_dir_all(&target_dir);
            let _ = launch_for_recovery(args, &current, &args.from_version);
            let detail = format!("新版本启动检查失败，已保留当前版本：{reason}");
            let _ = report_update_result(args, "rolled_back", &detail);
            let _ = update_local_status(args, "rolled_back", &detail);
            return Err(detail.into());
        }
    }
    if let Some(extension) = staged_vscode_extension.as_deref() {
        if let Err(error) = install_vscode_extension(&root, extension) {
            // 内置 VSIX 是可选资产：文件被占用或被杀软拦下时换不进去，不值得把
            // 已经跑起来的新版本撤掉；下一次更新或重新安装会再试一次。
            append_warning(&mut warning, &format!("内置 HiMind VSIX 未更新：{error}"));
            log_update(args, &warning);
        }
    }
    if !had_active_version {
        if let Err(error) = replace_helper(
            &root.join(install_layout::LAUNCHER_FILE),
            &root.join("himind-agent-launcher.previous.exe"),
            &staged_launcher,
        ) {
            append_warning(&mut warning, &format!("稳定 launcher 未替换：{error}"));
            log_update(args, &warning);
        }
    }
    if let Err(error) = install_layout::write_active_version(&root, &args.target_version) {
        // 活动版本指针是「哪个版本算数」的唯一依据：写不进去说明安装目录本身有
        // 问题，此时回退同样不可靠，如实报错并保留新进程。
        let detail = format!("写入 Agent 活动版本指针失败：{error}");
        log_update(args, &detail);
        let _ = report_update_result(args, "update_failed", &detail);
        let _ = update_local_status(args, "failed", &detail);
        return Err(detail.into());
    }
    let _ = fs::remove_file(&staged_launcher);
    runtime_bundle.commit();
    if let Err(error) = schedule_updater_self_replace(&root, &staged_updater, &staged_package) {
        append_warning(
            &mut warning,
            &format!("updater 将在后续更新中重试：{error}"),
        );
    }
    if warning.is_empty() {
        let _ = update_local_status(args, "idle", "");
        let _ = report_update_result(args, "update_success", "");
    } else {
        // 更新算成功，但把没做到的那部分原样写给用户和更新服务，免得只剩一句
        // 「更新没有完成」。
        log_update(args, &format!("更新完成但留有警告：{warning}"));
        let _ = update_local_status(args, "idle", &warning);
        let _ = report_update_result(args, "update_success", &warning);
    }
    Ok(())
}

struct RuntimeBundleTransaction {
    target: PathBuf,
    backup: Option<PathBuf>,
    committed: bool,
}

impl RuntimeBundleTransaction {
    fn commit(mut self) {
        self.committed = true;
        if let Some(backup) = self.backup.take() {
            let _ = fs::remove_dir_all(backup);
        }
    }
}

impl Drop for RuntimeBundleTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_dir_all(&self.target);
        if let Some(backup) = self.backup.take() {
            let _ = fs::rename(backup, &self.target);
        }
    }
}

fn install_runtime_bundle(
    root: &Path,
    manifest_path: &Path,
    package_path: &Path,
    signature_path: &Path,
) -> Result<RuntimeBundleTransaction, Box<dyn Error>> {
    let manifest = read_json_value(manifest_path)?;
    let file_name = manifest
        .get("file_name")
        .and_then(|value| value.as_str())
        .ok_or("bundled Runtime manifest has no file_name")?;
    if file_name.contains('/')
        || file_name.contains('\\')
        || !file_name.ends_with(".zip")
        || file_name.len() > 180
    {
        return Err("bundled Runtime package name is unsafe".into());
    }
    let expected_sha = manifest
        .get("sha256")
        .and_then(|value| value.as_str())
        .ok_or("bundled Runtime manifest has no sha256")?;
    if expected_sha.len() != 64 || !expected_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("bundled Runtime manifest has an invalid sha256".into());
    }
    let mut package = fs::File::open(package_path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = package.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual_sha = format!("{:x}", digest.finalize());
    if !actual_sha.eq_ignore_ascii_case(expected_sha) {
        return Err("bundled Runtime package SHA-256 mismatch".into());
    }
    let signature = read_json_value(signature_path)?;
    if signature.get("file_name").and_then(|value| value.as_str()) != Some(file_name)
        || signature.get("sha256").and_then(|value| value.as_str()) != Some(expected_sha)
    {
        return Err("bundled Runtime signature metadata does not match its manifest".into());
    }

    let resources = root.join("resources");
    fs::create_dir_all(&resources)?;
    let staging = resources.join(format!(".runtime.installing-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<RuntimeBundleTransaction, Box<dyn Error>> {
        fs::copy(manifest_path, staging.join("himind-runtime-release.json"))?;
        fs::copy(package_path, staging.join(file_name))?;
        fs::copy(
            signature_path,
            staging.join(format!("{file_name}.signature.json")),
        )?;
        let target = resources.join("runtime");
        let backup = resources.join(format!(".runtime.previous-{}", std::process::id()));
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        let had_target = target.exists();
        if had_target {
            fs::rename(&target, &backup)?;
        }
        if let Err(error) = fs::rename(&staging, &target) {
            if had_target {
                let _ = fs::rename(&backup, &target);
            }
            return Err(error.into());
        }
        Ok(RuntimeBundleTransaction {
            target,
            backup: had_target.then_some(backup),
            committed: false,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn append_warning(target: &mut String, message: &str) {
    if !target.is_empty() {
        target.push_str("；");
    }
    target.push_str(message);
}
fn mark_install_failed_if_pending(args: &UpdateArgs, error: &str) -> Result<(), Box<dyn Error>> {
    let path = args.state_path.with_file_name("agent-update-state.json");
    if !path.is_file() {
        return Ok(());
    }
    let value = read_json_value(&path)?;
    if value.get("status").and_then(|value| value.as_str()) != Some("installing") {
        return Ok(());
    }
    update_local_status(args, "failed", error)
}

fn update_local_status(args: &UpdateArgs, status: &str, error: &str) -> Result<(), Box<dyn Error>> {
    let path = args.state_path.with_file_name("agent-update-state.json");
    if !path.is_file() {
        return Ok(());
    }
    let mut value = read_json_value(&path)?;
    value["status"] = serde_json::Value::String(status.to_string());
    value["last_error"] = serde_json::Value::String(error.to_string());
    value["current_version"] = serde_json::Value::String(if status == "idle" {
        args.target_version.clone()
    } else {
        args.from_version.clone()
    });
    if status == "idle" {
        for key in [
            "available_version",
            "release_id",
            "file_name",
            "sha256",
            "signature",
            "signature_key_id",
            "signature_algorithm",
            "download_url",
            "min_supported_version",
            "release_notes",
            "staged_package_path",
            "staged_agent_path",
            "staged_mcp_path",
            "staged_updater_path",
            "staged_launcher_path",
            "staged_vscode_extension_path",
        ] {
            value[key] = serde_json::Value::String(String::new());
        }
        value["size_bytes"] = serde_json::json!(0);
        value["downloaded_bytes"] = serde_json::json!(0);
        value["progress_percent"] = serde_json::json!(0);
        value["mandatory"] = serde_json::json!(false);
        value["package_type"] = serde_json::Value::String("directory-zip".to_string());
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_json_value(path: &Path) -> Result<serde_json::Value, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let bytes = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(bytes.as_slice());
    Ok(serde_json::from_slice(bytes)?)
}

/// How long the updater waits for the freshly written version to answer
/// /health. A first launch of a new version may spend this time on data
/// migrations, storage repair and WebView2 profile creation before the local
/// service binds, so a slow first start must never be mistaken for a broken
/// version.
const HEALTH_PROBE_TIMEOUT_SECS: u64 = 90;

/// What the health probe proved about the Agent the updater just launched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchOutcome {
    /// /health answered with the expected version and this very executable.
    Confirmed,
    /// /health did not settle the question, but the process this updater
    /// started is still running from the target executable.
    UnconfirmedButAlive(String),
    /// The new Agent is no longer running from the target path.
    Failed(String),
}

fn launch_and_confirm(
    args: &UpdateArgs,
    executable: &Path,
    expected_version: &str,
) -> LaunchOutcome {
    launch_and_confirm_with_policy(args, executable, expected_version, true)
}

// Recovery must be conservative: a failed health probe does not prove that
// the previous Agent process is unusable. Keep that process alive so a
// transient probe failure cannot leave the installation without an Agent.
fn launch_for_recovery(args: &UpdateArgs, executable: &Path, expected_version: &str) -> bool {
    matches!(
        launch_and_confirm_with_policy(args, executable, expected_version, false),
        LaunchOutcome::Confirmed | LaunchOutcome::UnconfirmedButAlive(_)
    )
}

fn launch_and_confirm_with_policy(
    args: &UpdateArgs,
    executable: &Path,
    expected_version: &str,
    terminate_on_failure: bool,
) -> LaunchOutcome {
    let root = install_layout::installation_root_from_executable(executable);
    let trusted_keys = root.join("trusted-keys");
    let mut command = Command::new(executable);
    command
        .args(&args.arguments)
        .current_dir(&root)
        .env("HIMIND_REQUIRE_SIGNED_UPDATES", "true");
    if trusted_keys.is_dir() {
        command.env("HIMIND_TRUSTED_SIGNING_KEYS_DIR", trusted_keys);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("启动 Agent 失败：{}：{}", executable.display(), error);
            log_update(args, &reason);
            return LaunchOutcome::Failed(reason);
        }
    };
    let assessment = probe_health(
        args.local_port,
        &args.state_path,
        expected_version,
        executable,
        child.id(),
    );
    for warning in &assessment.warnings {
        log_update(args, &format!("健康检查警告：{warning}"));
    }
    if assessment.confirmed {
        log_update(
            args,
            &format!("新 Agent 健康检查通过：{}", executable.display()),
        );
        return LaunchOutcome::Confirmed;
    }
    // The probe only reports what /health said. A leftover Agent holding the
    // port, a state file that was never written, or a first start that is still
    // migrating all look like "not healthy" without being one. Only the process
    // this updater started can decide whether the new version is actually up.
    let alive = pid_runs_from(child.id(), executable);
    log_update(
        args,
        &format!(
            "Agent 未通过健康检查：path={}，expected_version={}，pid={}，原因={}，目标进程存活={}",
            executable.display(),
            expected_version,
            child.id(),
            assessment.reason,
            alive
        ),
    );
    if alive {
        return LaunchOutcome::UnconfirmedButAlive(assessment.reason);
    }
    if terminate_on_failure {
        let _ = terminate_single(child.id());
    }
    LaunchOutcome::Failed(assessment.reason)
}
fn wait_for_old_agent_exit(args: &UpdateArgs) -> Result<(), Box<dyn Error>> {
    let pid = if args.wait_pid != 0 {
        args.wait_pid
    } else {
        args.old_pid
    };
    if pid == 0 || pid == std::process::id() || !process_exists(pid) {
        return Ok(());
    }
    log_update(args, &format!("等待旧 Agent 退出：pid={pid}"));
    if wait_for_process_exit(pid, Duration::from_secs(10)) {
        return Ok(());
    }
    log_update(args, &format!("旧 Agent 未自行退出，终止主进程：pid={pid}"));
    terminate_single(pid)?;
    if wait_for_process_exit(pid, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(format!("旧 Agent 进程未能退出：pid={pid}").into())
    }
}

fn report_update_result(
    args: &UpdateArgs,
    report_type: &str,
    detail: &str,
) -> Result<(), Box<dyn Error>> {
    #[derive(Deserialize)]
    struct DistributionState {
        #[serde(default)]
        token: String,
        #[serde(default)]
        token_protected: String,
    }
    let state_path = args
        .state_path
        .with_file_name("agent-state.distribution.json");
    if !state_path.is_file() {
        return Ok(());
    }
    let mut state = serde_json::from_value::<DistributionState>(read_json_value(&state_path)?)?;
    if state.token.trim().is_empty() && !state.token_protected.trim().is_empty() {
        state.token = unprotect_secret_for_current_user(&state.token_protected)?;
    }
    if state.token.trim().is_empty() {
        return Ok(());
    }
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
            "from_version": args.from_version,
            "to_version": args.target_version,
            "detail": detail,
        }))
        .send()?
        .error_for_status()?;
    Ok(())
}

fn unprotect_secret_for_current_user(secret: &str) -> Result<String, Box<dyn Error>> {
    let script = r#"$encrypted = [Console]::In.ReadToEnd(); $secure = ConvertTo-SecureString $encrypted; $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure); try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }"#;
    let mut last_error = String::new();
    for shell in ["pwsh", "powershell"] {
        let mut child = match Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(secret.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    Err(format!("failed to access local credential store: {last_error}").into())
}

fn canonical_staged_helper(
    path: &Path,
    expected_name: &str,
    installation_root: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    if path.file_name().and_then(|value| value.to_str()) != Some(expected_name) {
        return Err(format!("invalid staged helper executable: {expected_name}").into());
    }
    canonical_staged_path(path, installation_root)
}

fn canonical_staged_path(path: &Path, installation_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let canonical = plain_path(&fs::canonicalize(path)?);
    if !canonical.is_file() || !staged_path_allowed(&canonical, installation_root) {
        return Err("staged update file is outside the installation root".into());
    }
    Ok(canonical)
}

fn install_vscode_extension(root: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let target_dir = root.join("resources").join("vscode");
    fs::create_dir_all(&target_dir)?;
    let temporary = target_dir.join(format!(".himind-ai-{}.vsix.installing", std::process::id()));
    let target = target_dir.join("himind-ai.vsix");
    let _ = fs::remove_file(&temporary);
    fs::copy(staged, &temporary)?;
    if let Err(error) = install_layout::replace_file(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法更新内置 HiMind VSIX：{error}").into());
    }
    Ok(())
}

// fs::canonicalize returns \\?\-prefixed verbatim paths, but Win32_Process
// ExecutablePath and PowerShell cmdlets use plain Win32 paths. Normalize
// canonicalized paths so process matching and helper scripts keep working.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path.to_path_buf()
}

fn replace_helper(target: &Path, backup: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let next = target.with_extension("next.exe");
    let _ = fs::remove_file(&next);
    fs::copy(staged, &next)?;
    if target.is_file() {
        let _ = fs::remove_file(backup);
        fs::copy(target, backup)?;
        if let Err(error) = fs::remove_file(target) {
            let _ = fs::remove_file(&next);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(&next, target) {
        if backup.is_file() {
            let _ = fs::copy(backup, target);
        }
        let _ = fs::remove_file(&next);
        return Err(error.into());
    }
    Ok(())
}

fn schedule_updater_self_replace(
    root: &Path,
    staged_updater: &Path,
    staged_package: &Path,
) -> Result<(), Box<dyn Error>> {
    let target = root.join("himind-agent-updater.exe");
    let next = root.join("himind-agent-updater.next.exe");
    let _ = fs::remove_file(&next);
    fs::copy(staged_updater, &next)?;
    let payload = serde_json::json!({
        "operation": "replace_updater",
        "parent_pid": std::process::id(),
        "staged_package": staged_package,
        "staged_directory": staged_updater.parent().unwrap_or(root),
    });
    Command::new(&next)
        .arg(payload.to_string())
        .current_dir(root)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    if !target.is_file() {
        return Err("installed Agent updater is missing".into());
    }
    Ok(())
}

fn replace_running_updater(args: UpdaterReplacementArgs) -> Result<(), Box<dyn Error>> {
    if args.operation != "replace_updater" || args.parent_pid == 0 {
        return Err("invalid updater replacement request".into());
    }
    let current = plain_path(&fs::canonicalize(env::current_exe()?)?);
    if current.file_name().and_then(|value| value.to_str()) != Some("himind-agent-updater.next.exe")
    {
        return Err("updater replacement must run from the staged helper".into());
    }
    let root = current
        .parent()
        .ok_or("updater installation root is unavailable")?;
    let target = root.join(install_layout::UPDATER_FILE);
    let backup = root.join("himind-agent-updater.previous.exe");
    if !wait_for_process_exit(args.parent_pid, Duration::from_secs(30)) {
        return Err("timed out waiting for the previous updater to exit".into());
    }
    let temporary = root.join("himind-agent-updater.replacing.exe");
    let _ = fs::remove_file(&temporary);
    fs::copy(&current, &temporary)?;
    if target.is_file() {
        let _ = fs::remove_file(&backup);
        fs::copy(&target, &backup)?;
    }
    install_layout::replace_file(&temporary, &target)?;
    let _ = fs::remove_file(&args.staged_package);
    if args.staged_directory.starts_with(root.join("staging")) {
        let _ = fs::remove_dir_all(&args.staged_directory);
    }
    Ok(())
}

fn powershell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn staged_path_allowed(staged: &Path, installation_root: &Path) -> bool {
    let canonical_root =
        fs::canonicalize(installation_root).unwrap_or_else(|_| installation_root.to_path_buf());
    let root = plain_path(&canonical_root);
    let staged = plain_path(staged);
    staged.starts_with(&root)
}

/// The verdict of one /health conversation, plus everything the probe noticed
/// without letting it decide the outcome.
struct HealthAssessment {
    confirmed: bool,
    reason: String,
    warnings: Vec<String>,
}

/// Ask the freshly launched Agent about itself until it answers or the budget
/// runs out.
///
/// /health is served by the local Agent itself. It must never be routed
/// through an HTTP proxy, which can turn a healthy local response into a
/// proxy-generated 502 and make both upgrade and rollback look failed.
fn probe_health(
    port: u16,
    state_path: &Path,
    target_version: &str,
    executable: &Path,
    pid: u32,
) -> HealthAssessment {
    let client = match Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(value) => value,
        Err(error) => {
            return HealthAssessment {
                confirmed: false,
                reason: format!("无法创建本地健康检查客户端：{error}"),
                warnings: Vec::new(),
            }
        }
    };
    let deadline = Instant::now() + Duration::from_secs(HEALTH_PROBE_TIMEOUT_SECS);
    let mut last_observation = "尚未收到本地服务响应".to_string();
    let mut warnings: Vec<String> = Vec::new();
    let mut logged_observation = String::new();
    while Instant::now() < deadline {
        match client.get(format!("http://127.0.0.1:{port}/health")).send() {
            Ok(response) => {
                if !response.status().is_success() {
                    last_observation = format!("/health 返回 HTTP {}", response.status());
                } else {
                    match response.json::<serde_json::Value>() {
                        Ok(payload) => {
                            let assessment =
                                assess_health(&payload, target_version, executable, state_path);
                            for warning in assessment.warnings {
                                if !warnings.contains(&warning) {
                                    warnings.push(warning);
                                }
                            }
                            if assessment.confirmed {
                                return HealthAssessment {
                                    confirmed: true,
                                    reason: "ok".to_string(),
                                    warnings,
                                };
                            }
                            last_observation = assessment.reason;
                        }
                        Err(error) => {
                            last_observation = format!("/health 响应不是合法 JSON：{error}");
                        }
                    }
                }
            }
            Err(error) => {
                last_observation = format!("连接本地 /health 失败：{error}");
            }
        }
        // Log every change of observation. "connect failed" and "version
        // mismatch" are different bugs, and the updater log is the only trace
        // that survives a rollback.
        if last_observation != logged_observation {
            logged_observation = last_observation.clone();
            append_update_log(
                state_path,
                &format!("健康检查观测：pid={pid}，{last_observation}"),
            );
        }
        if !executable.exists() {
            return HealthAssessment {
                confirmed: false,
                reason: "目标 Agent 可执行文件已不存在".to_string(),
                warnings,
            };
        }
        thread::sleep(Duration::from_millis(500));
    }
    append_update_log(
        state_path,
        &format!(
            "健康检查超时：port={}，expected_version={}，pid={}，最后观测={}",
            port, target_version, pid, last_observation
        ),
    );
    HealthAssessment {
        confirmed: false,
        reason: last_observation,
        warnings,
    }
}

fn health_matches_update(
    payload: &serde_json::Value,
    target_version: &str,
    executable: &Path,
    state_path: &Path,
) -> bool {
    assess_health(payload, target_version, executable, state_path).confirmed
}

/// Judge one /health payload.
///
/// The facts that prove the new Agent is up are the status, the version and the
/// executable path. Storage is deliberately not one of them: an Agent whose
/// data root is not writable still starts, serves the window and answers
/// /health, and reading that pre-existing machine problem as "the new version
/// failed" is what turns a working update into a rollback.
fn assess_health(
    payload: &serde_json::Value,
    target_version: &str,
    executable: &Path,
    state_path: &Path,
) -> HealthAssessment {
    let mut warnings = Vec::new();
    if target_version.trim().is_empty() {
        return HealthAssessment {
            confirmed: false,
            reason: "目标版本为空".to_string(),
            warnings,
        };
    }
    if payload.get("status").and_then(|value| value.as_str()) != Some("online") {
        return HealthAssessment {
            confirmed: false,
            reason: format!(
                "Agent 状态不是 online：{}",
                payload
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("缺失")
            ),
            warnings,
        };
    }
    let reported_version = payload
        .get("version")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !versions_match(target_version, &reported_version) {
        return HealthAssessment {
            confirmed: false,
            reason: format!(
                "Agent 版本不一致：实际={}，期望={}",
                if reported_version.trim().is_empty() {
                    "缺失"
                } else {
                    reported_version.as_str()
                },
                target_version
            ),
            warnings,
        };
    }
    if !state_path.is_file() {
        warnings.push(format!(
            "Agent 状态文件不存在：{}（属于存储问题，不是版本问题）",
            state_path.display()
        ));
    }
    match payload
        .get("executable_path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(reported_path) => {
            if !executable_paths_match(executable, Path::new(reported_path)) {
                return HealthAssessment {
                    confirmed: false,
                    reason: format!(
                        "Agent 执行路径不一致：实际={}，期望={}",
                        reported_path,
                        executable.display()
                    ),
                    warnings,
                };
            }
        }
        None => warnings.push("健康信息缺少 executable_path，无法核对执行路径".to_string()),
    }
    HealthAssessment {
        confirmed: true,
        reason: "ok".to_string(),
        warnings,
    }
}

/// Version strings reach this comparison from three places: the release
/// metadata, the active-version pointer and the running binary. Only two of
/// them are normalized, so compare them the way a person reads them instead of
/// failing an upgrade over a `v` prefix or a stray space.
fn versions_match(expected: &str, actual: &str) -> bool {
    normalize_version(expected) == normalize_version(actual)
}

fn normalize_version(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_ascii_lowercase()
}

fn executable_paths_match(expected: &Path, reported: &Path) -> bool {
    let expected =
        plain_path(&fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf()));
    let reported =
        plain_path(&fs::canonicalize(reported).unwrap_or_else(|_| reported.to_path_buf()));
    if expected == reported {
        return true;
    }
    // 规范化失败时（8.3 短名、目录项尚未刷新）退回按大小写不敏感的全路径比较。
    expected
        .to_string_lossy()
        .eq_ignore_ascii_case(&reported.to_string_lossy())
}
fn log_update(context: &UpdateArgs, message: &str) {
    append_update_log(&context.state_path, message);
}

fn append_update_log(state_path: &Path, message: &str) {
    let Some(root) = state_path.parent().and_then(Path::parent) else {
        return;
    };
    let log_path = root.join("logs").join("agent-updater.log");
    let Some(parent) = log_path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(file, "{timestamp} {message}");
    }
}

fn running_agent_pids(current_executable: &Path) -> Vec<u32> {
    let target = powershell_escape_single_quoted(&current_executable.to_string_lossy());
    let script = format!(
        "$target='{}'; Get-CimInstance Win32_Process -Filter \"Name='himind-agent.exe'\" | Where-Object {{ $_.ExecutablePath -and $_.ExecutablePath.Equals($target, [System.StringComparison]::OrdinalIgnoreCase) }} | ForEach-Object {{ $_.ProcessId }}",
        target
    );
    for shell in ["pwsh", "powershell"] {
        let Ok(output) = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .filter(|pid| *pid != std::process::id())
            .collect();
    }
    Vec::new()
}

/// True only when `pid` is alive and its image is exactly `executable`.
///
/// A health probe cannot tell which process answered /health: a leftover Agent
/// holding the port replies with the old version. So "the new Agent did not come
/// up" has to be decided by looking at the process this updater started, not at
/// whoever answered the port.
fn pid_runs_from(pid: u32, executable: &Path) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    let target =
        plain_path(&fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf()));
    let target = powershell_escape_single_quoted(&target.to_string_lossy());
    let script = format!(
        "$p = Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\" -ErrorAction SilentlyContinue; if ($p -and $p.ExecutablePath -and $p.ExecutablePath.Equals('{target}', [System.StringComparison]::OrdinalIgnoreCase)) {{ 'match' }} else {{ 'miss' }}"
    );
    for shell in ["pwsh", "powershell"] {
        let Ok(output) = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("match") {
            return true;
        }
        if stdout.contains("miss") {
            return false;
        }
    }
    // 查询不可用时保守处理：进程还在，就不把它当成启动失败。
    process_exists(pid)
}
fn process_exists(pid: u32) -> bool {
    let mut command = Command::new("tasklist");
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    !process_exists(pid)
}

fn terminate_single(pid: u32) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("taskkill");
    command.creation_flags(CREATE_NO_WINDOW);
    command.args(["/PID", &pid.to_string(), "/F"]).output()?;
    Ok(())
}

fn terminate_process_for_path(executable: &Path) -> Result<(), Box<dyn Error>> {
    for pid in running_agent_pids(executable) {
        let _ = terminate_single(pid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::install_layout::{
        prepare_version_directory, read_active_version, write_active_version,
    };
    use super::{
        assess_health, canonical_staged_helper, health_matches_update, install_runtime_bundle,
        plain_path, probe_health, read_json_value, replace_helper, staged_path_allowed,
    };
    use serde_json::json;
    use sha2::Digest;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn installs_the_bundled_runtime_before_switching_agent_versions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-runtime-{unique}"));
        fs::create_dir_all(root.join("resources/runtime")).unwrap();
        fs::write(root.join("resources/runtime/old.zip"), b"old").unwrap();

        let package = root.join("staged-runtime.zip");
        let package_bytes = b"runtime-package";
        fs::write(&package, package_bytes).unwrap();
        let sha256 = format!("{:x}", sha2::Sha256::digest(package_bytes));
        let manifest = root.join("staged-runtime.package.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&json!({
                "version": "1.2.3",
                "file_name": "himind-runtime.zip",
                "sha256": sha256,
            }))
            .unwrap(),
        )
        .unwrap();
        let signature = root.join("staged-runtime.signature.json");
        fs::write(
            &signature,
            serde_json::to_vec(&json!({
                "file_name": "himind-runtime.zip",
                "sha256": sha256,
            }))
            .unwrap(),
        )
        .unwrap();

        install_runtime_bundle(&root, &manifest, &package, &signature)
            .unwrap()
            .commit();

        assert!(root
            .join("resources/runtime/himind-runtime-release.json")
            .is_file());
        assert_eq!(
            fs::read(root.join("resources/runtime/himind-runtime.zip")).unwrap(),
            package_bytes
        );
        assert!(!root.join("resources/runtime/old.zip").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn uncommitted_runtime_bundle_restores_previous_release() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-runtime-rollback-{unique}"));
        fs::create_dir_all(root.join("resources/runtime")).unwrap();
        fs::write(root.join("resources/runtime/old.zip"), b"old").unwrap();

        let package = root.join("staged-runtime.zip");
        let package_bytes = b"runtime-package";
        fs::write(&package, package_bytes).unwrap();
        let sha256 = format!("{:x}", sha2::Sha256::digest(package_bytes));
        let manifest = root.join("staged-runtime.package.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&json!({
                "version": "1.2.3",
                "file_name": "himind-runtime.zip",
                "sha256": sha256,
            }))
            .unwrap(),
        )
        .unwrap();
        let signature = root.join("staged-runtime.signature.json");
        fs::write(
            &signature,
            serde_json::to_vec(&json!({
                "file_name": "himind-runtime.zip",
                "sha256": sha256,
            }))
            .unwrap(),
        )
        .unwrap();

        let transaction = install_runtime_bundle(&root, &manifest, &package, &signature).unwrap();
        assert!(root.join("resources/runtime/himind-runtime.zip").is_file());
        drop(transaction);

        assert_eq!(
            fs::read(root.join("resources/runtime/old.zip")).unwrap(),
            b"old"
        );
        assert!(!root.join("resources/runtime/himind-runtime.zip").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installs_version_side_by_side_and_switches_pointer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-test-{unique}"));
        fs::create_dir_all(root.join("current")).unwrap();
        fs::write(root.join("current/himind-agent.exe"), b"old").unwrap();
        let staged = root.join("staging-agent.exe");
        fs::write(&staged, b"new").unwrap();
        let staged_mcp = root.join("staging-mcp.exe");
        fs::write(&staged_mcp, b"mcp").unwrap();

        let version_dir = prepare_version_directory(&root, "0.4.0", &staged, &staged_mcp).unwrap();
        write_active_version(&root, "0.4.0").unwrap();
        assert_eq!(
            fs::read(version_dir.join("himind-agent.exe")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(version_dir.join("himind-agent-mcp.exe")).unwrap(),
            b"mcp"
        );
        assert_eq!(
            read_active_version(&root).unwrap().as_deref(),
            Some("0.4.0")
        );
        assert_eq!(
            fs::read(root.join("current/himind-agent.exe")).unwrap(),
            b"old"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_staging_path_outside_installation_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let staged_path =
            std::env::temp_dir().join(format!("himind-agent-update-path-test-{unique}.exe"));
        fs::write(&staged_path, b"agent").unwrap();
        let staged = fs::canonicalize(&staged_path).unwrap();
        let unrelated_root = staged
            .parent()
            .unwrap()
            .join(format!("himind-agent-installation-{unique}"));

        assert!(!staged_path_allowed(&staged, &unrelated_root));
        let _ = fs::remove_file(staged_path);
    }

    #[test]
    fn plain_path_strips_verbatim_prefixes() {
        use super::plain_path;
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\Program Files\HiMind\agent.exe")),
            PathBuf::from(r"C:\Program Files\HiMind\agent.exe")
        );
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\server\share\agent.exe")),
            PathBuf::from(r"\\server\share\agent.exe")
        );
        assert_eq!(
            plain_path(Path::new(r"C:\Program Files\HiMind\agent.exe")),
            PathBuf::from(r"C:\Program Files\HiMind\agent.exe")
        );
    }

    #[test]
    fn reads_utf8_bom_json() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("himind-updater-bom-{unique}.json"));
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(br#"{"status":"installing"}"#);
        fs::write(&path, bytes).unwrap();

        let value = read_json_value(&path).unwrap();
        assert_eq!(
            value.get("status").and_then(|v| v.as_str()),
            Some("installing")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn health_confirmation_requires_target_version_and_executable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("himind-agent.exe");
        let state = root.join("agent-state.json");
        fs::write(&executable, b"agent").unwrap();
        fs::write(&state, b"{}").unwrap();
        let payload = json!({
            "status": "online",
            "dashboard_worker_online": false,
            "version": "0.3.0",
            "executable_path": executable,
        });
        assert!(health_matches_update(
            &payload,
            "0.3.0",
            &executable,
            &state
        ));
        assert!(!health_matches_update(
            &payload,
            "0.3.1",
            &executable,
            &state
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_health_probe_ignores_http_proxy_environment() {
        // Keep environment mutation isolated from other tests in this binary.
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-proxy-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("himind-agent.exe");
        let state = root.join("agent-state.json");
        fs::write(&executable, b"agent").unwrap();
        fs::write(&state, b"{}").unwrap();
        let body = json!({
            "status": "online",
            "version": "0.3.0",
            "executable_path": executable,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"];
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            std::env::set_var(key, "http://127.0.0.1:9");
        }
        let healthy = probe_health(port, &state, "0.3.0", &executable, 0);
        for (key, value) in previous {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
        server.join().unwrap();
        let _ = fs::remove_dir_all(root);
        assert!(healthy.confirmed, "{}", healthy.reason);
    }

    #[test]
    fn health_confirmation_survives_a_missing_state_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-state-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("himind-agent.exe");
        fs::write(&executable, b"agent").unwrap();
        // Deliberately never created: a machine whose data root is not writable
        // has no state file, and that says nothing about the new version.
        let state = root.join("agent-state.json");
        let payload = json!({
            "status": "online",
            "version": "0.3.76",
            "executable_path": executable,
        });

        let assessment = assess_health(&payload, "0.3.76", &executable, &state);
        assert!(assessment.confirmed, "{}", assessment.reason);
        assert!(
            assessment
                .warnings
                .iter()
                .any(|warning| warning.contains("状态文件不存在")),
            "{:?}",
            assessment.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn health_confirmation_tolerates_version_prefix_and_padding() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-version-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("himind-agent.exe");
        let state = root.join("agent-state.json");
        fs::write(&executable, b"agent").unwrap();
        fs::write(&state, b"{}").unwrap();
        let payload = json!({
            "status": "online",
            "version": "0.3.76",
            "executable_path": executable,
        });

        assert!(assess_health(&payload, " v0.3.76 ", &executable, &state).confirmed);
        assert!(!assess_health(&payload, "0.3.77", &executable, &state).confirmed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn health_confirmation_rejects_a_foreign_executable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-foreign-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("himind-agent.exe");
        let other = root.join("other-agent.exe");
        let state = root.join("agent-state.json");
        fs::write(&target, b"agent").unwrap();
        fs::write(&other, b"agent").unwrap();
        fs::write(&state, b"{}").unwrap();
        let payload = json!({
            "status": "online",
            "version": "0.3.76",
            "executable_path": other,
        });

        let assessment = assess_health(&payload, "0.3.76", &target, &state);
        assert!(!assessment.confirmed);
        assert!(
            assessment.reason.contains("执行路径不一致"),
            "{}",
            assessment.reason
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn validates_and_replaces_staged_helper() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-helper-test-{unique}"));
        let staged_dir = root.join("staging");
        fs::create_dir_all(&staged_dir).unwrap();
        let staged = staged_dir.join("himind-agent-launcher.exe");
        let wrong_name = staged_dir.join("launcher.exe");
        fs::write(&staged, b"new launcher").unwrap();
        fs::write(&wrong_name, b"wrong").unwrap();

        let validated =
            canonical_staged_helper(&staged, "himind-agent-launcher.exe", &root).unwrap();
        assert_eq!(validated, plain_path(&fs::canonicalize(&staged).unwrap()));
        assert!(canonical_staged_helper(&wrong_name, "himind-agent-launcher.exe", &root).is_err());

        let target = root.join("himind-agent-launcher.exe");
        let backup = root.join("himind-agent-launcher.previous.exe");
        fs::write(&target, b"old launcher").unwrap();
        replace_helper(&target, &backup, &staged).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new launcher");
        assert_eq!(fs::read(&backup).unwrap(), b"old launcher");
        let _ = fs::remove_dir_all(root);
    }
}
