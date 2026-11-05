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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    keeper_stop: Arc<AtomicBool>,
    _lock: File,
}

impl ServiceInstance {
    pub(crate) fn discovery(&self) -> &ServiceDiscovery {
        &self.discovery
    }

    pub(crate) fn discovery_path(&self) -> &Path {
        &self.discovery_path
    }

    /// 起一个守护线程，持续保证发现记录在磁盘上。
    ///
    /// 记录只在启动时写一次，而它是可以被外部删除的：桥接侧把「进程已死」当成
    /// 唯一判据后不会再误删活记录，但旧版本、清理脚本、手工操作都还能删掉它。
    /// 记录一旦消失，服务虽然活着，任何新桥接都看不见它，只能去 spawn 一个
    /// 新服务，而新服务会因为单例锁直接退出（`exit code: 1`），故障就一直留着。
    ///
    /// 不能在 accept 循环里补写：`accept` 会阻塞到下一个客户端连进来，而那正是
    /// 「记录已丢、没有客户端能连」的死结。用独立线程按固定间隔检查才是自愈。
    ///
    /// `Drop` 会先置停止标志再删记录，避免线程在服务退出后把记录重新写出来。
    pub(crate) fn spawn_discovery_keeper(&self) {
        let path = self.discovery_path.clone();
        let content = serde_json::to_vec_pretty(&self.discovery).unwrap_or_default();
        let stop = Arc::clone(&self.keeper_stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if !path.is_file() {
                    let _ = atomic_write(&path, &content);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    }
}

impl Drop for ServiceInstance {
    fn drop(&mut self) {
        self.keeper_stop.store(true, Ordering::Relaxed);
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
        started_at_unix: process_started_at_unix(std::process::id()).unwrap_or_else(unix_now),
    };
    let discovery_path = scope.discovery_path(runtime_dir);
    atomic_write(&discovery_path, &serde_json::to_vec_pretty(&discovery)?)?;
    Ok(ServiceStartOutcome::Started(ServiceInstance {
        discovery,
        discovery_path,
        keeper_stop: Arc::new(AtomicBool::new(false)),
        _lock: lock,
    }))
}

pub(crate) fn read_discovery(runtime_dir: &Path) -> Option<ServiceDiscovery> {
    let content = fs::read(runtime_dir.join("service.json")).ok()?;
    serde_json::from_slice(&content).ok()
}

/// 发现记录对应的服务是否真的还在。
///
/// `service.json` 是崩溃或被杀后的残留：ServiceInstance 的 Drop 在强制终止时不会执行，
/// 而 MCP 服务又是桥接的子进程，桥接一退出服务就跟着死，文件却留在磁盘上。
/// 只读文件不校验存活会让桥接永久卡在一个指向死进程的记录上，症状是
/// “系统找不到指定的文件 (os error 2)”，与配置错误无法区分。
///
/// 判据只看进程本身（存活 + 启动时刻与记录相符），**不碰管道**：
/// - 命名管道的每个实例只能被连一次，探测性连接会被服务的 accept 循环当成客户端收下，
///   服务读到半截连接就报 `MCP bridge closed before hello`，密集探测能把服务打掉
///   （实测 46 次零权限 open/close 即让 `--mcp-service` 以 os error 232 退出，
///   同时 Drop 删掉 service.json，于是故障从「可自愈」变成「永久不可用」）；
/// - 已有客户端占着连接时，同端点的 open 会返回 `ERROR_PIPE_BUSY`，
///   把「忙」当成「不可达」会让多客户端场景把活记录判成残留（实测 300 次探测里 292 次为 BUSY）。
///
/// 启动时刻用来防 pid 复用：进程号会被回收，只有「同一个 pid + 同一个启动时刻」
/// 才说明记录指向的就是当初那个服务。
fn discovery_is_live(discovery: &ServiceDiscovery) -> bool {
    process_matches_discovery(discovery.pid, discovery.started_at_unix)
}

/// 只在服务真实存活时返回发现记录；残留记录会被清掉并返回 `None`。
pub(crate) fn read_live_discovery(runtime_dir: &Path) -> Option<ServiceDiscovery> {
    let discovery = read_discovery(runtime_dir)?;
    if discovery_is_live(&discovery) {
        return Some(discovery);
    }
    discard_stale_discovery(runtime_dir);
    None
}

/// 丢弃指向已死服务的发现记录，避免下一轮继续读到它。
fn discard_stale_discovery(runtime_dir: &Path) {
    let path = runtime_dir.join("service.json");
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

#[cfg(windows)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0_u32;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE as u32
    }
}

/// 进程存活且启动时刻与发现记录一致。
#[cfg(windows)]
pub(crate) fn process_started_at_unix(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let filetime = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
        filetime
            .checked_div(10_000_000)
            .and_then(|seconds| seconds.checked_sub(WINDOWS_TO_UNIX_SECONDS))
    }
}

#[cfg(windows)]
fn process_matches_discovery(pid: u32, started_at_unix: u64) -> bool {
    let Some(expected_started_at) = nonzero_unix(started_at_unix) else {
        return false;
    };
    let Some(actual_started_at) = process_started_at_unix(pid) else {
        return false;
    };
    actual_started_at.abs_diff(expected_started_at) <= 1
}

#[cfg(not(windows))]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(windows))]
pub(crate) fn process_started_at_unix(pid: u32) -> Option<u64> {
    process_is_alive(pid).then_some(unix_now())
}

