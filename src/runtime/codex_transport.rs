//! Shared Codex transport for Workflow and claimed AgentRun. Never reads personal config.
use std::{error::Error, process::{Command, Stdio}};
use serde_json::Value;
use crate::runtime::process;
const RUNTIME_OUTPUT_LIMIT: usize = 64 * 1024;
#[cfg(all(test, windows))]
thread_local! {
    pub(crate) static AFTER_ROOT_EXIT: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) struct CodexOutcome {
    pub text: String,
    pub session_id: String,
    pub usage: Option<crate::agent_core_contracts::LocalRunUsage>,
    pub version: String,
    pub usage_receipt: Value,
}
pub(crate) fn execute(command: &mut Command, prompt: &str, result_path: &std::path::Path,
    timeout_seconds: u64, is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    shutdown_limit: std::time::Duration, run_id: &str, resume_session: &str) -> Result<CodexOutcome, Box<dyn Error>> {
    if is_canceled()? { return Err("Codex execution was canceled before launch".into()); }
    let meter = crate::runtime::codex_usage::Meter::begin(
        &crate::runtime::codex_usage::default_root(), run_id, resume_session)?;
    if let Some(receipt)=meter.receipt()? {
        return Err(Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
            message:"Codex receipt recovered; prior business completion is unknown; refusing duplicate provider execution".into(),
            report:None,usage:crate::runtime::codex_usage::increment(&receipt),
            provenance:Some(serde_json::json!({"provider":"personal.codex","receipt_recovery_only":true,"usage_receipt":receipt})),
        }));
    }
    process::remove_file_if_present(result_path);
    command.stdin(Stdio::from(process::prompt_input(prompt).map_err(|e|format!("Codex input preparation failed: {e}"))?));
    let (mut child, tree) = crate::runtime::owned_tree::OwnedTree::spawn(command)
        .map_err(|e|format!("Codex owned process launch failed: {e}"))?;
    let completion = std::sync::Arc::new(std::sync::Mutex::new(CodexCompletion::default()));
    let observed = completion.clone();
    let stdout = child.stdout.take().map(|pipe| process::capture_runtime_output(pipe, true, move |reader| {
        // Parse and validate outside the completion mutex. Large tool records
        // cannot block the supervisor's cancellation/timeout checks on this lock.
        let Some(event) = CodexProtocolEvent::read(reader) else { return false; };
        observed.lock().map(|mut state| state.observe_event(event)).is_ok()
    }));
    let stderr = child.stderr.take().map(|pipe| process::capture_runtime_output(pipe, false, |_| true));
    let result = (|| -> Result<CodexOutcome, Box<dyn Error>> {
        let started = std::time::Instant::now();
        let timeout = std::env::var("HIMIND_CODEX_TIMEOUT_SECONDS").ok()
            .and_then(|s| s.parse::<u64>().ok()).filter(|n| *n > 0).unwrap_or(timeout_seconds);
        let status = loop {
            // Cancellation has precedence over an exit/final observed on the
            // same poll. A cancellation-read error also cleans up the child.
            match is_canceled() {
                Ok(false) => {}
                result => {
                    tree.terminate(&mut child)?;
                    result?;
                    return Err("Codex workflow runtime execution was canceled".into());
                }
            }
            if let Some(status) = child.try_wait()? { break Ok(status); }
            let terminal_at = completion.lock().map_err(|_| "Codex completion lock poisoned")?.terminal_at;
            if terminal_at.is_some_and(|at| at.elapsed() >= shutdown_limit) {
                tree.terminate(&mut child)?;
                break Err("Codex reported a terminal turn but did not exit within the shutdown deadline".to_string());
            }
            if started.elapsed() >= std::time::Duration::from_secs(timeout) {
                tree.terminate(&mut child)?;
                break Err(format!("Codex workflow runtime execution exceeded {timeout} seconds and was terminated"));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        #[cfg(all(test, windows))]
        if status.is_ok() {
            AFTER_ROOT_EXIT.with(|hook| { if let Some(hook) = hook.borrow_mut().as_mut() { hook(); } });
        }
        // The root has exited (or was terminated). Close the owned Job before
        // draining: inherited pipe writers must not consume the entire drain
        // deadline and keep descendants running after their root is gone.
        // Let reader threads consume root-exit EOF before classifying output.
        // Activity alone does not prove a member still owns a pipe writer.
        // This short grace fits inside the existing two-second drain bound.
        let drain_started = std::time::Instant::now();
        while status.is_ok() && (stdout.as_ref().is_some_and(|p| !p.finished()) || stderr.as_ref().is_some_and(|p| !p.finished())) {
            if is_canceled()? { return Err("Codex workflow runtime execution was canceled".into()); }
            if drain_started.elapsed() >= std::time::Duration::from_millis(500) { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let clean_before_cleanup = !stdout.as_ref().is_some_and(|p| !p.finished()) && !stderr.as_ref().is_some_and(|p| !p.finished());
        // Query before cleanup: EOF caused by killing a surviving writer must
        // not turn an incomplete provider shutdown into business success.
        let surviving_members = if status.is_ok() { tree.has_active_processes() } else { Ok(false) };
        drop(tree);
        while stdout.as_ref().is_some_and(|p| !p.finished()) || stderr.as_ref().is_some_and(|p| !p.finished()) {
            if is_canceled()? { return Err("Codex workflow runtime execution was canceled".into()); }
            if drain_started.elapsed() >= std::time::Duration::from_secs(2) { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if is_canceled()? { return Err("Codex workflow runtime execution was canceled".into()); }
        let stdout = stdout.map(|p| p.finish()).transpose();
        let stderr = stderr.map(|p| p.finish()).transpose();
        // Snapshot fixed event kinds and booleans only. Never include the CLI
        // error message, prompt, raw output, credentials or final text.
        let diagnostics = completion.lock().map_err(|_| "Codex completion lock poisoned")?.diagnostics();
        let exit_code = status.as_ref().ok().and_then(|status|status.code());
        let failure = |message: String| -> Box<dyn Error> {
            // Retain a complete negative report as evidence, never as success.
            let report = read_codex_final(&result_path).ok().and_then(|text| {
                let raw = text.trim();
                crate::workflow::parse_runtime_payload(raw)
            })
                .filter(|report| report.get("passed").and_then(Value::as_bool) == Some(false));
            Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
                provenance: completion.lock().ok().map(|state|serde_json::json!({
                    "provider":"personal.codex","session_id":state.session_id,
                    "exit_code":exit_code,
                    "cost_status":"unknown","model_source":"personal_cli_config",
                    "completion":state.completed,"failed":state.failed,
                })),
                message: format!("{message} ({diagnostics})"), report, usage: completion.lock().ok().and_then(|state| state.usage.clone()),
            })
        };
        let status = status.map_err(&failure)?;
        if !status.success() { return Err(failure(format!("Codex runtime step failed (exit={})", status.code().unwrap_or(-1)))); }
        if surviving_members.map_err(|e| failure(format!("Codex owned shutdown could not be verified: {e}")))? && !clean_before_cleanup {
            return Err(failure("Codex output drain deadline cannot establish clean EOF: owned descendants survived root exit and were terminated".into()));
        }
        // Both pipes must reach EOF cleanly. A surviving writer or truncated
        // capture cannot be accepted merely because the root exited zero.
        stdout.map_err(|e| failure(format!("Codex stdout: {e}")))?;
        stderr.map_err(|e| failure(format!("Codex stderr: {e}")))?;
        let text = read_codex_final(&result_path).map_err(|e| failure(e.to_string()))?;
        let state = completion.lock().map_err(|_| "Codex completion lock poisoned")?;
        if state.session_id.is_empty() || state.failed || !state.completed || state.final_message.as_deref().map(str::trim) != Some(text.trim()) {
            drop(state);
            return Err(failure("Codex final file is not bound to a complete agent_message and turn.completed".into()));
        }
        // Preserve passed=false in the output. Mission/verification policy
        // decides its business meaning; it is never replaced with true.
        if is_canceled()? { return Err("Codex workflow runtime execution was canceled".into()); }
        Ok(CodexOutcome { text: text.trim().to_owned(), session_id: state.session_id.clone(), usage: None,version:String::new(),usage_receipt:Value::Null })
    })();
    if result.is_err() && child.try_wait().ok().flatten().is_none() {
        // The closure has already dropped its owned Job, including on early
        // error. Reap only this retained root handle; never run an unbounded
        // PID-enumerating external command after the runtime deadline.
        let _ = child.kill();
        let _ = child.wait();
    }
    let result = result.map_err(|error| {
        if error.is::<crate::runtime::deepseek_harness::WorkflowRuntimeFailure>() { return error; }
        // Cancellation and pipe/discovery errors can occur before the normal
        // terminal branch; retain any already observed session/token snapshot.
        let state = completion.lock().ok();
        Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
            message: error.to_string(), report: None,
            usage: state.as_ref().and_then(|state|state.usage.clone()),
            provenance: state.as_ref().map(|state|serde_json::json!({
                "provider":"personal.codex","session_id":state.session_id,
                "exit_code":child.try_wait().ok().flatten().and_then(|status|status.code()),
                "cost_status":"unknown","model_source":"personal_cli_config",
            })),
        }) as Box<dyn Error>
    });
    let state = completion.lock().map_err(|_| "Codex usage completion lock poisoned")?;
    let receipt = meter.settle(&state.session_id, state.raw_usage.as_ref())?;
    let usage = crate::runtime::codex_usage::increment(&receipt);
    result.map(|mut outcome| { outcome.usage=usage.clone(); outcome.usage_receipt=receipt.clone(); outcome })
        .map_err(|error| {
            let message=error.to_string();
            if let Ok(mut failure) = error.downcast::<crate::runtime::deepseek_harness::WorkflowRuntimeFailure>() {
                failure.usage=usage;
                let provenance=failure.provenance.get_or_insert_with(||serde_json::json!({}));
                provenance["usage_receipt"]=receipt;
                failure as Box<dyn Error>
            } else { message.into() }
        })
}


#[derive(Default)]
pub(crate) struct CodexCompletion {
    pub(crate) final_message: Option<String>,
    pub(crate) completed: bool,
    pub(crate) terminal_at: Option<std::time::Instant>,
    pub(crate) failed: bool,
    pub(crate) error_notifications: u64,
    pub(crate) terminal_event: Option<&'static str>,
    pub(crate) error_class: Option<&'static str>,
    session_id: String,
    usage: Option<crate::agent_core_contracts::LocalRunUsage>,
    raw_usage: Option<crate::runtime::codex_usage::Counters>,
}

// Deserialize only control fields. serde skips tool output with IgnoredAny, so
// a multi-megabyte aggregated_output is validated without copying it into Value.
#[derive(serde::Deserialize)]
pub(crate) struct CodexProtocolEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_protocol_optional_object")]
    item: Option<CodexProtocolItem>,
    message: Option<String>,
    thread_id: Option<String>,
    usage: Option<CodexTokenUsage>,
    #[serde(default, deserialize_with = "deserialize_protocol_optional_object")]
    error: Option<CodexProtocolError>,
}

