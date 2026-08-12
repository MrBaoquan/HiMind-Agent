use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::client::{
    claim_agent_run, is_task_canceled_error, renew_agent_run_lease, update_agent_run_status,
    TaskCancelGuard,
};
use crate::api::distribution::{resolve_runtime_component, RuntimeComponentUpdate};
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::app::system::{validate_update_download_url, verify_runtime_component_signature};
use crate::runtime::normalize_execution_result;
use crate::runtime::process;
use crate::store::paths::agent_home;
use crate::Options;

const RUNTIME_PROVIDER: &str = "himind.openhands";
const DEFAULT_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const OUTPUT_CAPTURE_LIMIT: usize = 64 * 1024;
const ERROR_DETAIL_LIMIT: usize = 4_000;
const INSTALL_TIMEOUT_SECONDS: u64 = 15 * 60;
const RUNTIME_PRODUCT_ID: &str = "com.himind.runtime.openhands";
const RUNTIME_CHANNEL: &str = "stable";
const RUNTIME_PLATFORM: &str = "windows";
const RUNTIME_ARCHITECTURE: &str = "x64";
const RUNTIME_MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const RUNTIME_MAX_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const INSTALL_COMMAND: &str = "Dashboard Runtime Resolve + SHA-256/RSA-PSS 校验 + 离线安装";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstalledRuntimeState {
    schema_version: u32,
    product_id: String,
    provider: String,
    version: String,
    artifact_sha256: String,
    install_directory: String,
    executable_path: String,
    uv_path: String,
    python_path: String,
    environment_path: String,
}

