use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::api::oauth::{platform_access_token, AgentAccessToken, AI_CONVERSATION_SCOPE};
use crate::app::builtin_ai_proxy::BuiltinAiProxyControl;
use crate::Options;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const CAPABILITY_PROBE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct RuntimeCommand {
    id: String,
    provider: String,
    provider_session_id: String,
    command_type: String,
    claim_token: String,
    payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeCapabilities {
    pub(crate) supports_message_inject: bool,
    pub(crate) supports_interrupt: bool,
    pub(crate) supports_approval: bool,
    pub(crate) supports_question: bool,
    pub(crate) supports_live_session: bool,
}

pub(crate) type RuntimeCapabilitiesState = Arc<Mutex<RuntimeCapabilities>>;

impl RuntimeCapabilities {
    pub(crate) fn conservative() -> Self {
        Self {
            supports_message_inject: false,
            supports_interrupt: false,
            supports_approval: false,
            supports_question: false,
            supports_live_session: false,
        }
    }

    pub(crate) fn as_json(self) -> Value {
        json!({
            "supports_live_session": self.supports_live_session,
            "supports_event_stream": true,
            "supports_message_inject": self.supports_message_inject,
            "supports_interrupt": self.supports_interrupt,
            "supports_approval": self.supports_approval,
            "supports_question": self.supports_question,
            "supports_resume": false,
            "supports_artifact": false,
            "supports_remote_control": self.supports_message_inject
                || self.supports_interrupt
                || self.supports_approval
                || self.supports_question
        })
    }

    pub(crate) fn remote_control_status(self) -> &'static str {
        if self.supports_message_inject || self.supports_interrupt {
            "verified"
        } else {
            "unverified"
        }
    }
}

pub(crate) struct BuiltinAiCommandGateway {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl BuiltinAiCommandGateway {
    pub(crate) fn start(
        options: Options,
        control: BuiltinAiProxyControl,
        capabilities: RuntimeCapabilitiesState,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            eprintln!("HiMind AI 远程会话控制网关已启动");
            let client = match Client::builder().timeout(POLL_TIMEOUT).build() {
                Ok(client) => client,
                Err(error) => {
                    eprintln!("HiMind AI 远程会话控制不可用：{error}");
                    return;
                }
            };
            let mut last_capability_probe = Instant::now() - CAPABILITY_PROBE_INTERVAL;
            while !worker_shutdown.load(Ordering::Acquire) {
                if !options.mode().dashboard_enabled() {
                    break;
                }
                if last_capability_probe.elapsed() >= CAPABILITY_PROBE_INTERVAL {
                    if let Ok(access) = platform_access_token(&options, AI_CONVERSATION_SCOPE) {
                        refresh_runtime_capabilities(
                            &client,
                            &options,
                            &access,
                            &control,
                            &capabilities,
                        );
                    }
                    last_capability_probe = Instant::now();
                }
                let did_work = poll_and_execute(&client, &options, &control, &worker_shutdown);
                if !did_work {
                    for _ in 0..10 {
                        if worker_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(POLL_INTERVAL / 10);
                    }
                }
            }
        });
        Self {
            shutdown,
            worker: Some(worker),
        }
    }

    pub(crate) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for BuiltinAiCommandGateway {
    fn drop(&mut self) {
        self.stop();
    }
}

fn poll_and_execute(
    client: &Client,
    options: &Options,
    control: &BuiltinAiProxyControl,
    shutdown: &AtomicBool,
) -> bool {
    if !options.mode().dashboard_enabled() {
        return false;
    }
    let access = match platform_access_token(options, AI_CONVERSATION_SCOPE) {
        Ok(access) => access,
        Err(error) => {
            eprintln!("HiMind AI 远程会话控制获取授权失败：{error}");
            return false;
        }
    };
    let commands = match claim_commands(client, options, &access) {
        Ok(commands) => commands,
        Err(error) => {
            eprintln!(
                "HiMind AI 远程会话控制领取失败（transient={}）：{}",
                error.transient, error.message
            );
            return false;
        }
    };
    if commands.is_empty() {
        return false;
    }
    for command in commands {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let (status, result, error) = execute_command(control, &command);
        if status == "defer" {
            continue;
        }
        if let Err(error) =
            complete_command(client, options, &access, &command, status, result, error)
        {
            eprintln!(
                "HiMind AI 远程会话控制完成命令失败（transient={}）：{}",
                error.transient, error.message
            );
        }
    }
    true
}

