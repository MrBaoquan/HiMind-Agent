use serde::Serialize;
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

/// Stable diagnostic contract returned by Agent-owned extension authoring
/// orchestration capabilities. External AI clients can branch on `code` and
/// `stage` without parsing localized error text.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuthoringBlocker {
    pub code: String,
    pub stage: String,
    pub severity: String,
    pub message: String,
    pub remediation: String,
    pub retryable: bool,
}

#[derive(Debug)]
pub(crate) struct AuthoringError {
    payload: Value,
}

impl AuthoringError {
    pub(crate) fn new(payload: Value) -> Self {
        Self { payload }
    }
}

impl fmt::Display for AuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.payload)
    }
}

impl Error for AuthoringError {}

pub(crate) fn blocker(
    code: impl Into<String>,
    stage: impl Into<String>,
    message: impl Into<String>,
    remediation: impl Into<String>,
    retryable: bool,
) -> AuthoringBlocker {
    AuthoringBlocker {
        code: code.into(),
        stage: stage.into(),
        severity: "error".to_string(),
        message: message.into(),
        remediation: remediation.into(),
        retryable,
    }
}

pub(crate) fn warning(message: impl Into<String>) -> Value {
    json!({"message": message.into()})
}

pub(crate) fn success(kind: &str, details: Value) -> Value {
    let mut output = details;
    if let Some(object) = output.as_object_mut() {
        object.insert("state".to_string(), json!("passed"));
        object.insert("kind".to_string(), json!(kind));
        object.insert("blockers".to_string(), json!([]));
        object
            .entry("warnings".to_string())
            .or_insert_with(|| json!([]));
        object
            .entry("next_steps".to_string())
            .or_insert_with(|| json!([]));
    }
    output
}

pub(crate) fn blocked(
    kind: &str,
    blockers: Vec<AuthoringBlocker>,
    warnings: Vec<Value>,
    next_steps: Vec<String>,
) -> Value {
    json!({
        "state": "blocked",
        "kind": kind,
        "blockers": blockers,
        "warnings": warnings,
        "next_steps": next_steps,
    })
}

pub(crate) fn blocked_error(
    kind: &str,
    blockers: Vec<AuthoringBlocker>,
    warnings: Vec<Value>,
    next_steps: Vec<String>,
) -> Box<dyn Error> {
    Box::new(AuthoringError::new(blocked(
        kind, blockers, warnings, next_steps,
    )))
}

pub(crate) fn operation_error(
    kind: &str,
    stage: &str,
    error: impl std::fmt::Display,
    remediation: &str,
) -> Box<dyn Error> {
    operation_error_with_code(
        kind,
        stage,
        "extension_operation_failed",
        error,
        remediation,
    )
}

pub(crate) fn operation_error_with_code(
    kind: &str,
    stage: &str,
    code: &str,
    error: impl std::fmt::Display,
    remediation: &str,
) -> Box<dyn Error> {
    blocked_error(
        kind,
        vec![blocker(code, stage, error.to_string(), remediation, true)],
        Vec::new(),
        vec![format!("修复 {stage} 阶段后重新调用 extension.test")],
    )
}

pub(crate) fn payload_from_error(error: &dyn Error) -> Value {
    serde_json::from_str::<Value>(&error.to_string()).unwrap_or_else(|_| {
        blocked(
            "unknown",
            vec![blocker(
                "extension_operation_failed",
                "unknown",
                error.to_string(),
                "根据 message 修复后重试",
                true,
            )],
            Vec::new(),
            Vec::new(),
        )
    })
}