#[derive(Debug, Deserialize)]
struct RuntimePackageManifest {
    schema_version: u32,
    product_id: String,
    runtime_provider: String,
    version: String,
    uv_version: String,
    python_version: String,
    requirements_lock: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OpenHandsRuntimeStatus {
    pub provider: String,
    pub status: String,
    pub version: String,
    pub cli_compatible: bool,
    pub executable_path: String,
    pub uv_available: bool,
    pub uv_version: String,
    pub python_available: bool,
    pub python_version: String,
    pub install_command: String,
    pub message: String,
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    let runtime = managed_runtime_status();
    RuntimeInstallationReport {
        provider: RUNTIME_PROVIDER.to_string(),
        version: runtime.version,
        status: runtime.status,
        capabilities: json!({"managed_execution":true,"billing_owner":"himind","ai_proxy":true,"cli_compatible":runtime.cli_compatible}),
    }
}

pub(crate) fn status() -> OpenHandsRuntimeStatus {
    managed_runtime_status()
}

pub(crate) fn install(
    options: &Options,
    client_instance_id: &str,
) -> Result<OpenHandsRuntimeStatus, String> {
    install_from_dashboard(options, client_instance_id)
}

fn first_output_line(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn runtime_root() -> PathBuf {
    agent_home().join("runtimes").join("openhands")
}

fn runtime_state_path() -> PathBuf {
    runtime_root().join("state.json")
}

fn load_runtime_state() -> Result<Option<InstalledRuntimeState>, String> {
    let path = runtime_state_path();
    if !path.is_file() {
        return Ok(None);
    }
    let state = serde_json::from_slice::<InstalledRuntimeState>(
        &fs::read(&path).map_err(|error| format!("读取 OpenHands Runtime 状态失败: {error}"))?,
    )
    .map_err(|error| format!("OpenHands Runtime 状态损坏: {error}"))?;
    if state.schema_version != 1
        || state.product_id != RUNTIME_PRODUCT_ID
        || state.provider != RUNTIME_PROVIDER
    {
        return Err("OpenHands Runtime 状态版本或产品标识不匹配".to_string());
    }
    Ok(Some(state))
}

fn managed_runtime_status() -> OpenHandsRuntimeStatus {
    if let Some(executable) =
        env::var_os("HIMIND_OPENHANDS_EXECUTABLE").filter(|value| !value.is_empty())
    {
        return status_for_paths(executable, None, None, "开发调试覆盖");
    }
    match load_runtime_state() {
        Ok(Some(state)) => status_for_paths(
            OsString::from(&state.executable_path),
            Some(OsString::from(&state.uv_path)),
            Some(OsString::from(&state.python_path)),
            &state.version,
        ),
        Ok(None) => unavailable_runtime_status(
            "尚未安装 OpenHands Runtime。点击安装即可从 Dashboard 获取签名组件。",
        ),
        Err(error) => unavailable_runtime_status(&error),
    }
}

fn unavailable_runtime_status(message: &str) -> OpenHandsRuntimeStatus {
    OpenHandsRuntimeStatus {
        provider: RUNTIME_PROVIDER.to_string(),
        status: "unavailable".to_string(),
        version: String::new(),
        cli_compatible: false,
        executable_path: String::new(),
        uv_available: false,
        uv_version: String::new(),
        python_available: false,
        python_version: String::new(),
        install_command: INSTALL_COMMAND.to_string(),
        message: message.to_string(),
    }
}

fn status_for_paths(
    executable: OsString,
    uv: Option<OsString>,
    python: Option<OsString>,
    version_hint: &str,
) -> OpenHandsRuntimeStatus {
    let openhands = process::verify_command(&executable, &["--version"])
        .ok()
        .map(|value| openhands_version_line(&value))
        .unwrap_or_default();
    let cli_compatible = !openhands.is_empty() && cli_supports_required_flags(&executable);
    let uv_version = uv
        .as_ref()
        .and_then(|path| process::verify_command(path, &["--version"]).ok())
        .map(|value| first_output_line(&value))
        .unwrap_or_default();
    let python_version = python
        .as_ref()
        .and_then(|path| process::verify_command(path, &["--version"]).ok())
        .map(|value| first_output_line(&value))
        .unwrap_or_default();
    let ready = cli_compatible;
    OpenHandsRuntimeStatus {
        provider: RUNTIME_PROVIDER.to_string(),
        status: if ready { "ready" } else { "unavailable" }.to_string(),
        version: if openhands.is_empty() {
            version_hint.to_string()
        } else {
            openhands
        },
        cli_compatible,
        executable_path: executable.to_string_lossy().to_string(),
        uv_available: !uv_version.is_empty(),
        uv_version,
        python_available: !python_version.is_empty(),
        python_version,
        install_command: INSTALL_COMMAND.to_string(),
        message: if ready {
            "OpenHands Runtime 已就绪，可通过 HiMind AI 代理执行任务。".to_string()
        } else {
            "Runtime 已安装但 OpenHands CLI 预检未通过，请重新安装或检查发布包。".to_string()
        },
    }
}

fn install_from_dashboard(
    options: &Options,
    client_instance_id: &str,
) -> Result<OpenHandsRuntimeStatus, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(INSTALL_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("创建 Dashboard 客户端失败: {error}"))?;
    let update = resolve_runtime_component(
        &client,
        &options.api_base,
        RUNTIME_PRODUCT_ID,
        "0.0.0",
        RUNTIME_CHANNEL,
        RUNTIME_PLATFORM,
        RUNTIME_ARCHITECTURE,
        client_instance_id,
    )
    .map_err(|error| format!("解析 OpenHands Runtime 发布失败: {error}"))?
    .ok_or_else(|| {
        "Dashboard 没有发布可用的 OpenHands Runtime，请先创建并发布 Runtime Release。".to_string()
    })?;
    if update.product_id != RUNTIME_PRODUCT_ID
        || update.channel != RUNTIME_CHANNEL
        || update.package_type != "directory-zip"
        || update.file_name.to_ascii_lowercase().ends_with(".zip") == false
    {
        return Err("Dashboard Runtime manifest 不符合 OpenHands directory-zip 契约".to_string());
    }
    validate_update_download_url(&options.api_base, &update.artifact_url)
        .map_err(|error| format!("Runtime 下载地址校验失败: {error}"))?;
    if update.size == 0 || update.size > RUNTIME_MAX_PACKAGE_BYTES {
        return Err("Runtime 包大小超出 Agent 安全限制".to_string());
    }
    let archive = download_runtime_archive(&client, &update)?;
    let result = install_runtime_archive(&archive, &update);
    if result.is_ok() {
        let _ = fs::remove_file(&archive);
    }
    result.map(|_| managed_runtime_status())
}

fn download_runtime_archive(
    client: &Client,
    update: &RuntimeComponentUpdate,
) -> Result<PathBuf, String> {
    let download_dir = runtime_root().join("downloads");
    fs::create_dir_all(&download_dir)
        .map_err(|error| format!("创建 Runtime 下载目录失败: {error}"))?;
    let file_name = format!("{}.zip", update.sha256.to_ascii_lowercase());
    let target = download_dir.join(file_name);
    let partial = target.with_extension("zip.part");
    let mut response = client
        .get(&update.artifact_url)
        .send()
        .map_err(|error| format!("下载 Runtime 失败: {error}"))?;
    response
        .error_for_status_ref()
        .map_err(|error| format!("Runtime 下载响应失败: {error}"))?;
    let mut file =
        File::create(&partial).map_err(|error| format!("创建 Runtime 临时文件失败: {error}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response
            .read(&mut buffer)
            .map_err(|error| format!("读取 Runtime 下载失败: {error}"))?;
        if count == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(count as u64);
        if downloaded > update.size || downloaded > RUNTIME_MAX_PACKAGE_BYTES {
            let _ = fs::remove_file(&partial);
            return Err("Runtime 下载大小超过 Dashboard manifest".to_string());
        }
        file.write_all(&buffer[..count])
            .map_err(|error| format!("写入 Runtime 下载失败: {error}"))?;
        hasher.update(&buffer[..count]);
    }
    file.flush()
        .map_err(|error| format!("刷新 Runtime 下载失败: {error}"))?;
    if downloaded != update.size {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "Runtime 下载大小不匹配: expected {}, got {}",
            update.size, downloaded
        ));
    }
    let actual = format!("{:x}", hasher.finalize());
    if update.sha256.len() != 64 || !actual.eq_ignore_ascii_case(&update.sha256) {
        let _ = fs::remove_file(&partial);
        return Err("Runtime SHA-256 校验失败".to_string());
    }
    verify_runtime_component_signature(
        &partial,
        &update.signature,
        &update.signature_key_id,
        &update.signature_algorithm,
    )
    .map_err(|error| format!("Runtime RSA-PSS 签名校验失败: {error}"))?;
    let _ = fs::remove_file(&target);
    fs::rename(&partial, &target).map_err(|error| format!("保存 Runtime 下载包失败: {error}"))?;
    Ok(target)
}

