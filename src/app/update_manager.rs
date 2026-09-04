use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

use crate::api::distribution::{self, DistributionState, UpdateCheckResponse};
use crate::Options;

const BACKGROUND_CHECK_MIN_SECONDS: u64 = 30 * 60;
const BACKGROUND_CHECK_JITTER_SECONDS: u64 = 30 * 60;
// The MCP companion is part of the Agent runtime contract. VSIX remains
// optional, but every directory package must carry the console companion so a
// release cannot silently regress stdio MCP clients.
const DIRECTORY_PACKAGE_FILES: [&str; 5] = [
    "himind-agent.exe",
    "himind-agent-mcp.exe",
    "himind-agent-updater.exe",
    "himind-agent-launcher.exe",
    "himind-ai.vsix",
];
const REQUIRED_DIRECTORY_PACKAGE_FILES: [&str; 4] = [
    "himind-agent.exe",
    "himind-agent-mcp.exe",
    "himind-agent-updater.exe",
    "himind-agent-launcher.exe",
];
const MAX_EXTRACTED_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

static UPDATE_OPERATION: OnceLock<Mutex<()>> = OnceLock::new();
static DOWNLOAD_CANCELED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentUpdateStatus {
    #[serde(default = "default_idle")]
    pub status: String,
    #[serde(default)]
    pub current_version: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default = "default_update_source")]
    pub source: String,
    #[serde(default)]
    pub available_version: String,
    #[serde(default)]
    pub release_id: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default = "default_package_type")]
    pub package_type: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_key_id: String,
    #[serde(default)]
    pub signature_algorithm: String,
    #[serde(default)]
    pub download_url: String,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub min_supported_version: String,
    #[serde(default)]
    pub release_notes: String,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub progress_percent: u8,
    #[serde(default)]
    pub staged_package_path: String,
    #[serde(default)]
    pub staged_agent_path: String,
    #[serde(default)]
    pub staged_mcp_path: String,
    #[serde(default)]
    pub staged_updater_path: String,
    #[serde(default)]
    pub staged_launcher_path: String,
    #[serde(default)]
    pub staged_vscode_extension_path: String,
    #[serde(default)]
    pub last_checked_at: u64,
    #[serde(default)]
    pub last_error: String,
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default = "default_true")]
    pub auto_download: bool,
}

impl Default for AgentUpdateStatus {
    fn default() -> Self {
        Self {
            status: default_idle(),
            current_version: crate::VERSION.to_string(),
            channel: default_channel(),
            source: default_update_source(),
            available_version: String::new(),
            release_id: String::new(),
            file_name: String::new(),
            package_type: default_package_type(),
            size_bytes: 0,
            sha256: String::new(),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: String::new(),
            mandatory: false,
            min_supported_version: String::new(),
            release_notes: String::new(),
            downloaded_bytes: 0,
            progress_percent: 0,
            staged_package_path: String::new(),
            staged_agent_path: String::new(),
            staged_mcp_path: String::new(),
            staged_updater_path: String::new(),
            staged_launcher_path: String::new(),
            staged_vscode_extension_path: String::new(),
            last_checked_at: 0,
            last_error: String::new(),
            auto_check: true,
            auto_download: true,
        }
    }
}

fn default_idle() -> String {
    "idle".to_string()
}

fn default_channel() -> String {
    std::env::var("HIMIND_DISTRIBUTION_CHANNEL").unwrap_or_else(|_| "stable".to_string())
}

fn default_update_source() -> String {
    "dashboard".to_string()
}

fn update_source_for_mode(options: &Options) -> &'static str {
    if options.mode().dashboard_enabled() {
        "dashboard"
    } else {
        "github"
    }
}

fn default_package_type() -> String {
    "directory-zip".to_string()
}

fn default_true() -> bool {
    true
}

pub(crate) fn status_path(agent_state_path: &Path) -> PathBuf {
    agent_state_path.with_file_name("agent-update-state.json")
}

fn read_status_file(path: &Path) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let bytes = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(bytes.as_slice());
    Ok(serde_json::from_slice::<AgentUpdateStatus>(bytes)?)
}

