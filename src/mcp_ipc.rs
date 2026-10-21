use fs4::fs_std::FileExt;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const IPC_PROTOCOL_VERSION: u32 = 1;
pub(crate) const DISCOVERY_SCHEMA_VERSION: u32 = 1;
const MAX_IPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceScope {
    identity_hash: String,
    profile: String,
    state_hash: String,
}

impl ServiceScope {
    pub(crate) fn for_current_user(
        profile: &str,
        state_path: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        Self::for_identity(&current_user_identity()?, profile, state_path)
    }

    pub(crate) fn for_identity(
        identity: &str,
        profile: &str,
        state_path: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        let profile = profile.trim();
        if profile.is_empty() {
            return Err("MCP service profile must not be empty".into());
        }
        Ok(Self {
            identity_hash: sha256_hex(identity.as_bytes()),
            profile: profile.to_string(),
            state_hash: sha256_hex(
                canonical_state_path(state_path)
                    .as_os_str()
                    .as_encoded_bytes(),
            ),
        })
    }

    pub(crate) fn profile(&self) -> &str {
        &self.profile
    }

    pub(crate) fn state_hash(&self) -> &str {
        &self.state_hash
    }

    pub(crate) fn identity_hash(&self) -> &str {
        &self.identity_hash
    }

    pub(crate) fn slug(&self) -> String {
        format!(
            "{}-{}-{}",
            sanitize_component(&self.profile),
            &self.identity_hash[..16],
            &self.state_hash[..16]
        )
    }

    pub(crate) fn runtime_dir(&self, agent_home: &Path) -> PathBuf {
        agent_home
            .join("runtime")
            .join("mcp-service")
            .join(self.slug())
    }

    pub(crate) fn discovery_path(&self, runtime_dir: &Path) -> PathBuf {
        runtime_dir.join("service.json")
    }

    pub(crate) fn lock_path(&self, runtime_dir: &Path) -> PathBuf {
        runtime_dir.join("service.lock")
    }

    pub(crate) fn endpoint(&self, runtime_dir: &Path) -> String {
        #[cfg(windows)]
        {
            let _ = runtime_dir;
            format!(r"\\.\pipe\himind-agentd-{}", self.slug())
        }
        #[cfg(not(windows))]
        {
            runtime_dir
                .join("agentd.sock")
                .to_string_lossy()
                .to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ServiceDiscovery {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub pid: u32,
    pub profile: String,
    pub identity_hash: String,
    pub state_hash: String,
    pub endpoint: String,
    pub token: String,
    pub agent_version: String,
    pub started_at_unix: u64,
}

pub(crate) struct ServiceInstance {
    discovery: ServiceDiscovery,
    discovery_path: PathBuf,
    _lock: File,
}

impl ServiceInstance {
    pub(crate) fn discovery(&self) -> &ServiceDiscovery {
        &self.discovery
    }

    pub(crate) fn discovery_path(&self) -> &Path {
        &self.discovery_path
    }
}

impl Drop for ServiceInstance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.discovery_path);
    }
}

pub(crate) enum ServiceStartOutcome {
    Started(ServiceInstance),
    AlreadyRunning(Option<ServiceDiscovery>),
}

pub(crate) fn start_service(
    scope: &ServiceScope,
    runtime_dir: &Path,
    agent_version: &str,
) -> Result<ServiceStartOutcome, Box<dyn Error>> {
    fs::create_dir_all(runtime_dir)?;
    let lock_path = scope.lock_path(runtime_dir);
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    match lock.try_lock_exclusive() {
        Ok(true) => {}
        Ok(false) => {
            return Ok(ServiceStartOutcome::AlreadyRunning(
                read_discovery_with_retry(runtime_dir),
            ))
        }
        Err(error) if is_lock_conflict(&error) => {
            return Ok(ServiceStartOutcome::AlreadyRunning(
                read_discovery_with_retry(runtime_dir),
            ))
        }
        Err(error) => return Err(error.into()),
    }

    let discovery = ServiceDiscovery {
        schema_version: DISCOVERY_SCHEMA_VERSION,
        protocol_version: IPC_PROTOCOL_VERSION,
        pid: std::process::id(),
        profile: scope.profile.clone(),
        identity_hash: scope.identity_hash.clone(),
        state_hash: scope.state_hash.clone(),
        endpoint: scope.endpoint(runtime_dir),
        token: random_token(),
        agent_version: agent_version.to_string(),
        started_at_unix: unix_now(),
    };
    let discovery_path = scope.discovery_path(runtime_dir);
    atomic_write(&discovery_path, &serde_json::to_vec_pretty(&discovery)?)?;
    Ok(ServiceStartOutcome::Started(ServiceInstance {
        discovery,
        discovery_path,
        _lock: lock,
    }))
}

