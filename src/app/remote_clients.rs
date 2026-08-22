use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const CONFIGURED_BY_MANUAL: &str = "manual";
const CONFIGURED_BY_AUTO: &str = "auto";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct RemoteClientPathConfig {
    pub path: String,
    pub configured_by: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct RemoteClientSettings {
    pub sunlogin: RemoteClientPathConfig,
    pub todesk: RemoteClientPathConfig,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRemoteClient {
    pub path: PathBuf,
    pub source: String,
    pub auto_configured: bool,
}

static SETTINGS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn settings_lock() -> &'static Mutex<()> {
    SETTINGS_LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn settings_path(agent_state_path: &Path) -> PathBuf {
    agent_state_path.with_file_name("agent-remote-clients.json")
}

pub(crate) fn process_names(vendor: &str) -> Result<Vec<String>, Box<dyn Error>> {
    match vendor_key(vendor)? {
        "todesk" => Ok(vec!["ToDesk".to_string(), "ToDeskService".to_string()]),
        "sunlogin" => Ok(vec![
            "AweSun".to_string(),
            "SunloginClient".to_string(),
            "SunloginClientService".to_string(),
        ]),
        _ => unreachable!(),
    }
}

pub(crate) fn discovery_candidates(vendor: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let vendor = vendor_key(vendor)?;
    let mut candidates = registry_candidates(vendor);
    candidates.extend(start_menu_candidates(vendor));
    candidates.extend(standard_candidates(vendor));
    candidates.extend(
        relative_executable_paths(vendor)
            .into_iter()
            .map(|path| PathBuf::from(path)),
    );
    Ok(dedupe_paths(candidates)
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect())
}

pub(crate) fn resolve(
    vendor: &str,
    agent_state_path: &Path,
) -> Result<Option<ResolvedRemoteClient>, Box<dyn Error>> {
    let _guard = settings_lock()
        .lock()
        .map_err(|_| "远控客户端配置锁不可用")?;
    let mut settings = load_unlocked(agent_state_path)?;
    resolve_unlocked(vendor, agent_state_path, &mut settings)
}

pub(crate) fn overview(agent_state_path: &Path) -> Result<Value, Box<dyn Error>> {
    let _guard = settings_lock()
        .lock()
        .map_err(|_| "远控客户端配置锁不可用")?;
    let mut settings = load_unlocked(agent_state_path)?;
    let mut items = Vec::new();
    for vendor in ["sunlogin", "todesk"] {
        let before = config_for(&settings, vendor)?.clone();
        let resolved = resolve_unlocked(vendor, agent_state_path, &mut settings)?;
        let configured = config_for(&settings, vendor)?.clone();
        let configured_valid = configured_path(vendor, &configured).is_some();
        items.push(json!({
            "vendor": vendor,
            "name": vendor_label(vendor),
            "available": resolved.is_some(),
            "configured_path": configured.path,
            "configured_by": configured.configured_by,
            "configured_valid": configured_valid,
            "resolved_path": resolved.as_ref().map(|item| item.path.to_string_lossy().to_string()),
            "source": resolved.as_ref().map(|item| item.source.as_str()).unwrap_or("missing"),
            "auto_configured": resolved.as_ref().is_some_and(|item| item.auto_configured)
                || (before.path.is_empty() && !configured.path.is_empty()),
        }));
    }
    Ok(json!({
        "items": items,
        "settings_file": settings_path(agent_state_path).to_string_lossy(),
    }))
}

pub(crate) fn configure(
    vendor: &str,
    path: &str,
    agent_state_path: &Path,
) -> Result<Value, Box<dyn Error>> {
    let vendor = vendor_key(vendor)?;
    let _guard = settings_lock()
        .lock()
        .map_err(|_| "远控客户端配置锁不可用")?;
    let mut settings = load_unlocked(agent_state_path)?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        *config_for_mut(&mut settings, vendor)? = RemoteClientPathConfig::default();
    } else {
        let executable = PathBuf::from(trimmed);
        if !valid_executable(vendor, &executable) {
            return Err(format!(
                "{} 客户端路径无效，请选择正确的 Windows 可执行文件",
                vendor_label(vendor)
            )
            .into());
        }
        *config_for_mut(&mut settings, vendor)? = RemoteClientPathConfig {
            path: executable.to_string_lossy().to_string(),
            configured_by: CONFIGURED_BY_MANUAL.to_string(),
        };
    }
    save_unlocked(agent_state_path, &settings)?;
    drop(_guard);
    overview(agent_state_path)
}

fn resolve_unlocked(
    vendor: &str,
    agent_state_path: &Path,
    settings: &mut RemoteClientSettings,
) -> Result<Option<ResolvedRemoteClient>, Box<dyn Error>> {
    let vendor = vendor_key(vendor)?;
    let configured = config_for(settings, vendor)?.clone();
    if let Some(path) = configured_path(vendor, &configured) {
        let preferred = prefer_sunlogin_launcher(&path);
        if vendor == "sunlogin" && preferred != path {
            let mutable = configured.configured_by != CONFIGURED_BY_MANUAL;
            if mutable {
                *config_for_mut(settings, vendor)? = RemoteClientPathConfig {
                    path: preferred.to_string_lossy().to_string(),
                    configured_by: CONFIGURED_BY_AUTO.to_string(),
                };
                save_unlocked(agent_state_path, settings)?;
            }
            return Ok(Some(ResolvedRemoteClient {
                path: preferred,
                source: "configured".to_string(),
                auto_configured: mutable,
            }));
        }
        return Ok(Some(ResolvedRemoteClient {
            path,
            source: if configured.configured_by == CONFIGURED_BY_MANUAL {
                CONFIGURED_BY_MANUAL.to_string()
            } else {
                "configured".to_string()
            },
            auto_configured: false,
        }));
    }

    persist_discovered_unlocked(vendor, agent_state_path, settings, discover(vendor))
}

fn persist_discovered_unlocked(
    vendor: &str,
    agent_state_path: &Path,
    settings: &mut RemoteClientSettings,
    discovered: Option<(PathBuf, String)>,
) -> Result<Option<ResolvedRemoteClient>, Box<dyn Error>> {
    let Some((path, source)) = discovered else {
        return Ok(None);
    };

    let configured = config_for(settings, vendor)?.clone();
    let can_replace = configured.path.trim().is_empty()
        || configured.configured_by.trim().is_empty()
        || configured.configured_by == CONFIGURED_BY_AUTO;
    let auto_configured = can_replace;
    if can_replace {
        *config_for_mut(settings, vendor)? = RemoteClientPathConfig {
            path: path.to_string_lossy().to_string(),
            configured_by: CONFIGURED_BY_AUTO.to_string(),
        };
        save_unlocked(agent_state_path, settings)?;
    }
    Ok(Some(ResolvedRemoteClient {
        path,
        source,
        auto_configured,
    }))
}

fn discover(vendor: &str) -> Option<(PathBuf, String)> {
    if let Some(path) = running_process_path(vendor) {
        return Some((
            prefer_sunlogin_launcher(&path),
            "running_process".to_string(),
        ));
    }
    for path in registry_candidates(vendor) {
        if valid_executable(vendor, &path) {
            return Some((prefer_sunlogin_launcher(&path), "registry".to_string()));
        }
    }
    for path in start_menu_candidates(vendor) {
        if valid_executable(vendor, &path) {
            return Some((prefer_sunlogin_launcher(&path), "start_menu".to_string()));
        }
    }
    for path in standard_candidates(vendor) {
        if valid_executable(vendor, &path) {
            return Some((
                prefer_sunlogin_launcher(&path),
                "standard_directory".to_string(),
            ));
        }
    }
    path_candidate(vendor).map(|path| (prefer_sunlogin_launcher(&path), "path".to_string()))
}

fn running_process_path(vendor: &str) -> Option<PathBuf> {
    let names = process_names(vendor).ok()?;
    let literals = names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$names=@({literals}); Get-Process -Name $names -ErrorAction SilentlyContinue | Where-Object {{ $_.Path }} | Select-Object -ExpandProperty Path"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .find(|path| valid_executable(vendor, path))
}

#[cfg(windows)]
fn registry_candidates(vendor: &str) -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    let mut candidates = Vec::new();
    let names = executable_names(vendor);
    for (root, key_path) in [
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\App Paths",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"Software\Microsoft\Windows\CurrentVersion\App Paths",
        ),
    ] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            for executable in &names {
                if let Ok(key) = root
                    .open_subkey_with_flags(format!(r"{key_path}\{executable}"), KEY_READ | view)
                {
                    if let Ok(value) = key.get_value::<String, _>("") {
                        push_registry_value(&mut candidates, vendor, &value);
                    }
                }
            }
        }
    }

    for (root, key_path) in [
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let Ok(uninstall) = root.open_subkey_with_flags(key_path, KEY_READ | view) else {
                continue;
            };
            for child_name in uninstall.enum_keys().flatten() {
                let Ok(child) = uninstall.open_subkey_with_flags(&child_name, KEY_READ | view)
                else {
                    continue;
                };
                let display_name = child
                    .get_value::<String, _>("DisplayName")
                    .unwrap_or_default();
                if !vendor_text_matches(vendor, &format!("{child_name} {display_name}")) {
                    continue;
                }
                for value_name in ["DisplayIcon", "InstallLocation", "UninstallString"] {
                    if let Ok(value) = child.get_value::<String, _>(value_name) {
                        push_registry_value(&mut candidates, vendor, &value);
                    }
                }
            }
        }
    }
    dedupe_paths(candidates)
}