pub(crate) fn load(agent_state_path: &Path) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let path = status_path(agent_state_path);
    let mut status = if path.is_file() {
        read_status_file(&path)?
    } else {
        AgentUpdateStatus::default()
    };
    let mut changed = status.current_version != crate::VERSION;
    status.current_version = crate::VERSION.to_string();
    if status.channel.trim().is_empty() {
        status.channel = default_channel();
        changed = true;
    }
    if status.available_version == crate::VERSION
        && !status.available_version.is_empty()
        && status.status != "idle"
    {
        // The update may have completed before the previous status write. Do
        // not keep showing the just-installed version as an available update.
        clear_release(&mut status);
        status.status = "idle".to_string();
        status.last_error.clear();
        changed = true;
    }
    if status.status == "installing" && status.available_version == crate::VERSION {
        status.status = "idle".to_string();
        status.last_error.clear();
        clear_staged_paths(&mut status);
        status.downloaded_bytes = 0;
        status.progress_percent = 0;
        changed = true;
    }
    if status.status == "installing" && status.available_version != crate::VERSION {
        let target_is_newer =
            crate::skill::resolver::compare_versions(&status.available_version, crate::VERSION)
                == std::cmp::Ordering::Greater;
        if target_is_newer && staged_payload_available(&status) {
            status.status = "ready".to_string();
            status.last_error = "上一次更新未完成，更新包仍可继续安装".to_string();
        } else if target_is_newer {
            status.status = "available".to_string();
            status.downloaded_bytes = 0;
            status.progress_percent = 0;
            clear_staged_paths(&mut status);
            status.last_error = "上一次更新未完成，请重新下载更新包".to_string();
        } else {
            clear_release(&mut status);
            status.status = "idle".to_string();
            status.last_error.clear();
        }
        changed = true;
    }
    if status.status == "ready" && !staged_payload_available(&status) {
        status.status = "available".to_string();
        status.downloaded_bytes = 0;
        status.progress_percent = 0;
        clear_staged_paths(&mut status);
        status.last_error = "已下载的更新包不再可用，请重新下载".to_string();
        changed = true;
    }
    if changed {
        save(agent_state_path, &status)?;
    }
    Ok(status)
}

pub(crate) fn set_preferences(
    agent_state_path: &Path,
    auto_check: bool,
    auto_download: bool,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let mut status = load(agent_state_path)?;
    status.auto_check = auto_check;
    status.auto_download = auto_check && auto_download;
    save(agent_state_path, &status)?;
    Ok(status)
}

pub(crate) fn cancel_download(
    agent_state_path: &Path,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    DOWNLOAD_CANCELED.store(true, Ordering::Relaxed);
    load(agent_state_path)
}

pub(crate) fn check_now(options: &Options) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let distribution_state = if options.mode().dashboard_enabled() {
        Some(
            distribution::load_state(&distribution::distribution_state_path(&options.state_path))?
                .ok_or("软件更新服务尚未完成设备注册")?,
        )
    } else {
        None
    };
    check_with_state(&client, options, distribution_state.as_ref(), true)
}

pub(crate) fn background_check(
    client: &reqwest::blocking::Client,
    options: &Options,
    distribution_state: &DistributionState,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let current = load(&options.state_path)?;
    if !current.auto_check || !background_check_due(&current, options) {
        return Ok(current);
    }
    check_with_state(client, options, Some(distribution_state), false)
}

pub(crate) fn background_check_independent(
    client: &reqwest::blocking::Client,
    options: &Options,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let current = load(&options.state_path)?;
    if !current.auto_check || !background_check_due(&current, options) {
        return Ok(current);
    }
    check_with_state(client, options, None, false)
}

