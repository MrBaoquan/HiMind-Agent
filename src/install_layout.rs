//! Stable installation layout shared by the Agent, launcher and updater.
//!
//! Once an installation is migrated, Agent executables live in
//! `versions/<version>`. External MCP clients may keep an old executable alive
//! indefinitely, so updates must never replace that executable in place.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const AGENT_FILE: &str = "himind-agent.exe";
pub(crate) const LAUNCHER_FILE: &str = "himind-agent-launcher.exe";
pub(crate) const UPDATER_FILE: &str = "himind-agent-updater.exe";
pub(crate) const ACTIVE_VERSION_FILE: &str = "active-version";

pub(crate) fn installation_root_from_executable(executable: &Path) -> PathBuf {
    let Some(parent) = executable.parent() else {
        return executable.to_path_buf();
    };
    if parent
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("current"))
    {
        return parent.parent().unwrap_or(parent).to_path_buf();
    }
    if parent
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("versions"))
    {
        return parent
            .parent()
            .and_then(Path::parent)
            .unwrap_or(parent)
            .to_path_buf();
    }
    parent.to_path_buf()
}

pub(crate) fn launcher_path(root: &Path) -> PathBuf {
    root.join(LAUNCHER_FILE)
}

pub(crate) fn updater_path(root: &Path) -> PathBuf {
    root.join(UPDATER_FILE)
}

pub(crate) fn stable_launcher_for_executable(executable: &Path) -> PathBuf {
    let root = installation_root_from_executable(executable);
    let launcher = launcher_path(&root);
    if launcher.is_file() {
        launcher
    } else {
        executable.to_path_buf()
    }
}

pub(crate) fn read_active_version(root: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let path = root.join(ACTIVE_VERSION_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let value = fs::read_to_string(path)?;
    let value = value.trim();
    validate_version(value)?;
    Ok(Some(value.to_string()))
}

pub(crate) fn active_agent_path(root: &Path) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let Some(version) = read_active_version(root)? else {
        return Ok(None);
    };
    let executable = root.join("versions").join(version).join(AGENT_FILE);
    if !executable.is_file() {
        return Err(format!("active Agent version is missing: {}", executable.display()).into());
    }
    Ok(Some(executable))
}

pub(crate) fn resolve_agent_path(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = active_agent_path(root)? {
        return Ok(path);
    }
    let legacy = root.join("current").join(AGENT_FILE);
    if legacy.is_file() {
        return Ok(legacy);
    }
    Err(format!("installed Agent is missing under {}", root.display()).into())
}

pub(crate) fn write_active_version(root: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    validate_version(version)?;
    fs::create_dir_all(root)?;
    let target = root.join(ACTIVE_VERSION_FILE);
    let temporary = root.join(format!(".{ACTIVE_VERSION_FILE}.{}.tmp", std::process::id()));
    fs::write(&temporary, format!("{version}\n"))?;
    replace_file(&temporary, &target)
}

pub(crate) fn validate_version(version: &str) -> Result<(), Box<dyn Error>> {
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("Agent version pointer is invalid".into());
    }
    Ok(())
}

pub(crate) fn version_directory(root: &Path, version: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_version(version)?;
    Ok(root.join("versions").join(version))
}

pub(crate) fn prepare_version_directory(
    root: &Path,
    version: &str,
    staged_agent: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let versions = root.join("versions");
    fs::create_dir_all(&versions)?;
    let target = version_directory(root, version)?;
    let temporary = versions.join(format!(".{version}.installing-{}", std::process::id()));
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }
    fs::create_dir(&temporary)?;
    if let Err(error) = fs::copy(staged_agent, temporary.join(AGENT_FILE)) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error.into());
    }
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&temporary, &target)?;
    Ok(target)
}

