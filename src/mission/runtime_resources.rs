//! Host-owned generated outputs, separate from confirmed business path grants.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub(crate) struct CheckRuntimeResources {
    root: PathBuf,
    workspace: PathBuf,
    mission_id: String,
    task_id: String,
    // On Windows keep the verified directory chain open without write/delete
    // sharing, so it cannot be replaced or converted to a reparse point.
    _directory_guards: Vec<fs::File>,
}

impl CheckRuntimeResources {
    pub(crate) fn prepare(
        workspace: &Path,
        mission_id: &str,
        task_id: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let home = crate::store::paths::agent_home();
        Self::prepare_in(
            workspace,
            mission_id,
            task_id,
            &home.join("cache/mission-checks"),
            Some(&home.join("mission-runtime-storage.json")),
        )
    }

    fn prepare_in(
        workspace: &Path,
        mission_id: &str,
        task_id: &str,
        base: &Path,
        policy_path: Option<&Path>,
    ) -> Result<Self, Box<dyn Error>> {
        if mission_id.trim().is_empty() || task_id.trim().is_empty() {
            return Err("check runtime resources require mission and task IDs".into());
        }
        let workspace = ordinary_absolute(&std::path::absolute(workspace)?)?;
        let mut guards = checked_directory_chain(&workspace, false)?;
        let workspace = canonical_directory(&workspace)?;
        let base = resolve_future_directory(&ordinary_absolute(base)?, &mut guards)?;
        let mission_hash = format!("{:x}", Sha256::digest(mission_id.as_bytes()));
        let task_hash = format!("{:x}", Sha256::digest(task_id.as_bytes()));
        let legacy_root = base.join(&mission_hash).join(&task_hash);
        // Check the future path before creating anything, including a base
        // that has not been created yet. Existing ancestors may not redirect.
        let legacy_root = resolve_future_directory(&legacy_root, &mut guards)?;
        ensure_separate(&legacy_root, &workspace)?;
        let allocations = base.join(".allocations-v1");
        ensure_separate(&allocations, &workspace)?;
        let _lock = storage_lock(&allocations)?;
        // Read even for a pinned task: invalid host control state fails closed.
        let preferred = match policy_path {
            Some(path) => read_policy(path, &mut guards)?,
            None => None,
        };
        if let Some(preferred) = &preferred { reject_control_base(preferred, &allocations)?; }
        let record_path = allocations.join(&mission_hash).join(format!("{task_hash}.json"));
        let existing = read_control_json::<StorageAllocation>(&record_path, &mut guards)?;
        let other_roots = registered_task_roots(&base, &allocations, &mission_hash, &task_hash, &mut guards)?;
        let (root, allocation) = if let Some((record, file)) = existing {
            if record.version != 1 || record.mission_hash != mission_hash
                || record.task_hash != task_hash || record.workspace != workspace
            {
                return Err("runtime storage allocation identity/workspace mismatch".into());
            }
            let pinned_base = resolve_future_directory(&ordinary_absolute(&record.base)?, &mut guards)?;
            validate_local_disk(&pinned_base)?;
            let root = resolve_future_directory(&ordinary_absolute(&record.root)?, &mut guards)?;
            if pinned_base != record.base || root != record.root
                || root != pinned_base.join(&mission_hash).join(&task_hash)
            {
                return Err("runtime storage allocation path mismatch".into());
            }
            guards.push(file); // Keep the authoritative pin open for this resource's lifetime.
            (root, None)
        } else {
            // An existing legacy root always wins, even after policy changes.
            let legacy_exists = directory_exists(&legacy_root)?;
            let selected_base = if legacy_exists { base.clone() } else { preferred.unwrap_or(base.clone()) };
            let root = resolve_future_directory(&selected_base.join(&mission_hash).join(&task_hash), &mut guards)?;
            ensure_separate(&root, &workspace)?;
            ensure_separate(&root, &allocations)?;
            for other in &other_roots { ensure_separate(&root, other)?; }
            let record = StorageAllocation {
                version: 1, mission_hash, task_hash, workspace: workspace.clone(),
                base: selected_base, root: root.clone(),
            };
            control_bytes(&record)?;
            // A previously unallocated alternate root is not ours to adopt.
            // Exclusive creation also prevents two host homes claiming one root.
            if !legacy_exists {
                guards.extend(checked_directory_chain(root.parent().ok_or("missing root parent")?, true)?);
                fs::create_dir(&root)?;
            }
            (root, Some(record))
        };
        ensure_separate(&root, &workspace)?;
        ensure_separate(&root, &allocations)?;
        for other in &other_roots { ensure_separate(&root, other)?; }
        guards.extend(checked_directory_chain(&root, true)?);
        let root = canonical_directory(&root)?;
        ensure_separate(&root, &workspace)?;
        for name in ["tmp", "npm-cache"] {
            guards.extend(checked_directory_chain(&root.join(name), true)?);
        }
        validate_output_tree(&root)?;
        if let Some(record) = allocation {
            guards.extend(checked_directory_chain(record_path.parent().ok_or("missing allocation parent")?, true)?);
            atomic_control_json(&record_path, &record, false)?;
            let (_, file) = read_control_json::<StorageAllocation>(&record_path, &mut guards)?
                .ok_or("runtime storage allocation disappeared")?;
            guards.push(file);
        }
        Ok(Self {
            root,
            workspace,
            mission_id: mission_id.to_owned(),
            task_id: task_id.to_owned(),
            _directory_guards: guards,
        })
    }