fn check_with_state(
    client: &reqwest::blocking::Client,
    options: &Options,
    distribution_state: Option<&DistributionState>,
    manual: bool,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let mut status = load(&options.state_path)?;
    status.source = update_source_for_mode(options).to_string();
    status.status = "checking".to_string();
    status.last_error.clear();
    save(&options.state_path, &status)?;

    let update = match if let Some(state) = distribution_state {
        distribution::check_update(client, &options.api_base, state)
    } else {
        crate::app::update_source::check_github(client, options)
    } {
        Ok(update) => update,
        Err(error) => {
            status.status = "failed".to_string();
            status.last_error = format!("检查更新失败：{error}");
            status.last_checked_at = unix_now();
            save(&options.state_path, &status)?;
            return Err(status.last_error.clone().into());
        }
    };

    let was_same_release = update.has_update
        && status.available_version == update.version
        && status.release_id == update.release_id;
    if let Err(error) = apply_update_check(&mut status, &update) {
        status.status = "failed".to_string();
        status.last_error = format!("更新信息校验失败：{error}");
        status.last_checked_at = unix_now();
        save(&options.state_path, &status)?;
        return Err(status.last_error.clone().into());
    }
    status.last_checked_at = unix_now();
    status.last_error.clear();
    save(&options.state_path, &status)?;

    if update.has_update && !was_same_release {
        if let Some(state) = distribution_state {
            let _ = distribution::report_update_result(
                client,
                &options.api_base,
                state,
                "update_available",
                crate::VERSION,
                &update.version,
                if manual {
                    "update discovered by manual check"
                } else {
                    "update discovered by automatic check"
                },
            );
        }
    }
    if update.has_update && status.auto_download && status.status != "ready" {
        return download_locked(options, distribution_state, status);
    }
    Ok(status)
}

fn apply_update_check(
    status: &mut AgentUpdateStatus,
    update: &UpdateCheckResponse,
) -> Result<(), Box<dyn Error>> {
    if !update.has_update {
        if status.status != "installing" {
            clear_release(status);
            status.status = "idle".to_string();
        }
        return Ok(());
    }
    if update.version.trim().is_empty()
        || crate::skill::resolver::compare_versions(&update.version, crate::VERSION)
            != std::cmp::Ordering::Greater
    {
        return Err("软件更新服务返回了无效的目标版本".into());
    }
    let package_type = update.package_type.trim();
    if package_type != "directory-zip" {
        return Err(format!("unsupported Agent update package type: {package_type}").into());
    }
    let keep_ready = status.status == "ready"
        && status.available_version == update.version
        && status.sha256.eq_ignore_ascii_case(&update.sha256)
        && status.package_type == package_type
        && staged_payload_available(status);
    status.available_version = update.version.clone();
    status.release_id = update.release_id.clone();
    status.file_name = update.file_name.clone();
    status.package_type = package_type.to_string();
    status.size_bytes = update.size_bytes.max(0) as u64;
    status.sha256 = update.sha256.clone();
    status.signature = update.signature.clone();
    status.signature_key_id = update.signature_key_id.clone();
    status.signature_algorithm = update.signature_algorithm.clone();
    status.download_url = update.download_url.clone();
    status.mandatory = update.mandatory;
    status.min_supported_version = update.min_supported_version.clone();
    status.release_notes = update.release_notes.clone();
    if !keep_ready {
        status.status = "available".to_string();
        status.downloaded_bytes = 0;
        status.progress_percent = 0;
        clear_staged_paths(status);
    }
    Ok(())
}

pub(crate) fn download(options: &Options) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let distribution_state = if options.mode().dashboard_enabled() {
        Some(
            distribution::load_state(&distribution::distribution_state_path(&options.state_path))?
                .ok_or("软件更新服务尚未完成设备注册")?,
        )
    } else {
        None
    };
    let status = load(&options.state_path)?;
    let expected_source = update_source_for_mode(options);
    if !status.available_version.trim().is_empty() && status.source != expected_source {
        return Err("当前更新信息来自其他更新源，请先重新检查更新".into());
    }
    download_locked(options, distribution_state.as_ref(), status)
}