#[cfg(not(windows))]
fn registry_candidates(_vendor: &str) -> Vec<PathBuf> {
    Vec::new()
}

fn push_registry_value(candidates: &mut Vec<PathBuf>, vendor: &str, value: &str) {
    if let Some(path) = executable_from_text(value) {
        candidates.push(path);
    }
    let directory = PathBuf::from(value.trim().trim_matches('"'));
    if directory.is_dir() {
        for relative in relative_executable_paths(vendor) {
            candidates.push(directory.join(relative));
        }
    }
}

fn executable_from_text(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let end = lower.find(".exe")? + 4;
    let candidate = trimmed[..end].trim().trim_start_matches('"').trim();
    (!candidate.is_empty()).then(|| PathBuf::from(candidate))
}

fn start_menu_candidates(vendor: &str) -> Vec<PathBuf> {
    let pattern = if vendor == "todesk" {
        "todesk"
    } else {
        "sunlogin|awesun|向日葵"
    };
    let script = r#"
$roots = @($env:APPDATA, $env:ProgramData) | Where-Object { $_ } | ForEach-Object { Join-Path $_ 'Microsoft\Windows\Start Menu\Programs' }
$shell = New-Object -ComObject WScript.Shell
Get-ChildItem -Path $roots -Filter *.lnk -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match $env:HIMIND_REMOTE_CLIENT_PATTERN } |
    Select-Object -First 20 |
    ForEach-Object { $shell.CreateShortcut($_.FullName).TargetPath }
