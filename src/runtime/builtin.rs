use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::Value;
use std::error::Error;

use crate::api::types::{RuntimeInstallationReport, Task};
use crate::runtime::{deepseek_harness, AgentRunEnvelope, PROVIDER_BUILTIN};
use crate::Options;

pub(crate) const ENGINE_ID: &str = "deepseek-harness";
pub(crate) const CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct BuiltinAIRuntimeEvent {
    pub schema_version: u32,
    pub session_id: String,
    pub event_id: String,
    pub sequence: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub turn_id: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAIRuntimeStatus {
    pub provider: String,
    pub status: String,
    pub version: String,
    pub compatible: bool,
    pub message: String,
    pub diagnostics: BuiltinAIRuntimeDiagnostics,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAIRuntimeDiagnostics {
    pub engine_id: String,
    pub executable_path: String,
    pub contract_version: u32,
    pub update_mode: String,
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    deepseek_harness::probe()
}

pub(crate) fn status() -> BuiltinAIRuntimeStatus {
    from_engine_status(deepseek_harness::status())
}

pub(crate) fn install(
    options: &Options,
    client_instance_id: &str,
) -> Result<BuiltinAIRuntimeStatus, String> {
    deepseek_harness::install(options, client_instance_id).map(from_engine_status)
}

pub(crate) fn prepare_interactive_launch(
    options: &Options,
) -> Result<deepseek_harness::InteractiveLaunch, String> {
    deepseek_harness::prepare_interactive_launch(options)
}

pub(crate) fn interactive_event_projector() -> deepseek_harness::InteractiveEventProjector {
    deepseek_harness::InteractiveEventProjector::default()
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    deepseek_harness::execute(client, options, agent_id, task, envelope)
}

fn from_engine_status(
    engine: deepseek_harness::DeepSeekHarnessRuntimeStatus,
) -> BuiltinAIRuntimeStatus {
    let ready = engine.status == "ready" && engine.cli_compatible;
    BuiltinAIRuntimeStatus {
        provider: PROVIDER_BUILTIN.to_string(),
        status: if ready { "ready" } else { "unavailable" }.to_string(),
        version: engine.version,
        compatible: ready,
        message: if ready {
            "HiMind AI 已就绪。".to_string()
        } else {
            "内置 AI 组件尚未安装或需要修复。".to_string()
        },
        diagnostics: BuiltinAIRuntimeDiagnostics {
            engine_id: ENGINE_ID.to_string(),
            executable_path: engine.executable_path,
            contract_version: CONTRACT_VERSION,
            update_mode: "managed-distribution".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_status_is_projected_to_stable_product_contract() {
        let status = from_engine_status(deepseek_harness::DeepSeekHarnessRuntimeStatus {
            provider: "internal-engine".to_string(),
            status: "ready".to_string(),
            version: "1.2.3".to_string(),
            cli_compatible: true,
            executable_path: "engine.cmd".to_string(),
            install_command: String::new(),
            message: String::new(),
            candidate: false,
        });
        assert_eq!(status.provider, PROVIDER_BUILTIN);
        assert_eq!(status.status, "ready");
        assert_eq!(status.diagnostics.engine_id, ENGINE_ID);
        assert!(!status.message.contains("DeepSeek"));
    }
}