fn download_locked(
    options: &Options,
    distribution_state: Option<&DistributionState>,
    mut status: AgentUpdateStatus,
) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    if status.available_version.trim().is_empty() || status.download_url.trim().is_empty() {
        return Err("当前没有可下载的软件更新".into());
    }
    crate::app::system::validate_update_download_url_for_source(
        &options.api_base,
        &status.download_url,
        &status.source,
    )?;
    DOWNLOAD_CANCELED.store(false, Ordering::Relaxed);
    status.status = "downloading".to_string();
    status.downloaded_bytes = 0;
    status.progress_percent = 0;
    status.last_error.clear();
    save(&options.state_path, &status)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    if let Some(state) = distribution_state {
        let _ = distribution::report_update_result(
            &client,
            &options.api_base,
            state,
            "download_started",
            crate::VERSION,
            &status.available_version,
            "",
        );
    }
    let result = download_to_staging(&client, options, distribution_state, &mut status);
    if let Err(error) = result {
        status.status = if DOWNLOAD_CANCELED.load(Ordering::Relaxed) {
            "available".to_string()
        } else {
            "failed".to_string()
        };
        status.last_error = error.to_string();
        save(&options.state_path, &status)?;
        if let Some(state) = distribution_state {
            let _ = distribution::report_update_result(
                &client,
                &options.api_base,
                state,
                if DOWNLOAD_CANCELED.load(Ordering::Relaxed) {
                    "download_canceled"
                } else {
                    "update_failed"
                },
                crate::VERSION,
                &status.available_version,
                &status.last_error,
            );
        }
        return Err(error);
    }
    status.status = "ready".to_string();
    status.downloaded_bytes = status.size_bytes.max(status.downloaded_bytes);
    status.progress_percent = 100;
    status.last_error.clear();
    save(&options.state_path, &status)?;
    if let Some(state) = distribution_state {
        let _ = distribution::report_update_result(
            &client,
            &options.api_base,
            state,
            "download_ready",
            crate::VERSION,
            &status.available_version,
            "artifact verified and ready to install",
        );
    }
    Ok(status)
}