fn install_runtime_archive(
    archive_path: &Path,
    update: &RuntimeComponentUpdate,
) -> Result<(), String> {
    let root = runtime_root();
    let versions = root.join("versions");
    fs::create_dir_all(&versions).map_err(|error| format!("创建 Runtime 安装目录失败: {error}"))?;
    let suffix = &update.sha256[..12.min(update.sha256.len())];
    let install_directory = versions.join(format!(
        "{}-{}",
        safe_runtime_segment(&update.version)?,
        suffix
    ));
    if install_directory.exists() {
        fs::remove_dir_all(&install_directory)
            .map_err(|error| format!("清理旧 Runtime 安装目录失败: {error}"))?;
    }
    extract_runtime_archive(archive_path, &install_directory)?;
    let manifest: RuntimePackageManifest = serde_json::from_slice(
        &fs::read(install_directory.join("runtime.json"))
            .map_err(|error| format!("读取 runtime.json 失败: {error}"))?,
    )
    .map_err(|error| format!("runtime.json 无效: {error}"))?;
    if manifest.schema_version != 1
        || manifest.product_id != RUNTIME_PRODUCT_ID
        || manifest.runtime_provider != RUNTIME_PROVIDER
        || manifest.version != update.version
        || manifest.uv_version.trim().is_empty()
        || !manifest.python_version.trim().starts_with("3.12.")
        || manifest.requirements_lock != "requirements.lock"
    {
        let _ = fs::remove_dir_all(&install_directory);
        return Err("runtime.json 与 Dashboard Runtime manifest 不一致".to_string());
    }
    let uv_path = install_directory.join("tools").join("uv.exe");
    let python_path = install_directory.join("python").join("python.exe");
    let python_dll = install_directory.join("python").join("python312.dll");
    let python_stdlib = install_directory.join("python").join("Lib").join("os.py");
    let wheelhouse = install_directory.join("wheelhouse");
    let lock_file = install_directory.join(&manifest.requirements_lock);
    if !uv_path.is_file()
        || !python_path.is_file()
        || !python_dll.is_file()
        || !python_stdlib.is_file()
        || !wheelhouse.is_dir()
        || !lock_file.is_file()
    {
        let _ = fs::remove_dir_all(&install_directory);
        return Err("Runtime 包缺少 uv、Python、wheelhouse 或 requirements.lock".to_string());
    }
    let environment = install_directory.join("environment");
    if let Err(error) = run_runtime_command(
        &uv_path,
        &[
            "venv",
            "--python",
            python_path.to_string_lossy().as_ref(),
            environment.to_string_lossy().as_ref(),
        ],
        &install_directory,
    ) {
        let _ = fs::remove_dir_all(&install_directory);
        return Err(error);
    }
    let environment_python = environment.join("Scripts").join("python.exe");
    if !environment_python.is_file() {
        let _ = fs::remove_dir_all(&install_directory);
        return Err("uv 未创建可用的 Windows Runtime Python 环境".to_string());
    }
    if let Err(error) = run_runtime_command(
        &uv_path,
        &[
            "pip",
            "install",
            "--python",
            environment_python.to_string_lossy().as_ref(),
            "--no-index",
            "--find-links",
            wheelhouse.to_string_lossy().as_ref(),
            "--requirement",
            lock_file.to_string_lossy().as_ref(),
        ],
        &install_directory,
    ) {
        let _ = fs::remove_dir_all(&install_directory);
        return Err(error);
    }
    let executable = find_runtime_executable(&environment).ok_or_else(|| {
        let _ = fs::remove_dir_all(&install_directory);
        "Runtime 安装完成但未找到 openhands.exe".to_string()
    })?;
    let state = InstalledRuntimeState {
        schema_version: 1,
        product_id: RUNTIME_PRODUCT_ID.to_string(),
        provider: RUNTIME_PROVIDER.to_string(),
        version: update.version.clone(),
        artifact_sha256: update.sha256.clone(),
        install_directory: install_directory.to_string_lossy().to_string(),
        executable_path: executable.to_string_lossy().to_string(),
        uv_path: uv_path.to_string_lossy().to_string(),
        python_path: python_path.to_string_lossy().to_string(),
        environment_path: environment.to_string_lossy().to_string(),
    };
    write_runtime_state(&state)?;
    Ok(())
}

