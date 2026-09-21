use semver::Version;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use super::WorkflowPackage;
use crate::api::types::RuntimeInstallationReport;
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
pub(crate) struct WorkflowSkillPreflight {
    pub id: String,
    pub available: bool,
    pub version: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowRuntimePreflight {
    pub id: String,
    pub available: bool,
    pub status: String,
    pub version: String,
    pub network_isolated: bool,
    pub tool_access: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowConnectorCredentialPreflight {
    pub handle: String,
    pub target: String,
    pub kind: String,
    pub required: bool,
    pub configured: bool,
    pub configured_connector_id: String,
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
    pub credentials: Vec<WorkflowConnectorCredentialPreflight>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowConnectorProbe {
    pub id: String,
    pub status: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowDiagnostic {
    pub severity: String,
    pub code: String,
    pub stage: String,
    pub message: String,
    pub remediation: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkflowPreflight {
    pub ready: bool,
    pub package_id: String,
    pub package_version: String,
    pub agent_version: String,
    pub capabilities: Vec<WorkflowCapabilityPreflight>,
    pub skills: Vec<WorkflowSkillPreflight>,
    pub runtimes: Vec<WorkflowRuntimePreflight>,
    pub connectors: Vec<WorkflowConnectorPreflight>,
    pub tools: Vec<WorkflowToolPreflight>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

fn push_diagnostic(
    diagnostics: &mut Vec<WorkflowDiagnostic>,
    severity: &str,
    code: &str,
    stage: &str,
    message: impl Into<String>,
    remediation: impl Into<String>,
    retryable: bool,
) {
    diagnostics.push(WorkflowDiagnostic {
        severity: severity.to_string(),
        code: code.to_string(),
        stage: stage.to_string(),
        message: message.into(),
        remediation: remediation.into(),
        retryable,
    });
}

fn push_blocker(
    diagnostics: &mut Vec<WorkflowDiagnostic>,
    code: &str,
    stage: &str,
    message: impl Into<String>,
    remediation: impl Into<String>,
    retryable: bool,
) {
    push_diagnostic(
        diagnostics,
        "blocker",
        code,
        stage,
        message,
        remediation,
        retryable,
    );
}

fn push_warning(
    diagnostics: &mut Vec<WorkflowDiagnostic>,
    code: &str,
    stage: &str,
    message: impl Into<String>,
    remediation: impl Into<String>,
    retryable: bool,
) {
    push_diagnostic(
        diagnostics,
        "warning",
        code,
        stage,
        message,
        remediation,
        retryable,
    );
}

fn normalize_diagnostics(mut diagnostics: Vec<WorkflowDiagnostic>) -> Vec<WorkflowDiagnostic> {
    diagnostics.sort_by(|left, right| {
        (
            left.severity.as_str(),
            left.code.as_str(),
            left.stage.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.severity.as_str(),
                right.code.as_str(),
                right.stage.as_str(),
                right.message.as_str(),
            ))
    });
    diagnostics.dedup();
    diagnostics
}

fn legacy_messages(diagnostics: &[WorkflowDiagnostic], severity: &str) -> Vec<String> {
    let mut messages = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == severity)
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>();
    messages.sort();
    messages.dedup();
    messages
}

impl WorkflowPreflight {
    pub(crate) fn push_blocker(
        &mut self,
        code: &str,
        stage: &str,
        message: impl Into<String>,
        remediation: impl Into<String>,
        retryable: bool,
    ) {
        self.diagnostics = normalize_diagnostics(std::mem::take(&mut self.diagnostics));
        push_blocker(
            &mut self.diagnostics,
            code,
            stage,
            message,
            remediation,
            retryable,
        );
        self.diagnostics = normalize_diagnostics(std::mem::take(&mut self.diagnostics));
        self.blockers = legacy_messages(&self.diagnostics, "blocker");
        self.warnings = legacy_messages(&self.diagnostics, "warning");
        self.ready = self.blockers.is_empty();
    }
}

pub(crate) fn preflight(
    package: &WorkflowPackage,
    agent_version: &str,
    available_capabilities: &[CapabilityDescriptor],
    input: &Value,
) -> WorkflowPreflight {
    let mut diagnostics = Vec::new();

    match (
        Version::parse(agent_version),
        Version::parse(&package.min_agent_version),
    ) {
        (Ok(current), Ok(minimum)) if current < minimum => push_blocker(
            &mut diagnostics,
            "agent.version.too_old",
            "preflight",
            format!(
                "Agent {} is older than required version {}",
                current, minimum
            ),
            "升级 HiMind Agent 后重新执行启动前检查。",
            false,
        ),
        (Err(error), _) => push_warning(
            &mut diagnostics,
            "agent.version.invalid",
            "preflight",
            format!("Agent version cannot be parsed: {error}"),
            "确认 Agent 版本由标准 semver 暴露；不建议在版本无法识别时执行生产交付。",
            false,
        ),
        (_, Err(error)) => push_blocker(
            &mut diagnostics,
            "workflow.version.invalid",
            "preflight",
            format!("workflow minimum Agent version is invalid: {error}"),
            "修正 Workflow Package 的 min_agent_version。",
            false,
        ),
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
                push_blocker(
                    &mut diagnostics,
                    "capability.unavailable",
                    "dependencies",
                    format!("required capability is unavailable: {capability_id}"),
                    "启用对应 Capability，或安装提供该 Capability 的 Plugin/Connector。",
                    true,
                );
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
            push_blocker(
                &mut diagnostics,
                "plugin.unavailable",
                "dependencies",
                format!("required workflow plugin is unavailable or disabled: {plugin_id}"),
                "安装并启用该 Plugin，然后重新执行启动前检查。",
                true,
            );
        }
    }

    validate_capability_step_inputs(package, &capability_map, input, &mut diagnostics);

    let skills = package
        .dependencies
        .skills
        .iter()
        .map(|skill_id| {
            let record = crate::skill::store::SkillStore::new()
                .get_record(skill_id)
                .ok()
                .flatten();
            let available = record.is_some();
            if !available {
                push_blocker(
                    &mut diagnostics,
                    "skill.unavailable",
                    "dependencies",
                    format!("required workflow skill is unavailable: {skill_id}"),
                    "安装或启用该 Skill，然后重新执行启动前检查。",
                    true,
                );
            }
            WorkflowSkillPreflight {
                id: skill_id.clone(),
                available,
                version: record
                    .as_ref()
                    .map(|record| record.manifest.version.clone())
                    .unwrap_or_default(),
                scope: record
                    .as_ref()
                    .map(|record| format!("{:?}", record.manifest.scope).to_ascii_lowercase())
                    .unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let runtime_reports = workflow_runtime_reports_for(package);
    let runtimes = evaluate_runtime_preflight(package, &runtime_reports, &mut diagnostics);

    let dashboard_available = available_capabilities
        .iter()
        .any(|capability| capability.dashboard_provider);
    let requires_connector_credentials = package
        .connectors
        .iter()
        .any(|connector| !connector.credentials.is_empty());
    let connector_credentials = if requires_connector_credentials {
        match crate::store::connector_credentials::list() {
            Ok(items) => items,
            Err(error) => {
                push_blocker(
                    &mut diagnostics,
                    "connector.credentials.store_unavailable",
                    "connector",
                    format!("connector credential store is unavailable: {error}"),
                    "检查 Agent 凭据存储目录和权限；修复后重新执行启动前检查。",
                    true,
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let connectors = package
        .connectors
        .iter()
        .map(|connector| {
            let mut available = connector.availability() != CapabilityAvailability::ControlPlane
                || dashboard_available;
            if !available {
                push_blocker(
                    &mut diagnostics,
                    "connector.unavailable",
                    "connector",
                    format!("required workflow connector is unavailable: {}", connector.id),
                    "启用该 Connector，或切换到可用的连接器实现。",
                    true,
                );
            }
            if let Err(error) = crate::store::connector_state::ensure_available(&connector.id) {
                available = false;
                push_blocker(
                    &mut diagnostics,
                    "connector.inactive",
                    "connector",
                    format!("workflow connector {} is not active: {error}", connector.id),
                    "检查 Connector 状态和其依赖服务，然后重新执行启动前检查。",
                    true,
                );
            }
            if connector.credential_ownership == "dashboard" && !dashboard_available {
                push_blocker(
                    &mut diagnostics,
                    "connector.dashboard_required",
                    "connector",
                    format!("workflow connector {} requires Dashboard credential ownership", connector.id),
                    "连接 Dashboard，或将 Connector 改为 Agent 持有凭据的受支持模式。",
                    true,
                );
            }
            for capability_id in &connector.capabilities {
                if !package.capabilities.contains(capability_id) {
                    push_blocker(
                        &mut diagnostics,
                        "connector.capability_undeclared",
                        "contract",
                        format!("workflow connector {} exposes undeclared capability: {capability_id}", connector.id),
                        "在 Workflow Package capabilities 中声明该 Capability，或从 Connector 清单中移除它。",
                        false,
                    );
                }
            }
            let health_type = connector
                .health_check
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("none");
            let credentials = evaluate_connector_credentials(
                &connector.id,
                &connector.credentials,
                &connector_credentials,
                &mut diagnostics,
            );
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
                credentials,
            }
        })
        .collect::<Vec<_>>();

    let mut tools = Vec::new();
    for tool in required_tools(&package.local_requirements) {
        let resolved = resolve_executable(&tool);
        let available = resolved.is_some();
        if !available {
            push_blocker(
                &mut diagnostics,
                "tool.required_unavailable",
                "toolchain",
                format!("required local tool is unavailable: {tool}"),
                "安装该工具并确保其可从 Agent 进程的 PATH 中解析。",
                true,
            );
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
            push_warning(
                &mut diagnostics,
                "tool.recommended_unavailable",
                "toolchain",
                format!("recommended local tool is unavailable: {tool}"),
                "如需完整体验，安装该推荐工具；当前检查不因此阻断。",
                true,
            );
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
        if step.capability_id.trim().is_empty() && step.loop_config.is_none() {
            push_warning(
                &mut diagnostics,
                "step.executor_missing",
                "contract",
                format!(
                    "workflow step {} requires a Runtime or provider executor",
                    step.id
                ),
                "为该 Step 配置 capability_id、Runtime 或 Loop 执行器。",
                false,
            );
        } else if let Some(capability) = capability_map.get(step.capability_id.as_str()) {
            let effective = crate::approval::policy::effective_risk_level(
                &step.capability_id,
                &capability.risk_level,
            );
            if crate::approval::policy::risk_rank(effective) >= 3 && !step.approval_required {
                push_blocker(
                    &mut diagnostics,
                    "approval.required",
                    "preflight",
                    format!("workflow step {} exposes {effective} capability {} without an approval gate", step.id, step.capability_id),
                    "为该 Step 设置 approval_required=true，并通过 Agent 审批中心完成审批。",
                    false,
                );
            }
        }
    }

    let diagnostics = normalize_diagnostics(diagnostics);
    let blockers = legacy_messages(&diagnostics, "blocker");
    let warnings = legacy_messages(&diagnostics, "warning");

    WorkflowPreflight {
        ready: blockers.is_empty(),
        package_id: package.id.clone(),
        package_version: package.version.clone(),
        agent_version: agent_version.to_string(),
        capabilities,
        skills,
        runtimes,
        connectors,
        tools,
        diagnostics,
        blockers,
        warnings,
    }
}

// 启动前就用运行期同一套规则校验 Capability 入参。
// 非法取值（例如列表字段被写成一整串 "cv,llm,ar-vr"）必须在这里被拦住：
// 否则用户要等一次必然失败的执行，再从运行详情里翻错误。
fn validate_capability_step_inputs(
    package: &WorkflowPackage,
    capability_map: &std::collections::BTreeMap<&str, &CapabilityDescriptor>,
    run_input: &Value,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) {
    for step in &package.steps {
        if step.kind.trim() != "capability" {
            continue;
        }
        let capability_id = step.capability_id.trim();
        if capability_id.is_empty() {
            continue;
        }
        let Some(capability) = capability_map.get(capability_id) else {
            continue;
        };
        let mut merged = run_input.as_object().cloned().unwrap_or_default();
        if let Some(step_input) = step.input.as_object() {
            for (name, value) in step_input {
                merged.insert(name.clone(), value.clone());
            }
        }
        // 运行器会给每个步骤注入 workflow_context，这里补上同样的键，
        // 免得把「运行期才注入的输入」误判成缺失。
        merged
            .entry("workflow_context".to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        let merged = Value::Object(merged);
        // 先用运行期的过滤规则对齐输入，再校验：预检查的对象与真正下发给能力的对象一致。
        let filtered = super::executor::capability_input(&capability.input_schema, &merged);
        if let Err(error) = crate::capability::service::validate_capability_input_schema(
            &capability.input_schema,
            &filtered,
        ) {
            push_blocker(
                diagnostics,
                "capability.input.invalid",
                "input",
                format!("{capability_id}（步骤 {}）：{error}", step.id),
                "按启动表单的字段声明修正参数：列表字段每行一项，取值必须在允许范围内；不改也可以清空该项用默认值。",
                true,
            );
        }
    }
}

pub(crate) fn validate_environment_lock(
    lock: &crate::extension_contracts::ExtensionLock,
    capabilities: &[CapabilityDescriptor],
    report: &WorkflowPreflight,
) -> Result<(), Box<dyn Error>> {
    if lock.environment.is_empty() {
        return Ok(());
    }
    for expected in &lock.environment.capabilities {
        let current = capabilities
            .iter()
            .find(|capability| capability.id == expected.id)
            .ok_or_else(|| format!("workflow environment lost Capability {}", expected.id))?;
        if !expected.provider.is_empty() && current.source != expected.provider {
            return Err(format!(
                "workflow environment Capability {} provider changed from {} to {}",
                expected.id, expected.provider, current.source
            )
            .into());
        }
        let availability = current.availability.as_str();
        if !expected.availability.is_empty() && availability != expected.availability {
            return Err(format!(
                "workflow environment Capability {} availability changed from {} to {}",
                expected.id, expected.availability, availability
            )
            .into());
        }
    }
    for expected in &lock.environment.connectors {
        let current = report
            .connectors
            .iter()
            .find(|connector| connector.id == expected.id)
            .ok_or_else(|| format!("workflow environment lost Connector {}", expected.id))?;
        if !current.available {
            return Err(format!(
                "workflow environment Connector {} is unavailable",
                expected.id
            )
            .into());
        }
        if !expected.credential_ownership.is_empty()
            && current.credential_ownership != expected.credential_ownership
        {
            return Err(format!(
                "workflow environment Connector {} credential ownership changed from {} to {}",
                expected.id, expected.credential_ownership, current.credential_ownership
            )
            .into());
        }
    }
    for expected in &lock.environment.runtimes {
        if !expected.required {
            continue;
        }
        let current = report
            .runtimes
            .iter()
            .find(|runtime| runtime.id == expected.id)
            .ok_or_else(|| format!("workflow environment lost Runtime {}", expected.id))?;
        if !current.available {
            return Err(format!(
                "workflow environment Runtime {} is unavailable",
                expected.id
            )
            .into());
        }
    }
    Ok(())
}

fn evaluate_connector_credentials(
    connector_id: &str,
    credentials: &[super::WorkflowConnectorCredential],
    configured_credentials: &[crate::store::connector_credentials::ConnectorCredentialSummary],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Vec<WorkflowConnectorCredentialPreflight> {
    credentials
        .iter()
        .map(|credential| {
            let configured = configured_credentials
                .iter()
                .find(|configured| configured.handle == credential.handle);
            let configured_connector_id = configured
                .map(|configured| configured.connector_id.clone())
                .unwrap_or_default();
            let is_configured = configured.is_some_and(|configured| {
                configured.connector_id == connector_id && configured.kind == credential.kind
            });
            if credential.required && configured.is_none() {
                push_blocker(
                    diagnostics,
                    "connector.credential_missing",
                    "connector",
                    format!(
                        "workflow connector credential is missing: {}",
                        credential.handle
                    ),
                    "配置该 Credential Handle，并确认它属于当前 Connector。",
                    true,
                );
            } else if credential.required && !is_configured {
                push_blocker(
                    diagnostics,
                    "connector.credential_mismatch",
                    "connector",
                    format!(
                        "workflow connector credential {} is not usable for {}: configured by {}",
                        credential.handle, connector_id, configured_connector_id
                    ),
                    "使用当前 Connector 和 Credential 类型重新保存凭据。",
                    true,
                );
            }
            WorkflowConnectorCredentialPreflight {
                handle: credential.handle.clone(),
                target: credential.target.clone(),
                kind: credential.kind.clone(),
                required: credential.required,
                configured: is_configured,
                configured_connector_id,
            }
        })
        .collect()
}

fn runtime_requirements(
    steps: &[super::WorkflowStep],
) -> (std::collections::BTreeSet<String>, bool) {
    fn walk(
        steps: &[super::WorkflowStep],
        explicit: &mut std::collections::BTreeSet<String>,
        auto: &mut bool,
    ) {
        for step in steps {
            if let Some(runtime) = step.runtime.as_ref() {
                let provider = runtime.provider.trim();
                if provider == "auto" {
                    *auto = true;
                } else if !provider.is_empty() {
                    explicit.insert(provider.to_string());
                }
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(&loop_config.steps, explicit, auto);
            }
        }
    }

    let mut explicit = std::collections::BTreeSet::new();
    let mut auto = false;
    walk(steps, &mut explicit, &mut auto);
    (explicit, auto)
}

fn runtime_requires_network_isolation(steps: &[super::WorkflowStep]) -> bool {
    steps.iter().any(|step| {
        step.runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.allow_network)
            || step
                .loop_config
                .as_ref()
                .is_some_and(|loop_config| runtime_requires_network_isolation(&loop_config.steps))
    })
}

fn runtime_is_network_isolated(report: &RuntimeInstallationReport) -> bool {
    report
        .capabilities
        .get("network_isolated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn runtime_tool_access(report: &RuntimeInstallationReport) -> String {
    report
        .capabilities
        .get("tool_access")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn workflow_runtime_reports_for(package: &WorkflowPackage) -> Vec<RuntimeInstallationReport> {
    let (explicit, auto) = runtime_requirements(&package.steps);
    if package.dependencies.runtimes.is_empty() && explicit.is_empty() && !auto {
        return Vec::new();
    }
    workflow_runtime_reports()
}

fn evaluate_runtime_preflight(
    package: &WorkflowPackage,
    runtime_reports: &[RuntimeInstallationReport],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Vec<WorkflowRuntimePreflight> {
    let (explicit_runtimes, auto_runtime) = runtime_requirements(&package.steps);
    let requires_network_isolation = runtime_requires_network_isolation(&package.steps);
    let mut runtime_ids = if !package.dependencies.runtimes.is_empty() {
        package
            .dependencies
            .runtimes
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    } else if auto_runtime {
        package
            .supported_runtimes
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    runtime_ids.extend(explicit_runtimes.iter().cloned());
    if auto_runtime {
        runtime_ids.extend(package.supported_runtimes.iter().cloned());
        runtime_ids.extend(
            runtime_reports
                .iter()
                .filter(|report| report.status == "ready")
                .map(|report| report.provider.clone()),
        );
    }
    let runtimes = runtime_ids
        .iter()
        .map(|runtime_id| {
            let report = runtime_reports
                .iter()
                .find(|report| report.provider == *runtime_id);
            let network_isolated = report.is_some_and(runtime_is_network_isolated);
            let available = report.is_some_and(|report| {
                report.status == "ready" && (!requires_network_isolation || network_isolated)
            });
            WorkflowRuntimePreflight {
                id: runtime_id.clone(),
                available,
                status: report
                    .map(|report| report.status.clone())
                    .unwrap_or_else(|| "unavailable".to_string()),
                version: report
                    .map(|report| report.version.clone())
                    .unwrap_or_default(),
                network_isolated,
                tool_access: report.map(runtime_tool_access).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    for runtime_id in &explicit_runtimes {
        let report = runtime_reports
            .iter()
            .find(|report| report.provider == *runtime_id);
        if report.is_some_and(|report| {
            report.status == "ready"
                && requires_network_isolation
                && !runtime_is_network_isolated(report)
        }) {
            push_blocker(
                diagnostics,
                "runtime.network_isolation_missing",
                "runtime",
                format!("workflow runtime {runtime_id} cannot enforce allow_network=false"),
                "安装或选择声明 network_isolated=true 的 Runtime，或在 Workflow 中明确允许网络访问。",
                true,
            );
        } else if !report.is_some_and(|report| report.status == "ready") {
            push_blocker(
                diagnostics,
                "runtime.unavailable",
                "runtime",
                format!("required workflow runtime is unavailable: {runtime_id}"),
                "安装并启用该 Runtime，或把 Step provider 改为已安装的 Runtime。",
                true,
            );
        }
    }
    if auto_runtime && !runtimes.iter().any(|runtime| runtime.available) {
        if requires_network_isolation {
            push_blocker(
                diagnostics,
                "runtime.auto_isolation_unavailable",
                "runtime",
                "workflow runtime provider auto has no ready provider with network isolation",
                "配置隔离 Runtime，或明确允许网络访问后重试。",
                true,
            );
        } else {
            push_blocker(
                diagnostics,
                "runtime.auto_unavailable",
                "runtime",
                "workflow runtime provider auto is unavailable; install or configure one supported Runtime",
                "安装或配置 package.supported_runtimes 中的任一 Runtime。",
                true,
            );
        }
    }
    if !package.dependencies.runtimes.is_empty()
        && explicit_runtimes.is_empty()
        && !auto_runtime
        && !runtimes.iter().any(|runtime| runtime.available)
    {
        push_blocker(
            diagnostics,
            "runtime.dependency_unavailable",
            "runtime",
            "no declared workflow runtime dependency is available",
            "安装 Workflow 声明的 Runtime 依赖，或更新依赖清单。",
            true,
        );
    }
    runtimes
}

fn workflow_runtime_reports() -> Vec<RuntimeInstallationReport> {
    if std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1") {
        return vec![RuntimeInstallationReport {
            provider: "himind.fixture".to_string(),
            version: crate::VERSION.to_string(),
            status: "ready".to_string(),
            capabilities: serde_json::json!({
                "contract_double": true,
                "network_isolated": true,
                "tool_access": "fixture"
            }),
        }];
    }
    crate::runtime::probe_installations()
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
    let mut report = preflight(package, agent_version, available_capabilities, input);
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
                let mut check =
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
                if let Err(error) =
                    super::connector::resolve_http_health_credential(connector, &mut check)
                {
                    return WorkflowConnectorProbe {
                        id: connector.id.clone(),
                        status: "failed".to_string(),
                        target: check.url,
                        message: error.to_string(),
                    };
                }
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
            report.push_blocker(
                "connector.health_probe_failed",
                "connector",
                format!(
                    "connector {} health probe failed: {}",
                    probe.id,
                    if probe.message.trim().is_empty() {
                        "unknown error"
                    } else {
                        probe.message.as_str()
                    }
                ),
                "检查 Connector 服务地址、凭据和网络连通性，然后重新执行启动前检查。",
                true,
            );
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
            execution_policy: "strict".to_string(),
            entrypoints: Vec::new(),
            default_entrypoint: String::new(),
            default_exitpoint: String::new(),
            exits: Vec::new(),
            steps: vec![crate::workflow::WorkflowStep {
                id: "STEP-1".to_string(),
                title: "Health".to_string(),
                kind: "capability".to_string(),
                capability_id: "system.health".to_string(),
                runtime: None,
                loop_config: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: json!({}),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                on_failure: String::new(),
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

    fn runtime_report(provider: &str, status: &str) -> RuntimeInstallationReport {
        RuntimeInstallationReport {
            provider: provider.to_string(),
            version: "1.2.3".to_string(),
            status: status.to_string(),
            capabilities: json!({
                "network_isolated": true,
                "tool_access": "test"
            }),
        }
    }

    #[test]
    fn connector_credential_preflight_exposes_configured_and_missing_handles() {
        let credentials = vec![
            super::super::WorkflowConnectorCredential {
                handle: "configured-key".to_string(),
                target: "private_key_path".to_string(),
                kind: "file_path".to_string(),
                required: true,
            },
            super::super::WorkflowConnectorCredential {
                handle: "missing-secret".to_string(),
                target: "api_token".to_string(),
                kind: "secret".to_string(),
                required: true,
            },
        ];
        let configured = vec![
            crate::store::connector_credentials::ConnectorCredentialSummary {
                handle: "configured-key".to_string(),
                connector_id: "wechat-miniprogram".to_string(),
                kind: "file_path".to_string(),
                updated_at: "1".to_string(),
            },
        ];
        let mut diagnostics = Vec::new();

        let report = evaluate_connector_credentials(
            "wechat-miniprogram",
            &credentials,
            &configured,
            &mut diagnostics,
        );

        assert!(report[0].configured);
        assert!(!report[1].configured);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missing-secret")));
    }

    #[test]
    fn explicit_runtime_provider_must_be_ready() {
        let mut package = package(Vec::new());
        package.supported_runtimes = vec!["personal.codex".to_string()];
        package.steps[0].runtime = Some(crate::workflow::WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Implement.".to_string(),
            workspace_path: String::new(),
            result_schema: String::new(),
            allow_network: false,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            timeout_seconds: 0,
        });
        package.steps[0].capability_id.clear();
        package.steps[0].kind = "runtime".to_string();

        let mut diagnostics = Vec::new();
        let runtimes = evaluate_runtime_preflight(
            &package,
            &[runtime_report("personal.codex", "unavailable")],
            &mut diagnostics,
        );
        assert_eq!(runtimes.len(), 1);
        assert!(!runtimes[0].available);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("personal.codex")));

        let mut diagnostics = Vec::new();
        let runtimes = evaluate_runtime_preflight(
            &package,
            &[runtime_report("personal.codex", "ready")],
            &mut diagnostics,
        );
        assert!(runtimes[0].available);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn network_disabled_runtime_requires_provider_isolation() {
        let mut package = package(Vec::new());
        package.supported_runtimes = vec!["personal.codex".to_string()];
        package.steps[0].runtime = Some(crate::workflow::WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Implement.".to_string(),
            workspace_path: String::new(),
            result_schema: String::new(),
            allow_network: false,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            timeout_seconds: 0,
        });
        package.steps[0].capability_id.clear();
        package.steps[0].kind = "runtime".to_string();

        let mut diagnostics = Vec::new();
        let runtimes = evaluate_runtime_preflight(
            &package,
            &[RuntimeInstallationReport {
                provider: "personal.codex".to_string(),
                version: "1.2.3".to_string(),
                status: "ready".to_string(),
                capabilities: json!({
                    "network_isolated": false,
                    "tool_access": "workspace-write"
                }),
            }],
            &mut diagnostics,
        );

        assert!(!runtimes[0].available);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("personal.codex")
                && diagnostic.message.contains("allow_network=false")
        }));
    }

    #[test]
    fn auto_runtime_accepts_any_available_candidate() {
        let mut package = package(Vec::new());
        package.supported_runtimes = vec![
            "personal.codex".to_string(),
            "personal.github-copilot".to_string(),
        ];
        package.steps[0].runtime = Some(crate::workflow::WorkflowRuntimeStep {
            provider: "auto".to_string(),
            prompt: "Implement.".to_string(),
            workspace_path: String::new(),
            result_schema: String::new(),
            allow_network: false,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            timeout_seconds: 0,
        });
        package.steps[0].capability_id.clear();
        package.steps[0].kind = "runtime".to_string();

        let mut diagnostics = Vec::new();
        let runtimes = evaluate_runtime_preflight(
            &package,
            &[
                runtime_report("personal.codex", "unavailable"),
                runtime_report("personal.github-copilot", "ready"),
            ],
            &mut diagnostics,
        );
        assert!(runtimes.iter().any(|runtime| runtime.available));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn missing_skill_dependency_blocks_preflight() {
        let mut package = package(Vec::new());
        package.dependencies.skills = vec!["definitely-missing-himind-skill".to_string()];
        let report = preflight(
            &package,
            "0.3.47",
            &[capability("system.health")],
            &json!({}),
        );
        assert!(!report.ready);
        assert_eq!(report.skills.len(), 1);
        assert!(!report.skills[0].available);
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("definitely-missing-himind-skill")));
    }

    #[test]
    fn blocks_missing_capability_and_tool() {
        let report = preflight(
            &package(vec!["definitely-not-a-real-himind-tool"]),
            "0.3.47",
            &[],
            &json!({}),
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
            &json!({}),
        );
        assert!(report.ready, "{:?}", report.blockers);
    }

    #[test]
    fn rejects_environment_lock_provider_drift() {
        let mut package = package(Vec::new());
        package.capabilities = vec!["test.capability".to_string()];
        let capability = capability("test.capability");
        let report = preflight(&package, "0.3.47", &[capability.clone()], &json!({}));
        let lock = crate::extension_contracts::ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: crate::extension_contracts::ExtensionAssetKind::Workflow,
                id: package.id.clone(),
                version: package.version.clone(),
                sha256: "a".repeat(64),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment {
                capabilities: vec![crate::extension_contracts::ExtensionLockCapability {
                    id: capability.id.clone(),
                    provider: "different-provider".to_string(),
                    availability: "local".to_string(),
                    required: true,
                }],
                ..Default::default()
            },
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        let error = validate_environment_lock(&lock, &[capability], &report)
            .unwrap_err()
            .to_string();
        assert!(error.contains("provider changed"));
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
    fn managed_connector_health_uses_authenticated_dashboard_capability() {
        let mut capability = capability("business.connector.health");
        capability.availability = CapabilityAvailability::ControlPlane;
        capability.dashboard_provider = true;
        capability.input_schema = json!({
            "type": "object",
            "properties": {"connector_id": {"type": "string"}},
            "required": ["connector_id"],
            "additionalProperties": false
        });
        let mut package = connector_health_package("business.connector.health");
        package.connectors[0].availability = "control_plane".to_string();
        package.connectors[0].credential_ownership = "dashboard".to_string();
        package.connectors[0].health_check = json!({
            "type": "capability",
            "target": "business.connector.health",
            "input": {"connector_id": "managed.example"}
        });
        let mut observed = Value::Null;
        let report = preflight_with_connector_probes(
            &package,
            "0.3.47",
            &[capability],
            &json!({}),
            |target, input| {
                assert_eq!(target, "business.connector.health");
                observed = input;
                Ok(json!({"status": "available"}))
            },
        );
        assert!(report.ready, "{:?}", report.blockers);
        assert_eq!(report.connectors[0].health_status, "passed");
        assert_eq!(observed["connector_id"], "managed.example");
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