fn download_to_staging(
    client: &reqwest::blocking::Client,
    options: &Options,
    distribution_state: Option<&DistributionState>,
    status: &mut AgentUpdateStatus,
) -> Result<(), Box<dyn Error>> {
    let mut request = client.get(&status.download_url);
    if let Some(state) = distribution_state {
        request = request.bearer_auth(&state.token);
    }
    let mut response = request.send()?;
    response.error_for_status_ref()?;
    let staging = staging_directory()?;
    fs::create_dir_all(&staging)?;
    let version = safe_version_segment(&status.available_version)?;
    if status.package_type != "directory-zip" {
        return Err(format!(
            "unsupported Agent update package type: {}",
            status.package_type
        )
        .into());
    }
    let final_path = staging.join(format!("himind-agent-{version}.zip"));
    let partial_path = final_path.with_extension("zip.part");
    let _ = fs::remove_file(&partial_path);
    let mut file = File::create(&partial_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut last_saved_percent = 0_u8;
    loop {
        if DOWNLOAD_CANCELED.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&partial_path);
            return Err("下载已取消".into());
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        status.downloaded_bytes = status.downloaded_bytes.saturating_add(read as u64);
        let percent = if status.size_bytes > 0 {
            ((status.downloaded_bytes.saturating_mul(100) / status.size_bytes).min(99)) as u8
        } else {
            0
        };
        status.progress_percent = percent;
        if percent >= last_saved_percent.saturating_add(2) {
            save(&options.state_path, status)?;
            last_saved_percent = percent;
        }
    }
    file.flush()?;
    if status.size_bytes > 0 && status.downloaded_bytes != status.size_bytes {
        let _ = fs::remove_file(&partial_path);
        return Err(format!(
            "更新包大小校验失败，期望 {} 字节，实际 {} 字节",
            status.size_bytes, status.downloaded_bytes
        )
        .into());
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if status.sha256.len() != 64 || !actual_sha256.eq_ignore_ascii_case(&status.sha256) {
        let _ = fs::remove_file(&partial_path);
        return Err("更新包 SHA-256 校验失败".into());
    }
    crate::app::system::verify_agent_package_signature(
        &partial_path,
        &status.signature,
        &status.signature_key_id,
        &status.signature_algorithm,
    )?;
    let _ = fs::remove_file(&final_path);
    fs::rename(&partial_path, &final_path)?;
    status.staged_package_path = final_path.to_string_lossy().to_string();
    prepare_staged_payload(status, &staging, &version)?;
    Ok(())
}

fn prepare_staged_payload(
    status: &mut AgentUpdateStatus,
    staging: &Path,
    version: &str,
) -> Result<(), Box<dyn Error>> {
    if status.package_type != "directory-zip" {
        return Err(format!(
            "unsupported Agent update package type: {}",
            status.package_type
        )
        .into());
    }

    let artifact = PathBuf::from(&status.staged_package_path);
    let package_dir = staging.join(format!("himind-agent-{version}"));
    extract_directory_package(&artifact, &package_dir)?;
    status.staged_agent_path = package_dir
        .join("himind-agent.exe")
        .to_string_lossy()
        .to_string();
    status.staged_mcp_path = package_dir
        .join("himind-agent-mcp.exe")
        .to_string_lossy()
        .to_string();
    status.staged_updater_path = package_dir
        .join("himind-agent-updater.exe")
        .to_string_lossy()
        .to_string();
    status.staged_launcher_path = package_dir
        .join("himind-agent-launcher.exe")
        .to_string_lossy()
        .to_string();
    let extension_path = package_dir.join("himind-ai.vsix");
    status.staged_vscode_extension_path = if extension_path.is_file() {
        extension_path.to_string_lossy().to_string()
    } else {
        String::new()
    };
    Ok(())
}

fn extract_directory_package(archive_path: &Path, target_dir: &Path) -> Result<(), Box<dyn Error>> {
    let target_parent = target_dir
        .parent()
        .ok_or("Agent update staging directory is invalid")?;
    fs::create_dir_all(target_parent)?;
    let temporary_dir = target_parent.join(format!(
        ".{}.extracting",
        target_dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("Agent update staging directory is invalid")?
    ));
    if temporary_dir.exists() {
        fs::remove_dir_all(&temporary_dir)?;
    }
    fs::create_dir(&temporary_dir)?;

    let result = (|| -> Result<(), Box<dyn Error>> {
        let mut archive = ZipArchive::new(File::open(archive_path)?)?;
        if archive.len() != REQUIRED_DIRECTORY_PACKAGE_FILES.len()
            && archive.len() != DIRECTORY_PACKAGE_FILES.len()
        {
            return Err("Agent directory update package must contain Agent, MCP companion and helper executables; himind-ai.vsix is optional".into());
        }
        let allowed = DIRECTORY_PACKAGE_FILES.into_iter().collect::<HashSet<_>>();
        let required = REQUIRED_DIRECTORY_PACKAGE_FILES
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut total_size = 0_u64;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir() || entry.name().contains('\\') {
                return Err(
                    "Agent directory update package contains a directory or invalid path".into(),
                );
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Agent directory update package contains a symbolic link".into());
            }
            let enclosed = entry
                .enclosed_name()
                .ok_or("Agent directory update package contains an unsafe path")?;
            if enclosed.components().count() != 1 {
                return Err("Agent directory update package files must be at the ZIP root".into());
            }
            let name = enclosed
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or("Agent directory update package contains a non-UTF-8 file name")?
                .to_string();
            if !allowed.contains(name.as_str()) || !seen.insert(name.clone()) {
                return Err(format!("Agent directory update package contains an unexpected or duplicate file: {name}").into());
            }
            let declared_size = entry.size();
            if declared_size == 0 || declared_size > MAX_EXTRACTED_FILE_BYTES {
                return Err(
                    format!("Agent directory update file has an invalid size: {name}").into(),
                );
            }
            total_size = total_size
                .checked_add(declared_size)
                .ok_or("Agent directory update package size overflow")?;
            if total_size > MAX_EXTRACTED_PACKAGE_BYTES {
                return Err(
                    "Agent directory update package expands beyond the allowed size".into(),
                );
            }
            let target = temporary_dir.join(&name);
            let mut output = File::options().write(true).create_new(true).open(target)?;
            let copied = std::io::copy(
                &mut entry.by_ref().take(MAX_EXTRACTED_FILE_BYTES + 1),
                &mut output,
            )?;
            output.flush()?;
            if copied != declared_size {
                return Err(format!(
                    "Agent directory update file size changed while extracting: {name}"
                )
                .into());
            }
        }
        if !required.is_subset(&seen) {
            return Err("Agent directory update package is missing a required executable".into());
        }
        if target_dir.exists() {
            fs::remove_dir_all(target_dir)?;
        }
        fs::rename(&temporary_dir, target_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary_dir);
    }
    result
}

fn staged_payload_available(status: &AgentUpdateStatus) -> bool {
    status.package_type == "directory-zip"
        && Path::new(&status.staged_package_path).is_file()
        && Path::new(&status.staged_agent_path).is_file()
        && Path::new(&status.staged_mcp_path).is_file()
        && Path::new(&status.staged_updater_path).is_file()
        && Path::new(&status.staged_launcher_path).is_file()
        && (status.staged_vscode_extension_path.is_empty()
            || Path::new(&status.staged_vscode_extension_path).is_file())
}

fn clear_staged_paths(status: &mut AgentUpdateStatus) {
    status.staged_package_path.clear();
    status.staged_agent_path.clear();
    status.staged_mcp_path.clear();
    status.staged_updater_path.clear();
    status.staged_launcher_path.clear();
    status.staged_vscode_extension_path.clear();
}

pub(crate) fn install(options: &Options) -> Result<AgentUpdateStatus, Box<dyn Error>> {
    let _guard = operation_lock()?;
    let mut status = load(&options.state_path)?;
    if status.status != "ready" || !staged_payload_available(&status) {
        return Err("更新包尚未下载就绪".into());
    }
    let staged_package = PathBuf::from(&status.staged_package_path);
    verify_staged_sha256(&staged_package, &status.sha256)?;
    crate::app::system::verify_agent_package_signature(
        &staged_package,
        &status.signature,
        &status.signature_key_id,
        &status.signature_algorithm,
    )?;
    let staging = staging_directory()?;
    let version = safe_version_segment(&status.available_version)?;
    prepare_staged_payload(&mut status, &staging, &version)?;
    let current_executable = std::env::current_exe()?;
    status.status = "installing".to_string();
    status.last_error.clear();
    save(&options.state_path, &status)?;
    // DSH runs as a child process and may reconnect its MCP bridge while the
    // updater replaces the Agent executable. Stop it before the Agent exits
    // so it cannot survive as an orphan and keep the current binary locked.
    crate::app::ui::stop_builtin_ai_process();
    if let Err(error) = crate::app::system::schedule_agent_replace_and_restart(
        Path::new(&status.staged_agent_path),
        Path::new(&status.staged_mcp_path),
        &staged_package,
        Path::new(&status.staged_updater_path),
        Path::new(&status.staged_launcher_path),
        (!status.staged_vscode_extension_path.is_empty())
            .then(|| Path::new(&status.staged_vscode_extension_path)),
        &current_executable,
        options,
        &status.available_version,
    ) {
        status.status = "failed".to_string();
        status.last_error = format!("启动更新程序失败：{error}");
        save(&options.state_path, &status)?;
        return Err(error);
    }
    thread::spawn(|| {
        // Legacy updaters ignore wait_pid. Exit before their built-in 300 ms
        // handoff delay elapses so they can start the new Agent without using
        // the old taskkill /T path.
        thread::sleep(Duration::from_millis(100));
        std::process::exit(0);
    });
    Ok(status)
}

fn verify_staged_sha256(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("更新包缺少合法的 SHA-256 摘要".into());
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("已暂存的更新包 SHA-256 校验失败，请重新下载".into());
    }
    Ok(())
}