"#;
    let Ok(output) = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .env("HIMIND_REMOTE_CLIENT_PATTERN", pattern)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    dedupe_paths(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect(),
    )
}

fn standard_candidates(vendor: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for name in [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "LOCALAPPDATA",
        "APPDATA",
        "ProgramData",
    ] {
        if let Some(value) = env::var_os(name) {
            roots.push(PathBuf::from(value));
        }
    }
    let mut candidates = Vec::new();
    for root in roots {
        for relative in relative_executable_paths(vendor) {
            candidates.push(root.join(relative));
        }
    }
    dedupe_paths(candidates)
}

fn path_candidate(vendor: &str) -> Option<PathBuf> {
    for executable in executable_names(vendor) {
        let Ok(output) = Command::new("where.exe")
            .arg(executable)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            continue;
        };
        if let Some(path) = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .find(|path| valid_executable(vendor, path))
        {
            return Some(path);
        }
    }
    None
}

fn relative_executable_paths(vendor: &str) -> Vec<PathBuf> {
    if vendor == "todesk" {
        return [
            r"ToDesk\ToDesk.exe",
            r"Programs\ToDesk\ToDesk.exe",
            r"ToDesk.exe",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }
    // Keep the bootstrap launcher ahead of its Flutter shell. Launching
    // flutter\AweSun.exe directly while Sunlogin is fully stopped opens a
    // blank/white window because the client services are not running; the
    // top-level AweSun.exe bootstraps the UI correctly.
    [
        r"Oray\SunLogin\SunloginClient\AweSun.exe",
        r"Oray\SunLogin\SunloginClient\flutter\AweSun.exe",
        r"Oray\SunLogin\SunloginClient\SunloginClient.exe",
        r"Sunlogin\SunloginClient.exe",
        r"Programs\Oray\SunLogin\SunloginClient\AweSun.exe",
        r"Programs\Oray\SunLogin\SunloginClient\flutter\AweSun.exe",
        r"AweSun.exe",
        r"SunloginClient.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// Sunlogin's Flutter UI shell (`flutter\AweSun.exe`) renders a blank white
/// window when launched without the client bootstrap. When a reference points
/// at that shell, prefer the sibling launcher in the install root instead.
fn prefer_sunlogin_launcher(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let parent = path
        .parent()
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name != "awesun.exe" || parent != "flutter" {
        return path.to_path_buf();
    }
    if let Some(launcher) = path
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("AweSun.exe"))
    {
        if valid_executable("sunlogin", &launcher) {
            return launcher;
        }
    }
    path.to_path_buf()
}

fn executable_names(vendor: &str) -> Vec<&'static str> {
    if vendor == "todesk" {
        vec!["ToDesk.exe"]
    } else {
        vec!["AweSun.exe", "SunloginClient.exe"]
    }
}

fn valid_executable(vendor: &str, path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if vendor == "todesk" {
        name == "todesk.exe"
    } else {
        matches!(name.as_str(), "awesun.exe" | "sunloginclient.exe")
    }
}

fn configured_path(vendor: &str, config: &RemoteClientPathConfig) -> Option<PathBuf> {
    let path = PathBuf::from(config.path.trim());
    valid_executable(vendor, &path).then_some(path)
}

fn vendor_key(vendor: &str) -> Result<&'static str, Box<dyn Error>> {
    let normalized = vendor.trim().to_lowercase();
    if normalized.contains("todesk") {
        return Ok("todesk");
    }
    if normalized.contains("sunlogin")
        || normalized.contains("向日葵")
        || normalized.contains("oray")
    {
        return Ok("sunlogin");
    }
    Err("unsupported remote vendor".into())
}

