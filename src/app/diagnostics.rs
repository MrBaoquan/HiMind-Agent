use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::FileOptions;

use crate::api::client::load_agent_state;
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

/// Run checks that do not require the WebView or a running Agent window.
///
/// The command is intentionally machine-readable so support can ask users to
/// attach one small JSON file instead of transcribing a message box. Exit code
/// 0 means no failed check, 2 means warnings, and 1 means an installation
/// check failed.
pub(crate) fn check_installation(options: &Options) -> Result<(Value, i32), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let root = crate::install_layout::installation_root_from_executable(&executable);
    let mut checks = Vec::new();
    let mut failures = 0_u32;
    let mut warnings = 0_u32;

    let installed = crate::install_layout::executable_is_installed(&executable);
    push_check(
        &mut checks,
        &mut failures,
        "installed_layout",
        installed,
        if installed {
            "安装目录结构完整"
        } else {
            "当前可执行文件不在完整安装布局中"
        },
        "error",
    );

    for (name, path) in [
        ("launcher", root.join(crate::install_layout::LAUNCHER_FILE)),
        ("updater", root.join(crate::install_layout::UPDATER_FILE)),
        ("icon", root.join("himind-agent.ico")),
        (
            "active_version",
            root.join(crate::install_layout::ACTIVE_VERSION_FILE),
        ),
    ] {
        let present = path.is_file();
        push_check(
            &mut checks,
            &mut failures,
            name,
            present,
            if present {
                "文件存在"
            } else {
                "文件缺失"
            },
            "error",
        );
    }

    let icon_path = root.join("himind-agent.ico");
    match inspect_ico(&icon_path) {
        Ok(report) => {
            if !report.valid {
                failures += 1;
            }
            checks.push(json!({
                "name": "icon_format",
                "ok": report.valid,
                "severity": if report.valid { "info" } else { "error" },
                "message": report.message,
                "sizes": report.sizes,
            }));
        }
        Err(error) => {
            failures += 1;
            checks.push(json!({ "name": "icon_format", "ok": false, "severity": "error", "message": error.to_string() }));
        }
    }

    let webview_dir = crate::store::paths::webview_user_data_dir(options.local_port);
    match probe_writable_directory(&webview_dir) {
        Ok(()) => checks.push(json!({ "name": "webview_data_dir", "ok": true, "severity": "info", "message": "WebView2 用户数据目录可写", "path": webview_dir })),
        Err(error) => {
            failures += 1;
            checks.push(json!({ "name": "webview_data_dir", "ok": false, "severity": "error", "message": error.to_string(), "path": webview_dir }));
        }
    }

    let runtime = runtime_snapshot();
    let runtime_compatible = runtime
        .get("compatible_with_agent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !runtime_compatible {
        warnings += 1;
    }
    checks.push(json!({
        "name": "runtime",
        "ok": runtime_compatible,
        "severity": if runtime_compatible { "info" } else { "warning" },
        "message": if runtime_compatible { "内置运行时可用" } else { "内置运行时不可用或尚未完成首次自愈" },
        "status": runtime,
    }));

    let shortcut = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|home| home.join("Desktop").join("HiMind Agent.lnk"));
    if let Some(path) = shortcut {
        let exists = path.is_file();
        if !exists {
            warnings += 1;
        }
        checks.push(json!({ "name": "desktop_shortcut", "ok": exists, "severity": if exists { "info" } else { "warning" }, "message": if exists { "桌面快捷方式存在" } else { "未找到桌面快捷方式（安装时可能未勾选）" }, "path": path }));
    }

    let result = json!({
        "schema_version": 1,
        "generated_at": unix_now(),
        "agent_version": VERSION,
        "executable": executable,
        "installation_root": root,
        "profile": crate::store::paths::profile_name(),
        "local_port": options.local_port,
        "checks": checks,
        "summary": { "failures": failures, "warnings": warnings },
    });
    let exit_code = if failures > 0 {
        1
    } else if warnings > 0 {
        2
    } else {
        0
    };
    Ok((result, exit_code))
}

fn push_check(
    checks: &mut Vec<Value>,
    failures: &mut u32,
    name: &str,
    ok: bool,
    message: &str,
    severity: &str,
) {
    if !ok && severity == "error" {
        *failures += 1;
    }
    checks.push(json!({ "name": name, "ok": ok, "severity": severity, "message": message }));
}