    /// Only tests can substitute a private cache base; never accept a root
    /// environment variable or a user-approved absolute path as this authority.
    #[cfg(test)]
    pub(crate) fn prepare_at(
        workspace: &Path,
        mission_id: &str,
        task_id: &str,
        base: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        Self::prepare_in(workspace, mission_id, task_id, base, None)
    }

    pub(crate) fn input(&self) -> Value {
        json!({
            "schema_version": "mission_check_runtime.v1",
            "source": "host",
            "root": self.root,
            "mission_id": self.mission_id,
            "task_id": self.task_id,
            "authority": "host_owned_generated_outputs",
            "allowed_operations": [
                "Create and write only generated test evidence (including screenshots), build outputs, caches and temporary files under root.",
                "Read referenced check evidence under root; external evidence still requires its existing read authorization.",
                "This resource is not business source authorization and grants no additional business read/write paths or approved_absolute_paths."
            ],
            "retention": "Preserve generated evidence for review; no automatic deletion."
        })
    }

    /// Apply only to a child command, before spawn. Environment variables route
    /// cooperative tools; the resource contract does not replace an OS sandbox.
    pub(crate) fn apply(&self, command: &mut Command) -> Result<(), Box<dyn Error>> {
        self.apply_for_workspace(&self.workspace, command)
    }

