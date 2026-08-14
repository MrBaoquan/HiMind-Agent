use fs4::fs_std::FileExt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    path.with_file_name(format!("{file_name}.bak"))
}

pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    path.with_file_name(format!("{file_name}.lock"))
}

pub(crate) fn lock(path: &Path) -> io::Result<AtomicFileLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path(path))?;
    file.lock_exclusive()?;
    Ok(AtomicFileLock(file))
}

pub(crate) struct AtomicFileLock(fs::File);

impl Drop for AtomicFileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.is_file() {
        let current = fs::read(path)?;
        atomic_write_inner(&backup_path(path), &current)?;
    }
    atomic_write_inner(path, content)
}

pub(crate) fn restore_backup(path: &Path) -> io::Result<bool> {
    let backup = backup_path(path);
    if !backup.is_file() {
        return Ok(false);
    }
    let content = fs::read(backup)?;
    atomic_write_inner(path, &content)?;
    Ok(true)
}

fn atomic_write_inner(path: &Path, content: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    path.with_file_name(format!(
        ".{file_name}.himind-tmp-{}-{nonce}",
        std::process::id()
    ))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }

    #[cfg(test)]
    if fail_before_replace_enabled() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected replace failure",
        ));
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if fail_before_replace_enabled() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected replace failure",
        ));
    }
    fs::rename(source, destination)
}

#[cfg(test)]
thread_local! {
    static FAIL_BEFORE_REPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_before_replace_enabled() -> bool {
    FAIL_BEFORE_REPLACE.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::{atomic_write, backup_path, restore_backup, FAIL_BEFORE_REPLACE};
    use std::fs;

    fn test_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "himind-atomic-{label}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn replacement_keeps_last_good_backup() {
        let path = test_path("backup");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read(backup_path(&path)).unwrap(), b"first");
        fs::write(&path, b"corrupt").unwrap();
        assert!(restore_backup(&path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(backup_path(&path));
    }

    #[test]
    fn failed_replace_preserves_destination() {
        let path = test_path("failure");
        atomic_write(&path, b"stable").unwrap();
        FAIL_BEFORE_REPLACE.with(|value| value.set(true));
        let result = atomic_write(&path, b"next");
        FAIL_BEFORE_REPLACE.with(|value| value.set(false));
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"stable");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(backup_path(&path));
    }
}