fn extract_runtime_archive(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(
        File::open(archive_path).map_err(|error| format!("打开 Runtime ZIP 失败: {error}"))?,
    )
    .map_err(|error| format!("Runtime ZIP 无效: {error}"))?;
    if archive.len() == 0 || archive.len() > 20_000 {
        return Err("Runtime ZIP 文件数量不合法".to_string());
    }
    let temporary = target_dir.with_extension("installing");
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    fs::create_dir_all(&temporary)
        .map_err(|error| format!("创建 Runtime 临时目录失败: {error}"))?;
    let result = (|| -> Result<(), String> {
        let mut seen = HashSet::new();
        let mut total = 0_u64;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| format!("读取 Runtime ZIP 条目失败: {error}"))?;
            let name = entry.name().to_string();
            let enclosed = entry
                .enclosed_name()
                .ok_or_else(|| "Runtime ZIP 包含不安全路径".to_string())?;
            if name.contains('\\')
                || !seen.insert(name.clone())
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Runtime ZIP 包含重复、反斜杠或符号链接路径".to_string());
            }
            total = total.saturating_add(entry.size());
            if total > RUNTIME_MAX_UNCOMPRESSED_BYTES {
                return Err("Runtime ZIP 解压内容过大".to_string());
            }
            let destination = temporary.join(enclosed);
            if entry.is_dir() {
                fs::create_dir_all(&destination)
                    .map_err(|error| format!("创建 Runtime 目录失败: {error}"))?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("创建 Runtime 目录失败: {error}"))?;
            }
            let mut output = File::create(&destination)
                .map_err(|error| format!("创建 Runtime 文件失败: {error}"))?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|error| format!("解压 Runtime 文件失败: {error}"))?;
        }
        for required in [
            "runtime.json",
            "tools/uv.exe",
            "python/python.exe",
            "python/python312.dll",
            "python/Lib/os.py",
            "requirements.lock",
        ] {
            if !temporary.join(required).is_file() {
                return Err(format!("Runtime ZIP 缺少 {required}"));
            }
        }
        if !temporary.join("wheelhouse").is_dir() {
            return Err("Runtime ZIP 缺少 wheelhouse".to_string());
        }
        fs::rename(&temporary, target_dir)
            .map_err(|error| format!("提交 Runtime 安装目录失败: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn run_runtime_command(
    executable: &Path,
    args: &[&str],
    working_directory: &Path,
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    configure_hidden_process(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("运行 Runtime 安装命令失败: {error}"))?;
    if !output.status.success() {
        let detail = if output.stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };
        return Err(format!(
            "Runtime 安装命令失败 (exit={}): {}",
            output.status.code().unwrap_or(-1),
            process::summarize_output(detail.trim(), ERROR_DETAIL_LIMIT)
        ));
    }
    Ok(())
}

fn find_runtime_executable(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| {
                name.eq_ignore_ascii_case("openhands.exe") || name.eq_ignore_ascii_case("openhands")
            }) {
                return Some(path);
            }
        }
    }
    None
}

fn write_runtime_state(state: &InstalledRuntimeState) -> Result<(), String> {
    let path = runtime_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 Runtime 状态目录失败: {error}"))?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(state)
            .map_err(|error| format!("序列化 Runtime 状态失败: {error}"))?,
    )
    .map_err(|error| format!("写入 Runtime 状态失败: {error}"))?;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| format!("替换 Runtime 状态失败: {error}"))?;
    }
    fs::rename(&temporary, &path).map_err(|error| format!("提交 Runtime 状态失败: {error}"))
}