fn vendor_label(vendor: &str) -> &'static str {
    if vendor == "todesk" {
        "ToDesk"
    } else {
        "向日葵"
    }
}

fn vendor_text_matches(vendor: &str, value: &str) -> bool {
    let value = value.to_lowercase();
    if vendor == "todesk" {
        value.contains("todesk")
    } else {
        value.contains("sunlogin") || value.contains("awesun") || value.contains("向日葵")
    }
}

fn config_for<'a>(
    settings: &'a RemoteClientSettings,
    vendor: &str,
) -> Result<&'a RemoteClientPathConfig, Box<dyn Error>> {
    match vendor_key(vendor)? {
        "todesk" => Ok(&settings.todesk),
        "sunlogin" => Ok(&settings.sunlogin),
        _ => unreachable!(),
    }
}

fn config_for_mut<'a>(
    settings: &'a mut RemoteClientSettings,
    vendor: &str,
) -> Result<&'a mut RemoteClientPathConfig, Box<dyn Error>> {
    match vendor_key(vendor)? {
        "todesk" => Ok(&mut settings.todesk),
        "sunlogin" => Ok(&mut settings.sunlogin),
        _ => unreachable!(),
    }
}

fn load_unlocked(agent_state_path: &Path) -> Result<RemoteClientSettings, Box<dyn Error>> {
    let path = settings_path(agent_state_path);
    if !path.is_file() {
        return Ok(RemoteClientSettings::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn save_unlocked(
    agent_state_path: &Path,
    settings: &RemoteClientSettings,
) -> Result<(), Box<dyn Error>> {
    let path = settings_path(agent_state_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(settings)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "himind-remote-client-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    #[test]
    fn manual_configuration_round_trips_and_validates_vendor() {
        let root = test_root("manual");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("ToDesk.exe");
        fs::write(&executable, b"test").unwrap();
        let state_path = root.join("agent-state.json");

        configure("todesk", &executable.to_string_lossy(), &state_path).unwrap();
        let settings = load_unlocked(&state_path).unwrap();

        assert_eq!(settings.todesk.configured_by, CONFIGURED_BY_MANUAL);
        assert_eq!(settings.todesk.path, executable.to_string_lossy());
        assert!(configure("sunlogin", &executable.to_string_lossy(), &state_path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_value_parser_keeps_complete_executable_path() {
        assert_eq!(
            executable_from_text(r#""D:\Apps\ToDesk\ToDesk.exe",0"#),
            Some(PathBuf::from(r"D:\Apps\ToDesk\ToDesk.exe"))
        );
    }

    #[test]
    fn first_discovered_path_is_saved_as_machine_default() {
        let root = test_root("auto");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("ToDesk.exe");
        fs::write(&executable, b"test").unwrap();
        let state_path = root.join("agent-state.json");
        let mut settings = RemoteClientSettings::default();

        let resolved = persist_discovered_unlocked(
            "todesk",
            &state_path,
            &mut settings,
            Some((executable.clone(), "standard_directory".to_string())),
        )
        .unwrap()
        .expect("discovered executable");

        assert_eq!(resolved.path, executable);
        assert!(resolved.auto_configured);
        let saved = load_unlocked(&state_path).unwrap();
        assert_eq!(saved.todesk.configured_by, CONFIGURED_BY_AUTO);
        assert_eq!(saved.todesk.path, executable.to_string_lossy());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sunlogin_flutter_shell_is_redirected_to_install_root_launcher() {
        let root = test_root("sunlogin-launcher");
        let flutter_dir = root.join("flutter");
        fs::create_dir_all(&flutter_dir).unwrap();
        let shell = flutter_dir.join("AweSun.exe");
        let launcher = root.join("AweSun.exe");
        fs::write(&shell, b"test-shell").unwrap();
        fs::write(&launcher, b"test-launcher").unwrap();

        let preferred = prefer_sunlogin_launcher(&shell);
        assert_eq!(preferred, launcher);

        // The same correction is persisted when an auto-configured reference
        // points at the Flutter shell, so the next cold start uses the launcher.
        let auto_state_path = root.join("auto-agent-state.json");
        let mut auto_settings = RemoteClientSettings::default();
        auto_settings.sunlogin = RemoteClientPathConfig {
            path: shell.to_string_lossy().to_string(),
            configured_by: CONFIGURED_BY_AUTO.to_string(),
        };
        let resolved = resolve_unlocked("sunlogin", &auto_state_path, &mut auto_settings)
            .unwrap()
            .expect("resolved sunlogin client");
        assert_eq!(resolved.path, launcher);
        assert!(resolved.auto_configured);
        let saved = load_unlocked(&auto_state_path).unwrap();
        assert_eq!(saved.sunlogin.path, launcher.to_string_lossy());
        assert_eq!(saved.sunlogin.configured_by, CONFIGURED_BY_AUTO);

        // A manual reference must be respected: report the launcher as the
        // effective executable but do not rewrite the user's configured path.
        let manual_state_path = root.join("manual-agent-state.json");
        let mut manual_settings = RemoteClientSettings::default();
        manual_settings.sunlogin = RemoteClientPathConfig {
            path: shell.to_string_lossy().to_string(),
            configured_by: CONFIGURED_BY_MANUAL.to_string(),
        };
        let resolved = resolve_unlocked("sunlogin", &manual_state_path, &mut manual_settings)
            .unwrap()
            .expect("resolved sunlogin client");
        assert_eq!(resolved.path, launcher);
        assert!(!resolved.auto_configured);
        assert_eq!(manual_settings.sunlogin.path, shell.to_string_lossy());
        assert_eq!(manual_settings.sunlogin.configured_by, CONFIGURED_BY_MANUAL);

        fs::remove_dir_all(root).unwrap();
    }
}