fn staging_directory() -> Result<PathBuf, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let installation_root = crate::install_layout::installation_root_from_executable(&executable);
    Ok(installation_root.join("staging"))
}

fn safe_version_segment(version: &str) -> Result<String, Box<dyn Error>> {
    let normalized = version.trim();
    if normalized.is_empty()
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("更新版本号不适合用于本地暂存文件".into());
    }
    Ok(normalized.to_string())
}

fn background_check_due(status: &AgentUpdateStatus, options: &Options) -> bool {
    if status.last_checked_at == 0 {
        return true;
    }
    let seed = options
        .state_path
        .to_string_lossy()
        .bytes()
        .fold(0_u64, |value, byte| {
            value.wrapping_mul(31).wrapping_add(byte as u64)
        });
    let interval = BACKGROUND_CHECK_MIN_SECONDS + seed % BACKGROUND_CHECK_JITTER_SECONDS;
    unix_now().saturating_sub(status.last_checked_at) >= interval
}

fn clear_release(status: &mut AgentUpdateStatus) {
    status.available_version.clear();
    status.release_id.clear();
    status.file_name.clear();
    status.package_type = default_package_type();
    status.size_bytes = 0;
    status.sha256.clear();
    status.signature.clear();
    status.signature_key_id.clear();
    status.signature_algorithm.clear();
    status.download_url.clear();
    status.mandatory = false;
    status.min_supported_version.clear();
    status.release_notes.clear();
    status.downloaded_bytes = 0;
    status.progress_percent = 0;
    clear_staged_paths(status);
}