pub(crate) fn read_discovery(runtime_dir: &Path) -> Option<ServiceDiscovery> {
    let content = fs::read(runtime_dir.join("service.json")).ok()?;
    serde_json::from_slice(&content).ok()
}

pub(crate) fn ensure_service_running(
    profile: &str,
    state_path: &Path,
    agent_home: &Path,
    service_executable: &Path,
    mode: Option<&str>,
    api_base: Option<&str>,
) -> Result<ServiceDiscovery, Box<dyn Error>> {
    let scope = ServiceScope::for_current_user(profile, state_path)?;
    let runtime_dir = scope.runtime_dir(agent_home);
    if let Some(discovery) = read_discovery(&runtime_dir) {
        return Ok(discovery);
    }
    if !service_executable.is_file() {
        return Err(format!(
            "HiMind Agent service executable is missing: {}",
            service_executable.display()
        )
        .into());
    }
    let mut command = Command::new(service_executable);
    command
        .arg("--mcp-service")
        .arg("--profile")
        .arg(profile)
        .arg("--state")
        .arg(state_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(mode) = mode {
        command.arg("--mode").arg(mode);
    }
    if let Some(api_base) = api_base {
        command.arg("--api").arg(api_base);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    for _ in 0..200 {
        if let Some(discovery) = read_discovery(&runtime_dir) {
            return Ok(discovery);
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("HiMind Agent service exited during startup: {status}").into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("timed out waiting for HiMind Agent service startup".into())
}

pub(crate) fn read_discovery_with_retry(runtime_dir: &Path) -> Option<ServiceDiscovery> {
    for _ in 0..40 {
        if let Some(discovery) = read_discovery(runtime_dir) {
            return Some(discovery);
        }
        thread::sleep(Duration::from_millis(25));
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServiceFrame {
    Hello {
        protocol_version: u32,
        token: String,
        profile: String,
        state_hash: String,
        client: Value,
    },
    Mcp {
        session_id: String,
        request_id: u64,
        payload: Value,
    },
    Cancel {
        session_id: String,
        request_id: u64,
        reason: Option<String>,
    },
    Ping {
        request_id: u64,
    },
    Close {
        session_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServiceEvent {
    HelloOk {
        protocol_version: u32,
        service_pid: u32,
        agent_version: String,
        session_id: String,
        registry_watchers: usize,
    },
    Mcp {
        session_id: String,
        request_id: u64,
        payload: Value,
    },
    Progress {
        session_id: String,
        request_id: u64,
        payload: Value,
    },
    Notification {
        session_id: String,
        payload: Value,
    },
    Error {
        session_id: Option<String>,
        request_id: Option<u64>,
        code: String,
        message: String,
    },
    Pong {
        request_id: u64,
    },
}

pub(crate) fn write_frame<T: Serialize, W: Write>(
    writer: &mut W,
    value: &T,
) -> Result<(), Box<dyn Error>> {
    let mut frame = serde_json::to_vec(value)?;
    if frame.len() > MAX_IPC_FRAME_BYTES {
        return Err(format!("IPC frame exceeds {MAX_IPC_FRAME_BYTES} bytes").into());
    }
    frame.push(b'\n');
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn read_frame<T: DeserializeOwned, R: BufRead>(
    reader: &mut R,
) -> Result<Option<T>, Box<dyn Error>> {
    let mut frame = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            if frame.is_empty() {
                return Ok(None);
            }
            break;
        }
        frame.push(byte[0]);
        if frame.len() > MAX_IPC_FRAME_BYTES {
            return Err(format!("IPC frame exceeds {MAX_IPC_FRAME_BYTES} bytes").into());
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    while matches!(frame.last(), Some(b'\r' | b'\n')) {
        frame.pop();
    }
    Ok(Some(serde_json::from_slice(&frame)?))
}

fn is_lock_conflict(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(33 | 35))
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_state_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        let parent = path.parent().and_then(|parent| parent.canonicalize().ok());
        match (parent, path.file_name()) {
            (Some(parent), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    })
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

#[cfg(windows)]
fn current_user_identity() -> Result<String, Box<dyn Error>> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: windows_sys::Win32::Foundation::HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut required = 0_u32;
        let _ = GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required);
        if required == 0 {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error().into());
        }
        let mut buffer = vec![0_u8; required as usize];
        if GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(error.into());
        }
        let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_string = null_mut();
        if ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(error.into());
        }
        let mut length = 0_usize;
        while *sid_string.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, length));
        LocalFree(sid_string as _);
        CloseHandle(token);
        Ok(sid)
    }
}

#[cfg(not(windows))]
fn current_user_identity() -> Result<String, Box<dyn Error>> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .map(|value| format!("user:{value}"))
        .map_err(|error| error.into())
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}
