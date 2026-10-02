use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, Implementation, InitializeRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    Agent, Client as AcpClient, ConnectionTo, Error as AcpSdkError, Lines, UntypedMessage,
};
use futures::channel::mpsc as futures_mpsc;
use futures::{SinkExt, StreamExt};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api::client::{
    is_task_canceled_error, task_canceled_error, update_agent_run_status, TaskCancelGuard,
};
use crate::api::types::{AgentRunClaim, RuntimeInstallationReport, Task};
use crate::runtime::process;
use crate::runtime::{execute_managed, AgentRunEnvelope};
use crate::Options;

const ACP_PROTOCOL_VERSION: i64 = 1;
const ACP_OUTPUT_LIMIT: usize = 64 * 1024;
const ACP_SESSION_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;
const ACP_PROFILE_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_PROFILES_JSON";
const ACP_EXECUTABLE_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_EXECUTABLE";
const ACP_ARGS_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_ARGS_JSON";
const ACP_PERMISSION_ENVIRONMENT: &str = "HIMIND_ACP_STDIO_PERMISSION_POLICY";
const ACP_MESSAGE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const ACP_CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(250);
const ACP_TRACE_ENVIRONMENT: &str = "HIMIND_ACP_TRACE";
const ACP_TRACE_FULL_VALUE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcpProfile {
    executable: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    permission_policy: String,
    #[serde(skip)]
    source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionPolicy {
    Deny,
    AllowOnce,
    Prompt,
}

impl PermissionPolicy {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value.trim() {
            "" | "deny" => Ok(Self::Deny),
            "allow_once" => Ok(Self::AllowOnce),
            "prompt" => Ok(Self::Prompt),
            value => Err(format!(
                "ACP permission_policy must be deny, allow_once or prompt, got {value}"
            )
            .into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowOnce => "allow_once",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug)]
pub(crate) struct AcpExecution {
    pub(crate) session_id: String,
    pub(crate) stop_reason: String,
    pub(crate) final_text: String,
    pub(crate) update_count: usize,
    pub(crate) tool_call_count: usize,
    pub(crate) permission_requests: Vec<Value>,
    pub(crate) denied_client_methods: Vec<String>,
    pub(crate) stderr: String,
    pub(crate) log_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcpTraceMode {
    Off,
    Summary,
    Full,
}

impl AcpTraceMode {
    /// `HIMIND_ACP_TRACE=off|summary|full`，缺省 `summary`。
    ///
    /// 默认就要留下「谁在什么时候发了什么方法」这一层，因为 ACP 出问题时
    /// 用户已经跑完一次会话了，事后没有开关可补；而 `full` 会记下完整报文，
    /// 里面可能带 prompt 正文和文件内容，所以必须是显式选择。
    fn from_environment() -> Self {
        match env::var(ACP_TRACE_ENVIRONMENT)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("full") | Some("verbose") => Self::Full,
            Some("off") | Some("0") | Some("false") | Some("disabled") | Some("none") => Self::Off,
            _ => Self::Summary,
        }
    }

    fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// ACP 会话的本地留痕，落到 `<agent_home>/acp-logs/<run_id>.jsonl`。
///
/// 出站排障过去只有 `stderr`，而它只在会话正常收尾时才被汇总一次：进程崩在
/// 中途、或者 Agent 回了个我们没预期的方法，日志里什么都没有。这里把 JSON-RPC
/// 双向报文逐行落盘，`summary` 只留方法/id/更新类型，`full` 再带上脱敏后的完整
/// 报文。落盘失败一律吞掉——留痕是排障设施，不能反过来把主链路带崩。
#[derive(Clone, Debug)]
struct AcpTrace {
    mode: AcpTraceMode,
    path: PathBuf,
}

impl AcpTrace {
    fn for_run(run_id: &str) -> Self {
        Self {
            mode: AcpTraceMode::from_environment(),
            path: crate::store::paths::agent_home()
                .join("acp-logs")
                .join(format!("{}.jsonl", trace_file_name(run_id))),
        }
    }

    fn path_string(&self) -> Option<String> {
        self.mode
            .enabled()
            .then(|| self.path.to_string_lossy().into_owned())
    }

    fn record(&self, direction: &str, message: &Value) {
        if !self.mode.enabled() {
            return;
        }
        let mut entry = json!({
            "ts": unix_millis(),
            "dir": direction,
        });
        if let Some(id) = message.get("id") {
            entry["id"] = id.clone();
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            entry["method"] = json!(method);
        }
        if let Some(kind) = message
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
        {
            entry["update"] = json!(kind);
        }
        if let Some(error) = message.get("error") {
            entry["error"] = json!(process::summarize_output(&error.to_string(), 2_000));
        }
        if self.mode == AcpTraceMode::Full {
            entry["payload"] = json!(process::summarize_output(
                &crate::approval::manager::redact_message(&message.to_string()),
                ACP_TRACE_FULL_VALUE_LIMIT,
            ));
        }
        self.append(entry);
    }

    /// 记一条非报文的会话事件（启动参数、结束原因、stderr 摘要）。
    fn event(&self, event: &str, detail: Value) {
        if !self.mode.enabled() {
            return;
        }
        self.append(json!({
            "ts": unix_millis(),
            "event": event,
            "detail": detail,
        }));
    }

    fn append(&self, entry: Value) {
        if let Some(parent) = self.path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// run id 来自远端，落到文件名前先收敛成路径安全的形式。
fn trace_file_name(run_id: &str) -> String {
    let mut name: String = run_id
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
        .take(96)
        .collect();
    if name.is_empty() {
        name.push_str("acp-session");
    }
    name
}

#[derive(Clone)]
struct AcpAttribution {
    provider: String,
    run_id: String,
}

#[derive(Default)]
struct AcpSessionState {
    final_text: String,
    update_count: usize,
    tool_call_count: usize,
    permission_requests: Vec<Value>,
    denied_client_methods: Vec<String>,
}

/// ACP 会话的共享状态：连接线程（SDK 事件循环）写，等待线程读。
///
/// 取消与超时要回答的是「此刻在等哪个方法的回包」「session 协商出来没有」，
/// 所以这里用 `Mutex` + 原子标志而不是消息队列——需要的是当前状态，不是历史事件。
#[derive(Default)]
struct AcpSessionShared {
    state: Mutex<AcpSessionState>,
    /// 当前在等回包的方法名：initialize / session/new / session/prompt。
    phase: Mutex<String>,
    /// 协商出的 session id：只有拿到它，取消时才能发 session/cancel。
    session_id: Mutex<Option<String>>,
    /// 连接句柄。取消要从等待线程发出 session/cancel，SDK 的连接对象可跨线程克隆。
    connection: Mutex<Option<ConnectionTo<Agent>>>,
    /// 失败原因只落一次：等待线程判定的取消/超时文案优先于传输层错误。
    failure: Mutex<Option<String>>,
    /// 取消或超时后置位，权限审批与连接线程据此尽快退出。
    aborted: AtomicBool,
    /// agent 的 stdout 是否已关闭，用来把「连接断了」翻译成既有文案。
    stdout_closed: AtomicBool,
}

impl AcpSessionShared {
    fn set_phase(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.to_string();
    }

    fn phase(&self) -> String {
        self.phase.lock().unwrap().clone()
    }

    /// 第一条失败原因说了算：等待线程判定取消/超时时先落文案，随后连接线程
    /// 因进程被终止而收到的传输层报错不会覆盖它。
    fn fail(&self, message: String) -> String {
        let mut slot = self.failure.lock().unwrap();
        if slot.is_none() {
            *slot = Some(message);
        }
        slot.clone().unwrap_or_default()
    }

    fn failure_text(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }

    fn take_state(&self) -> AcpSessionState {
        std::mem::take(&mut *self.state.lock().unwrap())
    }
}

/// 一次 ACP 会话的固定输入，连接线程按 initialize → session/new → session/prompt 驱动。
struct AcpSessionPlan {
    workspace: PathBuf,
    prompt: String,
    policy: PermissionPolicy,
    attribution: AcpAttribution,
}

#[derive(Clone)]
struct AcpSessionOutcome {
    session_id: String,
    stop_reason: String,
}

struct AcpProcess {
    child: Option<Child>,
    /// stdin 由写线程持有，但句柄留在共享槽位：会话收尾时取走它，agent 立刻看到
    /// EOF 自行退出，而不是只能靠杀进程。
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    outgoing: Option<futures_mpsc::Sender<String>>,
    shared: Arc<AcpSessionShared>,
    outcome: Receiver<Result<AcpSessionOutcome, String>>,
    connection: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<String>>,
    /// 入站原始 `session/update` 帧数，兜住 SDK 静默丢弃的未知更新变体。
    raw_update_frames: Arc<AtomicUsize>,
    trace: AcpTrace,
}

pub(crate) fn is_provider(provider: &str) -> bool {
    provider == "acp.stdio" || provider.starts_with("acp.")
}

/// 桌面端「一键接入」在落库之前就要知道前置 CLI 在不在：预设里 Codex / Claude /
/// Copilot 都靠 `npx` 拉起适配器，OpenCode 靠自身命令。缺了前置却让用户先接入、
/// 再从列表里看到一句「不可用」，等于把排查成本丢回给用户。这里只探预设真正
/// 依赖的命令行，不做进程探测。
const ACP_PROBE_EXECUTABLES: [&str; 3] = ["npx", "node", "opencode"];

/// 一条命令行探测的结果。`source` 区分「PATH 里就有」和「只在已知安装位置找到」：
/// 只有后者才需要把绝对路径写进配置，前者必须继续留命令名，否则版本管理器换了
/// 一个 Node 版本，存档里的旧路径立刻失效。
struct ProbedProgram {
    path: PathBuf,
    version: String,
    from_install_location: bool,
}

pub(crate) fn probe_executables() -> Value {
    let mut executables = serde_json::Map::new();
    for name in ACP_PROBE_EXECUTABLES {
        let probed = resolve_probe_program(name);
        let mut entry = serde_json::Map::new();
        entry.insert("available".to_string(), json!(probed.is_some()));
        entry.insert(
            "path".to_string(),
            json!(probed
                .as_ref()
                .map(|program| program.path.to_string_lossy().to_string())
                .unwrap_or_default()),
        );
        entry.insert(
            "version".to_string(),
            json!(probed
                .as_ref()
                .map(|program| program.version.clone())
                .unwrap_or_default()),
        );
        entry.insert(
            "source".to_string(),
            json!(match probed.as_ref() {
                Some(program) if program.from_install_location => "install_location",
                _ => "path",
            }),
        );
        // 装了桌面版却没把 CLI 写进 PATH 是常态：配置目录这层旁证能让面板把
        // 「确实没装」和「装了但命令不在 PATH」分开说，不参与可用性判定。
        if let Some(directory) = program_config_directory(name) {
            entry.insert(
                "config_dir".to_string(),
                json!(directory.to_string_lossy().to_string()),
            );
        }
        executables.insert(name.to_string(), Value::Object(entry));
    }
    Value::Object(executables)
}

fn resolve_probe_program(program: &str) -> Option<ProbedProgram> {
    if let Ok(path) = resolve_executable(program) {
        return Some(ProbedProgram {
            path,
            version: String::new(),
            from_install_location: false,
        });
    }
    install_location_candidates(program)
        .into_iter()
        .find(|candidate| candidate.path.is_file())
}

/// 已知安装位置。OpenCode 桌面版把自带 CLI 放在「应用数据/ai.opencode.desktop/cli/
/// <版本>/opencode-cli.exe」，这个 CLI 支持 `acp` 子命令，但永远不在 PATH 里；
/// 只看 PATH 会把「已安装」报成「未安装」。别的预设依赖 npx / node，走 PATH 即可。
fn install_location_candidates(program: &str) -> Vec<ProbedProgram> {
    if program != "opencode" {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let plain = |path: PathBuf| ProbedProgram {
        path,
        version: String::new(),
        from_install_location: true,
    };
    if let Some(app_data) = env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        candidates.extend(versioned_cli_candidates(
            PathBuf::from(app_data)
                .join("ai.opencode.desktop")
                .join("cli"),
        ));
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        let root = PathBuf::from(local_app_data);
        candidates.extend(versioned_cli_candidates(
            root.join("Programs").join("@opencodedesktop").join("cli"),
        ));
        candidates.push(plain(
            root.join("Programs").join("opencode").join("opencode.exe"),
        ));
    }
    if let Some(home) = home_directory() {
        candidates.push(plain(
            home.join(".opencode").join("bin").join("opencode.exe"),
        ));
        candidates.push(plain(home.join(".opencode").join("bin").join("opencode")));
        candidates.push(plain(home.join(".local").join("bin").join("opencode")));
    }
    candidates
}

/// `<根目录>/<版本>/opencode-cli(.exe)`：目录名就是客户端版本，顺手带出去，
/// 面板就不用给用户看一个写死的版本号。高版本优先。
fn versioned_cli_candidates(root: PathBuf) -> Vec<ProbedProgram> {
    // 先按解析出的版本号排序，再脱掉版本壳，避免调用方拿到顺序不稳定的安装目录。
    let mut candidates: Vec<(Vec<u64>, ProbedProgram)> = Vec::new();
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let version = entry.file_name().to_string_lossy().to_string();
        let Some(parsed) = parse_version(&version) else {
            continue;
        };
        let directory = entry.path();
        let Some(path) = [
            "opencode-cli.exe",
            "opencode-cli",
            "opencode.exe",
            "opencode",
        ]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|candidate| candidate.is_file()) else {
            continue;
        };
        candidates.push((
            parsed,
            ProbedProgram {
                path,
                version,
                from_install_location: true,
            },
        ));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates.into_iter().map(|(_, program)| program).collect()
}

fn parse_version(value: &str) -> Option<Vec<u64>> {
    let parts = value
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!parts.is_empty()).then_some(parts)
}

fn program_config_directory(program: &str) -> Option<PathBuf> {
    if program != "opencode" {
        return None;
    }
    if let Some(path) = env::var_os("OPENCODE_CONFIG").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        return Some(if path.is_dir() {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        });
    }
    let directory = home_directory()?.join(".config").join("opencode");
    directory.is_dir().then_some(directory)
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn probe_installations() -> Vec<RuntimeInstallationReport> {
    let profiles = match configured_profiles() {
        Ok(profiles) => profiles,
        Err(error) => {
            return vec![RuntimeInstallationReport {
                provider: "acp.stdio".to_string(),
                version: String::new(),
                status: "unsupported".to_string(),
                capabilities: json!({
                    "protocol_version": ACP_PROTOCOL_VERSION,
                    "error": error.to_string(),
                }),
            }]
        }
    };
    profiles
        .into_iter()
        .map(|(provider, profile)| installation_report(&provider, &profile))
        .collect()
}

fn installation_report(provider: &str, profile: &AcpProfile) -> RuntimeInstallationReport {
    let policy = profile_permission_policy(profile);
    let executable = resolve_executable(&profile.executable);
    let ready = executable.is_ok() && policy.is_ok();
    let permission_policy = policy.map(PermissionPolicy::as_str).unwrap_or("invalid");
    RuntimeInstallationReport {
        provider: provider.to_string(),
        version: profile.version.trim().to_string(),
        status: if ready {
            "ready".to_string()
        } else {
            "unavailable".to_string()
        },
        capabilities: json!({
            "protocol_version": ACP_PROTOCOL_VERSION,
            "streaming": true,
            "cancel": true,
            "permission_policy": permission_policy,
            // 状态只说「不可用」等于没说：面板要能区分「命令没装」和「权限策略
            // 非法」，所以把判定依据一并带出去。
            "executable_available": executable.is_ok(),
            "executable_error": executable
                .as_ref()
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default(),
            "permission_policy_valid": permission_policy != "invalid",
            "permission_ownership": "runtime_step_or_agent_run",
            "source": profile.source,
            "client_file_system": false,
            "client_terminal": false,
            "network_isolated": false,
            "tool_access": "provider_defined",
        }),
    }
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    let provider = envelope.runtime_provider.clone();
    execute_managed(
        client,
        options,
        agent_id,
        task,
        envelope,
        &provider,
        |claim| execute_claimed(client, options, agent_id, task, claim, &provider),
    )
}

fn execute_claimed(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    claim: &AgentRunClaim,
    provider: &str,
) -> Result<Value, Box<dyn Error>> {
    if !claim.ai_model.trim().is_empty() {
        return Err("ACP Runtime runs must not receive a HiMind AI model".into());
    }
    let workspace = process::canonical_workspace(&claim.workspace_path)?;
    let prompt = build_prompt(claim)?;
    update_agent_run_status(
        client,
        &options.api_base(),
        agent_id,
        &claim.run.id,
        &claim.claim_token,
        "running",
        None,
        "",
        &options.agent_credential(),
    )?;
    let _renewal = process::start_run_lease_renewal(client, options, agent_id, claim);
    // 派发链路（Dashboard claim）过去把取消判据写死成「永不取消」，导致通过工作台
    // 下发的 ACP 运行无法被用户打断。这里接上与其他运行时一致的 TaskCancelGuard：
    // 轮询任务取消状态，一旦确认取消就让 ACP 侧发出 session/cancel 并终止子进程。
    let mut cancel_guard = TaskCancelGuard::new();
    let execution = {
        let mut is_canceled = || -> Result<bool, Box<dyn Error>> {
            match cancel_guard.check(client, options, agent_id, &task.id) {
                Ok(()) => Ok(false),
                Err(error) if is_task_canceled_error(&error.to_string()) => Ok(true),
                Err(error) => {
                    eprintln!(
                        "Agent Run task {} cancellation check failed while running the ACP session: {error}",
                        task.id
                    );
                    Ok(false)
                }
            }
        };
        run_session(
            provider,
            &workspace,
            &prompt,
            ACP_SESSION_TIMEOUT_SECONDS,
            &claim.run.id,
            &mut is_canceled,
        )
    };
    // ACP 侧只会报告「ACP <method> was canceled」，远端只认标准取消文案，
    // 因此在这里把它归一到 canceled，避免把用户取消记成失败。
    if cancel_guard.canceled() {
        return Err(task_canceled_error());
    }
    let execution = execution?;
    let attribution = AcpAttribution {
        provider: provider.to_string(),
        run_id: claim.run.id.clone(),
    };
    Ok(json!({
        "run_id": claim.run.id,
        "runtime_provider": provider,
        "completed": true,
        "session_id": execution.session_id,
        "final_message": execution.final_text,
        "billing_owner": "user",
        "acp": execution_metadata(&execution, &attribution),
    }))
}

pub(crate) fn execute_workflow(
    provider: &str,
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    run_id: &str,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<AcpExecution, Box<dyn Error>> {
    let workspace = process::canonical_workspace(workspace)?;
    run_session(
        provider,
        &workspace,
        prompt,
        timeout_seconds,
        run_id,
        &mut || is_canceled(),
    )
}

fn run_session(
    provider: &str,
    workspace: &Path,
    prompt: &str,
    timeout_seconds: u64,
    run_id: &str,
    is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<AcpExecution, Box<dyn Error>> {
    if prompt.trim().is_empty() {
        return Err("ACP prompt is empty".into());
    }
    let profile = profile_for(provider)?;
    let policy = profile_permission_policy(&profile)?;
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let attribution = AcpAttribution {
        provider: provider.to_string(),
        run_id: run_id.to_string(),
    };
    let trace = AcpTrace::for_run(run_id);
    trace.event(
        "session_start",
        json!({
            "provider": provider,
            "workspace": workspace.to_string_lossy(),
            "executable": &profile.executable,
            "args": &profile.args,
            "permission_policy": policy.as_str(),
            "timeout_seconds": timeout_seconds,
        }),
    );
    let plan = AcpSessionPlan {
        workspace: workspace.to_path_buf(),
        prompt: prompt.to_string(),
        policy,
        attribution,
    };
    let mut process = AcpProcess::spawn(&profile, plan, trace.clone())?;
    let outcome = process.drive(timeout, is_canceled)?;
    let mut state = process.shared.take_state();
    // 未知的 sessionUpdate 变体在 SDK 反序列化阶段就被丢弃，typed handler 收不到，
    // 因此 update_count 取「typed 计数」与「原始帧计数」的较大者。
    state.update_count = state.update_count.max(process.raw_update_frames());
    let stderr = process.finish()?;
    let stderr = process::summarize_output(stderr.trim(), 8 * 1024);
    let session_id = outcome.session_id;
    let stop_reason = outcome.stop_reason;
    trace.event(
        "session_end",
        json!({
            "stop_reason": &stop_reason,
            "update_count": state.update_count,
            "tool_call_count": state.tool_call_count,
            "permission_request_count": state.permission_requests.len(),
            "denied_client_methods": &state.denied_client_methods,
            "stderr": &stderr,
        }),
    );
    Ok(AcpExecution {
        session_id,
        stop_reason,
        final_text: process::summarize_output(&state.final_text, ACP_OUTPUT_LIMIT),
        update_count: state.update_count,
        tool_call_count: state.tool_call_count,
        permission_requests: state.permission_requests,
        denied_client_methods: state.denied_client_methods,
        stderr,
        log_path: trace.path_string(),
    })
}

impl AcpProcess {
    fn spawn(
        profile: &AcpProfile,
        plan: AcpSessionPlan,
        trace: AcpTrace,
    ) -> Result<Self, Box<dyn Error>> {
        let executable = resolve_executable(&profile.executable)?;
        let mut command = Command::new(executable);
        command
            .args(&profile.args)
            .current_dir(&plan.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::remove_himind_secret_environment(&mut command);
        // Profile 级环境变量：真实 ACP Agent 常用环境变量切换运行模式
        // （例如 codex-acp 的 INITIAL_AGENT_MODE），由用户在 profile 中显式声明。
        for (key, value) in &profile.env {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            command.env(key, value);
        }
        process::configure_hidden_process(&mut command);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or("ACP agent stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("ACP agent stdout is unavailable")?;
        let stderr = child.stderr.take().map(process::capture_output);

        let shared = Arc::new(AcpSessionShared::default());
        let raw_update_frames = Arc::new(AtomicUsize::new(0));

        // 入站：stdout 逐行 → 留痕 + 原始 update 计数 → 交给 SDK 的 Stream。
        // 留痕放在这里而不是 SDK 回调里，因为 SDK 只把能反序列化的报文交给 typed
        // handler：未知方法、未知 sessionUpdate 变体都到不了回调，只有这里看得见。
        let (inbound_tx, incoming) = futures_mpsc::unbounded::<std::io::Result<String>>();
        {
            let reader_shared = shared.clone();
            let reader_trace = trace.clone();
            let reader_frames = raw_update_frames.clone();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(error) => {
                            let _ = inbound_tx.unbounded_send(Err(error));
                            break;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                        if value.get("method").and_then(Value::as_str) == Some("session/update") {
                            reader_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        reader_trace.record("in", &value);
                    }
                    if inbound_tx.unbounded_send(Ok(line)).is_err() {
                        break;
                    }
                }
                reader_shared.stdout_closed.store(true, Ordering::SeqCst);
            });
        }

        // 出站：SDK 的 Sink 收报文，写线程写进 stdin。stdin 句柄留在共享槽位里，
        // 会话收尾时可以随时取走关闭，不必等 SDK 连接对象被回收。
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let (outgoing, mut outgoing_rx) = futures_mpsc::channel::<String>(64);
        // Sink 需要拿到 Sender 的所有权；struct 里留一个克隆句柄，收尾时丢掉它
        // 就等于关掉出站通道。
        let outgoing_sink = outgoing.clone();
        {
            let writer_stdin = stdin.clone();
            let writer_trace = trace.clone();
            thread::spawn(move || {
                while let Some(line) = futures::executor::block_on(outgoing_rx.next()) {
                    {
                        let mut guard = writer_stdin.lock().unwrap();
                        let Some(stdin) = guard.as_mut() else {
                            return;
                        };
                        let written = stdin
                            .write_all(line.as_bytes())
                            .and_then(|()| stdin.write_all(b"\n"))
                            .and_then(|()| stdin.flush());
                        if written.is_err() {
                            return;
                        }
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        writer_trace.record("out", &value);
                    }
                }
            });
        }

        let permission_policy = plan.policy;
        let permission_attribution = plan.attribution.clone();
        let (outcome_tx, outcome) = mpsc::channel::<Result<AcpSessionOutcome, String>>();
        let connection = {
            let notification_shared = shared.clone();
            let permission_shared = shared.clone();
            let fallback_shared = shared.clone();
            let connection_shared = shared.clone();
            let main_shared = shared.clone();
            let main_trace = trace.clone();
            let outcome_from_main = outcome_tx.clone();
            thread::spawn(move || {
                let sink = outgoing_sink.sink_map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, error.to_string())
                });
                let result = futures::executor::block_on(
                    AcpClient
                        .builder()
                        .name("himind-agent")
                        .on_receive_notification(
                            async move |notification: SessionNotification, _cx| {
                                // 复用自研时期的更新归并：最终答复正文与工具调用计数
                                // 都按报文字段判定，避免跟着 SDK 的枚举变体改口径。
                                let update = serde_json::to_value(&notification.update)
                                    .unwrap_or(Value::Null);
                                let mut state = notification_shared.state.lock().unwrap();
                                apply_session_update(&update, &mut state);
                                Ok(())
                            },
                            agent_client_protocol::on_receive_notification!(),
                        )
                        .on_receive_request(
                            async move |request: RequestPermissionRequest, responder, _cx| {
                                let params =
                                    serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
                                let mut is_aborted = || -> Result<bool, Box<dyn Error>> {
                                    Ok(permission_shared.aborted.load(Ordering::Relaxed))
                                };
                                let outcome = match permission_outcome(
                                    &params,
                                    permission_policy,
                                    &permission_attribution,
                                    &mut is_aborted,
                                ) {
                                    Ok(outcome) => outcome,
                                    Err(error) => {
                                        return responder.respond_with_error(
                                            AcpSdkError::internal_error()
                                                .data(json!(error.to_string())),
                                        );
                                    }
                                };
                                permission_shared
                                    .state
                                    .lock()
                                    .unwrap()
                                    .permission_requests
                                    .push(json!({
                                        "tool_call": params
                                            .get("toolCall")
                                            .cloned()
                                            .unwrap_or(Value::Null),
                                        "options": params
                                            .get("options")
                                            .cloned()
                                            .unwrap_or_else(|| json!([])),
                                        "outcome": outcome.clone(),
                                    }));
                                responder.respond(RequestPermissionResponse::new(
                                    permission_response_outcome(&outcome),
                                ))
                            },
                            agent_client_protocol::on_receive_request!(),
                        )
                        .on_receive_request(
                            // SDK 对未注册请求默认返回 Handled::No：带 sessionId 的报文
                            // 会被压进 pending 永久等待，连内置的 -32601 都等不到。
                            // 这里显式兜底，仍然回「不支持该方法」，与自研报文一致。
                            async move |request: UntypedMessage, responder, _cx| {
                                fallback_shared
                                    .state
                                    .lock()
                                    .unwrap()
                                    .denied_client_methods
                                    .push(request.method.clone());
                                responder.respond_with_error(
                                    AcpSdkError::method_not_found().data(json!(request.method)),
                                )
                            },
                            agent_client_protocol::on_receive_request!(),
                        )
                        .connect_with(Lines::new(sink, incoming), {
                            move |cx: ConnectionTo<Agent>| {
                                let shared = main_shared;
                                let trace = main_trace;
                                let plan = plan;
                                async move {
                                    *shared.connection.lock().unwrap() = Some(cx.clone());
                                    let result = drive_session(&cx, &shared, &trace, &plan).await;
                                    if let Err(message) = &result {
                                        shared.fail(message.clone());
                                    }
                                    // 会话结果必须在 main_fn 里就交出去：connect_with
                                    // 要等 stdout EOF 才返回，等到那时再报结果就晚了。
                                    let _ = outcome_from_main.send(result.clone());
                                    match result {
                                        Ok(_) => Ok(()),
                                        Err(_) => Err(AcpSdkError::internal_error()),
                                    }
                                }
                            }
                        }),
                );
                if let Err(error) = result {
                    let message = connection_shared.failure_text().unwrap_or_else(|| {
                        if connection_shared.stdout_closed.load(Ordering::Relaxed) {
                            "ACP agent closed stdout before responding".to_string()
                        } else {
                            process::summarize_output(&error.to_string(), 2_000)
                        }
                    });
                    let _ = outcome_tx.send(Err(message));
                }
            })
        };
        Ok(Self {
            child: Some(child),
            stdin,
            outgoing: Some(outgoing),
            shared,
            outcome,
            connection: Some(connection),
            stderr,
            raw_update_frames,
            trace,
        })
    }

    /// 等会话出结果。SDK 没有请求级超时，取消与超时都在这里兜：超时按剩余时间
    /// 收敛成 100ms 轮询，既能及时响应取消，又不会空转。
    fn drive(
        &mut self,
        timeout: Duration,
        is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
    ) -> Result<AcpSessionOutcome, Box<dyn Error>> {
        let started = Instant::now();
        loop {
            if is_canceled()? {
                let message = format!("ACP {} was canceled", self.shared.phase());
                return Err(self.abort("canceled", message));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                let message = format!("ACP {} timed out", self.shared.phase());
                return Err(self.abort("timeout", message));
            }
            match self
                .outcome
                .recv_timeout(remaining.min(ACP_MESSAGE_POLL_INTERVAL))
            {
                Ok(Ok(outcome)) => return Ok(outcome),
                Ok(Err(message)) => {
                    self.trace.event(
                        "session_abort",
                        json!({"reason": "protocol", "method": self.shared.phase(), "error": &message}),
                    );
                    return Err(message.into());
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let message = self
                        .shared
                        .failure_text()
                        .unwrap_or_else(|| "ACP agent closed stdout before responding".to_string());
                    self.trace.event(
                        "session_abort",
                        json!({"reason": "stdout_closed", "method": self.shared.phase(), "error": &message}),
                    );
                    return Err(message.into());
                }
            }
        }
    }

    /// 取消/超时的收尾：先让协议层知道（session/cancel），再留一点宽限期，
    /// 最后才终止进程树。文案先落进共享槽位，避免被随后的传输层报错顶掉。
    fn abort(&mut self, reason: &str, message: String) -> Box<dyn Error> {
        self.shared.fail(message.clone());
        self.shared.aborted.store(true, Ordering::SeqCst);
        let session_id = self.shared.session_id.lock().unwrap().clone();
        let connection = self.shared.connection.lock().unwrap().clone();
        if let (Some(session_id), Some(connection)) = (session_id, connection) {
            let _ = connection.send_notification(CancelNotification::new(session_id));
        }
        self.trace.event(
            "session_abort",
            json!({"reason": reason, "method": self.shared.phase()}),
        );
        thread::sleep(ACP_CANCEL_GRACE_PERIOD);
        self.terminate();
        message.into()
    }

    fn raw_update_frames(&self) -> usize {
        self.raw_update_frames.load(Ordering::Relaxed)
    }

    fn finish(mut self) -> Result<String, Box<dyn Error>> {
        // 先松开 SDK 的出站通道与 stdin：连接线程要等 stdout EOF 才返回，
        // 先关掉 stdin 让对端退出，再终止进程，join 才不会挂住。
        self.outgoing.take();
        let _ = self.stdin.lock().unwrap().take();
        let deadline = Instant::now() + Duration::from_secs(2);
        if let Some(child) = self.child.as_mut() {
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        self.terminate();
        if let Some(connection) = self.connection.take() {
            let _ = connection.join();
        }
        Ok(process::join_output(self.stderr.take()))
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            process::terminate_process_tree(&mut child);
        }
    }
}

impl Drop for AcpProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// 权限结果 → SDK 的类型化回包。落库沿用自研时期的形状：
/// `{"outcome":"selected","optionId":..}` 选一项，其余一律按取消处理。
fn permission_response_outcome(outcome: &Value) -> RequestPermissionOutcome {
    let selected = outcome
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|kind| *kind == "selected")
        .and_then(|_| outcome.get("optionId"))
        .and_then(Value::as_str);
    match selected {
        Some(option_id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option_id.to_string(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    }
}

/// 会话主体的三步：initialize → session/new → session/prompt。
///
/// 每一步先把「当前在等哪个方法的回包」写进共享槽位，取消与超时才能给出对的文案；
/// 回包里的协议错误统一收敛成一句话，交给等待线程按既有语义往上报。
async fn drive_session(
    cx: &ConnectionTo<Agent>,
    shared: &Arc<AcpSessionShared>,
    trace: &AcpTrace,
    plan: &AcpSessionPlan,
) -> Result<AcpSessionOutcome, String> {
    shared.set_phase("initialize");
    let initialize = cx
        .send_request(
            InitializeRequest::new(ProtocolVersion::V1)
                .client_info(Implementation::new("HiMind Agent", crate::VERSION)),
        )
        .block_task()
        .await
        .map_err(|error| process::summarize_output(&error.to_string(), 2_000))?;
    let negotiated = initialize.protocol_version.as_u16();
    if negotiated != ACP_PROTOCOL_VERSION as u16 {
        return Err(format!(
            "ACP agent negotiated unsupported protocol version {negotiated}"
        ));
    }
    trace.event("initialize", json!({"protocol_version": negotiated}));

    shared.set_phase("session/new");
    let session = cx
        .send_request(NewSessionRequest::new(plan.workspace.clone()))
        .block_task()
        .await
        .map_err(|error| process::summarize_output(&error.to_string(), 2_000))?;
    let session_id = session.session_id.0.to_string();
    *shared.session_id.lock().unwrap() = Some(session_id.clone());

    shared.set_phase("session/prompt");
    let prompt = cx
        .send_request(PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(plan.prompt.clone()))],
        ))
        .block_task()
        .await
        .map_err(|error| process::summarize_output(&error.to_string(), 2_000))?;
    let stop_reason = serde_json::to_value(prompt.stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "end_turn".to_string());
    if stop_reason == "cancelled" {
        return Err("ACP prompt was canceled".to_string());
    }
    Ok(AcpSessionOutcome {
        session_id,
        stop_reason,
    })
}

