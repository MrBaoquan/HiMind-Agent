use semver::Version;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use super::WorkflowPackage;
use crate::capability::types::{CapabilityAvailability, CapabilityDescriptor};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowCapabilityPreflight {
    pub id: String,
    pub available: bool,
    pub source: String,
    pub availability: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowToolPreflight {
    pub id: String,
    pub available: bool,
    pub required: bool,
    pub resolved_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowConnectorPreflight {
    pub id: String,
    pub available: bool,
    pub availability: String,
    pub credential_ownership: String,
    pub health_check: String,
    pub health_target: String,
    pub health_status: String,
    pub health_message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowConnectorProbe {
    pub id: String,
    pub status: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowPreflight {
    pub ready: bool,
    pub package_id: String,
    pub package_version: String,
    pub agent_version: String,
    pub capabilities: Vec<WorkflowCapabilityPreflight>,
    pub connectors: Vec<WorkflowConnectorPreflight>,
    pub tools: Vec<WorkflowToolPreflight>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn preflight(
    package: &WorkflowPackage,
    agent_version: &str,
    available_capabilities: &[CapabilityDescriptor],
) -> WorkflowPreflight {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    match (
        Version::parse(agent_version),
        Version::parse(&package.min_agent_version),
    ) {
        (Ok(current), Ok(minimum)) if current < minimum => blockers.push(format!(
            "Agent {} is older than required version {}",
            current, minimum
        )),
        (Err(error), _) => warnings.push(format!("Agent version cannot be parsed: {error}")),
        (_, Err(error)) => blockers.push(format!(
            "workflow minimum Agent version is invalid: {error}"
        )),
        _ => {}
    }

    let capability_map = available_capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<std::collections::BTreeMap<_, _>>();
    let capabilities = package
        .capabilities
        .iter()
        .map(|capability_id| {
            let capability = capability_map.get(capability_id.as_str()).copied();
            let available = capability.is_some_and(|capability| {
                capability.availability != CapabilityAvailability::ControlPlane
                    || capability.dashboard_provider
            });
            if !available {
                blockers.push(format!(
                    "required capability is unavailable: {capability_id}"
                ));
            }
            WorkflowCapabilityPreflight {
                id: capability_id.clone(),
                available,
                source: capability
                    .map(|capability| capability.source.clone())
                    .unwrap_or_default(),
                availability: capability
                    .map(|capability| capability.availability.as_str().to_string())
                    .unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    for plugin_id in &package.dependencies.plugins {
        let source = format!("plugin:{plugin_id}");
        if !available_capabilities
            .iter()
            .any(|capability| capability.source == source)
        {
            blockers.push(format!(
                "required workflow plugin is unavailable or disabled: {plugin_id}"
            ));
        }
    }

    let dashboard_available = available_capabilities
        .iter()
        .any(|capability| capability.dashboard_provider);
    let connectors = package
        .connectors
        .iter()
        .map(|connector| {
            let available = connector.availability() != CapabilityAvailability::ControlPlane
                || dashboard_available;
            if !available {
                blockers.push(format!(
                    "required workflow connector is unavailable: {}",
                    connector.id
                ));
            }
            if connector.credential_ownership == "dashboard" && !dashboard_available {
                blockers.push(format!(
                    "workflow connector {} requires Dashboard credential ownership",
                    connector.id
                ));
            }
            for capability_id in &connector.capabilities {
                if !package.capabilities.contains(capability_id) {
                    blockers.push(format!(
                        "workflow connector {} exposes undeclared capability: {capability_id}",
                        connector.id
                    ));
                }
            }
            let health_type = connector
                .health_check
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("none");
            WorkflowConnectorPreflight {
                id: connector.id.clone(),
                available,
                availability: connector.availability.clone(),
                credential_ownership: connector.credential_ownership.clone(),
                health_check: health_type.to_string(),
                health_target: if health_type == "capability" {
                    connector
                        .health_check
                        .get("target")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                } else if health_type == "http" {
                    connector
                        .health_check
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                } else {
                    String::new()
                },
                health_status: if matches!(health_type, "capability" | "http") {
                    "not_run".to_string()
                } else {
                    "not_configured".to_string()
                },
                health_message: String::new(),
            }
        })
        .collect::<Vec<_>>();

    let mut tools = Vec::new();
    for tool in required_tools(&package.local_requirements) {
        let resolved = resolve_executable(&tool);
        let available = resolved.is_some();
        if !available {
            blockers.push(format!("required local tool is unavailable: {tool}"));
        }
        tools.push(WorkflowToolPreflight {
            id: tool,
            available,
            required: true,
            resolved_path: resolved
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
        });
    }
    for tool in recommended_tools(&package.local_requirements) {
        let resolved = resolve_executable(&tool);
        let available = resolved.is_some();
        if !available {
            warnings.push(format!("recommended local tool is unavailable: {tool}"));
        }
        if tools.iter().any(|candidate| candidate.id == tool) {
            continue;
        }
        tools.push(WorkflowToolPreflight {
            id: tool,
            available,
            required: false,
            resolved_path: resolved
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
        });
    }

    for step in &package.steps {
        if step.capability_id.trim().is_empty() {
            warnings.push(format!(
                "workflow step {} requires a Runtime or provider executor",
                step.id
            ));
        } else if let Some(capability) = capability_map.get(step.capability_id.as_str()) {
            let effective = crate::approval::policy::effective_risk_level(
                &step.capability_id,
                &capability.risk_level,
            );
            if crate::approval::policy::risk_rank(effective) >= 3 && !step.approval_required {
                blockers.push(format!(
                    "workflow step {} exposes {effective} capability {} without an approval gate",
                    step.id, step.capability_id
                ));
            }
        }
    }

    WorkflowPreflight {
        ready: blockers.is_empty(),
        package_id: package.id.clone(),
        package_version: package.version.clone(),
        agent_version: agent_version.to_string(),
        capabilities,
        connectors,
        tools,
        blockers,
        warnings,
    }
}

pub(crate) fn preflight_with_connector_probes<F>(
    package: &WorkflowPackage,
    agent_version: &str,
    available_capabilities: &[CapabilityDescriptor],
    input: &Value,
    invoke: F,
) -> WorkflowPreflight
where
    F: FnMut(&str, Value) -> Result<Value, Box<dyn Error>>,
{
    let mut report = preflight(package, agent_version, available_capabilities);
    let probes = probe_connectors(package, available_capabilities, input, invoke);
    apply_connector_probes(&mut report, &probes);
    report
}

pub(crate) fn probe_connectors<F>(
    package: &WorkflowPackage,
    available_capabilities: &[CapabilityDescriptor],
    input: &Value,
    mut invoke: F,
) -> Vec<WorkflowConnectorProbe>
where
    F: FnMut(&str, Value) -> Result<Value, Box<dyn Error>>,
{
    let capability_map = available_capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<std::collections::BTreeMap<_, _>>();
    package
        .connectors
        .iter()
        .map(|connector| {
            let health = connector.health_check.as_object();
            let check_type = health
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("none");
            if check_type == "http" {
                let check =
                    match super::WorkflowHttpHealthCheck::from_manifest(&connector.health_check) {
                        Ok(check) => check,
                        Err(error) => {
                            return WorkflowConnectorProbe {
                                id: connector.id.clone(),
                                status: "failed".to_string(),
                                target: String::new(),
                                message: error,
                            };
                        }
                    };
                return match super::execute_http_health_check(&check) {
                    Ok(status) => WorkflowConnectorProbe {
                        id: connector.id.clone(),
                        status: "passed".to_string(),
                        target: check.url,
                        message: format!("HTTP status {status}"),
                    },
                    Err(error) => WorkflowConnectorProbe {
                        id: connector.id.clone(),
                        status: "failed".to_string(),
                        target: check.url,
                        message: error.to_string(),
                    },
                };
            }
            if check_type != "capability" {
                return WorkflowConnectorProbe {
                    id: connector.id.clone(),
                    status: "not_configured".to_string(),
                    target: String::new(),
                    message: String::new(),
                };
            }
            let target = health
                .and_then(|value| value.get("target"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let Some(capability) = capability_map.get(target.as_str()).copied() else {
                return WorkflowConnectorProbe {
                    id: connector.id.clone(),
                    status: "failed".to_string(),
                    target,
                    message: "health target capability is unavailable".to_string(),
                };
            };
            let effective_risk = crate::approval::policy::effective_risk_level(
                &capability.id,
                &capability.risk_level,
            );
            if crate::approval::policy::risk_rank(effective_risk)
                > crate::approval::policy::risk_rank("R1")
            {
                return WorkflowConnectorProbe {
                    id: connector.id.clone(),
                    status: "failed".to_string(),
                    target,
                    message: format!(
                        "health target capability must be read_only/R1, got {effective_risk}"
                    ),
                };
            }
            let probe_input =
                connector_health_input(input, health.and_then(|value| value.get("input")));
            let probe_input = match super::connector::resolve_connector_credentials_for_capability(
                package,
                &target,
                &probe_input,
            ) {
                Ok(input) => input,
                Err(error) => {
                    return WorkflowConnectorProbe {
                        id: connector.id.clone(),
                        status: "failed".to_string(),
                        target,
                        message: error.to_string(),
                    };
                }
            };
            let probe_input = filter_capability_input(&capability.input_schema, &probe_input);
            match invoke(&target, probe_input) {
                Ok(_) => WorkflowConnectorProbe {
                    id: connector.id.clone(),
                    status: "passed".to_string(),
                    target,
                    message: String::new(),
                },
                Err(error) => WorkflowConnectorProbe {
                    id: connector.id.clone(),
                    status: "failed".to_string(),
                    target,
                    message: error.to_string(),
                },
            }
        })
        .collect()
}

fn apply_connector_probes(report: &mut WorkflowPreflight, probes: &[WorkflowConnectorProbe]) {
    for probe in probes {
        if let Some(connector) = report
            .connectors
            .iter_mut()
            .find(|connector| connector.id == probe.id)
        {
            connector.health_status = probe.status.clone();
            connector.health_target = probe.target.clone();
            connector.health_message = probe.message.clone();
        }
        if probe.status == "failed" {
            report.blockers.push(format!(
                "connector {} health probe failed: {}",
                probe.id,
                if probe.message.trim().is_empty() {
                    "unknown error"
                } else {
                    probe.message.as_str()
                }
            ));
        }
    }
    report.ready = report.blockers.is_empty();
}

fn connector_health_input(workflow_input: &Value, health_input: Option<&Value>) -> Value {
    let mut input = workflow_input.as_object().cloned().unwrap_or_default();
    if let Some(overrides) = health_input.and_then(Value::as_object) {
        input.extend(overrides.clone());
    }
    Value::Object(input)
}

fn filter_capability_input(schema: &Value, input: &Value) -> Value {
    let Some(input) = input.as_object() else {
        return input.clone();
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Value::Object(input.clone());
    };
    if schema.get("additionalProperties").and_then(Value::as_bool) != Some(false) {
        return Value::Object(input.clone());
    }
    Value::Object(
        input
            .iter()
            .filter(|(name, _)| properties.contains_key(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    )
}

fn required_tools(requirements: &Value) -> Vec<String> {
    tool_values(requirements, "required_tools")
}

fn recommended_tools(requirements: &Value) -> Vec<String> {
    let mut values = tool_values(requirements, "tools");
    values.extend(tool_values(requirements, "recommended_tools"));
    values.sort();
    values.dedup();
    values
}

fn tool_values(requirements: &Value, key: &str) -> Vec<String> {
    requirements
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.is_absolute() || candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path = env::var_os("PATH")?;
    let extensions = executable_extensions();
    for directory in env::split_paths(&path) {
        let direct = directory.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for extension in &extensions {
            let with_extension = directory.join(format!("{name}{extension}"));
            if with_extension.is_file() {
                return Some(with_extension);
            }
        }
    }
    None
}

fn executable_extensions() -> HashSet<String> {
    #[cfg(windows)]
    {
        let from_environment = env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .map(|extension| extension.to_ascii_lowercase())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        if from_environment.is_empty() {
            [".exe", ".cmd", ".bat", ".com"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect()
        } else {
            from_env_extensions(from_environment)
        }
    }
    #[cfg(not(windows))]
    {
        HashSet::new()
    }
}

#[cfg(windows)]
fn from_env_extensions(values: HashSet<String>) -> HashSet<String> {
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::types::CapabilityAvailability;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn package(required_tools: Vec<&str>) -> WorkflowPackage {
        WorkflowPackage {
            schema_version: crate::workflow::WORKFLOW_PACKAGE_SCHEMA_VERSION.to_string(),
            id: "com.himind.workflow.test".to_string(),
            version: "1.0.0".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            min_agent_version: "0.3.47".to_string(),
            local_requirements: json!({"required_tools": required_tools}),
            optional_providers: Vec::new(),
            capabilities: vec!["system.health".to_string()],
            dependencies: Default::default(),
            candidate: None,
            steps: vec![crate::workflow::WorkflowStep {
                id: "STEP-1".to_string(),
                title: "Health".to_string(),
                kind: "capability".to_string(),
                capability_id: "system.health".to_string(),
                runtime: None,
                loop_config: None,
                when: None,
                candidate_action: String::new(),
                input: json!({}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                depends_on: Vec::new(),
            }],
            artifacts: Vec::new(),
            ui: crate::workflow::WorkflowUi {
                mode: "standard".to_string(),
                entry: String::new(),
                surfaces: Vec::new(),
            },
            supported_runtimes: vec!["himind.builtin".to_string()],
            created_at: String::new(),
            source_root: PathBuf::new(),
            connectors: Vec::new(),
        }
    }

    fn capability(id: &str) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: id.to_string(),
            version: "1.0.0".to_string(),
            name: id.to_string(),
            description: String::new(),
            risk_level: "read_only".to_string(),
            source: "test".to_string(),
            contract_source: "test".to_string(),
            contract_generation: None,
            availability: CapabilityAvailability::Local,
            execution_mode: "sync".to_string(),
            supports_progress: false,
            supports_cancel: false,
            idempotency: "safe".to_string(),
            retry_policy: "none".to_string(),
            concurrency: "parallel_safe".to_string(),
            approval_required: false,
            dashboard_provider: false,
            required_scope: None,
            dashboard_route: None,
            input_schema: json!({"type": "object"}),
        }
    }

    fn connector_health_package(target: &str) -> WorkflowPackage {
        let mut package = package(Vec::new());
        package.capabilities = vec![target.to_string()];
        package.connectors = vec![crate::workflow::WorkflowConnectorManifest {
            schema_version: crate::workflow::connector::CONNECTOR_MANIFEST_SCHEMA_VERSION
                .to_string(),
            id: "test-connector".to_string(),
            version: "1.0.0".to_string(),
            name: "Test Connector".to_string(),
            description: String::new(),
            availability: "local".to_string(),
            credential_ownership: "agent".to_string(),
            auth: vec!["none".to_string()],
            capabilities: vec![target.to_string()],
            scopes: Vec::new(),
            supported_platforms: Vec::new(),
            health_check: json!({
                "type": "capability",
                "target": target
            }),
            credentials: Vec::new(),
        }];
        package
    }

    #[test]
    fn blocks_missing_capability_and_tool() {
        let report = preflight(
            &package(vec!["definitely-not-a-real-himind-tool"]),
            "0.3.47",
            &[],
        );
        assert!(!report.ready);
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("system.health")));
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("definitely-not-a-real-himind-tool")));
    }

    #[test]
    fn accepts_available_capability_and_tool() {
        let tool = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let report = preflight(
            &package(vec![tool]),
            "0.3.47",
            &[capability("system.health")],
        );
        assert!(report.ready, "{:?}", report.blockers);
    }

    #[test]
    fn connector_health_probe_filters_input_and_records_success() {
        let mut capability = capability("wechat.miniprogram.project.inspect");
        capability.input_schema = json!({
            "type": "object",
            "properties": {
                "workspace_root": {"type": "string"},
                "project_root": {"type": "string"}
            },
            "required": ["workspace_root", "project_root"],
            "additionalProperties": false
        });
        let mut observed = Value::Null;
        let report = preflight_with_connector_probes(
            &connector_health_package(&capability.id),
            "0.3.47",
            &[capability],
            &json!({
                "workspace_root": "C:\\workspace",
                "project_root": "C:\\workspace\\miniprogram",
                "credential_handles": {"private_key_path": "wechat-key"}
            }),
            |target, input| {
                assert_eq!(target, "wechat.miniprogram.project.inspect");
                observed = input;
                Ok(json!({"ok": true}))
            },
        );
        assert!(report.ready, "{:?}", report.blockers);
        assert_eq!(report.connectors[0].health_status, "passed");
        assert_eq!(observed["workspace_root"], "C:\\workspace");
        assert_eq!(observed["project_root"], "C:\\workspace\\miniprogram");
        assert!(observed.get("credential_handles").is_none());
    }

    #[test]
    fn connector_health_probe_failure_blocks_preflight() {
        let capability = capability("wechat.miniprogram.project.inspect");
        let report = preflight_with_connector_probes(
            &connector_health_package(&capability.id),
            "0.3.47",
            &[capability],
            &json!({}),
            |_, _| Err("plugin failed to start".into()),
        );
        assert!(!report.ready);
        assert_eq!(report.connectors[0].health_status, "failed");
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("plugin failed to start")));
    }

    #[test]
    fn connector_health_probe_rejects_mutating_target() {
        let mut capability = capability("wechat.miniprogram.upload");
        capability.risk_level = "local_write".to_string();
        let mut invoked = false;
        let report = preflight_with_connector_probes(
            &connector_health_package(&capability.id),
            "0.3.47",
            &[capability],
            &json!({}),
            |_, _| {
                invoked = true;
                Ok(json!({"ok": true}))
            },
        );
        assert!(!invoked);
        assert!(!report.ready);
        assert!(report.connectors[0]
            .health_message
            .contains("must be read_only"));
    }

    #[test]
    fn connector_http_health_probe_records_real_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let target = format!("http://{address}/health");
        let mut package = connector_health_package("network.health");
        package.capabilities.clear();
        package.connectors[0].capabilities.clear();
        package.connectors[0].health_check = json!({
            "type": "http",
            "url": target,
            "method": "GET",
            "expected_status": [200],
            "timeout_seconds": 3
        });
        let report =
            preflight_with_connector_probes(&package, "0.3.47", &[], &json!({}), |_, _| {
                panic!("Capability probe must not be called for HTTP health checks")
            });
        assert!(report.ready, "{:?}", report.blockers);
        assert_eq!(report.connectors[0].health_status, "passed");
        assert!(report.connectors[0].health_message.contains("200"));
        server.join().unwrap();
    }
}
