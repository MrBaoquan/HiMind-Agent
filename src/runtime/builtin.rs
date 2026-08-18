use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::Value;
use std::error::Error;
use std::path::PathBuf;

use crate::api::types::{RuntimeInstallationReport, Task};
use crate::runtime::{deepseek_harness, AgentRunEnvelope, PROVIDER_BUILTIN};
use crate::Options;

pub(crate) const ENGINE_ID: &str = "deepseek-harness";
pub(crate) const CONTRACT_VERSION: u32 = 1;

/// Product-facing runtime boundary. The active implementation can change
/// without leaking its process, profile, or event details into the app layer.
trait AIRuntimeAdapter: Sync {
    fn probe(&self) -> RuntimeInstallationReport;
    fn status(&self) -> BuiltinAIRuntimeStatus;
    fn check_update(
        &self,
        options: &Options,
        client_instance_id: &str,
    ) -> Result<BuiltinAIRuntimeUpdateStatus, String>;
    fn install(
        &self,
        options: &Options,
        client_instance_id: &str,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String>;
    fn update(
        &self,
        options: &Options,
        client_instance_id: &str,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String>;
    fn uninstall(
        &self,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String>;
    fn prepare_interactive_launch(
        &self,
        options: &Options,
        requested_model: Option<&str>,
    ) -> Result<BuiltinAIInteractiveLaunch, String>;
    fn interactive_tool_context_summary(
        &self,
        options: &Options,
    ) -> Result<BuiltinAIToolContextSummary, String>;
    fn interactive_event_projector(&self) -> BuiltinAIEventProjector;
    fn execute(
        &self,
        client: &Client,
        options: &Options,
        agent_id: &str,
        task: &Task,
        envelope: &AgentRunEnvelope,
    ) -> Result<Value, Box<dyn Error>>;
}

struct DeepSeekHarnessAdapter;

static ACTIVE_RUNTIME_ADAPTER: DeepSeekHarnessAdapter = DeepSeekHarnessAdapter;

fn active_adapter() -> &'static dyn AIRuntimeAdapter {
    &ACTIVE_RUNTIME_ADAPTER
}

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
pub(crate) struct BuiltinAIRuntimeUpdateStatus {
    pub update_available: bool,
    pub current_version: String,
    pub available_version: String,
    pub release_notes: String,
    pub mandatory: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAIModelOptions {
    pub selected_model: String,
    pub models: Vec<String>,
    pub source_type: String,
    pub source_name: String,
    pub source_provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAIToolContextSummary {
    pub skills: usize,
    pub mcp_services: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct BuiltinAIInteractiveLaunch {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub permission_mode: &'static str,
}

pub(crate) struct BuiltinAIEventProjector {
    inner: deepseek_harness::InteractiveEventProjector,
}

impl BuiltinAIEventProjector {
    pub(crate) fn project(&mut self, message: &Value) -> Option<BuiltinAIRuntimeEvent> {
        self.inner.project(message)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltinAIRuntimeDiagnostics {
    pub engine_id: String,
    pub executable_path: String,
    pub contract_version: u32,
    pub update_mode: String,
}

pub(crate) fn probe() -> RuntimeInstallationReport {
    active_adapter().probe()
}

pub(crate) fn status() -> BuiltinAIRuntimeStatus {
    active_adapter().status()
}

pub(crate) fn check_update(
    options: &Options,
    client_instance_id: &str,
) -> Result<BuiltinAIRuntimeUpdateStatus, String> {
    active_adapter().check_update(options, client_instance_id)
}

pub(crate) fn install(
    options: &Options,
    client_instance_id: &str,
) -> Result<BuiltinAIRuntimeStatus, String> {
    let mut ignore_progress = |_: &str, _: u8, _: &str| {};
    install_with_progress(options, client_instance_id, &mut ignore_progress)
}

pub(crate) fn install_with_progress(
    options: &Options,
    client_instance_id: &str,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<BuiltinAIRuntimeStatus, String> {
    active_adapter().install(options, client_instance_id, report_progress)
}

pub(crate) fn update_with_progress(
    options: &Options,
    client_instance_id: &str,
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<BuiltinAIRuntimeStatus, String> {
    active_adapter().update(options, client_instance_id, report_progress)
}

pub(crate) fn uninstall_with_progress(
    report_progress: &mut dyn FnMut(&str, u8, &str),
) -> Result<BuiltinAIRuntimeStatus, String> {
    active_adapter().uninstall(report_progress)
}

pub(crate) fn model_options(options: &Options) -> Result<BuiltinAIModelOptions, String> {
    crate::api::ai::fetch_client_model_options(options, "himind-agent")
        .map(|options| BuiltinAIModelOptions {
            selected_model: options.selected_model,
            models: options.models,
            source_type: options.source_type,
            source_name: options.source_name,
            source_provider: options.source_provider,
        })
        .map_err(|error| error.to_string())
}

pub(crate) fn prepare_interactive_launch(
    options: &Options,
    requested_model: Option<&str>,
) -> Result<BuiltinAIInteractiveLaunch, String> {
    active_adapter().prepare_interactive_launch(options, requested_model)
}

pub(crate) fn interactive_tool_context_summary(
    options: &Options,
) -> Result<BuiltinAIToolContextSummary, String> {
    active_adapter().interactive_tool_context_summary(options)
}

pub(crate) fn interactive_event_projector() -> BuiltinAIEventProjector {
    active_adapter().interactive_event_projector()
}

pub(crate) fn execute(
    client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
    envelope: &AgentRunEnvelope,
) -> Result<Value, Box<dyn Error>> {
    active_adapter().execute(client, options, agent_id, task, envelope)
}

impl AIRuntimeAdapter for DeepSeekHarnessAdapter {
    fn probe(&self) -> RuntimeInstallationReport {
        deepseek_harness::probe()
    }

    fn status(&self) -> BuiltinAIRuntimeStatus {
        from_engine_status(deepseek_harness::status())
    }

    fn check_update(
        &self,
        options: &Options,
        client_instance_id: &str,
    ) -> Result<BuiltinAIRuntimeUpdateStatus, String> {
        deepseek_harness::check_update(options, client_instance_id)
            .map(|update| BuiltinAIRuntimeUpdateStatus {
                update_available: update.update_available,
                current_version: update.current_version,
                available_version: update.available_version,
                release_notes: update.release_notes,
                mandatory: update.mandatory,
            })
            .map_err(productize_runtime_error)
    }

    fn install(
        &self,
        options: &Options,
        client_instance_id: &str,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String> {
        deepseek_harness::install_with_progress(options, client_instance_id, report_progress)
            .map(from_engine_status)
            .map_err(productize_runtime_error)
    }

    fn update(
        &self,
        options: &Options,
        client_instance_id: &str,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String> {
        deepseek_harness::update_with_progress(options, client_instance_id, report_progress)
            .map(from_engine_status)
            .map_err(productize_runtime_error)
    }

    fn uninstall(
        &self,
        report_progress: &mut dyn FnMut(&str, u8, &str),
    ) -> Result<BuiltinAIRuntimeStatus, String> {
        deepseek_harness::uninstall_with_progress(report_progress)
            .map(from_engine_status)
            .map_err(productize_runtime_error)
    }

    fn prepare_interactive_launch(
        &self,
        options: &Options,
        requested_model: Option<&str>,
    ) -> Result<BuiltinAIInteractiveLaunch, String> {
        deepseek_harness::prepare_interactive_launch(options, requested_model).map(|launch| {
            BuiltinAIInteractiveLaunch {
                executable: launch.executable,
                home: launch.home,
                api_key: launch.api_key,
                base_url: launch.base_url,
                model: launch.model,
                permission_mode: launch.permission_mode,
            }
        })
    }

    fn interactive_tool_context_summary(
        &self,
        options: &Options,
    ) -> Result<BuiltinAIToolContextSummary, String> {
        deepseek_harness::interactive_tool_context_summary(options).map(|summary| {
            BuiltinAIToolContextSummary {
                skills: summary.skills,
                mcp_services: summary.mcp_services,
            }
        })
    }

    fn interactive_event_projector(&self) -> BuiltinAIEventProjector {
        BuiltinAIEventProjector {
            inner: deepseek_harness::InteractiveEventProjector::default(),
        }
    }

    fn execute(
        &self,
        client: &Client,
        options: &Options,
        agent_id: &str,
        task: &Task,
        envelope: &AgentRunEnvelope,
    ) -> Result<Value, Box<dyn Error>> {
        deepseek_harness::execute(client, options, agent_id, task, envelope)
    }
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
            "HiMind AI 运行时尚未安装或需要修复。".to_string()
        },
        diagnostics: BuiltinAIRuntimeDiagnostics {
            engine_id: ENGINE_ID.to_string(),
            executable_path: engine.executable_path,
            contract_version: CONTRACT_VERSION,
            update_mode: "managed-distribution".to_string(),
        },
    }
}

fn productize_runtime_error(error: String) -> String {
    error
        .replace("DeepSeek Harness Runtime", "HiMind AI 运行时")
        .replace("Dashboard Runtime", "HiMind AI 运行时")
        .replace("Runtime", "运行时")
        .replace("manifest", "安装清单")
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

    #[test]
    fn runtime_errors_do_not_leak_internal_product_names() {
        let error = productize_runtime_error(
            "Dashboard Runtime manifest for DeepSeek Harness Runtime is invalid".to_string(),
        );
        assert!(!error.contains("DeepSeek"));
        assert!(!error.contains("Dashboard Runtime"));
        assert!(error.contains("HiMind AI 运行时"));
        assert!(error.contains("安装清单"));
    }
}