fn deserialize_protocol_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where D: serde::Deserializer<'de>, T: serde::Deserialize<'de> {
    struct Object<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Object<T> {
        type Value = T;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("a protocol JSON object") }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(Object(std::marker::PhantomData))
}

fn deserialize_protocol_optional_object<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: serde::Deserialize<'de> {
    struct Optional<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Optional<T> {
        type Value = Option<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("an optional protocol JSON object") }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value,E> { Ok(None) }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value,E> { Ok(None) }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value,D::Error> { deserialize_protocol_object(d).map(Some) }
    }
    deserializer.deserialize_option(Optional(std::marker::PhantomData))
}

impl CodexProtocolEvent {
    pub(crate) fn read(reader: &mut dyn std::io::Read) -> Option<Self> {
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        let event = deserialize_protocol_object(&mut deserializer).ok()?;
        deserializer.end().ok()?;
        Some(event)
    }
}

#[derive(serde::Deserialize)]
struct CodexProtocolItem {
    #[serde(rename = "type")]
    kind: String,
    phase: Option<String>,
    text: Option<String>,
}

#[derive(serde::Deserialize)]
struct CodexProtocolError {
    message: Option<String>,
}

pub(crate) fn codex_error_class(message: &str) -> &'static str {
    // Persist fixed categories only: upstream messages can contain credentials,
    // prompts and URLs. Never forward their arbitrary text to runtime events.
    let message = message.to_ascii_lowercase();
    let status = |code| message.contains(&format!("status {code}")) || message.contains(&format!("http {code}"));
    if status(503) { "upstream_service_unavailable" }
    else if status(502) || status(504) { "upstream_gateway_error" }
    else if status(429) || message.contains("rate limit") { "rate_limited" }
    else if status(401) || status(403) || message.contains("unauthorized") { "authentication_or_access" }
    else if message.contains("stream") { "response_stream" }
    else if message.contains("connect") || message.contains("network") { "connection" }
    else { "unclassified" }
}