fn operation_lock() -> Result<std::sync::MutexGuard<'static, ()>, Box<dyn Error>> {
    UPDATE_OPERATION
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "软件更新操作锁不可用".into())
}

fn save(agent_state_path: &Path, status: &AgentUpdateStatus) -> Result<(), Box<dyn Error>> {
    let path = status_path(agent_state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(status)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        apply_update_check, extract_directory_package, load, safe_version_segment, save,
        status_path, AgentUpdateStatus, DIRECTORY_PACKAGE_FILES,
    };
    use crate::api::distribution::UpdateCheckResponse;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::FileOptions;

    #[test]
    fn load_persists_the_running_agent_version() {
        let root = temporary_test_root("persist-version");
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let mut status = AgentUpdateStatus::default();
        status.current_version = "0.0.0".to_string();
        save(&state_path, &status).unwrap();

        let loaded = load(&state_path).unwrap();
        let persisted = serde_json::from_slice::<AgentUpdateStatus>(
            &fs::read(status_path(&state_path)).unwrap(),
        )
        .unwrap();

        assert_eq!(loaded.current_version, crate::VERSION);
        assert_eq!(persisted.current_version, crate::VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_accepts_utf8_bom_and_clears_completed_release() {
        let root = temporary_test_root("utf8-bom");
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let mut status = AgentUpdateStatus::default();
        status.status = "installing".to_string();
        status.current_version = "0.3.25".to_string();
        status.available_version = crate::VERSION.to_string();
        status.release_id = "completed-release".to_string();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(serde_json::to_vec_pretty(&status).unwrap());
        fs::write(status_path(&state_path), bytes).unwrap();

        let loaded = load(&state_path).unwrap();
        let persisted = fs::read(status_path(&state_path)).unwrap();

        assert_eq!(loaded.status, "idle");
        assert_eq!(loaded.current_version, crate::VERSION);
        assert!(loaded.available_version.is_empty());
        assert!(!persisted.starts_with(&[0xEF, 0xBB, 0xBF]));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_recovers_a_stale_installing_state() {
        let root = temporary_test_root("stale-install");
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let mut status = AgentUpdateStatus::default();
        status.status = "installing".to_string();
        status.current_version = "0.3.12".to_string();
        status.available_version = "9.8.7".to_string();
        save(&state_path, &status).unwrap();

        let loaded = load(&state_path).unwrap();

        assert_eq!(loaded.status, "available");
        assert_eq!(loaded.current_version, crate::VERSION);
        assert!(loaded.last_error.contains("重新下载"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_clears_an_installing_state_for_an_older_version() {
        let root = temporary_test_root("obsolete-install");
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let mut status = AgentUpdateStatus::default();
        status.status = "installing".to_string();
        status.available_version = "0.0.1".to_string();
        status.release_id = "old-release".to_string();
        save(&state_path, &status).unwrap();

        let loaded = load(&state_path).unwrap();

        assert_eq!(loaded.status, "idle");
        assert!(loaded.available_version.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn applies_directory_distribution_manifest() {
        let mut status = AgentUpdateStatus::default();
        let update = UpdateCheckResponse {
            has_update: true,
            version: "9.8.7".to_string(),
            release_id: "release-1".to_string(),
            file_name: "himind-agent-update.zip".to_string(),
            package_type: "directory-zip".to_string(),
            size_bytes: 42,
            sha256: "a".repeat(64),
            signature: "signature".to_string(),
            signature_key_id: "key-1".to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
            download_url: "https://example.test/download".to_string(),
            mandatory: true,
            min_supported_version: "1.0.0".to_string(),
            release_notes: "notes".to_string(),
        };
        apply_update_check(&mut status, &update).unwrap();
        assert_eq!(status.status, "available");
        assert_eq!(status.available_version, "9.8.7");
        assert_eq!(status.size_bytes, 42);
        assert!(status.mandatory);
    }

    #[test]
    fn rejects_single_file_distribution_manifest() {
        let mut status = AgentUpdateStatus::default();
        let update = UpdateCheckResponse {
            has_update: true,
            version: "9.8.7".to_string(),
            release_id: "release-1".to_string(),
            file_name: "himind-agent.exe".to_string(),
            package_type: "content".to_string(),
            size_bytes: 42,
            sha256: "a".repeat(64),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: "https://example.test/download".to_string(),
            mandatory: false,
            min_supported_version: String::new(),
            release_notes: String::new(),
        };
        assert!(apply_update_check(&mut status, &update).is_err());
    }

    #[test]
    fn rejects_unsafe_staging_version() {
        assert!(safe_version_segment("0.4.0-beta.1").is_ok());
        assert!(safe_version_segment("../0.4.0").is_err());
    }

    #[test]
    fn extracts_strict_agent_directory_package() {
        let root = temporary_test_root("extract");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("agent.zip");
        write_test_archive(&archive_path, &DIRECTORY_PACKAGE_FILES).unwrap();
        let extracted = root.join("payload");

        extract_directory_package(&archive_path, &extracted).unwrap();

        for name in DIRECTORY_PACKAGE_FILES {
            assert_eq!(fs::read(extracted.join(name)).unwrap(), name.as_bytes());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_agent_directory_package_without_mcp_companion() {
        let root = temporary_test_root("legacy-extract");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("agent.zip");
        let entries = [
            "himind-agent.exe",
            "himind-agent-updater.exe",
            "himind-agent-launcher.exe",
        ];
        write_test_archive(&archive_path, &entries).unwrap();
        let extracted = root.join("payload");

        assert!(extract_directory_package(&archive_path, &extracted).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_directory_package_with_extra_or_nested_files() {
        for (label, entries) in [
            (
                "extra",
                vec![
                    "himind-agent.exe",
                    "himind-agent-updater.exe",
                    "himind-agent-launcher.exe",
                    "notes.txt",
                ],
            ),
            (
                "nested",
                vec![
                    "current/himind-agent.exe",
                    "himind-agent-updater.exe",
                    "himind-agent-launcher.exe",
                ],
            ),
            (
                "traversal",
                vec![
                    "../himind-agent.exe",
                    "himind-agent-updater.exe",
                    "himind-agent-launcher.exe",
                ],
            ),
            (
                "duplicate",
                vec![
                    "himind-agent.exe",
                    "himind-agent-updater.exe",
                    "himind-agent-updater.exe",
                ],
            ),
        ] {
            let root = temporary_test_root(label);
            fs::create_dir_all(&root).unwrap();
            let archive_path = root.join("agent.zip");
            write_test_archive(&archive_path, &entries).unwrap();

            assert!(extract_directory_package(&archive_path, &root.join("payload")).is_err());
            let _ = fs::remove_dir_all(root);
        }
    }

    fn write_test_archive(
        path: &std::path::Path,
        entries: &[&str],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut archive = zip::ZipWriter::new(File::create(path)?);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for name in entries {
            archive.start_file(*name, options)?;
            archive.write_all(name.as_bytes())?;
        }
        archive.finish()?;
        Ok(())
    }

    fn temporary_test_root(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("himind-agent-update-{label}-{unique}"))
    }
}