fn safe_runtime_segment(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err("Runtime 版本号不能用于本地目录名".to_string());
    }
    Ok(value.to_string())
}

fn cli_supports_required_flags(executable: &OsStr) -> bool {
    let Ok(help) = process::verify_command(executable, &["--help"]) else {
        return false;
    };
    ["--headless", "--json", "--override-with-envs", "--task"]
        .iter()
        .all(|flag| help.contains(flag))
}

fn openhands_version_line(output: &str) -> String {
    let value = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.to_ascii_lowercase().contains("openhands"))
        .or_else(|| output.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or_default();
    crate::runtime::process::summarize_output(value, 200)
}

#[cfg(windows)]
const WINDOWS_PYTHON_COMPAT: &str = r#"# HiMind compatibility for Rich in CREATE_NO_WINDOW child processes.
try:
    import rich.console as _rich_console
    _rich_console.detect_legacy_windows = lambda: False
except Exception:
    pass
"#;

#[derive(Debug, Deserialize)]
struct AgentRunEnvelope {
    run_id: String,
    runtime_provider: String,
}

#[derive(Debug)]
struct OpenHandsInvocation {
    executable: OsString,
    args: Vec<OsString>,
    workspace: PathBuf,
    api_key: String,
    model: String,
    base_url: String,
    run_id: String,
}

struct RunLeaseRenewal {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for RunLeaseRenewal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
) -> Result<Value, Box<dyn Error>> {
    let envelope = serde_json::from_value::<AgentRunEnvelope>(
        task.payload.clone().ok_or("Agent Run Task 缺少执行信封")?,
    )?;
    if envelope.run_id.trim().is_empty() || envelope.runtime_provider != RUNTIME_PROVIDER {
        return Err("Agent Run Task 只允许 himind.openhands Runtime".into());
    }
    let credential = options.agent_credential();
    let claim = claim_agent_run(
        client,
        &options.api_base,
        agent_id,
        &task.id,
        &envelope.run_id,
        &credential,
    )?;
    if claim.run.id != envelope.run_id || claim.run.runtime_provider != RUNTIME_PROVIDER {
        return Err("Dashboard 返回的 Agent Run 身份或 Runtime 无效".into());
    }
    if claim.run.status != "claimed" || claim.run.created_by_user_id.trim().is_empty() {
        return Err("Dashboard 返回的 Agent Run 状态或用户身份无效".into());
    }

    let result = execute_claimed(client, options, agent_id, task, &claim);
    match result {
        Ok(value) => {
            let value = normalize_execution_result(value, RUNTIME_PROVIDER);
            update_agent_run_status(
                client,
                &options.api_base,
                agent_id,
                &claim.run.id,
                &claim.claim_token,
                "succeeded",
                Some(&value),
                "",
                &credential,
            )?;
            Ok(value)
        }
        Err(error) => {
            let message = redact_error(&error.to_string(), &claim, &credential);
            let status = if is_task_canceled_error(&message) {
                "canceled"
            } else {
                "failed"
            };
            if let Err(report_error) = update_agent_run_status(
                client,
                &options.api_base,
                agent_id,
                &claim.run.id,
                &claim.claim_token,
                status,
                None,
                &message,
                &credential,
            ) {
                eprintln!(
                    "Agent Run {} failure report failed: {report_error}",
                    claim.run.id
                );
            }
            Err(message.into())
        }
    }
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
) -> Result<Value, Box<dyn Error>> {
    let invocation = build_invocation(options, claim)?;
    verify_openhands_available(&invocation.executable)?;
    update_agent_run_status(
        client,
        &options.api_base,
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = start_run_lease_renewal(client, options, agent_id, claim);
    let mut child = spawn_openhands(&invocation)?;
    let stdout = child.stdout.take().map(capture_output);
    let stderr = child.stderr.take().map(capture_output);
    let status = wait_for_openhands(client, options, agent_id, &task.id, &mut child)?;
    let stdout = join_output(stdout);
    let stderr = join_output(stderr);
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let detail = redact_error(&detail, claim, &options.agent_credential());
        return Err(format!(
            "OpenHands 执行失败（exit={}）：{}",
            status.code().unwrap_or(-1),
            detail
        )
        .into());
    }
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": RUNTIME_PROVIDER,
        "completed": true,
        "final_message": process::summarize_output(stdout.trim(), OUTPUT_CAPTURE_LIMIT),
        "billing_owner": "himind"
    }))
}

