use semver::Version;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::env;
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
            WorkflowConnectorPreflight {
                id: connector.id.clone(),
                available,
                availability: connector.availability.clone(),
                credential_ownership: connector.credential_ownership.clone(),
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
}