#[cfg(not(windows))]
fn process_matches_discovery(pid: u32, started_at_unix: u64) -> bool {
    nonzero_unix(started_at_unix).is_some() && process_is_alive(pid)
}

fn nonzero_unix(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

// 这里刻意没有「探测端点是否可连」的辅助函数：命名管道的每个实例只能被连一次，
// 探测性 open 会被服务当成真实客户端收下并报 `MCP bridge closed before hello`；
// 已有客户端占着连接时它还会返回 `ERROR_PIPE_BUSY`。真正的可用性判定交给
// `mcp_pipe::connect` 的重试，它区分 BUSY 与 NOT_FOUND。

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
        if discovery_is_live(&discovery) {
            return Ok(discovery);
        }
        // 残留记录：服务进程已死或管道不通。清掉它，让下面的逻辑重新拉起服务，
        // 而不是把一个死端点交给调用方。
        discard_stale_discovery(&runtime_dir);
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
            if discovery_is_live(&discovery) {
                return Ok(discovery);
            }
        }
        if let Some(status) = child.try_wait()? {
            // Several OpenCode workspaces can launch the bridge at the same time.
            // Only one child wins the service lock; the others exit quickly. Do not
            // fail those bridges before the winner has published discovery.
            if let Some(discovery) = wait_for_live_discovery(&runtime_dir, Duration::from_secs(2)) {
                return Ok(discovery);
            }
            return Err(format!("HiMind Agent service exited during startup: {status}").into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("timed out waiting for HiMind Agent service startup".into())
}

fn wait_for_live_discovery(runtime_dir: &Path, timeout: Duration) -> Option<ServiceDiscovery> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(discovery) = read_live_discovery(runtime_dir) {
            return Some(discovery);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_runtime_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("himind-ipc-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn discovery_for(pid: u32, endpoint: &str) -> ServiceDiscovery {
        ServiceDiscovery {
            schema_version: DISCOVERY_SCHEMA_VERSION,
            protocol_version: IPC_PROTOCOL_VERSION,
            pid,
            profile: "production".to_string(),
            identity_hash: "identity".to_string(),
            state_hash: "state".to_string(),
            endpoint: endpoint.to_string(),
            token: "token".to_string(),
            agent_version: "test".to_string(),
            started_at_unix: process_started_at_unix(std::process::id()).unwrap_or_else(unix_now),
        }
    }

    /// 死进程留下的发现记录必须被识别为 stale 并清掉，否则桥接会永久指向死端点。
    #[test]
    fn stale_discovery_with_dead_pid_is_discarded() {
        let dir = temp_runtime_dir("stale-pid");
        let dead_pid = u32::MAX - 1;
        let record = discovery_for(dead_pid, "\\\\.\\pipe\\himind-agent-definitely-absent");
        fs::write(
            dir.join("service.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        assert!(read_live_discovery(&dir).is_none());
        assert!(
            !dir.join("service.json").exists(),
            "stale discovery record must be removed"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 记录指向一个活着、但不是当初那个进程的 pid（pid 复用）同样不算可用。
    #[test]
    fn discovery_pointing_at_a_reused_pid_is_discarded() {
        let dir = temp_runtime_dir("pid-reuse");
        let mut record = discovery_for(std::process::id(), "\\\\.\\pipe\\himind-agent-live-pid");
        // 当前进程绝不是 started_at_unix=1 那一刻启动的。
        record.started_at_unix = 1;
        fs::write(
            dir.join("service.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        assert!(read_live_discovery(&dir).is_none());
        assert!(!dir.join("service.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 关键回归：指向「本进程、本时刻」的活记录必须原样保留。
    ///
    /// 早期修复用「零权限打开端点」判断存活，会在管道忙时把活服务判成残留，
    /// 也会把探测连接当客户端塞给服务（服务读到半截连接就退出）。这条测试锁住
    /// 「活记录不删、不碰管道」这个行为。
    #[test]
    fn competing_bridge_waits_for_winner_discovery() {
        let dir = temp_runtime_dir("competing-start");
        let publisher_dir = dir.clone();
        let publisher = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let record = discovery_for(
                std::process::id(),
                "\\\\.\\pipe\\himind-agent-competing-winner",
            );
            fs::write(
                publisher_dir.join("service.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        });
        let live = wait_for_live_discovery(&dir, std::time::Duration::from_secs(2))
            .expect("competitor should observe the winning discovery");
        assert_eq!(live.pid, std::process::id());
        publisher.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn live_discovery_for_this_process_is_kept() {
        let dir = temp_runtime_dir("live");
        let record = discovery_for(
            std::process::id(),
            "\\\\.\\pipe\\himind-agent-probe-must-not-connect",
        );
        let path = dir.join("service.json");
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

        let live = read_live_discovery(&dir).expect("live record must be returned");
        assert_eq!(live.pid, std::process::id());
        assert!(path.exists(), "live discovery record must not be deleted");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_discovery_returns_none_without_error() {
        let dir = temp_runtime_dir("absent");
        assert!(read_live_discovery(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_process_is_alive_and_zero_pid_is_not() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(0));
    }

    #[test]
    fn current_process_matches_a_fresh_record_and_not_an_old_one() {
        let started_at = process_started_at_unix(std::process::id()).expect("process start time");
        assert!(process_matches_discovery(std::process::id(), started_at));
        assert!(!process_matches_discovery(std::process::id(), 1));
        assert!(!process_matches_discovery(0, unix_now()));
        assert!(!process_matches_discovery(std::process::id(), 0));
    }
}