struct IcoReport {
    valid: bool,
    message: String,
    sizes: Vec<u16>,
}

fn inspect_ico(path: &Path) -> Result<IcoReport, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 6 || bytes[0..4] != [0, 0, 1, 0] {
        return Ok(IcoReport {
            valid: false,
            message: "ICO 头无效".into(),
            sizes: Vec::new(),
        });
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut sizes = Vec::new();
    for index in 0..count {
        let offset = 6 + index * 16;
        if offset + 16 > bytes.len() {
            return Ok(IcoReport {
                valid: false,
                message: "ICO 目录被截断".into(),
                sizes,
            });
        }
        let width = if bytes[offset] == 0 {
            256
        } else {
            bytes[offset] as u16
        };
        let height = if bytes[offset + 1] == 0 {
            256
        } else {
            bytes[offset + 1] as u16
        };
        let size = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into()?) as usize;
        let data_offset = u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into()?) as usize;
        let out_of_bounds = data_offset
            .checked_add(size)
            .map(|end| end > bytes.len())
            .unwrap_or(true);
        if width != height || out_of_bounds {
            return Ok(IcoReport {
                valid: false,
                message: "ICO 图像条目无效".into(),
                sizes,
            });
        }
        sizes.push(width);
    }
    let required = [16, 24, 32, 48, 64, 256];
    let valid = required.iter().all(|size| sizes.contains(size));
    Ok(IcoReport {
        valid,
        message: if valid {
            "ICO 包含完整的桌面/快捷方式尺寸".into()
        } else {
            "ICO 缺少 Windows 快捷方式所需的多尺寸图像".into()
        },
        sizes,
    })
}

fn probe_writable_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(path)?;
    let probe = path.join(format!(".doctor-{}.tmp", std::process::id()));
    fs::write(&probe, b"himind")?;
    fs::remove_file(probe)?;
    Ok(())
}

pub(crate) fn export_bundle(
    destination: &Path,
    options: &Options,
    worker: &LocalWorkerStatus,
) -> Result<PathBuf, Box<dyn Error>> {
    export_bundle_from(destination, options, worker, "ui")
}

/// `origin` records how the bundle was collected. A bundle pulled from the
/// command line while the window is blank must be distinguishable from one the
/// user exported from a working UI, because the worker fields below are only
/// meaningful in the first case.
pub(crate) fn export_bundle_from(
    destination: &Path,
    options: &Options,
    worker: &LocalWorkerStatus,
    origin: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(destination)?;
    let mut archive = zip::ZipWriter::new(file);
    let zip_options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    archive.start_file("diagnostics.json", zip_options)?;
    archive.write_all(
        serde_json::to_vec_pretty(&snapshot(options, worker, origin))?.as_slice(),
    )?;
    add_logs(
        &mut archive,
        &crate::store::paths::agent_home().join("logs"),
        &crate::app::crash::crash_dir(),
        zip_options,
    )?;

    archive.start_file("feedback.txt", zip_options)?;
    archive.write_all(
        "请补充以下信息后提交诊断包：\r\n\
         1. 问题发生的大致时间\r\n\
         2. 当时执行的操作和展项名称\r\n\
         3. 是否安装加密或终端安全软件，以及产品名称\r\n\
         4. 问题是否可稳定复现\r\n\
         诊断包不会收集密码、Agent credential 或 OAuth token。\r\n"
            .as_bytes(),
    )?;
    archive.finish()?;
    Ok(destination.to_path_buf())
}

/// Everything a support case needs has to be in here, because the user should
/// not have to be walked through copying files by hand. `runtime-install.log`
/// carries the reason the Runtime import failed (the installer already reads its
/// last line for its failure dialog) and the crash directory is where a panic or
/// an abort leaves its report, so both belong in the bundle.
fn add_logs<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    log_dir: &Path,
    crash_dir: &Path,
    options: FileOptions,
) -> Result<(), Box<dyn Error>> {
    for name in [
        "agent-events.jsonl",
        "agent-events.jsonl.1",
        "agent-events.jsonl.2",
        "runtime-install.log",
        "agent-updater.log",
        "svn-events.jsonl",
        "svn-events.jsonl.1",
        "svn-events.jsonl.2",
    ] {
        add_file(archive, &log_dir.join(name), &format!("logs/{name}"), options)?;
    }
    if let Ok(entries) = fs::read_dir(crash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            add_file(archive, &path, &format!("logs/crashes/{name}"), options)?;
        }
    }
    Ok(())
}