    // Bind the caller workspace and validate the entire resource tree once.
    // Required checks previously walked the same build/cache tree twice here.
    pub(super) fn apply_for_workspace(&self, workspace: &Path, command: &mut Command) -> Result<(), Box<dyn Error>> {
        self.validate_for_workspace(workspace)?;
        command
            .env("HIMIND_MISSION_CHECK_ROOT", &self.root)
            .env("SERVICE_UI_RUNTIME_ROOT", &self.root)
            .env("TEMP", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("npm_config_cache", self.root.join("npm-cache"))
            .env("npm_config_update_notifier", "false")
            .env("PYTHONDONTWRITEBYTECODE", "1");
        Ok(())
    }

    pub(super) fn validate_for_workspace(&self, workspace: &Path) -> Result<(), Box<dyn Error>> {
        let workspace = ordinary_absolute(&std::path::absolute(workspace)?)?;
        let _workspace_guards = checked_directory_chain(&workspace, false)?;
        let workspace = canonical_directory(&workspace)?;
        if workspace != self.workspace {
            return Err("check runtime resources belong to a different workspace".into());
        }
        let _root_guards = checked_directory_chain(&self.root, false)?;
        ensure_separate(&canonical_directory(&self.root)?, &workspace)?;
        for name in ["tmp", "npm-cache"] {
            let _guards = checked_directory_chain(&self.root.join(name), false)?;
        }
        validate_output_tree(&self.root)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoragePolicy {
    version: u32,
    preferred_base: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageAllocation {
    version: u32,
    mission_hash: String,
    task_hash: String,
    workspace: PathBuf,
    base: PathBuf,
    root: PathBuf,
}

/// Host configuration only. No environment variable is consulted for authority.
pub(crate) fn runtime_storage_status() -> Result<Value, Box<dyn Error>> {
    storage_status_in(&crate::store::paths::agent_home())
}

pub(crate) fn configure_runtime_storage(base: &Path) -> Result<Value, Box<dyn Error>> {
    configure_storage_in(&crate::store::paths::agent_home(), base)
}

fn storage_status_in(home: &Path) -> Result<Value, Box<dyn Error>> {
    let home = ordinary_absolute(home)?;
    let mut guards = Vec::new();
    let default_base = resolve_future_directory(&home.join("cache/mission-checks"), &mut guards)?;
    let policy = read_policy(&home.join("mission-runtime-storage.json"), &mut guards)?;
    let allocations = resolve_future_directory(&default_base.join(".allocations-v1"), &mut guards)?;
    if let Some(preferred) = &policy { reject_control_base(preferred, &allocations)?; }
    Ok(json!({
        "version": 1,
        "policy_file": home.join("mission-runtime-storage.json"),
        "preferred_base": policy,
        "default_base": default_base,
        "default_capacity": storage_capacity(&default_base)?,
        "preferred_capacity": policy.as_deref().map(storage_capacity).transpose()?,
        "allocations": {
            "directory": allocations,
            "enumerated": false,
            "effect": "Existing allocations keep their root; existing legacy roots are pinned in place. Policy affects only unallocated tasks without a legacy root. No evidence is moved or deleted."
        }
    }))
}

fn configure_storage_in(home: &Path, base: &Path) -> Result<Value, Box<dyn Error>> {
    let home = ordinary_absolute(home)?;
    let mut guards = Vec::new();
    let base = resolve_future_directory(&ordinary_absolute(base)?, &mut guards)?;
    validate_local_disk(&base)?;
    let default_base = resolve_future_directory(&home.join("cache/mission-checks"), &mut guards)?;
    let allocations = default_base.join(".allocations-v1");
    reject_control_base(&base, &allocations)?;
    let _lock = storage_lock(&allocations)?;
    for root in registered_task_roots(&default_base, &allocations, "", "", &mut guards)? {
        if base.starts_with(&root) {
            return Err("runtime storage base is inside an existing task resource".into());
        }
    }
    let policy_path = home.join("mission-runtime-storage.json");
    let previous = read_policy(&policy_path, &mut guards)?;
    if previous.as_deref() == Some(base.as_path()) {
        return storage_status_in(&home);
    }
    let policy = StoragePolicy { version: 1, preferred_base: base.clone() };
    control_bytes(&policy)?;
    guards.extend(checked_directory_chain(&base, true)?);
    let base = canonical_directory(&base)?;
    atomic_control_json(&policy_path, &StoragePolicy { version: 1, preferred_base: base }, true)?;
    storage_status_in(&home)
}

fn read_policy(path: &Path, guards: &mut Vec<fs::File>) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let Some((policy, _file)) = read_control_json::<StoragePolicy>(path, guards)? else {
        return Ok(None);
    };
    if policy.version != 1 {
        return Err("unsupported runtime storage policy version".into());
    }
    let base = resolve_future_directory(&ordinary_absolute(&policy.preferred_base)?, guards)?;
    validate_local_disk(&base)?;
    Ok(Some(base))
}

fn directory_exists(path: &Path) -> Result<bool, Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            verify_kind(&metadata)?;
            if !metadata.is_dir() { return Err("runtime storage root is not a directory".into()); }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Inspect only host allocation records and the two-level legacy directory
/// layout, never build outputs. The caller holds the host allocation lock.
fn registered_task_roots(
    default_base: &Path, allocations: &Path, mission: &str, task: &str,
    guards: &mut Vec<fs::File>,
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let is_hash = |name: &str| name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
    let mut roots = std::collections::BTreeSet::new();
    let mut count = 0usize;
    for folder in fs::read_dir(allocations)? {
        let folder = folder?;
        let name = folder.file_name().to_str().ok_or("invalid allocation name")?.to_owned();
        if name == ".storage.lock" { continue; }
        if !is_hash(&name) { return Err("unexpected host allocation entry".into()); }
        guards.extend(checked_directory_chain(&folder.path(), false)?);
        for entry in fs::read_dir(folder.path())? {
            count += 1;
            if count > 10_000 { return Err("runtime allocation registry exceeds limit".into()); }
            let entry = entry?;
            let file_name = entry.file_name().to_str().ok_or("invalid allocation name")?.to_owned();
            if file_name.strip_prefix(".storage-").and_then(|name| name.strip_suffix(".tmp"))
                .and_then(|name| name.split_once('-')).is_some_and(|(pid, nonce)| {
                    !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
                        && nonce.len() == 16 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
                })
            {
                // Interrupted publication is not an allocation. Preserve it for
                // diagnosis, but reject redirected/multiple-link control files.
                let file = control_options(false)?.open(entry.path())?;
                verify_control_file(&file)?;
                continue;
            }
            let task_hash = file_name.strip_suffix(".json").filter(|value| is_hash(value))
                .ok_or("unexpected host allocation record")?;
            let (record, _file) = read_control_json::<StorageAllocation>(&entry.path(), guards)?
                .ok_or("runtime allocation disappeared")?;
            if record.version != 1 || record.mission_hash != name || record.task_hash != task_hash {
                return Err("runtime allocation registry identity mismatch".into());
            }
            let base = resolve_future_directory(&ordinary_absolute(&record.base)?, guards)?;
            let root = resolve_future_directory(&ordinary_absolute(&record.root)?, guards)?;
            if base != record.base || root != record.root || root != base.join(&name).join(task_hash) {
                return Err("runtime allocation registry path mismatch".into());
            }
            if name != mission || task_hash != task { roots.insert(root); }
        }
    }
    // Legacy tasks have not necessarily been opened since allocation pinning
    // was introduced. Their existing evidence still defines a task boundary.
    for folder in fs::read_dir(default_base)? {
        let folder = folder?;
        let name = folder.file_name().to_str().ok_or("invalid legacy name")?.to_owned();
        if !is_hash(&name) { continue; }
        guards.extend(checked_directory_chain(&folder.path(), false)?);
        for entry in fs::read_dir(folder.path())? {
            let entry = entry?;
            let task_hash = entry.file_name().to_str().ok_or("invalid legacy task")?.to_owned();
            if !is_hash(&task_hash) { continue; }
            count += 1;
            if count > 10_000 { return Err("runtime allocation registry exceeds limit".into()); }
            guards.extend(checked_directory_chain(&entry.path(), false)?);
            if name != mission || task_hash != task {
                roots.insert(canonical_directory(&entry.path())?);
            }
        }
    }
    Ok(roots.into_iter().collect())
}

fn reject_control_base(base: &Path, allocations: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(windows)]
    let (base, allocations) = (
        PathBuf::from(base.to_str().ok_or("invalid storage base")?.to_lowercase()),
        PathBuf::from(allocations.to_str().ok_or("invalid allocations path")?.to_lowercase()),
    );
    if base.starts_with(allocations) {
        return Err("runtime storage base overlaps host allocation control files".into());
    }
    Ok(())
}

fn control_options(write: bool) -> Result<fs::OpenOptions, Box<dyn Error>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(write);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        options.share_mode(FILE_SHARE_READ).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000); // O_NOFOLLOW
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100); // O_NOFOLLOW
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("runtime storage control files unsupported on this platform".into());
    }
    #[cfg(not(any(unix, windows)))]
    return Err("runtime storage control files unsupported on this platform".into());
    Ok(options)
}