fn configured_profiles() -> Result<BTreeMap<String, AcpProfile>, Box<dyn Error>> {
    let mut profiles = BTreeMap::new();
    for profile in crate::store::acp_profiles::list()? {
        if !profile.enabled {
            continue;
        }
        profiles.insert(
            profile.provider_id,
            AcpProfile {
                executable: profile.executable,
                args: profile.args,
                env: profile.env,
                version: profile.version,
                permission_policy: profile.permission_policy,
                source: "agent".to_string(),
            },
        );
    }
    if let Ok(raw) = env::var(ACP_PROFILE_ENVIRONMENT) {
        if !raw.trim().is_empty() {
            let value: Value = serde_json::from_str(&raw)?;
            let object = value
                .as_object()
                .ok_or("ACP runtime profiles must be a JSON object")?;
            for (name, value) in object {
                let profile: AcpProfile = serde_json::from_value(value.clone())?;
                let provider = if name.starts_with("acp.") {
                    name.clone()
                } else {
                    format!("acp.{}", name.trim())
                };
                if provider == "acp." {
                    return Err("ACP runtime profile name is empty".into());
                }
                profiles.insert(provider, profile);
            }
        }
    }
    if let Ok(executable) = env::var(ACP_EXECUTABLE_ENVIRONMENT) {
        if !executable.trim().is_empty() {
            let args = env::var(ACP_ARGS_ENVIRONMENT)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| serde_json::from_str::<Vec<String>>(&value))
                .transpose()?
                .unwrap_or_default();
            profiles.insert(
                "acp.stdio".to_string(),
                AcpProfile {
                    executable,
                    args,
                    env: BTreeMap::new(),
                    version: String::new(),
                    permission_policy: env::var(ACP_PERMISSION_ENVIRONMENT).unwrap_or_default(),
                    source: "environment".to_string(),
                },
            );
        }
    }
    Ok(profiles)
}