fn build_invocation(
    options: &Options,
    claim: &AgentRunClaim,
) -> Result<OpenHandsInvocation, Box<dyn Error>> {
    if claim.claim_token.trim().is_empty() {
        return Err("Dashboard 未返回 Agent Run AI 代理凭据".into());
    }
    if claim.ai_model.trim().is_empty() {
        return Err("Dashboard 未配置 OpenHands 使用的 HiMind AI 模型".into());
    }
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let mut instruction = claim.run.instruction.trim().to_string();
    if instruction.is_empty() {
        return Err("Agent Run 指令为空".into());
    }
    if !claim.run.input.is_null()
        && claim
            .run
            .input
            .as_object()
            .is_none_or(|value| !value.is_empty())
    {
        instruction.push_str("\n\n结构化输入（JSON）：\n");
        instruction.push_str(&serde_json::to_string(&claim.run.input)?);
    }
    let executable = env::var_os("HIMIND_OPENHANDS_EXECUTABLE")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            load_runtime_state()
                .ok()
                .flatten()
                .map(|state| OsString::from(state.executable_path))
        })
        .ok_or("OpenHands Runtime 尚未安装，请先从 Dashboard 安装签名 Runtime")?;
    Ok(OpenHandsInvocation {
        executable,
        args: vec![
            OsString::from("--headless"),
            OsString::from("--json"),
            OsString::from("--override-with-envs"),
            OsString::from("--task"),
            OsString::from(instruction),
        ],
        workspace,
        api_key: claim.claim_token.clone(),
        model: openai_compatible_model(&claim.ai_model),
        base_url: format!(
            "{}/api/agent/runs/{}/ai/v1",
            options.api_base.trim_end_matches('/'),
            claim.run.id
        ),
        run_id: claim.run.id.clone(),
    })
}

fn verify_openhands_available(executable: &OsStr) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_hidden_process(&mut command);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "OpenHands CLI 不可用（version exit={}）。请安装 OpenHands V1 CLI，或设置 HIMIND_OPENHANDS_EXECUTABLE",
            status.code().unwrap_or(-1)
        )
        .into()),
        Err(error) => Err(format!(
            "OpenHands CLI 未安装或不可执行：{error}。请使用 `uv tool install openhands --python 3.12` 安装，或设置 HIMIND_OPENHANDS_EXECUTABLE"
        )
        .into()),
    }
}

fn spawn_openhands(invocation: &OpenHandsInvocation) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(&invocation.executable);
    process::remove_himind_secret_environment(&mut command);
    command
        .args(&invocation.args)
        .current_dir(&invocation.workspace)
        .env("LLM_API_KEY", &invocation.api_key)
        .env("LLM_MODEL", &invocation.model)
        .env("LLM_BASE_URL", &invocation.base_url)
        .env("HIMIND_AGENT_RUN_ID", &invocation.run_id)
        .env("OPENHANDS_SUPPRESS_BANNER", "1")
        .env("PYTHONWARNINGS", "ignore::DeprecationWarning")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .env("TTY_COMPATIBLE", "0")
        .env("NO_COLOR", "1")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("AZURE_OPENAI_API_KEY")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.env("PYTHONPATH", windows_python_compat_path()?);
    configure_hidden_process(&mut command);
    Ok(command.spawn()?)
}

#[cfg(windows)]
fn windows_python_compat_path() -> Result<OsString, Box<dyn Error>> {
    let directory = env::temp_dir()
        .join("himind-agent")
        .join("openhands-python-compat-v1");
    fs::create_dir_all(&directory)?;
    let hook = directory.join("sitecustomize.py");
    let current = fs::read_to_string(&hook).unwrap_or_default();
    if current != WINDOWS_PYTHON_COMPAT {
        fs::write(&hook, WINDOWS_PYTHON_COMPAT)?;
    }
    let mut paths = vec![directory];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    Ok(env::join_paths(paths)?)
}