fn claim_commands(
    client: &Client,
    options: &Options,
    access: &AgentAccessToken,
) -> Result<Vec<RuntimeCommand>, GatewayError> {
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/commands/claim",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&json!({ "limit": 8 }))
        .send()
        .map_err(|error| GatewayError::transient(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(GatewayError {
            message: format!("Dashboard returned HTTP {status}"),
            transient: status == StatusCode::REQUEST_TIMEOUT
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error(),
        });
    }
    #[derive(Deserialize)]
    struct ClaimResponse {
        #[serde(default)]
        items: Vec<RuntimeCommand>,
    }
    response
        .json::<ClaimResponse>()
        .map(|value| value.items)
        .map_err(|error| GatewayError::transient(error.to_string()))
}

fn complete_command(
    client: &Client,
    options: &Options,
    access: &AgentAccessToken,
    command: &RuntimeCommand,
    status: &str,
    result: Value,
    error: String,
) -> Result<(), GatewayError> {
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/commands/{}/complete",
            options.api_base.trim_end_matches('/'),
            command.id
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&json!({
            "claim_token": command.claim_token,
            "status": status,
            "result": result,
            "error": error,
        }))
        .send()
        .map_err(|error| GatewayError::transient(error.to_string()))?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(GatewayError {
        message: format!(
            "Dashboard returned HTTP {} while completing command",
            response.status()
        ),
        transient: response.status().is_server_error()
            || response.status() == StatusCode::REQUEST_TIMEOUT,
    })
}

fn execute_command(
    control: &BuiltinAiProxyControl,
    command: &RuntimeCommand,
) -> (&'static str, Value, String) {
    if command.provider != "himind.builtin" {
        return (
            "unsupported",
            json!({}),
            format!("本机 Agent 不承载 {} 运行时", command.provider),
        );
    }
    let (method, payload) = match map_runtime_command(command) {
        Ok(request) => request,
        Err(error) => return ("unsupported", json!({}), error),
    };
    if command.command_type == "approval.respond" || command.command_type == "question.respond" {
        let rpc_id = command
            .payload
            .get("rpc_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = match command.command_type.as_str() {
            "approval.respond" => json!({
                "sessionId": command.provider_session_id,
                "approvalId": command.payload.get("approval_id").and_then(Value::as_str).unwrap_or_default(),
                "outcome": command.payload.get("outcome").and_then(Value::as_str).unwrap_or_default(),
            }),
            "question.respond" => json!({
                "sessionId": command.provider_session_id,
                "answer": command.payload.get("answer").cloned().unwrap_or_else(|| json!({"answers": []})),
            }),
            _ => json!({}),
        };
        return match control.respond_runtime_request(rpc_id, value) {
            Ok(response) => ("succeeded", response, String::new()),
            Err(error) if is_unsupported_error(&error) => ("unsupported", json!({}), error),
            Err(error) if is_transient_runtime_error(&error) => ("defer", json!({}), error),
            Err(error) => ("failed", json!({}), error),
        };
    }
    match control.call_runtime_api(method, payload) {
        Ok(response) => {
            let result = response.get("result");
            if result
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                == Some(false)
            {
                let code = result
                    .and_then(|value| value.get("error"))
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = result
                    .and_then(|value| value.get("error"))
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("DSH 拒绝了远程会话命令")
                    .to_string();
                if is_unsupported_code(code) || is_unsupported_error(&message) {
                    return ("unsupported", json!({}), message);
                }
                return ("failed", json!({}), message);
            }
            ("succeeded", response, String::new())
        }
        Err(error) => {
            if is_unsupported_error(&error) {
                ("unsupported", json!({}), error)
            } else if is_transient_runtime_error(&error) {
                ("defer", json!({}), error)
            } else {
                ("failed", json!({}), error)
            }
        }
    }
}

