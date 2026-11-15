use reqwest::blocking::Client;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::client::{is_task_canceled_error, renew_agent_run_lease, TaskCancelGuard};
use crate::api::types::AgentRunClaim;
use crate::Options;

const OUTPUT_CAPTURE_LIMIT: usize = 256 * 1024;
const OUTPUT_HEAD_LIMIT: usize = 64 * 1024;
// Diagnostic retention and protocol framing are different budgets. Large tool
// records spill to a private temporary file; they are never truncated and then
// interpreted as JSON. Keep a separate finite disk ceiling for hostile output.
const PROTOCOL_RECORD_LIMIT: usize = 64 * 1024 * 1024;
const ERROR_DETAIL_LIMIT: usize = 4_000;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) struct RunLeaseRenewal {
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

pub(crate) fn canonical_workspace(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = if value.trim().is_empty() {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir()?)
    } else {
        PathBuf::from(value.trim())
    };
    let workspace = path
        .canonicalize()
        .map_err(|error| format!("Agent Run workspace is unavailable: {error}"))?;
    if !workspace.is_dir() {
        return Err("Agent Run workspace is not a directory".into());
    }
    #[cfg(windows)]
    {
        // CreateProcessW's current-directory argument still rejects a long
        // canonical name on supported Windows hosts. Use this existing
        // directory's OS alias; never relocate, create a link or relax scope.
        use std::os::windows::ffi::{OsStrExt,OsStringExt};
        if workspace.as_os_str().encode_wide().count()>=248 {
            #[link(name="kernel32")]
            extern "system" {fn GetShortPathNameW(long:*const u16,short:*mut u16,size:u32)->u32;}
            let long:Vec<u16>=workspace.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut short=vec![0u16;32768];
            let len=unsafe {GetShortPathNameW(long.as_ptr(),short.as_mut_ptr(),short.len() as u32)} as usize;
            if len==0 || len>=short.len() {return Err("runtime workspace has no usable Windows short path".into());}
            let name=std::ffi::OsString::from_wide(&short[..len]).to_string_lossy().into_owned();
            let name=if let Some(unc)=name.strip_prefix(r"\\?\UNC\") {format!(r"\\{unc}")}
                else {name.strip_prefix(r"\\?\").unwrap_or(&name).to_string()};
            let alias=PathBuf::from(name);
            if alias.as_os_str().encode_wide().count()>=248 || alias.canonicalize()?!=workspace {
                return Err("runtime workspace Windows alias is too long or changed identity".into());
            }
            return Ok(alias);
        }
    }
    Ok(workspace)
}

pub(crate) fn verify_command(
    executable: &OsStr,
    arguments: &[&str],
) -> Result<String, Box<dyn Error>> {
    verify_command_with_cancel(executable, arguments, Duration::from_secs(5), &|| Ok(false))
}

/// Discovery is part of execution: an installed-but-hung CLI must not block cancel.
pub(crate) fn verify_command_with_cancel(
    executable: &OsStr,
    arguments: &[&str],
    timeout: Duration,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    if is_canceled()? { return Err("runtime preflight was canceled".into()); }
    let mut command = hidden_command(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    remove_himind_secret_environment(&mut command);
    configure_hidden_process(&mut command);
    let (mut child, _tree) = crate::runtime::owned_tree::OwnedTree::spawn(&mut command)?;
    let stdout = child.stdout.take().map(|pipe| capture_runtime_output(pipe, false, |_| true));
    let stderr = child.stderr.take().map(|pipe| capture_runtime_output(pipe, false, |_| true));
    let result = (|| {
        let started = Instant::now();
        let status = loop {
            if is_canceled()? { return Err("runtime preflight was canceled".into()); }
            if let Some(status) = child.try_wait()? { break status; }
            if started.elapsed() >= timeout { return Err("runtime preflight exceeded its deadline".into()); }
            thread::sleep(Duration::from_millis(20));
        };
        let drain = Instant::now();
        while stdout.as_ref().is_some_and(|p| !p.finished()) || stderr.as_ref().is_some_and(|p| !p.finished()) {
            if is_canceled()? { return Err("runtime preflight was canceled".into()); }
            if drain.elapsed() >= Duration::from_millis(500) { break; }
            thread::sleep(Duration::from_millis(10));
        }
        let stdout = stdout.map(|p| p.finish()).transpose()?.unwrap_or_default();
        let stderr = stderr.map(|p| p.finish()).transpose()?.unwrap_or_default();
        if !status.success() {
            // CLI diagnostics can contain credentials; fixed metadata is sufficient here.
            return Err(format!("command preflight failed (exit={})", status.code().unwrap_or(-1)).into());
        }
        Ok(format!("{stdout}\n{stderr}").trim().to_string())
    })();
    if result.is_err() { terminate_process_tree(&mut child); }
    result
}

/// A finite file-backed input cannot deadlock on a child's pipe consumption.
/// The file has no persistent name after the last inherited handle is closed.
pub(crate) fn prompt_input(prompt: &str) -> Result<std::fs::File, Box<dyn Error>> {
    const INPUT_LIMIT: usize = 16 * 1024 * 1024;
    if prompt.len() > INPUT_LIMIT { return Err("runtime prompt exceeds 16 MiB input budget".into()); }
    let path = safe_temp_path(&format!("input-{:016x}", rand::random::<u64>()), "stdin")?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE};
        options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE | FILE_ATTRIBUTE_TEMPORARY);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    #[cfg(not(windows))]
    std::fs::remove_file(&path)?;
    file.write_all(prompt.as_bytes())?;
    file.rewind()?;
    Ok(file)
}

pub(crate) fn remove_himind_secret_environment(command: &mut Command) {
    for key in [
        "HIMIND_AGENT_ENROLLMENT_TOKEN",
        "HIMIND_CHANNEL_ADAPTER_HMAC_KEY",
        "HIMIND_AI_INFERENCE_SERVICE_KEY",
        "HIMIND_MODEL_GATEWAY_KEY",
        "AI_GATEWAY_API_KEY",
        "LITELLM_MASTER_KEY",
        "LLM_API_KEY",
        "DASHBOARD_COOKIE",
        "DASHBOARD_ACCESS_TOKEN",
        "DASHBOARD_REFRESH_TOKEN",
    ] {
        command.env_remove(key);
    }
}

pub(crate) fn wait_for_child(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task_id: &str,
    child: &mut Child,
    timeout_environment: &str,
    default_timeout_seconds: u64,
    runtime_name: &str,
) -> Result<ExitStatus, Box<dyn Error>> {
    let timeout = configured_timeout(timeout_environment, default_timeout_seconds, 60);
    let started = Instant::now();
    let mut cancel_guard = TaskCancelGuard::new();
    loop {
        if let Err(error) = cancel_guard.check(client, options, agent_id, task_id) {
            if is_task_canceled_error(&error.to_string()) {
                terminate_process_tree(child);
                return Err(error);
            }
            eprintln!("Agent Run task {task_id} cancellation check failed: {error}");
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            terminate_process_tree(child);
            return Err(format!(
                "{runtime_name} execution exceeded {} seconds and was terminated",
                timeout.as_secs()
            )
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn wait_for_child_with_timeout_and_cancel<F>(
    child: &mut Child,
    timeout_environment: &str,
    default_timeout_seconds: u64,
    runtime_name: &str,
    mut is_canceled: F,
) -> Result<ExitStatus, Box<dyn Error>>
where
    F: FnMut() -> Result<bool, Box<dyn Error>>,
{
    let timeout = configured_timeout(timeout_environment, default_timeout_seconds, 1);
    wait_for_child_until_with_cancel(child, timeout, runtime_name, &mut is_canceled)
}

#[cfg(test)]
fn wait_for_child_until(
    child: &mut Child,
    timeout: Duration,
    runtime_name: &str,
) -> Result<ExitStatus, Box<dyn Error>> {
    wait_for_child_until_with_cancel(child, timeout, runtime_name, &mut || Ok(false))
}

pub(crate) fn wait_for_child_until_with_cancel<F>(
    child: &mut Child,
    timeout: Duration,
    runtime_name: &str,
    is_canceled: &mut F,
) -> Result<ExitStatus, Box<dyn Error>>
where
    F: FnMut() -> Result<bool, Box<dyn Error>>,
{
    let started = Instant::now();
    loop {
        match is_canceled() {
            Ok(false) => {},
            result => {
                terminate_process_tree(child);
                result?;
                return Err(format!("{runtime_name} execution was canceled").into());
            }
        }
        if let Some(status) = child.try_wait()? { return Ok(status); }
        if started.elapsed() >= timeout {
            terminate_process_tree(child);
            return Err(format!(
                "{runtime_name} execution exceeded {} seconds and was terminated",
                timeout.as_secs()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn configured_timeout(
    timeout_environment: &str,
    default_timeout_seconds: u64,
    minimum_seconds: u64,
) -> Duration {
    let seconds = env::var(timeout_environment)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= minimum_seconds)
        .unwrap_or(default_timeout_seconds);
    Duration::from_secs(seconds)
}

pub(crate) fn start_run_lease_renewal(
    client: &Client,
    options: &Options,
    agent_id: &str,
    claim: &AgentRunClaim,
) -> RunLeaseRenewal {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let client = client.clone();
    let api_base = options.api_base().clone();
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

pub(crate) fn capture_output<R: Read + Send + 'static>(
    mut reader: R,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut head = Vec::new();
        let mut tail = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let chunk = &buffer[..size];
                    if head.len() < OUTPUT_HEAD_LIMIT {
                        let remaining = OUTPUT_HEAD_LIMIT - head.len();
                        let copied = remaining.min(chunk.len());
                        head.extend_from_slice(&chunk[..copied]);
                        if copied == chunk.len() {
                            continue;
                        }
                        tail.extend_from_slice(&chunk[copied..]);
                    } else {
                        tail.extend_from_slice(chunk);
                    }
                    let tail_limit = OUTPUT_CAPTURE_LIMIT - OUTPUT_HEAD_LIMIT;
                    if tail.len() > tail_limit {
                        tail.drain(..tail.len() - tail_limit);
                    }
                }
                Err(_) => break,
            }
        }
        let mut captured = head;
        if !tail.is_empty() {
            captured.extend_from_slice(b"\n...[output truncated]...\n");
            captured.extend_from_slice(&tail);
        }
        String::from_utf8_lossy(&captured).trim().to_string()
    })
}

pub(crate) fn join_output(handle: Option<thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|value| value.join().ok())
        .unwrap_or_default()
}

/// Pipe capture for runtimes whose descendants can retain stdout/stderr after
/// the root exits. Windows polls available pipe bytes, so stopping this reader
/// never depends on an inherited writer closing its handle.
#[cfg(windows)]
pub(crate) trait RuntimePipe: Read + Send + std::os::windows::io::AsRawHandle {}
#[cfg(windows)]
impl<T: Read + Send + std::os::windows::io::AsRawHandle> RuntimePipe for T {}
#[cfg(not(windows))]
pub(crate) trait RuntimePipe: Read + Send {}
#[cfg(not(windows))]
impl<T: Read + Send> RuntimePipe for T {}

pub(crate) struct RuntimeOutputCapture {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    output: Arc<std::sync::Mutex<RuntimeCaptureText>>,
    overflowed: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct RuntimeCaptureText {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    truncated: bool,
}

impl RuntimeCaptureText {
    fn append(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(OUTPUT_HEAD_LIMIT.saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..count]);
        self.tail.extend(&bytes[count..]);
        let limit = OUTPUT_CAPTURE_LIMIT - OUTPUT_HEAD_LIMIT;
        if self.tail.len() > limit {
            self.tail.drain(..self.tail.len() - limit);
            self.truncated = true;
        }
    }
    fn text(&self) -> String {
        let mut bytes = self.head.clone();
        if self.truncated { bytes.extend_from_slice(b"\n...[diagnostic output truncated]...\n"); }
        bytes.extend(self.tail.iter());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl RuntimeOutputCapture {
    pub(crate) fn finished(&self) -> bool { self.finished.load(Ordering::Acquire) }
    pub(crate) fn finish_complete(self) -> Result<String, Box<dyn Error>> {
        if self.output.lock().map_err(|_|"runtime capture lock poisoned")?.truncated {
            return Err("runtime final output exceeded the capture budget".into());
        }
        self.finish()
    }
    pub(crate) fn finish(mut self) -> Result<String, Box<dyn Error>> {
        if !self.finished() { return Err("runtime output pipe did not reach EOF within its drain deadline".into()); }
        if let Some(handle) = self.handle.take() { handle.join().map_err(|_| "runtime output reader panicked")?; }
        if self.failed.load(Ordering::Acquire) { return Err("runtime output pipe could not be read".into()); }
        if self.overflowed.load(Ordering::Acquire) { return Err("a runtime protocol record exceeded the 64 MiB safety limit".into()); }
        Ok(self.output.lock().map_err(|_| "runtime output capture lock poisoned")?.text())
    }
}

impl Drop for RuntimeOutputCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Windows pipe polling and protocol parsing both observe stop. Joining
        // here guarantees private spool files have closed before cancellation
        // returns, without waiting on a descendant's inherited writer.
        #[cfg(windows)]
        if let Some(handle) = self.handle.take() { let _ = handle.join(); }
        // Only join an already finished reader. Never strand the dispatch lock
        // on a descendant's pipe handle (also fail closed on non-Windows).
        if self.handle.as_ref().is_some_and(|handle| handle.is_finished()) {
            if let Some(handle) = self.handle.take() { let _ = handle.join(); }
        }
    }
}

/// One complete protocol frame. File-backed records use create_new and an
/// exclusive handle, and disappear on drop (including cancellation/parse error).
#[derive(Default)]
struct RuntimeProtocolRecord {
    memory: Vec<u8>,
    spool: Option<ProtocolSpool>,
    len: usize,
    validation: ProtocolValidation,
}

/// Bound strings that the typed control parser materializes, while leaving
/// arbitrary tool payload strings streamable. Also validate UTF-8 in ignored
/// fields: serde's IgnoredAny deliberately does not perform that check.
#[derive(Default)]
struct ProtocolValidation {
    utf8_tail: Vec<u8>,
    frames: Vec<ProtocolFrame>,
    started: bool,
    in_string: bool,
    escaped: bool,
    key_string: bool,
    key_bytes: Vec<u8>,
    string_len: usize,
    string_limit: usize,
}

struct ProtocolFrame { path: u8, object: bool, expects_key: bool, key: String }

impl ProtocolValidation {
    fn invalid() -> std::io::Error { std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid or over-budget protocol field") }
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut utf8 = std::mem::take(&mut self.utf8_tail);
        utf8.extend_from_slice(bytes);
        if let Err(error) = std::str::from_utf8(&utf8) {
            if error.error_len().is_some() { return Err(Self::invalid()); }
            self.utf8_tail.extend_from_slice(&utf8[error.valid_up_to()..]);
        }
        for &byte in bytes {
            if self.in_string {
                self.string_len += 1;
                if self.string_len > self.string_limit { return Err(Self::invalid()); }
                if self.key_string { self.key_bytes.push(byte); }
                if self.escaped { self.escaped = false; }
                else if byte == b'\\' { self.escaped = true; }
                else if byte == b'"' {
                    self.in_string = false;
                    if self.key_string {
                        let key: String = serde_json::from_slice(&self.key_bytes).map_err(|_| Self::invalid())?;
                        if let Some(frame) = self.frames.last_mut() { frame.key = key; frame.expects_key = false; }
                    }
                }
                continue;
            }
            if !self.started && !byte.is_ascii_whitespace() {
                if byte != b'{' { return Err(Self::invalid()); }
                self.started = true;
            }
            match byte {
                b'"' => {
                    self.in_string = true; self.escaped = false; self.string_len = 0;
                    self.key_string = self.frames.last().is_some_and(|frame| frame.object && frame.expects_key);
                    if !self.key_string && self.frames.last().is_some_and(|frame| frame.path == 1 && matches!(frame.key.as_str(), "item" | "error")) {
                        return Err(Self::invalid());
                    }
                    self.key_bytes.clear();
                    if self.key_string { self.key_bytes.push(b'"'); }
                    self.string_limit = if self.key_string { 1536 } else {
                        self.frames.last().map(|frame| match (frame.path, frame.key.as_str()) {
                            (1 | 2, "type") | (2, "phase") => 1536,
                            (2, "text") => 6 * 64 * 1024 + 1,
                            (1 | 3, "message") => 16 * 1024,
                            _ => PROTOCOL_RECORD_LIMIT,
                        }).unwrap_or(PROTOCOL_RECORD_LIMIT)
                    };
                }
                b'{' | b'[' => {
                    let path = if self.frames.is_empty() { 1 } else {
                        match self.frames.last().map(|frame| (frame.path, frame.key.as_str())) {
                            Some((1, "item")) => 2,
                            Some((1, "error")) => 3,
                            _ => 0,
                        }
                    };
                    if byte == b'[' && path != 0 { return Err(Self::invalid()); }
                    if self.frames.len() >= 128 { return Err(Self::invalid()); }
                    self.frames.push(ProtocolFrame { path, object: byte == b'{', expects_key: byte == b'{', key: String::new() });
                }
                b'}' | b']' => { self.frames.pop(); }
                b',' => { if let Some(frame) = self.frames.last_mut() { frame.expects_key = frame.object; frame.key.clear(); } }
                _ => {}
            }
        }
        Ok(())
    }
    fn complete(&self) -> bool { self.started && self.utf8_tail.is_empty() && !self.in_string && self.frames.is_empty() }
}

struct StoppableReader<'a> { reader: &'a mut dyn Read, stop: &'a AtomicBool }
impl Read for StoppableReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::Acquire) { return Err(std::io::Error::other("protocol reading stopped")); }
        self.reader.read(bytes)
    }
}

struct ProtocolSpool {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl ProtocolSpool {
    fn new() -> std::io::Result<Self> {
        let path = env::temp_dir().join(format!("himind-protocol-{}-{:032x}.tmp", std::process::id(), rand::random::<u128>()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE. The handle
            // cannot be reopened or swapped while the parser consumes it.
            options.share_mode(0).custom_flags(0x100 | 0x04000000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        #[cfg(unix)]
        std::fs::remove_file(&path)?;
        Ok(Self { file: Some(file), path })
    }
}

impl Drop for ProtocolSpool {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

impl RuntimeProtocolRecord {
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.len.saturating_add(bytes.len()) > PROTOCOL_RECORD_LIMIT {
            return Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, "protocol record safety limit"));
        }
        self.validation.append(bytes)?;
        if self.spool.is_none() && self.len + bytes.len() > OUTPUT_CAPTURE_LIMIT {
            let mut spool = ProtocolSpool::new()?;
            spool.file.as_mut().unwrap().write_all(&self.memory)?;
            self.memory.clear();
            self.spool = Some(spool);
        }
        if let Some(spool) = &mut self.spool { spool.file.as_mut().unwrap().write_all(bytes)?; }
        else {
            // Avoid Vec's geometric spare capacity crossing the memory budget
            // when a JSON prefix offsets otherwise aligned pipe chunks.
            if self.memory.capacity() < self.memory.len() + bytes.len() { self.memory.reserve_exact(bytes.len()); }
            self.memory.extend_from_slice(bytes);
        }
        self.len += bytes.len();
        Ok(())
    }

    fn observe<F: FnMut(&mut dyn Read) -> bool>(&mut self, observe: &mut F, stop: &AtomicBool) -> std::io::Result<bool> {
        if !self.validation.complete() { return Ok(false); }
        if let Some(spool) = &mut self.spool {
            let file = spool.file.as_mut().unwrap();
            file.seek(SeekFrom::Start(0))?;
            Ok(observe(&mut StoppableReader { reader: &mut std::io::BufReader::new(file), stop }))
        } else { Ok(observe(&mut StoppableReader { reader: &mut self.memory.as_slice(), stop })) }
    }
}

pub(crate) fn capture_runtime_output<R, F>(mut reader: R, protocol: bool, mut observe: F) -> RuntimeOutputCapture
where R: RuntimePipe + 'static, F: FnMut(&mut dyn Read) -> bool + Send + 'static {
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let output = Arc::new(std::sync::Mutex::new(RuntimeCaptureText::default()));
    let overflowed = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let (thread_stop, thread_finished, thread_output, thread_overflowed, thread_failed) =
        (stop.clone(), finished.clone(), output.clone(), overflowed.clone(), failed.clone());
    let handle = thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        let mut line = RuntimeProtocolRecord::default();
        let mut skip_line = false;
        loop {
            if thread_stop.load(Ordering::Acquire) { break; }
            let read_limit = buffer.len();
            #[cfg(windows)]
            let read_limit = {
                let mut available = 0u32;
                let ok = unsafe { windows_sys::Win32::System::Pipes::PeekNamedPipe(
                    reader.as_raw_handle(), std::ptr::null_mut(), 0, std::ptr::null_mut(),
                    &mut available, std::ptr::null_mut()) };
                if ok == 0 {
                    let error = std::io::Error::last_os_error().raw_os_error();
                    if !matches!(error, Some(109 | 232)) { thread_failed.store(true, Ordering::Release); }
                    break;
                }
                if available == 0 { thread::sleep(Duration::from_millis(10)); continue; }
                read_limit.min(available as usize)
            };
            match reader.read(&mut buffer[..read_limit]) {
                Ok(0) => break,
                Ok(size) => {
                    if let Ok(mut output) = thread_output.lock() {
                        output.append(&buffer[..size]);
                    }
                    if !protocol { continue; }
                    for segment in buffer[..size].split_inclusive(|byte| *byte == b'\n') {
                        let terminated = segment.last() == Some(&b'\n');
                        let bytes = if terminated { &segment[..segment.len()-1] } else { segment };
                        if !skip_line {
                            if let Err(error) = line.append(bytes) {
                                if error.kind() == std::io::ErrorKind::FileTooLarge { thread_overflowed.store(true, Ordering::Release); }
                                else { thread_failed.store(true, Ordering::Release); }
                                line = RuntimeProtocolRecord::default();
                                skip_line = true;
                            }
                        }
                        if terminated {
                            if !skip_line && !matches!(line.observe(&mut observe, &thread_stop), Ok(true)) { thread_failed.store(true, Ordering::Release); }
                            line = RuntimeProtocolRecord::default();
                            skip_line = false;
                        }
                    }
                }
                Err(_) => { thread_failed.store(true, Ordering::Release); break; }
            }
        }
        // An unterminated JSON line is not a complete protocol event.
        if protocol && (line.len != 0 || skip_line) { thread_failed.store(true, Ordering::Release); }
        thread_finished.store(true, Ordering::Release);
    });
    RuntimeOutputCapture { stop, finished, output, overflowed, failed, handle: Some(handle) }
}

pub(crate) fn redact_error(value: &str, claim: &AgentRunClaim, agent_credential: &str) -> String {
    let mut redacted = value.to_string();
    if !claim.claim_token.is_empty() {
        redacted = redacted.replace(&claim.claim_token, "[redacted]");
    }
    if !agent_credential.is_empty() {
        redacted = redacted.replace(agent_credential, "[redacted]");
    }
    summarize_output(&redacted, ERROR_DETAIL_LIMIT)
}

pub(crate) fn summarize_output(value: &str, limit: usize) -> String {
    let length = value.chars().count();
    if length <= limit {
        return value.to_string();
    }
    let marker = "\n...[truncated]...\n";
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

pub(crate) fn safe_temp_path(run_id: &str, suffix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let safe_run_id = run_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || value == '-' || value == '_' {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let directory = env::temp_dir().join("himind-agent").join("agent-runs");
    std::fs::create_dir_all(&directory)?;
    Ok(directory.join(format!("{safe_run_id}-{}-{suffix}", std::process::id())))
}

pub(crate) fn remove_file_if_present(path: &Path) {
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut command = hidden_command("taskkill");
        command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_hidden_process(&mut command);
        let _ = command.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
pub(crate) fn configure_hidden_process(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn configure_hidden_process(_command: &mut Command) {}

pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    configure_hidden_process(&mut command);
    command
}

/// 把用户填的命令解析成真实可执行文件路径。
///
/// Windows 上 `npx`、`uvx`、`pnpm` 这类命令落地的是 `.cmd` 批处理，而
/// `Command::new("npx")` 只会按 `.exe` 去 PATH 里找，找不到就直接报
/// `program not found`——终端里敲得好好的命令到了这里必然失败。桌面端由启动器
/// 拉起时环境又比终端干净，所以除了 PATHEXT，这里还兜了几个常见安装目录，
/// 免得「装了 Node 但 GUI 里找不到」这种问题留给用户排查。
///
/// 带路径分隔符的输入按原样校验；其余一律走名字查找。
pub(crate) fn resolve_executable(program: &str) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    if program.contains('/') || program.contains('\\') {
        let direct = Path::new(program);
        return direct.is_file().then(|| direct.to_path_buf());
    }
    for directory in executable_search_directories() {
        for name in executable_candidate_names(program) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 待查文件名。Windows 下 PATHEXT 后缀优先，原名最后兜底。
///
/// 顺序很关键：`C:\Program Files\nodejs` 里同时存在无扩展名的 `npx`（Git for Windows
/// 的 shell 脚本）和 `npx.cmd`。按原名优先会先命中（或跳过）错误的那个文件，
/// Windows 无法直接 CreateProcess 无扩展名脚本，会报 os error 193。
fn executable_candidate_names(program: &str) -> Vec<String> {
    #[cfg(not(windows))]
    {
        vec![program.to_string()]
    }
    #[cfg(windows)]
    {
        let extensions = env::var_os("PATHEXT")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut names = Vec::new();
        // 已经写明后缀的（如 `npx.cmd`）优先按原样匹配，避免多走一轮后缀拼接。
        let named_extension = std::path::Path::new(program)
            .extension()
            .map(|value| format!(".{}", value.to_string_lossy()))
            .filter(|extension| {
                extensions
                    .split(';')
                    .any(|known| known.trim().eq_ignore_ascii_case(extension))
            });
        if named_extension.is_some() {
            names.push(program.to_string());
        }
        for extension in extensions.split(';') {
            let extension = extension.trim();
            if !extension.is_empty() {
                names.push(format!("{program}{extension}"));
            }
        }
        names.push(program.to_string());
        names
    }
}

fn executable_search_directories() -> Vec<PathBuf> {
    let mut directories = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect::<Vec<_>>();
    #[cfg(windows)]
    {
        for (key, suffix) in [
            ("ProgramFiles", "nodejs"),
            ("ProgramFiles(x86)", "nodejs"),
            ("LOCALAPPDATA", "Programs\\nodejs"),
            ("APPDATA", "npm"),
            ("LOCALAPPDATA", "pnpm"),
            ("LOCALAPPDATA", "Microsoft\\WinGet\\Links"),
            ("USERPROFILE", ".bun\\bin"),
            ("USERPROFILE", ".local\\bin"),
            ("USERPROFILE", ".cargo\\bin"),
            ("USERPROFILE", "scoop\\shims"),
        ] {
            if let Some(root) = env::var_os(key).filter(|value| !value.is_empty()) {
                directories.push(PathBuf::from(root).join(suffix));
            }
        }
        directories.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));
    }
    #[cfg(not(windows))]
    {
        directories.push(PathBuf::from("/usr/local/bin"));
        directories.push(PathBuf::from("/opt/homebrew/bin"));
    }
    directories
}

/// Real CLI fixture shared by cancellation and shutdown tests. No model, network,
/// installed Codex or production Agent data is used.
#[cfg(all(test, windows))]
pub(crate) mod codex_test_support {
    use std::path::{Path, PathBuf};
    use std::sync::LazyLock;
    use std::time::{Duration, Instant};

    pub(crate) struct Environment {
        pub root: PathBuf,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl Environment {
        pub(crate) fn new() -> Self {
            static EXECUTABLE: LazyLock<PathBuf> = LazyLock::new(|| {
                let root = std::env::temp_dir().join(format!("codex-fixture-build-{}", rand::random::<u64>()));
                std::fs::create_dir_all(&root).unwrap();
                let source = root.join("fake.rs");
                std::fs::write(&source, FAKE_CLI).unwrap();
                let exe = root.join("fake-codex.exe");
                let output = super::hidden_command("rustc").arg(&source)
                    .args(["--edition=2021", "-C", "debuginfo=0", "-o"]).arg(&exe)
                    .output().unwrap();
                assert!(output.status.success(), "fixture compile: {}", String::from_utf8_lossy(&output.stderr));
                exe
            });
            let root = std::env::temp_dir().join(format!("codex-lifecycle-{}", rand::random::<u64>()));
            std::fs::create_dir_all(&root).unwrap();
            let saved = ["HIMIND_AGENT_HOME", "HIMIND_CODEX_EXECUTABLE", "HIMIND_CODEX_TIMEOUT_SECONDS"]
                .into_iter().map(|key| (key, std::env::var_os(key))).collect();
            std::env::set_var("HIMIND_AGENT_HOME", root.join("home"));
            std::env::set_var("HIMIND_CODEX_EXECUTABLE", &*EXECUTABLE);
            std::env::remove_var("HIMIND_CODEX_TIMEOUT_SECONDS");
            Self { root, saved }
        }

        pub(crate) fn case(&self, case: &serde_json::Value) {
            std::fs::write(self.root.join("case.json"), serde_json::to_vec(case).unwrap()).unwrap();
        }
    }

    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value { Some(value) => std::env::set_var(key, value), None => std::env::remove_var(key) }
            }
        }
    }

    pub(crate) fn wait_file(path: &Path) {
        let started = Instant::now();
        while !path.is_file() {
            assert!(started.elapsed() < Duration::from_secs(15), "fixture did not create {}", path.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) struct ProcessProbe(windows_sys::Win32::Foundation::HANDLE, u32);
    impl ProcessProbe {
        pub(crate) fn open(path: &Path) -> Self {
            wait_file(path);
            let start=Instant::now();
            let pid=loop {
                if let Ok(pid)=std::fs::read_to_string(path).unwrap_or_default().trim().parse::<u32>() {break pid;}
                assert!(start.elapsed()<Duration::from_secs(5),"incomplete fixture PID receipt");
                std::thread::sleep(Duration::from_millis(5));
            };
            let handle = unsafe { windows_sys::Win32::System::Threading::OpenProcess(0x0010_0000 | 0x0001, 0, pid) };
            assert!(!handle.is_null(), "cannot retain owned fixture process {pid}: {}", std::io::Error::last_os_error());
            Self(handle, pid)
        }
        pub(crate) fn is_alive(&self) -> bool {
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(self.0, 0) == 258 }
        }
        pub(crate) fn assert_exited(&self) {
            assert_eq!(unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(self.0, 5000) }, 0,
                "owned fixture process {} survived", self.1);
        }
    }
    impl Drop for ProcessProbe {
        fn drop(&mut self) {
            if self.is_alive() {
                let _ = super::hidden_command("taskkill").args(["/PID", &self.1.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            }
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0); }
        }
    }

    const FAKE_CLI: &str = r##"
use std::{env,fs,io::{Read,Write},process::{Command,Stdio},thread,time::{Duration,Instant}};
fn pidfile(root:&std::path::Path,name:&str,pid:u32) {
 let temp=root.join(format!("{name}.pending"));
 fs::write(&temp,pid.to_string()).unwrap();fs::rename(&temp,root.join(name)).unwrap();
}
fn main() {
 let args:Vec<String>=env::args().collect();
 if args.iter().any(|a|a=="--version") {
  if env::current_exe().unwrap().file_stem().unwrap().to_string_lossy().contains("old") {println!("codex-cli 0.129.0");return;}
  let root=std::path::PathBuf::from(env::var_os("HIMIND_AGENT_HOME").unwrap()).parent().unwrap().to_path_buf();
  let case=fs::read_to_string(root.join("case.json")).unwrap_or_default();
  if case.contains("\"version_hang\":true") {
   pidfile(&root,"version.pid",std::process::id());
   thread::sleep(Duration::from_secs(60));
  }
  if case.contains("\"version_tree\":true") {
   let child=Command::new(env::current_exe().unwrap()).arg("--fixture-child").current_dir(&root)
    .stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn().unwrap();
   pidfile(&root,"child.pid",child.id());
   thread::sleep(Duration::from_millis(150));
  }
  println!("codex-cli 0.167.0-fixture"); return;
 }
 let root=env::current_dir().unwrap();
 if args.iter().any(|a|a=="--fixture-child") {
  fs::write(root.join("child.ready"),b"ready").unwrap();
  thread::sleep(Duration::from_secs(60)); return;
 }
 let case=fs::read_to_string(root.join("case.json")).unwrap();
 let has=|s:&str|case.contains(&format!("\"{s}\":true"));
 fs::write(root.join("argv.json"), format!("{:?}",args)).unwrap();
 let mut invocations=fs::OpenOptions::new().create(true).append(true).open(root.join("provider.invocations")).unwrap();
 writeln!(invocations,"exec").unwrap();
 if has("immediate_tree_exit") || has("immediate_tree_wait") {
  let _child=Command::new(env::current_exe().unwrap()).arg("--fixture-child")
   .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
  pidfile(&root,"child.pid",_child.id());
  pidfile(&root,"root.pid",std::process::id());
  if has("immediate_tree_exit") {return;}
  thread::sleep(Duration::from_secs(60));return;
 }
 if has("output_before_input") {
  for _ in 0..2048 {println!("{{\"type\":\"tool\",\"ignored\":\"{}\"}}","x".repeat(512));}
  for _ in 0..1024 {eprintln!("{}","e".repeat(512));}
 }
 if has("no_reader") {
  let mut child=Command::new(env::current_exe().unwrap()).arg("--fixture-child")
   .stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn().unwrap();
  pidfile(&root,"child.pid",child.id());
  pidfile(&root,"root.pid",std::process::id());
  thread::sleep(Duration::from_secs(60)); let _=child.wait(); return;
 }
 let mut input=String::new();std::io::stdin().read_to_string(&mut input).unwrap();
 fs::write(root.join("stdin.txt"),input.as_bytes()).unwrap();
 let mut child=if has("tree") {Some(Command::new(env::current_exe().unwrap()).arg("--fixture-child")
  .stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn().unwrap())}else{None};
 if let Some(c)=&child {
  pidfile(&root,"child.pid",c.id());
  let start=Instant::now();while !root.join("child.ready").exists() && start.elapsed()<Duration::from_secs(5){thread::sleep(Duration::from_millis(10));}
 }
 pidfile(&root,"root.pid",std::process::id());
 if has("wait_release") {let start=Instant::now();while !root.join("release").exists() && start.elapsed()<Duration::from_secs(8){thread::sleep(Duration::from_millis(20));}}
 let mut text=if has("negative"){r#"{"passed":false,"summary":"fixture negative"}"#.to_string()}else{r#"{"passed":true,"summary":"fixture positive"}"#.to_string()};
 if has("text_final") {text="plain workflow final".to_string();}
 if has("both_truncated") {text.truncate(text.len()-2);}
 if has("fallback_json") {text="{\"unfinished\":\n{\"passed\":true}".to_string();}
 let session=if args.iter().any(|a|a=="resume") {args[args.len()-2].as_str()}else if has("session_s"){"s"}else{"fixture-thread"};
 if !has("no_thread") {println!("{{\"type\":\"thread.started\",\"thread_id\":\"{session}\"}}");}
 let path=args.windows(2).find(|a|a[0]=="-o").unwrap()[1].clone();
 if !has("missing") {fs::write(&path,if has("truncated"){&text[..text.len()-2]}else{&text}).unwrap();}
 if has("many_events") {for _ in 0..1200 {println!("{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"aggregated_output\":\"{}\"}}}}","x".repeat(400));}}
 if has("large_line") {println!("{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"aggregated_output\":\"{}\"}}}}","x".repeat(900*1024));}
 if has("bad_large_line") {println!("{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"aggregated_output\":\"{}\"}}BROKEN","x".repeat(300*1024));}
 if has("stderr_no_newline") {eprint!("diagnostic without newline");}
 if !has("no_agent") {
  let escaped=text.replace('\\',"\\\\").replace('"',"\\\"").replace('\n',"\\n");
  println!("{{\"type\":\"item.completed\",\"item\":{{\"id\":\"final\",\"type\":\"agent_message\",\"text\":\"{}\"}}}}",escaped);
 }
 let (input,output)=if has("usage_40_4"){(40,4)}else if has("usage_100_10"){(100,10)}else if has("usage_200_20"){(200,20)}else if has("usage_230_23"){(230,23)}else if has("usage_250_25"){(250,25)}else{(1,1)};
 if !has("no_turn") {println!("{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":{input},\"output_tokens\":{output}}}}}");}
 if has("truncated_event") {print!("{{\"type\":\"item.completed\"");}
 std::io::stdout().flush().unwrap();
 fs::write(root.join("emitted"),b"ready").unwrap();
 if has("hang") {thread::sleep(Duration::from_secs(8));}
 if !has("inherit_writer") {if let Some(c)=&mut child {let _=c.kill();let _=c.wait();}}
 std::process::exit(if has("nonzero"){7}else{0});
}
"##;
}

#[cfg(test)]
mod tests {
    use super::{
        executable_candidate_names, summarize_output, wait_for_child_until,
        wait_for_child_until_with_cancel,
    };
    use std::error::Error;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[cfg(windows)]
    #[test]
    fn executor_long_workspace_cwd_preserves_identity_and_starts_owned_process() {
        let root=std::env::temp_dir().join(format!("long-workspace-{}",rand::random::<u64>()));
        let path=root.join("nested-directory".repeat(5)).join("nested-directory".repeat(5)).join("nested-directory".repeat(5));
        std::fs::create_dir_all(&path).unwrap();
        let expected=path.canonicalize().unwrap();
        let resolved=super::canonical_workspace(path.to_str().unwrap()).unwrap();
        assert_eq!(resolved.canonicalize().unwrap(),expected);
        let mut command=super::hidden_command("cmd.exe");
        command.args(["/d","/c","cd"]).current_dir(&resolved).stdout(Stdio::piped()).stderr(Stdio::piped());
        let (child,_tree)=crate::runtime::owned_tree::OwnedTree::spawn(&mut command).unwrap();
        let output=child.wait_with_output().unwrap();assert!(output.status.success());
        assert_eq!(std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).canonicalize().unwrap(),expected);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn output_summary_keeps_context_and_tail() {
        let value = format!("{}ROOT_CAUSE", "header".repeat(1_000));
        let summary = summarize_output(&value, 200);
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.starts_with("header"));
        assert!(summary.ends_with("ROOT_CAUSE"));
    }

    #[test]
    fn protocol_spool_bounds_memory_and_is_removed_on_error_or_drop() {
        use super::{RuntimeProtocolRecord, OUTPUT_CAPTURE_LIMIT, PROTOCOL_RECORD_LIMIT};
        let chunk = vec![b'x'; 8192];
        let mut record = RuntimeProtocolRecord::default();
        record.append(br#"{"type":"item.completed","item":{"type":"command_execution","aggregated_output":""#).unwrap();
        for _ in 0..128 { record.append(&chunk).unwrap(); }
        record.append(br#""}}"#).unwrap();
        assert!(record.len > 1024 * 1024);
        assert!(record.memory.capacity() <= OUTPUT_CAPTURE_LIMIT);
        assert!(record.memory.is_empty());
        let path = record.spool.as_ref().unwrap().path.clone();
        let mut observed = 0;
        assert!(record.observe(&mut |reader| {
            let mut buffer = [0u8; 8192];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 { break; }
                observed += count;
            }
            true
        }, &std::sync::atomic::AtomicBool::new(false)).unwrap());
        assert_eq!(observed, record.len, "the entire frame, including its tail, must be observed");
        record.len = PROTOCOL_RECORD_LIMIT;
        assert!(record.append(b"x").is_err());
        drop(record);
        assert!(!path.exists(), "no raw protocol output is retained");
    }

    #[test]
    fn protocol_validation_bounds_control_fields_and_checks_ignored_utf8() {
        use super::RuntimeProtocolRecord;
        for bad in [b"[\"turn.completed\",null,null,null]".as_slice(),
            br#"{"type":"item.completed","item":["agent_message",null,"final"]}"#.as_slice(),
            br#"{"type":"item.completed","item":"wrong object type"}"#.as_slice(),
            br#"{"type":"turn.failed","error":"wrong object type"}"#.as_slice(),
            b"{\"type\":\"turn.completed\",\"ignored\":\"\xff\"}".as_slice()] {
            assert!(RuntimeProtocolRecord::default().append(bad).is_err());
        }
        let mut large = RuntimeProtocolRecord::default();
        large.append(br#"{"type":"item.completed","item":{"type":"agent_message","text":""#).unwrap();
        let chunk = [b'x';8192];
        for _ in 0..48 { large.append(&chunk).unwrap(); }
        assert!(large.append(&chunk).is_err(), "reject before allocating a massive control string in serde");
        let mut unicode = RuntimeProtocolRecord::default();
        unicode.append(b"{\"type\":\"turn.completed\",\"ignored\":\"\xf0\x9f").unwrap();
        assert!(!unicode.validation.complete());
        unicode.append(b"\x98\x80\"}").unwrap();
        assert!(unicode.validation.complete());
    }

    #[test]
    fn protocol_spool_read_stops_and_closes_before_returning() {
        use super::RuntimeProtocolRecord;
        let mut record = RuntimeProtocolRecord::default();
        record.append(br#"{"ignored":""#).unwrap();
        for _ in 0..64 { record.append(&[b'x';8192]).unwrap(); }
        record.append(br#""}"#).unwrap();
        let path = record.spool.as_ref().unwrap().path.clone();
        assert!(!record.observe(&mut |reader| reader.read(&mut [0u8;1]).is_ok(),
            &std::sync::atomic::AtomicBool::new(true)).unwrap());
        drop(record);
        assert!(!path.exists());
    }

    /// Windows 的 `npx` 目录里同时有无扩展名脚本和 `npx.cmd`，候选顺序必须先看后缀，
    /// 否则会拿到无法直接 CreateProcess 的那个文件（os error 193）。
    #[test]
    fn windows_prefers_pathext_suffixed_candidates() {
        let names = executable_candidate_names("npx");
        #[cfg(windows)]
        {
            let first = names.first().expect("至少有一个候选名");
            assert!(
                first.len() > "npx".len(),
                "第一个候选应是带后缀的可执行文件，实际为 {first}"
            );
            assert_eq!(names.last().map(String::as_str), Some("npx"));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(names, vec!["npx".to_string()]);
        }
    }

    #[test]
    fn process_timeout_terminates_the_child() {
        let mut command;
        #[cfg(windows)]
        {
            command = Command::new("cmd");
            command.args(["/C", "ping 127.0.0.1 -n 6 >NUL"]);
        }
        #[cfg(not(windows))]
        {
            command = Command::new("sh");
            command.args(["-c", "sleep 5"]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let error =
            wait_for_child_until(&mut child, Duration::from_secs(1), "timeout contract test")
                .unwrap_err();
        assert!(error.to_string().contains("exceeded 1 seconds"));
    }

    #[test]
    fn process_cancellation_terminates_the_child() {
        let mut command;
        #[cfg(windows)]
        {
            command = Command::new("cmd");
            command.args(["/C", "ping 127.0.0.1 -n 6 >NUL"]);
        }
        #[cfg(not(windows))]
        {
            command = Command::new("sh");
            command.args(["-c", "sleep 5"]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let mut canceled = false;
        let mut is_canceled = || -> Result<bool, Box<dyn Error>> {
            canceled = true;
            Ok(canceled)
        };
        let error = wait_for_child_until_with_cancel(
            &mut child,
            Duration::from_secs(10),
            "cancel contract test",
            &mut is_canceled,
        )
        .unwrap_err();
        assert!(error.to_string().contains("was canceled"));
    }
}