fn wait_for_openhands(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task_id: &str,
    child: &mut Child,
) -> Result<ExitStatus, Box<dyn Error>> {
    let timeout = env::var("HIMIND_OPENHANDS_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 60)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let started = Instant::now();
    let mut cancel_guard = TaskCancelGuard::new();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if let Err(error) = cancel_guard.check(client, options, agent_id, task_id) {
            if is_task_canceled_error(&error.to_string()) {
                process::terminate_process_tree(child);
                return Err(error);
            }
            eprintln!("Agent Run task {task_id} cancellation check failed: {error}");
        }
        if started.elapsed() >= Duration::from_secs(timeout) {
            process::terminate_process_tree(child);
            return Err(format!("OpenHands 执行超过 {} 秒，已终止", timeout).into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn start_run_lease_renewal(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
) -> RunLeaseRenewal {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let client = client.clone();
    let api_base = options.api_base.clone();
    let credential = options.agent_credential();
    let agent_id = agent_id.to_string();
    let run_id = claim.run.id.clone();
    let claim_token = claim.claim_token.clone();
    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            for _ in 0..120 {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
            if let Err(error) = renew_agent_run_lease(
                &client,
                &api_base,
                &agent_id,
                &run_id,
                &claim_token,
                &credential,
            ) {
                eprintln!("Agent Run {run_id} lease renew failed: {error}");
            }
        }
    });
    RunLeaseRenewal {
        stop,
        handle: Some(handle),
    }
}

fn capture_output<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    captured.extend_from_slice(&buffer[..size]);
                    if captured.len() > OUTPUT_CAPTURE_LIMIT {
                        let excess = captured.len() - OUTPUT_CAPTURE_LIMIT;
                        captured.drain(..excess);
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).trim().to_string()
    })
}

fn join_output(handle: Option<thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|value| value.join().ok())
        .unwrap_or_default()
}

fn redact_error(value: &str, claim: &AgentRunClaim, agent_credential: &str) -> String {
    let mut redacted = value.to_string();
    if !claim.claim_token.is_empty() {
        redacted = redacted.replace(&claim.claim_token, "[redacted]");
    }
    if !agent_credential.is_empty() {
        redacted = redacted.replace(agent_credential, "[redacted]");
    }
    summarize_output(&redacted, ERROR_DETAIL_LIMIT)
}

fn openai_compatible_model(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("openai/") {
        model.to_string()
    } else {
        format!("openai/{model}")
    }
}

fn summarize_output(value: &str, limit: usize) -> String {
    let length = value.chars().count();
    if length <= limit {
        return value.to_string();
    }
    let marker = "\n...[truncated; preserving stderr tail]...\n";
    let marker_length = marker.chars().count();
    if limit <= marker_length {
        return value.chars().skip(length - limit).collect();
    }
    let head_limit = (limit - marker_length) / 4;
    let tail_limit = limit - marker_length - head_limit;
    let head = value.chars().take(head_limit).collect::<String>();
    let tail = value.chars().skip(length - tail_limit).collect::<String>();
    format!("{head}{marker}{tail}")
}