fn map_runtime_command(command: &RuntimeCommand) -> Result<(&'static str, Value), String> {
    match command.command_type.as_str() {
        "message.inject" => {
            let content = command
                .payload
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|content| !content.is_empty());
            let Some(content) = content else {
                return Err("message.inject payload requires content".to_string());
            };
            Ok((
                "session.prompt",
                json!({
                    "sessionId": command.provider_session_id,
                    "mode": "queue",
                    "content": [{"type": "text", "text": content}]
                }),
            ))
        }
        "session.interrupt" => Ok((
            "session.cancel",
            json!({"sessionId": command.provider_session_id}),
        )),
        "session.snapshot" => Ok((
            "session.history",
            json!({"sessionId": command.provider_session_id, "maxMessages": 200}),
        )),
        "approval.respond" | "question.respond" => Ok(("client-response", Value::Null)),
        _ => Err("unsupported runtime command type".to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct RuntimeSession {
    id: String,
    #[serde(default)]
    agent_id: String,
    provider: String,
    provider_session_id: String,
}

fn refresh_runtime_capabilities(
    client: &Client,
    options: &crate::Options,
    access: &AgentAccessToken,
    control: &BuiltinAiProxyControl,
    capabilities_state: &RuntimeCapabilitiesState,
) {
    let sessions = match list_runtime_sessions(client, options, access) {
        Ok(sessions) => sessions,
        Err(error) => {
            if error.transient {
                return;
            }
            eprintln!("HiMind AI Runtime 能力探测读取会话失败：{}", error.message);
            return;
        }
    };
    let capabilities = sessions
        .iter()
        .find(|session| {
            session.agent_id == access.agent_id
                && session.provider == "himind.builtin"
                && !session.id.is_empty()
        })
        .map(|session| probe_runtime_capabilities(control, &session.provider_session_id));
    if let Some(capabilities) = capabilities {
        if let Ok(mut current) = capabilities_state.lock() {
            *current = capabilities;
        }
        for session in sessions {
            if session.agent_id != access.agent_id
                || session.provider != "himind.builtin"
                || session.id.is_empty()
            {
                continue;
            }
            let _ = heartbeat_runtime_capabilities(client, options, access, &session, capabilities);
        }
    }
}

fn list_runtime_sessions(
    client: &Client,
    options: &crate::Options,
    access: &AgentAccessToken,
) -> Result<Vec<RuntimeSession>, GatewayError> {
    let response = client
        .get(format!(
            "{}/api/integrations/ai/runtime/sessions",
            options.api_base.trim_end_matches('/')
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .send()
        .map_err(|error| GatewayError::transient(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(GatewayError {
            message: format!("Dashboard returned HTTP {status}"),
            transient: status == StatusCode::REQUEST_TIMEOUT
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error(),
        });
    }
    #[derive(Deserialize)]
    struct RuntimeSessionListResponse {
        #[serde(default)]
        items: Vec<RuntimeSession>,
    }
    response
        .json::<RuntimeSessionListResponse>()
        .map(|value| value.items)
        .map_err(|error| GatewayError::transient(error.to_string()))
}

fn probe_runtime_capabilities(
    control: &BuiltinAiProxyControl,
    provider_session_id: &str,
) -> RuntimeCapabilities {
    let supports_message_inject = probe_business_route(
        control,
        "session.prompt",
        json!({
            "sessionId": provider_session_id,
            "mode": "queue",
            // Deliberately omit the required text field. DSH validates the
            // envelope before enqueueing, so this probe cannot create a
            // user-visible turn in a real session.
            "content": [{"type": "text"}]
        }),
    );
    let supports_interrupt =
        probe_business_route(control, "session.cancel", json!({"sessionId": ""}));
    let supports_live_session = control
        .probe_runtime_api(
            "session.history",
            json!({"sessionId": provider_session_id, "maxMessages": 1}),
        )
        .is_ok();
    RuntimeCapabilities {
        supports_message_inject,
        supports_interrupt,
        // The response carrier is part of the authenticated DSH protocol.
        // Actual commands still require a matching pending correlation in
        // Dashboard, so advertising these does not permit arbitrary RPCs.
        supports_approval: supports_message_inject,
        supports_question: supports_message_inject,
        supports_live_session,
    }
}

pub(crate) fn probe_builtin_ai_capabilities(
    control: &BuiltinAiProxyControl,
) -> RuntimeCapabilities {
    probe_runtime_capabilities(control, "himind-capability-probe")
}

fn probe_business_route(control: &BuiltinAiProxyControl, method: &str, payload: Value) -> bool {
    let Ok(response) = control.probe_runtime_api(method, payload) else {
        return false;
    };
    probe_response_supports_route(&response)
}

fn probe_response_supports_route(response: &Value) -> bool {
    let Some(result) = response.get("result") else {
        return false;
    };
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    matches!(
        result
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str),
        Some(
            "bad-request"
                | "invalid-request"
                | "invalid-payload"
                | "invalid-params"
                | "validation-error"
                | "session-not-found"
                | "session-conflict"
                | "agent-busy"
                | "queue-item-not-found"
        )
    )
}

fn heartbeat_runtime_capabilities(
    client: &Client,
    options: &crate::Options,
    access: &AgentAccessToken,
    session: &RuntimeSession,
    capabilities: RuntimeCapabilities,
) -> Result<(), GatewayError> {
    let response = client
        .post(format!(
            "{}/api/integrations/ai/runtime/sessions/{}/heartbeat",
            options.api_base.trim_end_matches('/'),
            session.id
        ))
        .bearer_auth(&access.token)
        .header("X-HiMind-Agent-ID", &access.agent_id)
        .header("X-HiMind-AI-Client", "himind-agent")
        .json(&json!({
            "provider": session.provider,
            "provider_session_id": session.provider_session_id,
            "status": "online",
            "generation": 1,
            "capabilities": capabilities.as_json(),
            "metadata": {
                "surface": "himind_agent",
                "runtime_contract": "himind.builtin",
                "remote_control_status": if capabilities.supports_message_inject || capabilities.supports_interrupt { "verified" } else { "unverified" }
            }
        }))
        .send()
        .map_err(|error| GatewayError::transient(error.to_string()))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(GatewayError {
            message: format!(
                "Dashboard returned HTTP {} while updating Runtime capabilities",
                response.status()
            ),
            transient: response.status().is_server_error()
                || response.status() == StatusCode::REQUEST_TIMEOUT,
        })
    }
}

fn is_unsupported_error(error: &str) -> bool {
    let value = error.to_ascii_lowercase();
    value.contains("unsupported")
        || value.contains("unknown method")
        || value.contains("method_not_found")
        || value.contains("http 404")
}

fn is_unsupported_code(code: &str) -> bool {
    matches!(
        code.trim().to_ascii_lowercase().as_str(),
        "method-not-found" | "method_not_found" | "unknown-method" | "unknown_method"
    )
}

fn is_transient_runtime_error(error: &str) -> bool {
    let value = error.to_ascii_lowercase();
    value.contains("请求失败")
        || value.contains("连接")
        || value.contains("timeout")
        || value.contains("timed out")
        || value.contains("connection")
}

struct GatewayError {
    message: String,
    transient: bool,
}

impl GatewayError {
    fn transient(message: String) -> Self {
        Self {
            message,
            transient: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_provider_errors_are_not_reported_as_success() {
        assert!(is_unsupported_error("method_not_found"));
        assert!(is_unsupported_error("DSH returned unsupported command"));
        assert!(is_unsupported_error("DSH session.prompt returned HTTP 404"));
        assert!(!is_unsupported_error("session-not-found"));
        assert!(!is_unsupported_error("DSH session not found"));
        assert!(!is_unsupported_error("permission denied"));
        assert!(is_unsupported_code("method-not-found"));
        assert!(!is_unsupported_code("session-not-found"));
    }

    #[test]
    fn runtime_capabilities_are_serialized_for_dashboard_negotiation() {
        let value = RuntimeCapabilities {
            supports_message_inject: true,
            supports_interrupt: true,
            supports_approval: false,
            supports_question: false,
            supports_live_session: true,
        }
        .as_json();
        assert_eq!(value["supports_message_inject"], true);
        assert_eq!(value["supports_interrupt"], true);
        assert_eq!(value["supports_remote_control"], true);
        assert_eq!(value["supports_approval"], false);
    }

    #[test]
    fn capability_probe_accepts_business_validation_errors() {
        let response = json!({
            "result": {
                "ok": false,
                "error": {"code": "bad-request"}
            }
        });
        assert!(probe_response_supports_route(&response));
        assert!(!probe_response_supports_route(&json!({
            "result": {"ok": false, "error": {"code": "permission-denied"}}
        })));
    }

    #[test]
    fn runtime_commands_use_installed_dsh_rpc_names_and_shapes() {
        let command = RuntimeCommand {
            id: "command-1".to_string(),
            provider: "himind.builtin".to_string(),
            provider_session_id: "session-1".to_string(),
            command_type: "message.inject".to_string(),
            claim_token: "claim-1".to_string(),
            payload: json!({"content": "继续处理"}),
        };
        let (method, payload) = map_runtime_command(&command).unwrap();
        assert_eq!(method, "session.prompt");
        assert_eq!(payload["sessionId"], "session-1");
        assert_eq!(payload["mode"], "queue");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "继续处理");

        let interrupt = RuntimeCommand {
            command_type: "session.interrupt".to_string(),
            ..command
        };
        let (method, payload) = map_runtime_command(&interrupt).unwrap();
        assert_eq!(method, "session.cancel");
        assert_eq!(payload["sessionId"], "session-1");
    }

    #[test]
    fn client_response_commands_use_the_authenticated_response_carrier() {
        let command = RuntimeCommand {
            id: "command-1".to_string(),
            provider: "himind.builtin".to_string(),
            provider_session_id: "session-1".to_string(),
            command_type: "approval.respond".to_string(),
            claim_token: "claim-1".to_string(),
            payload: json!({"rpc_id": "rpc-1", "approval_id": "approval-1", "outcome": "rejected"}),
        };
        let (method, payload) = map_runtime_command(&command).unwrap();
        assert_eq!(method, "client-response");
        assert!(payload.is_null());
    }
}