pub(crate) fn repair_pending_updater(executable: &Path) -> Result<bool, Box<dyn Error>> {
    let root = installation_root_from_executable(executable);
    let pending = root.join("himind-agent-updater.next.exe");
    if !pending.is_file() {
        return Ok(false);
    }
    let target = updater_path(&root);
    if target.is_file() {
        let pending_modified = fs::metadata(&pending)?.modified()?;
        let target_modified = fs::metadata(&target)?.modified()?;
        if pending_modified <= target_modified {
            let _ = fs::remove_file(&pending);
            return Ok(false);
        }
    }

    let backup = root.join("himind-agent-updater.previous.exe");
    if target.is_file() {
        let backup_temporary = root.join("himind-agent-updater.previous.repairing.exe");
        let _ = fs::remove_file(&backup_temporary);
        fs::copy(&target, &backup_temporary)?;
        replace_file(&backup_temporary, &backup)?;
    }
    let replacement = root.join("himind-agent-updater.repairing.exe");
    let _ = fs::remove_file(&replacement);
    fs::copy(&pending, &replacement)?;
    if let Err(error) = replace_file(&replacement, &target) {
        let _ = fs::remove_file(&replacement);
        return Err(error);
    }
    let _ = fs::remove_file(&pending);
    Ok(true)
}

pub(crate) fn replace_file(temporary: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source = wide_path(temporary);
        let destination = wide_path(target);
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        if target.exists() {
            fs::remove_file(target)?;
        }
        fs::rename(temporary, target)?;
        Ok(())
    }
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        active_agent_path, installation_root_from_executable, repair_pending_updater,
        resolve_agent_path, write_active_version,
    };
    use std::fs;
    use std::path::Path;

    #[test]
    fn resolves_active_version_before_legacy_current() {
        let root = std::env::temp_dir().join(format!("himind-layout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("current")).unwrap();
        fs::write(root.join("current/himind-agent.exe"), b"old").unwrap();
        fs::create_dir_all(root.join("versions/0.4.0")).unwrap();
        fs::write(root.join("versions/0.4.0/himind-agent.exe"), b"new").unwrap();
        write_active_version(&root, "0.4.0").unwrap();
        assert_eq!(
            active_agent_path(&root).unwrap().unwrap(),
            root.join("versions/0.4.0/himind-agent.exe")
        );
        assert_eq!(
            resolve_agent_path(&root).unwrap(),
            root.join("versions/0.4.0/himind-agent.exe")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn falls_back_to_legacy_current_when_pointer_is_absent() {
        let root =
            std::env::temp_dir().join(format!("himind-layout-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("current")).unwrap();
        fs::write(root.join("current/himind-agent.exe"), b"old").unwrap();
        assert_eq!(
            resolve_agent_path(&root).unwrap(),
            root.join("current/himind-agent.exe")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_pointer_path_traversal() {
        let root =
            std::env::temp_dir().join(format!("himind-layout-traversal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("active-version"), "../outside\n").unwrap();
        assert!(active_agent_path(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn derives_install_root_from_version_and_current_paths() {
        assert_eq!(
            installation_root_from_executable(Path::new(
                r"C:\HiMind\versions\0.4.0\himind-agent.exe",
            )),
            Path::new(r"C:\HiMind")
        );
        assert_eq!(
            installation_root_from_executable(Path::new(r"C:\HiMind\current\himind-agent.exe")),
            Path::new(r"C:\HiMind")
        );
    }

    #[test]
    fn repairs_a_newer_pending_updater_and_keeps_a_backup() {
        let root = std::env::temp_dir().join(format!(
            "himind-layout-updater-repair-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("versions/0.4.0")).unwrap();
        let agent = root.join("versions/0.4.0/himind-agent.exe");
        fs::write(&agent, b"agent").unwrap();
        fs::write(root.join("himind-agent-updater.exe"), b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("himind-agent-updater.next.exe"), b"new").unwrap();

        assert!(repair_pending_updater(&agent).unwrap());
        assert_eq!(
            fs::read(root.join("himind-agent-updater.exe")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(root.join("himind-agent-updater.previous.exe")).unwrap(),
            b"old"
        );
        assert!(!root.join("himind-agent-updater.next.exe").exists());
        let _ = fs::remove_dir_all(root);
    }
}