#[cfg(windows)]
fn configure_hidden_process(command: &mut Command) {
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn configure_hidden_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::{
        build_invocation, extract_runtime_archive, openai_compatible_model, openhands_version_line,
        summarize_output, AgentRunClaim, AgentRunEnvelope, RUNTIME_PROVIDER,
    };

    #[test]
    fn probe_keeps_only_the_openhands_version_line() {
        assert_eq!(
            openhands_version_line("OpenHands CLI 1.16.0\n\n+------ banner ------+"),
            "OpenHands CLI 1.16.0"
        );
    }
    #[cfg(windows)]
    use super::{windows_python_compat_path, WINDOWS_PYTHON_COMPAT};
    use crate::api::types::AgentRun;
    use crate::Options;
    use serde_json::json;
    use std::fs;
    use std::io::Write;

    #[test]
    fn parses_only_openhands_delivery_envelope() {
        let envelope: AgentRunEnvelope = serde_json::from_value(json!({
            "run_id": "run-1",
            "runtime_provider": "himind.openhands"
        }))
        .unwrap();
        assert_eq!(envelope.run_id, "run-1");
        assert_eq!(envelope.runtime_provider, RUNTIME_PROVIDER);
    }

    #[test]
    fn invocation_keeps_proxy_token_out_of_arguments() {
        let root = std::env::temp_dir().join(format!(
            "himind-openhands-invocation-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut options = Options::from_env();
        options.api_base = "https://dashboard.example".to_string();
        let claim = AgentRunClaim {
            run: AgentRun {
                id: "run-1".to_string(),
                instruction: "检查并修复测试".to_string(),
                status: "claimed".to_string(),
                created_by_user_id: "user-1".to_string(),
                runtime_provider: RUNTIME_PROVIDER.to_string(),
                access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
                input: json!({"suite":"unit"}),
            },
            claim_token: "run-secret-token".to_string(),
            workspace_path: root.to_string_lossy().to_string(),
            ai_model: "himind-coding".to_string(),
            access_mode: crate::app::remote_execution::ACCESS_MODE_EXHIBIT_LINKED.to_string(),
        };
        std::env::set_var("HIMIND_OPENHANDS_EXECUTABLE", "openhands");
        let invocation = build_invocation(&options, &claim).unwrap();
        std::env::remove_var("HIMIND_OPENHANDS_EXECUTABLE");
        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(args.contains("--headless"));
        assert!(args.contains("--override-with-envs"));
        assert!(args.contains("结构化输入"));
        assert!(!args.contains("run-secret-token"));
        assert_eq!(invocation.api_key, "run-secret-token");
        assert_eq!(invocation.model, "openai/himind-coding");
        assert_eq!(
            invocation.base_url,
            "https://dashboard.example/api/agent/runs/run-1/ai/v1"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn openhands_model_uses_openai_compatible_provider() {
        assert_eq!(openai_compatible_model("glm-5.1"), "openai/glm-5.1");
        assert_eq!(openai_compatible_model("openai/glm-5.1"), "openai/glm-5.1");
    }

    #[test]
    fn long_error_summary_preserves_the_traceback_tail() {
        let value = format!("{}ROOT_CAUSE", "banner".repeat(1_000));
        let summary = summarize_output(&value, 200);
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.starts_with("banner"));
        assert!(summary.contains("[truncated; preserving stderr tail]"));
        assert!(summary.ends_with("ROOT_CAUSE"));
    }

    #[test]
    fn runtime_archive_extracts_required_files_and_rejects_traversal() {
        let root = std::env::temp_dir().join(format!(
            "himind-openhands-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("runtime.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        for (name, content) in [
            ("runtime.json", "{}"),
            ("tools/uv.exe", "uv"),
            ("python/python.exe", "python"),
            ("python/python312.dll", "python dll"),
            ("python/Lib/os.py", "stdlib"),
            ("requirements.lock", "openhands==1.0.0"),
            ("wheelhouse/openhands.whl", "wheel"),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let target = root.join("installed");
        extract_runtime_archive(&archive_path, &target).unwrap();
        assert!(target.join("runtime.json").is_file());
        assert!(target.join("wheelhouse/openhands.whl").is_file());
        fs::remove_dir_all(&root).unwrap();

        let unsafe_archive = root.with_file_name(format!(
            "{}-unsafe",
            root.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir_all(&root).unwrap();
        let file = fs::File::create(&unsafe_archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file("../outside.txt", options).unwrap();
        writer.write_all(b"blocked").unwrap();
        writer.finish().unwrap();
        assert!(extract_runtime_archive(&unsafe_archive, &root.join("rejected")).is_err());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(&unsafe_archive).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_python_compat_disables_rich_legacy_renderer() {
        let python_path = windows_python_compat_path().unwrap();
        let directory = std::env::split_paths(&python_path).next().unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("sitecustomize.py")).unwrap(),
            WINDOWS_PYTHON_COMPAT
        );
    }

    #[test]
    fn installs_signed_runtime_from_dashboard_when_e2e_is_explicitly_enabled() {
        if std::env::var("HIMIND_OPENHANDS_RUNTIME_E2E").as_deref() != Ok("1") {
            return;
        }

        let dashboard_api = std::env::var("DASHBOARD_API_BASE")
            .expect("DASHBOARD_API_BASE is required for the OpenHands Runtime E2E test");
        assert!(
            dashboard_api.starts_with("http://127.0.0.1:")
                || dashboard_api.starts_with("http://localhost:"),
            "the OpenHands Runtime E2E test only accepts a local Dashboard API"
        );
        let runtime_home = std::env::var_os("HIMIND_AGENT_HOME")
            .map(std::path::PathBuf::from)
            .expect("HIMIND_AGENT_HOME is required for the OpenHands Runtime E2E test");
        let temp_directory = std::fs::canonicalize(std::env::temp_dir())
            .expect("the system temp directory must be available");
        let runtime_parent = std::fs::canonicalize(
            runtime_home
                .parent()
                .expect("the OpenHands Runtime E2E home must have a parent"),
        )
        .expect("the OpenHands Runtime E2E home parent must exist");
        assert_eq!(
            runtime_parent, temp_directory,
            "the OpenHands Runtime E2E test home must be directly under the system temp directory"
        );
        assert!(
            runtime_home
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("himind-openhands-runtime-e2e-")),
            "the OpenHands Runtime E2E test home must use its dedicated prefix"
        );

        let mut options = Options::from_env();
        options.api_base = dashboard_api.trim_end_matches('/').to_string();
        let status = super::install(&options, "openhands-runtime-e2e")
            .expect("signed OpenHands Runtime installation should succeed");

        assert_eq!(status.status, "ready");
        assert_eq!(status.provider, RUNTIME_PROVIDER);
        assert!(status.cli_compatible);
        assert!(std::path::Path::new(&status.executable_path).is_file());
        assert!(status.uv_available);
        assert!(status.python_available);
    }
}