fn profile_for(provider: &str) -> Result<AcpProfile, Box<dyn Error>> {
    configured_profiles()?
        .remove(provider)
        .ok_or_else(|| format!("ACP runtime profile is not configured: {provider}").into())
}

fn profile_permission_policy(profile: &AcpProfile) -> Result<PermissionPolicy, Box<dyn Error>> {
    PermissionPolicy::parse(&profile.permission_policy)
}

fn resolve_executable(executable: &str) -> Result<PathBuf, Box<dyn Error>> {
    let executable = executable.trim();
    if executable.is_empty() {
        return Err("ACP runtime executable is empty".into());
    }
    // 前缀 CLI（npx / node / opencode）和 MCP 下游命令踩的是同一个坑，
    // 查找规则统一放在 process 里，避免两处 PATHEXT 行为慢慢走偏。
    crate::runtime::process::resolve_executable(executable)
        .ok_or_else(|| format!("ACP runtime executable is unavailable: {executable}").into())
}

fn build_prompt(claim: &AgentRunClaim) -> Result<String, Box<dyn Error>> {
    let mut prompt = claim.run.instruction.trim().to_string();
    if prompt.is_empty() {
        return Err("Agent Run instruction is empty".into());
    }
    if !claim.run.input.is_null()
        && claim
            .run
            .input
            .as_object()
            .is_none_or(|value| !value.is_empty())
    {
        prompt.push_str("\n\nStructured input (JSON):\n");
        prompt.push_str(&serde_json::to_string(&claim.run.input)?);
    }
    Ok(prompt)
}