fn verify_control_file(file: &fs::File) -> Result<(), Box<dyn Error>> {
    let metadata = file.metadata()?;
    verify_kind(&metadata)?;
    if !metadata.is_file() { return Err("runtime storage control path is not a file".into()); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 { return Err("runtime storage control file must have one link".into()); }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if info.nNumberOfLinks != 1 { return Err("runtime storage control file must have one link".into()); }
    }
    Ok(())
}

fn read_control_json<T: serde::de::DeserializeOwned>(
    path: &Path, guards: &mut Vec<fs::File>,
) -> Result<Option<(T, fs::File)>, Box<dyn Error>> {
    let path = ordinary_absolute(path)?;
    // Guard every existing ancestor, even when the file/parent is absent.
    resolve_future_directory(path.parent().ok_or("control file has no parent")?, guards)?;
    let file = match control_options(false)?.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_control_file(&file)?;
    let mut bytes = Vec::new();
    (&file).take(16_385).read_to_end(&mut bytes)?;
    verify_control_file(&file)?;
    if bytes.len() > 16_384 { return Err("runtime storage control file is too large".into()); }
    // Typed serde decoding rejects duplicate fields, unknown fields and wrong types.
    Ok(Some((serde_json::from_slice(&bytes)?, file)))
}

struct StorageLock {
    _file: fs::File,
    _guards: Vec<fs::File>,
}