fn add_file<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    source: &Path,
    entry: &str,
    options: FileOptions,
) -> Result<(), Box<dyn Error>> {
    if !source.is_file() {
        return Ok(());
    }
    archive.start_file(entry, options)?;
    let mut file = fs::File::open(source)?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)?;
    archive.write_all(&content)?;
    Ok(())
}

fn snapshot(options: &Options, worker: &LocalWorkerStatus, origin: &str) -> Value {
    let state = match load_agent_state(&options.state_path) {
        Ok(state) => json!({
            "readable": true,
            "agent_id": state.agent_id,
            "device_id": state.device_id,
            "credential_present": !state.credential.is_empty(),
            "credential_pending": !state.credential_pending.is_empty(),
            "credential_updated_at": state.credential_updated_at,
        }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let authorization = match crate::api::oauth::authorization_snapshot(&options.state_path) {
        Ok(Some(value)) => json!({
            "readable": true,
            "present": true,
            "agent_id": value.agent_id,
            "user_id": value.user_id,
            "scope": value.scope,
            "refresh_expires_at": value.refresh_expires_at,
            "updated_at": value.updated_at,
            "last_verified_at": value.last_verified_at,
        }),
        Ok(None) => json!({ "readable": true, "present": false }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let trust = match crate::app::trust::status() {
        Ok(report) => json!({
            "readable": true,
            "healthy": report.healthy,
            "issues": report.issues,
            "local_key_ids": report.local_key_ids,
            "active_key_id": report.state.active_key_id,
            "key_ids": report.state.key_ids,
            "revoked_key_ids": report.state.revoked_key_ids,
            "bundle_digest": report.state.bundle_digest,
            "generated_at": report.state.generated_at,
            "synced_at": report.state.synced_at,
        }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let state_file = &options.state_path;
    json!({
        "generated_at": unix_now(),
        "collected_by": origin,
        "agent": {
            "version": VERSION,
            "profile": crate::store::paths::profile_name(),
            "api_base": sanitized_api_base(&options.api_base()),
            "local_port": options.local_port,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "native_dpapi": cfg!(windows),
        },
        // The Runtime is the other half of the install and the usual suspect
        // when a session will not start, so report what the Agent currently
        // believes about it rather than only the Agent's own identity.
        "runtime": runtime_snapshot(),
        "webview": {
            "user_data_dir": webview_dir(options.local_port),
        },
        "worker": {
            "online": worker.dashboard_worker_online,
            "agent_id": worker.dashboard_agent_id,
            "error": crate::approval::manager::redact_message(&worker.dashboard_worker_error),
            "local_service_online": worker.local_service_online,
            "local_service_error": crate::approval::manager::redact_message(&worker.local_service_error),
        },
        "storage": {
            "state_file_exists": state_file.is_file(),
            "state_backup_exists": crate::store::atomic_file::backup_path(state_file).is_file(),
            "authorization_file_exists":
                crate::api::oauth::authorization_path(state_file).is_file(),
        },
        "device_identity": state,
        "user_authorization": authorization,
        "distribution_trust": trust,
    })
}

/// What the Agent currently believes about the Runtime.
///
/// `status()` shells out to the Runtime CLI, which is exactly the thing that
/// usually fails on a broken machine, so it is captured best-effort: a failure
/// is recorded as the reason rather than aborting the whole bundle.
fn runtime_snapshot() -> Value {
    match serde_json::to_value(crate::runtime::builtin::status()) {
        Ok(value) => value,
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    }
}

/// The WebView2 user data directory this process would use.
///
/// A second Agent instance pointing at the same directory makes WebView2
/// creation fail with no error anywhere, which shows up as a tray icon with no
/// main window. Recording the path is what distinguishes that from a rendering
/// failure.
fn webview_dir(port: u16) -> String {
    crate::store::paths::webview_user_data_dir(port)
        .to_string_lossy()
        .to_string()
}

fn sanitized_api_base(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::export_bundle_from;
    use crate::api::client::save_agent_state;
    use crate::api::types::AgentState;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use std::fs;
    use std::io::Read;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, RwLock};

    #[test]
    fn bundle_contains_health_data_without_credentials() {
        let root = std::env::temp_dir().join(format!(
            "himind-diagnostics-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let credential = "device-credential-that-must-not-leak";
        save_agent_state(
            &state_path,
            &AgentState {
                agent_id: "agt-test".to_string(),
                credential: credential.to_string(),
                credential_protected: String::new(),
                credential_pending: String::new(),
                credential_pending_protected: String::new(),
                credential_updated_at: super::unix_now(),
                device_id: "device-test".to_string(),
                access_token: String::new(),
                access_token_expires_in: 0,
                access_scope: String::new(),
                user_id: String::new(),
            },
        )
        .unwrap();
        let options = Options {
            api_base: crate::api_base_cell("http://user:password@127.0.0.1:18083?token=secret"),
            state_path,
            workbench_mode: crate::app::runtime_mode::mode_cell(
                crate::app::runtime_mode::AgentMode::Connected,
            ),
            once: false,
            interval_seconds: 10,
            local_app: true,
            local_port: 18082,
            reenroll: false,
            enrollment_token: String::new(),
            agent_credential: Arc::new(RwLock::new(String::new())),
            identity_generation: Arc::new(AtomicU64::new(0)),
            platform_access: Arc::new(RwLock::new(None)),
            task_execution: Arc::new(RwLock::new(None)),
        };
        let destination = root.join("diagnostics.zip");
        export_bundle_from(
            &destination,
            &options,
            &LocalWorkerStatus::default(),
            "cli",
        )
        .unwrap();
        let file = fs::File::open(&destination).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut diagnostics = String::new();
        archive
            .by_name("diagnostics.json")
            .unwrap()
            .read_to_string(&mut diagnostics)
            .unwrap();
        assert!(diagnostics.contains("agt-test"));
        assert!(diagnostics.contains("distribution_trust"));
        assert!(!diagnostics.contains(credential));
        assert!(!diagnostics.contains("password"));
        assert!(!diagnostics.contains("token=secret"));
        // A bundle pulled from the command line must say so: it was collected
        // because the window would not come up, so its worker fields are
        // placeholders rather than observations, and the runtime and WebView2
        // facts are what the reader should go on.
        assert!(diagnostics.contains("\"collected_by\": \"cli\""));
        assert!(diagnostics.contains("\"runtime\""));
        assert!(diagnostics.contains("user_data_dir"));
        let _ = fs::remove_dir_all(root);
    }

    /// The bundle is what a support case is opened with, so it has to carry the
    /// logs that explain a failed install — a user should never be walked
    /// through copying files by hand.
    #[test]
    fn bundle_carries_install_logs_and_crash_reports() {
        let root = std::env::temp_dir().join(format!(
            "himind-diagnostics-logs-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        let logs = root.join("logs");
        let crashes = root.join("crashes");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&crashes).unwrap();
        fs::write(logs.join("runtime-install.log"), "error: 运行时安装失败").unwrap();
        fs::write(logs.join("agent-updater.log"), "updater line").unwrap();
        fs::write(logs.join("agent-events.jsonl"), "{\"level\":\"info\"}\n").unwrap();
        fs::write(logs.join("svn-events.jsonl"), "{}\n").unwrap();
        fs::write(crashes.join("crash-1.txt"), "panic report").unwrap();

        let destination = root.join("bundle.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&destination).unwrap());
        super::add_logs(
            &mut writer,
            &logs,
            &crashes,
            super::FileOptions::default(),
        )
        .unwrap();
        writer.finish().unwrap();

        let mut archive = zip::ZipArchive::new(fs::File::open(&destination).unwrap()).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect::<Vec<_>>();
        for expected in [
            "logs/runtime-install.log",
            "logs/agent-updater.log",
            "logs/agent-events.jsonl",
            "logs/svn-events.jsonl",
            "logs/crashes/crash-1.txt",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "bundle is missing {expected}: {names:?}"
            );
        }

        // A log that does not exist must not break the bundle: a fresh install
        // has no crash reports and no updater log yet.
        let mut sparse = zip::ZipWriter::new(fs::File::create(root.join("sparse.zip")).unwrap());
        assert!(super::add_logs(
            &mut sparse,
            &root.join("nowhere"),
            &root.join("nowhere-crashes"),
            super::FileOptions::default(),
        )
        .is_ok());

        let _ = fs::remove_dir_all(root);
    }
}