impl CodexCompletion {
    #[cfg(test)]
    pub(crate) fn observe(&mut self, line: &[u8]) {
        assert!(self.observe_reader(&mut &line[..]));
    }

    pub(crate) fn observe_reader(&mut self, reader: &mut dyn std::io::Read) -> bool {
        let Some(event) = CodexProtocolEvent::read(reader) else { return false; };
        self.observe_event(event);
        true
    }

    fn observe_event(&mut self, event: CodexProtocolEvent) {
        match event.kind.as_str() {
            "thread.started" => {
                if let Some(id)=event.thread_id.filter(|id|!id.is_empty() && id.len()<=512 && !id.chars().any(char::is_control)) {
                    self.session_id=id;
                } else {self.failed=true;}
            }
            "turn.started" => {
                // One CLI exec is one turn. A second turn after a terminal
                // signal cannot erase failure or replace the completed final.
                if self.completed || self.terminal_at.is_some() { self.failed = true; }
                else { self.final_message = None; }
            }
            "item.completed" if event.item.as_ref().is_some_and(|item| item.kind == "agent_message") => {
                let item = event.item.unwrap();
                if !self.completed && self.terminal_at.is_none() && item.phase.as_deref().is_none_or(|phase| phase == "final_answer") {
                    self.final_message = item.text
                        .filter(|text| !text.trim().is_empty() && text.len() <= RUNTIME_OUTPUT_LIMIT);
                }
            }
            "turn.completed" => {
                if self.completed { self.failed = true; }
                self.raw_usage = event.usage.map(|usage| crate::runtime::codex_usage::Counters {
                    input_tokens:usage.input_tokens, output_tokens:usage.output_tokens,
                    cached_input_tokens:usage.cached_input_tokens, reasoning_output_tokens:usage.reasoning_output_tokens,
                });
                self.completed = true;
                // Only a completed turn already bound to its agent final starts
                // shutdown. A bare completion cannot borrow a later message.
                if !self.failed && self.final_message.is_some() {
                    self.terminal_at.get_or_insert_with(std::time::Instant::now);
                    self.terminal_event.get_or_insert("turn.completed");
                }
            }
            "turn.failed" => {
                self.failed = true;
                self.terminal_at.get_or_insert_with(std::time::Instant::now);
                self.terminal_event = Some("turn.failed");
                if let Some(message) = event.error.and_then(|error| error.message) { self.error_class = Some(codex_error_class(&message)); }
            }
            // Codex emits type:error while reconnecting, before continuing the
            // same turn. Its message is diagnostic, not a terminal protocol
            // signal. Do not store arbitrary text (it may contain secrets).
            "error" => {
                self.error_notifications = self.error_notifications.saturating_add(1);
                if let Some(message) = event.message { self.error_class = Some(codex_error_class(&message)); }
            }
            _ => {}
        }
    }

    pub(crate) fn diagnostics(&self) -> String {
        format!("cli_error_event={}; error_notifications={}; terminal_event={}; completed={}; final_observed={}; failed={}; error_class={}",
            if self.error_notifications == 0 { "none" } else { "error" }, self.error_notifications,
            self.terminal_event.unwrap_or("none"), self.completed, self.final_message.is_some(), self.failed, self.error_class.unwrap_or("none"))
    }
}

#[derive(serde::Deserialize)]
struct CodexTokenUsage { input_tokens: u64, output_tokens: u64, cached_input_tokens: Option<u64>, reasoning_output_tokens: Option<u64> }

fn read_codex_final(path: &std::path::Path) -> Result<String, Box<dyn Error>> {
    if std::fs::metadata(path)?.len() > RUNTIME_OUTPUT_LIMIT as u64 { return Err("Codex final file exceeded the output limit".into()); }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() { return Err("Codex final file is empty".into()); }
    Ok(text)
}