fn storage_lock(allocations: &Path) -> Result<StorageLock, Box<dyn Error>> {
    let guards = checked_directory_chain(allocations, true)?;
    let path = allocations.join(".storage.lock");
    for _ in 0..200 {
        let attempt = (|| -> std::io::Result<fs::File> {
            let file = control_options(true).map_err(|e| std::io::Error::other(e.to_string()))?
                .create(true).open(&path)?;
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                unsafe extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
                if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 { // LOCK_EX | LOCK_NB
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(file)
        })();
        match attempt {
            Ok(file) => {
                verify_control_file(&file)?;
                return Ok(StorageLock { _file: file, _guards: guards });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                || cfg!(windows) && matches!(error.raw_os_error(), Some(32) | Some(33)) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("runtime storage is busy; retry the host operation".into())
}

/// Same-directory temporary + atomic publication. Call only under StorageLock.
fn control_bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Box<dyn Error>> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > 16_384 { return Err("runtime storage control file is too large".into()); }
    Ok(bytes)
}

fn replace_control(temp: &Path, path: &Path) -> Result<(), Box<dyn Error>> {
    for attempt in 0..200 {
        match fs::rename(temp, path) {
            Ok(()) => return Ok(()),
            Err(error) if cfg!(windows) && matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33)) && attempt < 199 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}

fn atomic_control_json<T: serde::Serialize>(path: &Path, value: &T, replace: bool) -> Result<(), Box<dyn Error>> {
    let bytes = control_bytes(value)?;
    let parent = path.parent().ok_or("control file has no parent")?;
    let _guards = checked_directory_chain(parent, false)?;
    let temp = parent.join(format!(".storage-{}-{:016x}.tmp", std::process::id(), rand::random::<u64>()));
    let mut file = control_options(true)?.create_new(true).open(&temp)?;
    verify_control_file(&file)?;
    // Remove only our own unpublished temporary, never an existing control file/evidence.
    let result = (|| -> Result<(), Box<dyn Error>> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        verify_control_file(&file)?;
        Ok(())
    })();
    drop(file);
    let result = result.and_then(|()| {
        if replace { replace_control(&temp, path)?; }
        else { publish_allocation(&temp, path)?; }
        Ok(())
    });
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result
}

fn publish_allocation(temp: &Path, path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        // std::fs adds the verbatim prefix for long paths; raw Win32 calls do
        // not. Keep external resource hints ordinary but publish long pins safely.
        let temp = r"\\?\".encode_utf16().chain(temp.as_os_str().encode_wide()).chain(Some(0)).collect::<Vec<_>>();
        let path = r"\\?\".encode_utf16().chain(path.as_os_str().encode_wide()).chain(Some(0)).collect::<Vec<_>>();
        // No REPLACE_EXISTING: never overwrite an allocation, even on a race.
        if unsafe { MoveFileExW(temp.as_ptr(), path.as_ptr(), 0x8) } == 0 { // WRITE_THROUGH
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(unix)]
    {
        // Atomic no-replace publication on Unix. The host lock excludes readers
        // until the temporary link is removed; interrupted publication fails closed.
        fs::hard_link(temp, path)?;
        fs::remove_file(temp)?;
        fs::File::open(path.parent().ok_or("allocation has no parent")?)?.sync_all()?;
    }
    Ok(())
}

fn validate_local_disk(path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        let drive = path.ancestors().last().ok_or("storage base has no drive")?;
        let drive = drive.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        if unsafe { GetDriveTypeW(drive.as_ptr()) } != 3 { // DRIVE_FIXED; rejects mapped/network/removable drives.
            return Err("runtime storage requires a fixed local disk".into());
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

fn storage_capacity(path: &Path) -> Result<Value, Box<dyn Error>> {
    let mut guards = Vec::new();
    resolve_future_directory(path, &mut guards)?;
    validate_local_disk(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let ancestor = path.ancestors().find(|p| p.is_dir()).ok_or("storage has no existing ancestor")?;
        let path = r"\\?\".encode_utf16().chain(ancestor.as_os_str().encode_wide()).chain(Some(0)).collect::<Vec<_>>();
        let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, &mut free) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(json!({ "available_bytes": available, "total_bytes": total, "free_bytes": free }))
    }
    #[cfg(not(windows))]
    Ok(json!({ "supported": false }))
}

/// Canonicalization on Windows produces verbatim paths, which some Node tools
/// misinterpret. Only ordinary local disk paths have equivalent Win32 syntax.
fn ordinary_absolute(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err("check resource path must be an absolute Unicode path".into());
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return Err("check resource path contains parent traversal".into()),
            Component::CurDir => {}
            #[cfg(windows)]
            Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    result.push(format!("{}:", drive as char));
                }
                _ => return Err("check resources require a local disk path".into()),
            },
            Component::Normal(name) => {
                #[cfg(windows)]
                {
                    let name = name.to_str().ok_or("invalid check resource path")?;
                    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                    if name.contains([':', '<', '>', '"', '|', '?', '*'])
                        || name.ends_with(['.', ' '])
                        || name.chars().any(char::is_control)
                        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                        || ["COM", "LPT"].iter().any(|prefix| {
                            stem.strip_prefix(*prefix).is_some_and(|suffix| {
                                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
                            })
                        })
                    {
                        return Err("check resource path has unsafe Win32 syntax".into());
                    }
                }
                result.push(name);
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    ordinary_absolute(&fs::canonicalize(path)?)
}

fn resolve_future_directory(
    path: &Path,
    guards: &mut Vec<fs::File>,
) -> Result<PathBuf, Box<dyn Error>> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                guards.extend(checked_directory_chain(ancestor, false)?);
                let mut resolved = canonical_directory(ancestor)?;
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.file_name().ok_or("check resource path has no ancestor")?);
                ancestor = ancestor.parent().ok_or("check resource path has no ancestor")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn ensure_separate(root: &Path, workspace: &Path) -> Result<(), Box<dyn Error>> {
    // Conservatively fold case on Windows, including for not-yet-created paths.
    #[cfg(windows)]
    let (root, workspace) = (
        PathBuf::from(root.to_str().ok_or("invalid resource path")?.to_lowercase()),
        PathBuf::from(workspace.to_str().ok_or("invalid workspace path")?.to_lowercase()),
    );
    if root.starts_with(&workspace) || workspace.starts_with(&root) {
        return Err("check resource root must be independent of the workspace".into());
    }
    Ok(())
}

fn verify_kind(metadata: &fs::Metadata) -> Result<(), Box<dyn Error>> {
    if metadata.file_type().is_symlink() {
        return Err("check resource paths must not contain symbolic links".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("check resource paths must not contain Windows reparse points".into());
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err("check resource path redirection cannot be verified on this platform".into());
    if !metadata.is_dir() && !metadata.is_file() {
        return Err("check resource path has an unsupported file type".into());
    }
    Ok(())
}

fn checked_directory_chain(path: &Path, create: bool) -> Result<Vec<fs::File>, Box<dyn Error>> {
    #[allow(unused_mut)] // Unix needs no Windows directory handles.
    let mut guards = Vec::new();
    for part in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = match fs::symlink_metadata(part) {
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(part) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                fs::symlink_metadata(part)?
            }
            result => result?,
        };
        verify_kind(&metadata)?;
        if !metadata.is_dir() {
            return Err("check resource path is not a directory".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ,
            };
            let file = fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(part)?;
            let metadata = file.metadata()?;
            verify_kind(&metadata)?;
            if !metadata.is_dir() {
                return Err("check resource directory changed during verification".into());
            }
            guards.push(file);
        }
    }
    Ok(guards)
}

fn validate_output_tree(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut pending = vec![root.to_owned()];
    let mut count = 0usize;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        verify_kind(&metadata)?;
        count += 1;
        if count > 100_000 {
            return Err("check resource tree exceeds the verification limit".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    pub(crate) struct Fixture {
        pub root: PathBuf,
        pub workspace: PathBuf,
        pub cache: PathBuf,
    }

    impl Fixture {
        pub fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "himind-check-resources-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir(&root).unwrap();
            let root = canonical_directory(&root).unwrap();
            let workspace = root.join("candidate");
            fs::create_dir(&workspace).unwrap();
            fs::write(workspace.join("source.txt"), "confirmed candidate\n").unwrap();
            let cache = root.join("host-cache");
            Self { root, workspace, cache }
        }

        pub fn prepare(&self) -> CheckRuntimeResources {
            CheckRuntimeResources::prepare_at(&self.workspace, "mission", "task", &self.cache)
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this randomly named test directory, never the Agent cache.
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[cfg(any(unix, windows))]
    pub fn redirect_directory(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            // Junctions need no developer-mode/symlink privilege. Both paths
            // are private, random test directories, not user-controlled input.
            use std::os::windows::process::CommandExt;
            let output = Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
    }

    #[cfg(any(unix, windows))]
    pub fn remove_redirect(link: &Path) {
        #[cfg(unix)]
        fs::remove_file(link).unwrap();
        #[cfg(windows)]
        fs::remove_dir(link).unwrap();
    }
}

#[cfg(test)]
#[path = "runtime_storage_tests.rs"]
mod storage_tests;

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn task_roots_are_stable_hashed_isolated_and_retain_evidence() {
        let fixture = Fixture::new();
        let mission = "../../mission/with\\separators";
        let task = "../task:untrusted";
        let first = CheckRuntimeResources::prepare_at(
            &fixture.workspace, mission, task, &fixture.cache,
        ).unwrap();
        let root = first.root.clone();
        assert_eq!(root.parent().unwrap().file_name().unwrap().to_str().unwrap().len(), 64);
        assert_eq!(root.file_name().unwrap().to_str().unwrap().len(), 64);
        assert!(root.file_name().unwrap().to_str().unwrap().bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!root.starts_with(&fixture.workspace));
        assert!(!fixture.workspace.starts_with(&root));
        assert!(root.is_absolute());
        assert!(!root.to_str().unwrap().starts_with(r"\\?\"));
        fs::write(root.join("evidence.json"), "retained").unwrap();
        let again = CheckRuntimeResources::prepare_at(
            &fixture.workspace, mission, task, &fixture.cache,
        ).unwrap();
        assert_eq!(again.root, root);
        let different_task = CheckRuntimeResources::prepare_at(
            &fixture.workspace, mission, "other-task", &fixture.cache,
        ).unwrap();
        let different_mission = CheckRuntimeResources::prepare_at(
            &fixture.workspace, "other-mission", task, &fixture.cache,
        ).unwrap();
        assert_ne!(different_task.root, root);
        assert_ne!(different_mission.root, root);
        let input = first.input();
        assert_eq!(input["schema_version"], "mission_check_runtime.v1");
        assert_eq!(input["source"], "host");
        assert_eq!(input["authority"], "host_owned_generated_outputs");
        assert_eq!(input["mission_id"], mission);
        assert_eq!(input["task_id"], task);
        assert!(input["allowed_operations"].to_string().contains("not business source authorization"));
        assert!(input["retention"].as_str().unwrap().contains("no automatic deletion"));
        assert!(input.get("approved_absolute_paths").is_none());
        drop((first, again, different_task, different_mission));
        assert_eq!(fs::read_to_string(root.join("evidence.json")).unwrap(), "retained");
    }

    #[test]
    fn invalid_roots_and_workspace_overlap_are_rejected_before_creation() {
        let fixture = Fixture::new();
        let file = fixture.root.join("file");
        fs::write(&file, "not a directory").unwrap();
        let internal = fixture.workspace.join("must-not-be-created");
        for base in [PathBuf::from("relative-cache"), fixture.root.join("../escape"), file, internal.clone()] {
            assert!(CheckRuntimeResources::prepare_at(&fixture.workspace, "m", "t", &base).is_err());
        }
        assert!(!internal.exists());
        let task_root = fixture.cache
            .join(format!("{:x}", Sha256::digest(b"m")))
            .join(format!("{:x}", Sha256::digest(b"t")));
        let nested_workspace = task_root.join("nested-workspace");
        fs::create_dir_all(&nested_workspace).unwrap();
        for workspace in [&task_root, &nested_workspace, &fixture.root.join("missing")] {
            assert!(CheckRuntimeResources::prepare_at(workspace, "m", "t", &fixture.cache).is_err());
        }
        assert!(!task_root.join("tmp").exists());
        assert!(CheckRuntimeResources::prepare_at(&fixture.workspace, "", "t", &fixture.cache).is_err());
    }

    #[test]
    fn apply_overrides_child_root_hints_without_changing_parent_environment() {
        let fixture = Fixture::new();
        let keys = [
            "HIMIND_MISSION_CHECK_ROOT", "SERVICE_UI_RUNTIME_ROOT", "TEMP", "TMP", "TMPDIR",
            "npm_config_cache", "npm_config_update_notifier", "PYTHONDONTWRITEBYTECODE",
        ];
        let before = keys.map(std::env::var_os);
        let resources = fixture.prepare();
        let mut child = Command::new("node");
        child.env("HIMIND_MISSION_CHECK_ROOT", &fixture.workspace)
            .env("SERVICE_UI_RUNTIME_ROOT", &fixture.workspace);
        assert!(resources.apply_for_workspace(&fixture.root, &mut child).is_err());
        resources.apply(&mut child).unwrap();
        let env = child.get_envs().filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(env[std::ffi::OsStr::new("HIMIND_MISSION_CHECK_ROOT")], resources.root.as_os_str());
        assert_eq!(env[std::ffi::OsStr::new("SERVICE_UI_RUNTIME_ROOT")], resources.root.as_os_str());
        assert_eq!(before, keys.map(std::env::var_os));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn redirected_cache_ancestors_and_existing_outputs_are_rejected() {
        let fixture = Fixture::new();
        let outside = fixture.root.join("redirect-target");
        fs::create_dir(&outside).unwrap();
        let redirect = fixture.root.join("redirect");
        redirect_directory(&outside, &redirect);
        let result = CheckRuntimeResources::prepare_at(
            &fixture.workspace, "m", "t", &redirect.join("missing-cache"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        remove_redirect(&redirect);

        let resources = fixture.prepare();
        let redirect = resources.root.join("redirected-output");
        redirect_directory(&outside, &redirect);
        let mut child = Command::new("node");
        assert!(resources.apply(&mut child).is_err());
        assert!(CheckRuntimeResources::prepare_at(
            &fixture.workspace, "mission", "task", &fixture.cache,
        ).is_err());
        remove_redirect(&redirect);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_ordinary_disk_syntax_and_reject_device_aliases() {
        assert_eq!(ordinary_absolute(Path::new(r"\\?\C:\safe\root")).unwrap(), Path::new(r"C:\safe\root"));
        for path in [r"\\server\share\cache", r"\\.\C:\cache", r"C:\cache:stream", r"C:\cache.\root", r"C:\NUL\root"] {
            assert!(ordinary_absolute(Path::new(path)).is_err(), "{path}");
        }
    }
}