fn permission_outcome(
    params: &Value,
    policy: PermissionPolicy,
    attribution: &AcpAttribution,
    is_canceled: &mut dyn FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<Value, Box<dyn Error>> {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if policy == PermissionPolicy::Prompt {
        let tool = params.get("toolCall").cloned().unwrap_or_else(|| json!({}));
        let tool_id = tool
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = tool
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ACP tool call");
        let kind = tool
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let approved = crate::approval::manager::ApprovalManager::global()
            .request_capability_approval_with_cancel(
                "acp.permission",
                "R3",
                process::summarize_output(&format!("{}: {title}", attribution.provider), 300),
                process::summarize_output(
                    &format!(
                        "provider={} run_id={} session_id={} tool_call_id={} kind={kind}",
                        attribution.provider, attribution.run_id, session_id, tool_id,
                    ),
                    1_000,
                ),
                || is_canceled().map_err(|error| error.to_string()),
            )?;
        let Some(approved) = approved else {
            return Err("ACP permission request was canceled".into());
        };
        return Ok(select_permission_option(
            &options,
            if approved {
                "allow_once"
            } else {
                "reject_once"
            },
        ));
    }
    let preferred_kind = match policy {
        PermissionPolicy::AllowOnce => "allow_once",
        PermissionPolicy::Deny => "reject_once",
        PermissionPolicy::Prompt => unreachable!(),
    };
    Ok(select_permission_option(&options, preferred_kind))
}

fn select_permission_option(options: &[Value], preferred_kind: &str) -> Value {
    if let Some(option_id) = options
        .iter()
        .find(|option| option.get("kind").and_then(Value::as_str) == Some(preferred_kind))
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
    {
        return json!({"outcome": "selected", "optionId": option_id});
    }
    if preferred_kind.starts_with("reject") {
        if let Some(option_id) = options
            .iter()
            .find(|option| {
                option
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.starts_with("reject"))
            })
            .and_then(|option| option.get("optionId"))
            .and_then(Value::as_str)
        {
            return json!({"outcome": "selected", "optionId": option_id});
        }
    }
    json!({"outcome": "cancelled"})
}

fn apply_session_update(update: &Value, state: &mut AcpSessionState) {
    state.update_count = state.update_count.saturating_add(1);
    match update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "agent_message_chunk" => {
            if let Some(text) = update
                .get("content")
                .filter(|content| content.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
            {
                state.final_text.push_str(text);
            }
        }
        "tool_call" | "tool_call_update" => {
            state.tool_call_count = state.tool_call_count.saturating_add(1);
        }
        _ => {}
    }
}

fn execution_metadata(execution: &AcpExecution, attribution: &AcpAttribution) -> Value {
    json!({
        "provider": attribution.provider,
        "run_id": attribution.run_id,
        "stop_reason": execution.stop_reason,
        "update_count": execution.update_count,
        "tool_call_count": execution.tool_call_count,
        "permission_requests": execution.permission_requests,
        "denied_client_methods": execution.denied_client_methods,
        "stderr": execution.stderr,
        "log_path": execution.log_path,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        permission_outcome, AcpAttribution, AcpTrace, PermissionPolicy, ACP_TRACE_ENVIRONMENT,
    };
    use serde_json::json;
    use std::env;
    use std::ffi::OsString;
    use std::fs;

    #[test]
    fn permission_policy_denies_by_default() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({
                "options": [
                    {"optionId":"allow","name":"Allow","kind":"allow_once"},
                    {"optionId":"deny","name":"Deny","kind":"reject_once"}
                ]
            }),
            PermissionPolicy::Deny,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"selected","optionId":"deny"}));
    }

    #[test]
    fn permission_policy_allows_once_only_when_explicit() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({
                "options": [
                    {"optionId":"allow","name":"Allow","kind":"allow_once"},
                    {"optionId":"deny","name":"Deny","kind":"reject_once"}
                ]
            }),
            PermissionPolicy::AllowOnce,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"selected","optionId":"allow"}));
    }

    #[test]
    fn permission_policy_cancels_without_a_matching_option() {
        let mut canceled = || Ok(false);
        let outcome = permission_outcome(
            &json!({"options":[]}),
            PermissionPolicy::AllowOnce,
            &AcpAttribution {
                provider: "acp.test".to_string(),
                run_id: "run-1".to_string(),
            },
            &mut canceled,
        )
        .unwrap();
        assert_eq!(outcome, json!({"outcome":"cancelled"}));
    }

    /// 桌面面板按固定 key 读取前置 CLI；改了这份契约，UI 只会静默显示「不可用」。
    #[test]
    fn probe_executables_reports_preset_prerequisites() {
        let report = super::probe_executables();
        for name in super::ACP_PROBE_EXECUTABLES {
            let entry = report
                .get(name)
                .unwrap_or_else(|| panic!("missing executable report: {name}"));
            assert!(
                entry
                    .get("available")
                    .and_then(serde_json::Value::as_bool)
                    .is_some(),
                "{name} must report an availability flag"
            );
            assert!(entry.get("path").is_some(), "{name} must report a path");
            // source 决定面板要不要把绝对路径写进配置：只有「只在已知安装位置找到」
            // 才写，PATH 命中的必须继续留命令名。没探到命令时没有安装位置可言。
            let source = entry["source"].as_str().unwrap_or_default();
            assert!(
                matches!(source, "path" | "install_location"),
                "{name} must report a known source, got {source:?}"
            );
            if !entry["available"].as_bool().unwrap_or(false) {
                assert_eq!(
                    source, "path",
                    "{name} is unavailable, so it cannot come from an install location"
                );
            }
        }
    }

    /// 目录名带版本号才算安装目录；顺手把版本号带出来，面板不用显示写死的假版本。
    #[test]
    fn version_directory_is_parsed_for_release_names_only() {
        assert_eq!(super::parse_version("2.0.20"), Some(vec![2, 0, 20]));
        assert_eq!(super::parse_version("10.0.1"), Some(vec![10, 0, 1]));
        assert_eq!(super::parse_version("2.0.20-beta"), None);
        assert_eq!(super::parse_version("cli"), None);
        assert_eq!(super::parse_version(""), None);
    }

    /// OpenCode 桌面版自带 CLI 只在安装目录里；探测必须能挑出版本最高的那个。
    #[test]
    fn versioned_cli_candidates_prefer_the_newest_release() {
        let root = env::temp_dir().join(format!("himind-acp-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for version in ["2.0.9", "2.0.20", "2.0.12", "not-a-version"] {
            let directory = root.join(version);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("opencode-cli.exe"), b"stub").unwrap();
        }
        let candidates = super::versioned_cli_candidates(root.clone());
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.version.as_str())
                .collect::<Vec<_>>(),
            vec!["2.0.20", "2.0.12", "2.0.9"]
        );
        assert!(candidates
            .iter()
            .all(|candidate| candidate.from_install_location));
    }

    #[test]
    fn unavailable_acp_profiles_report_why() {
        let profiles = std::collections::BTreeMap::from([(
            "acp.missing".to_string(),
            super::AcpProfile {
                executable: "himind-agent-definitely-missing-executable".to_string(),
                args: Vec::new(),
                env: Default::default(),
                version: "1.0.0".to_string(),
                permission_policy: "deny".to_string(),
                source: "test".to_string(),
            },
        )]);
        let report = super::installation_report("acp.missing", &profiles["acp.missing"]);
        assert_eq!(report.status, "unavailable");
        assert_eq!(report.capabilities["executable_available"], json!(false));
        assert_eq!(report.capabilities["permission_policy_valid"], json!(true));
        assert!(
            report.capabilities["executable_error"]
                .as_str()
                .is_some_and(|error| error.contains("unavailable")),
            "{}",
            report.capabilities
        );
    }

    /// 出站排障依赖 trace 文件本身，这里把「写在哪、写多少」钉住：
    /// 默认 summary 必须留下方法名与请求 id，但不能把 prompt 正文写进去。
    #[test]
    fn acp_trace_summary_keeps_methods_without_payload() {
        let home = trace_test_home("summary");
        let _guard = crate::store::paths::test_env_lock();
        let previous_home = env::var_os("HIMIND_AGENT_HOME");
        let previous_mode = env::var_os(ACP_TRACE_ENVIRONMENT);
        env::set_var("HIMIND_AGENT_HOME", &home);
        env::remove_var(ACP_TRACE_ENVIRONMENT);

        let trace = AcpTrace::for_run("run/with:unsafe*chars");
        assert_eq!(trace.path.file_name().unwrap(), "runwithunsafechars.jsonl");
        trace.record(
            "out",
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "session/prompt",
                "params": {"prompt": [{"type": "text", "text": "TOP-SECRET-PROMPT"}]}
            }),
        );
        trace.record(
            "in",
            &json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"update": {"sessionUpdate": "agent_message_chunk"}}
            }),
        );
        assert_eq!(trace.path_string().is_some(), true);

        let body = fs::read_to_string(&trace.path).unwrap();
        assert!(body.contains("\"method\":\"session/prompt\""), "{body}");
        assert!(body.contains("\"id\":1"), "{body}");
        assert!(
            body.contains("\"update\":\"agent_message_chunk\""),
            "{body}"
        );
        assert!(!body.contains("TOP-SECRET-PROMPT"), "{body}");
        assert!(!body.contains("\"payload\""), "{body}");

        restore_env("HIMIND_AGENT_HOME", previous_home);
        restore_env(ACP_TRACE_ENVIRONMENT, previous_mode);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn acp_trace_full_mode_includes_redacted_payload() {
        let home = trace_test_home("full");
        let _guard = crate::store::paths::test_env_lock();
        let previous_home = env::var_os("HIMIND_AGENT_HOME");
        let previous_mode = env::var_os(ACP_TRACE_ENVIRONMENT);
        env::set_var("HIMIND_AGENT_HOME", &home);
        env::set_var(ACP_TRACE_ENVIRONMENT, "full");

        let trace = AcpTrace::for_run("run-1");
        trace.record(
            "out",
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": {"cwd": "C:\\work", "headers": ["token=SUPER-SECRET"]}
            }),
        );

        let body = fs::read_to_string(&trace.path).unwrap();
        assert!(body.contains("\"payload\""), "{body}");
        assert!(body.contains("session/new"), "{body}");
        assert!(!body.contains("SUPER-SECRET"), "{body}");
        assert!(body.contains("[REDACTED]"), "{body}");

        restore_env("HIMIND_AGENT_HOME", previous_home);
        restore_env(ACP_TRACE_ENVIRONMENT, previous_mode);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn acp_trace_off_writes_no_file() {
        let home = trace_test_home("off");
        let _guard = crate::store::paths::test_env_lock();
        let previous_home = env::var_os("HIMIND_AGENT_HOME");
        let previous_mode = env::var_os(ACP_TRACE_ENVIRONMENT);
        env::set_var("HIMIND_AGENT_HOME", &home);
        env::set_var(ACP_TRACE_ENVIRONMENT, "off");

        let trace = AcpTrace::for_run("run-1");
        trace.record("out", &json!({"id": 1, "method": "initialize"}));
        trace.event("session_start", json!({"provider": "acp.test"}));
        assert_eq!(trace.path_string(), None);
        assert!(!trace.path.exists());

        restore_env("HIMIND_AGENT_HOME", previous_home);
        restore_env(ACP_TRACE_ENVIRONMENT, previous_mode);
        let _ = fs::remove_dir_all(&home);
    }

    fn trace_test_home(label: &str) -> std::path::PathBuf {
        let home = env::temp_dir().join(format!("himind-acp-trace-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        home
    }

    fn restore_env(key: &str, value: Option<OsString>) {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
}
