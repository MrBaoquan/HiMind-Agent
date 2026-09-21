use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

mod authoring;
mod candidate;
mod condition;
mod connector;
mod contract;
mod executor;
mod metrics;
mod preflight;
mod presets;
mod runner;
mod runtime;
mod store;

#[allow(unused_imports)]
pub(crate) use authoring::{
    confirm as confirm_authoring_candidate,
    confirm_with_capabilities as confirm_authoring_candidate_with_capabilities,
    list as list_authoring_drafts, mark_submitted as mark_workflow_candidate_submitted,
    read as read_authoring_draft, save_from_source as save_authoring_candidate,
    submit as submit_authoring_candidate, test as test_authoring_candidate,
    test_with_capabilities as test_authoring_candidate_with_capabilities, WorkflowDraft,
};
#[allow(unused_imports)]
pub(crate) use candidate::{freeze_candidate, read_candidate};
#[allow(unused_imports)]
pub(crate) use presets::{
    delete as delete_run_preset, list as list_run_presets, set as set_run_preset,
};
#[allow(unused_imports)]
pub(crate) use condition::evaluate_condition;
#[allow(unused_imports)]
pub(crate) use connector::{
    execute_http_health_check, load_connector_manifests, WorkflowConnectorCredential,
    WorkflowConnectorManifest, WorkflowHttpHealthCheck,
};
#[allow(unused_imports)]
pub(crate) use contract::contract_dry_run_report;
#[allow(unused_imports)]
pub(crate) use executor::WorkflowGatewayExecutor;
pub(crate) use metrics::workflow_metrics_by_package;
#[allow(unused_imports)]
pub(crate) use preflight::{
    preflight, preflight_with_connector_probes, probe_connectors, validate_environment_lock,
    WorkflowCapabilityPreflight, WorkflowConnectorCredentialPreflight, WorkflowConnectorPreflight,
    WorkflowConnectorProbe, WorkflowDiagnostic, WorkflowPreflight, WorkflowRuntimePreflight,
    WorkflowSkillPreflight, WorkflowToolPreflight,
};
#[allow(unused_imports)]
pub(crate) use runner::{
    verify_run, WorkflowArtifactOutput, WorkflowArtifactVerification, WorkflowRunOutcome,
    WorkflowRunVerification, WorkflowRunner, WorkflowStepExecution, WorkflowStepExecutor,
};
pub(crate) use runtime::execute_runtime_step;
pub(crate) use store::{InstalledWorkflow, WorkflowStore};

pub(crate) const WORKFLOW_PACKAGE_SCHEMA_VERSION: &str = "workflow_package.v1";

pub(crate) fn workflow_approval_id(run_id: &str, step_id: &str) -> String {
    format!("workflow:{}:{}", run_id.trim(), step_id.trim())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowPackage {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub min_agent_version: String,
    #[serde(default = "default_object")]
    pub local_requirements: Value,
    #[serde(default)]
    pub optional_providers: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub dependencies: WorkflowDependencies,
    #[serde(default)]
    pub candidate: Option<WorkflowCandidatePolicy>,
    #[serde(default = "default_execution_policy")]
    pub execution_policy: String,
    #[serde(default)]
    pub entrypoints: Vec<WorkflowEndpoint>,
    /// 调用方不指定入口时使用的默认入口（减少启动参数）。
    #[serde(default)]
    pub default_entrypoint: String,
    #[serde(default)]
    pub exits: Vec<WorkflowEndpoint>,
    /// 调用方不指定出口时使用的默认出口（减少启动参数）。
    #[serde(default)]
    pub default_exitpoint: String,
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub artifacts: Vec<WorkflowArtifact>,
    pub ui: WorkflowUi,
    #[serde(default)]
    pub supported_runtimes: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(skip)]
    pub source_root: PathBuf,
    #[serde(skip)]
    pub connectors: Vec<WorkflowConnectorManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowDependencies {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowEndpoint {
    pub id: String,
    pub at_step: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowStep {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub capability_id: String,
    #[serde(default)]
    pub runtime: Option<WorkflowRuntimeStep>,
    #[serde(default, rename = "loop")]
    pub loop_config: Option<Box<WorkflowLoop>>,
    #[serde(default)]
    pub when: Option<WorkflowCondition>,
    #[serde(default)]
    pub fail_when: Option<WorkflowCondition>,
    #[serde(default)]
    pub candidate_action: String,
    #[serde(default = "default_object")]
    pub input: Value,
    pub execution_mode: String,
    #[serde(default)]
    pub risk_level: String,
    #[serde(default)]
    pub approval_required: bool,
    /// How the runner treats a failing step.
    ///
    /// `fail` (default) stops the run, `continue` degrades the step to a
    /// tolerated failure so downstream steps still run with the step's output
    /// missing. The failure stays visible: the step records the error and the
    /// run keeps the `error` event.
    #[serde(default)]
    pub on_failure: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowRuntimeStep {
    pub provider: String,
    pub prompt: String,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub result_schema: String,
    #[serde(default)]
    pub allow_network: bool,
    /// 该步骤需要的上游 Artifact（按 package 里声明的 id）。
    ///
    /// 平台把 Artifact 以**文件路径**注入 `input.input_artifacts`，并把上游步骤输出
    /// 从提示词里移除：数据走文件，提示词只描述任务。这样提示词长度与 Artifact
    /// 体量无关，也不再把命令行当数据通道。
    #[serde(default)]
    pub input_artifacts: Vec<String>,
    /// 允许该步骤使用哪些工具：`default`（沿用 Runtime 默认）或 `none`（不挂载任何
    /// 模型可见工具）。`none` 由 Runtime Provider 强制，无法保证时预检失败。
    #[serde(default)]
    pub tool_policy: String,
    #[serde(default)]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowLoop {
    pub max_iterations: u32,
    #[serde(default)]
    pub pause_for_feedback: bool,
    #[serde(default)]
    pub continue_when: Option<WorkflowCondition>,
    #[serde(default)]
    pub exit_when: Option<WorkflowCondition>,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowCondition {
    pub operator: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub conditions: Vec<WorkflowCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowCandidatePolicy {
    #[serde(default = "default_true")]
    pub required: bool,
    pub artifact_id: String,
    #[serde(default = "default_git_source")]
    pub source: String,
    #[serde(default)]
    pub allow_dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowArtifact {
    pub id: String,
    pub artifact_type: String,
    pub name: String,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub validation: String,
    #[serde(default = "default_artifact_max_bytes")]
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowUi {
    pub mode: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub surfaces: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowViewManifest {
    schema_version: String,
    title: String,
    #[serde(default)]
    sections: Vec<WorkflowViewSection>,
    #[serde(default)]
    actions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowViewSection {
    id: String,
    title: String,
    #[serde(default)]
    fields: Vec<WorkflowViewField>,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WorkflowViewField {
    Id(String),
    Definition(WorkflowViewFieldDefinition),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowViewFieldDefinition {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default = "default_workflow_view_field_type", rename = "type")]
    field_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Value,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    placeholder: String,
    #[serde(default)]
    target: String,
    // picker 让字段声明自己要什么输入控件（当前支持 directory），
    // 这样「哪些字段是目录」由包自己说清楚，而不是前端靠字段名猜。
    #[serde(default, rename = "picker")]
    picker: String,
    // hint 是字段级说明，用来解释默认值的行为；缺省值不写就不占版面。
    #[serde(default)]
    hint: String,
    // span 控制栅格占位：full 表示整行，其它值按单列处理。
    #[serde(default)]
    span: String,
}

impl WorkflowPackage {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != WORKFLOW_PACKAGE_SCHEMA_VERSION {
            return Err("workflow package schema_version is invalid".to_string());
        }
        validate_workflow_id(&self.id)?;
        Version::parse(&self.version)
            .map_err(|error| format!("invalid workflow version: {error}"))?;
        Version::parse(&self.min_agent_version)
            .map_err(|error| format!("invalid minimum Agent version: {error}"))?;
        if self.name.trim().is_empty() {
            return Err("workflow name is required".to_string());
        }
        if !self.local_requirements.is_object() {
            return Err("workflow local_requirements must be an object".to_string());
        }
        validate_unique_text("capability", &self.capabilities)?;
        validate_unique_text("optional provider", &self.optional_providers)?;
        validate_unique_text("supported runtime", &self.supported_runtimes)?;

        if let Some(candidate) = self.candidate.as_ref() {
            validate_workflow_id(&candidate.artifact_id)?;
            if candidate.source != "git" {
                return Err("workflow candidate source must be git".to_string());
            }
        }
        let mut step_ids = HashSet::new();
        validate_step_scope(&self.steps, &mut step_ids, 0)?;
        validate_execution_contract(self, &step_ids)?;
        validate_runtime_declarations(&self.steps, &self.supported_runtimes)?;
        for runtime in &self.dependencies.runtimes {
            if !self.supported_runtimes.contains(runtime) {
                return Err(format!(
                    "workflow runtime dependency is not declared in supported_runtimes: {runtime}"
                ));
            }
        }

        let mut artifact_ids = HashSet::new();
        for artifact in &self.artifacts {
            validate_workflow_id(&artifact.id)?;
            if artifact.artifact_type.trim().is_empty() || artifact.name.trim().is_empty() {
                return Err(format!(
                    "workflow artifact {} identity is required",
                    artifact.id
                ));
            }
            if !artifact.schema.trim().is_empty() {
                validate_relative_asset_path(&artifact.schema)?;
            }
            if !matches!(artifact.validation.as_str(), "" | "strict" | "advisory") {
                return Err(format!(
                    "workflow artifact {} validation is invalid",
                    artifact.id
                ));
            }
            if artifact.max_bytes == 0 || artifact.max_bytes > 1024 * 1024 * 1024 {
                return Err(format!(
                    "workflow artifact {} max_bytes is out of range",
                    artifact.id
                ));
            }
            if !artifact_ids.insert(artifact.id.as_str()) {
                return Err(format!("duplicate workflow artifact id: {}", artifact.id));
            }
        }
        if let Some(candidate) = self.candidate.as_ref() {
            if !artifact_ids.contains(candidate.artifact_id.as_str()) {
                return Err(format!(
                    "workflow candidate artifact is not declared: {}",
                    candidate.artifact_id
                ));
            }
        }
        validate_candidate_actions(&self.steps, self.candidate.as_ref())?;
        // Runtime 步骤引用上游 Artifact 时，id 必须在包里有声明。
        for step in &self.steps {
            let Some(runtime) = step.runtime.as_ref() else {
                continue;
            };
            for artifact_id in &runtime.input_artifacts {
                if !artifact_ids.contains(artifact_id.trim()) {
                    return Err(format!(
                        "workflow runtime step {} input_artifacts is not declared by the package: {}",
                        step.id, artifact_id
                    ));
                }
            }
        }

        if !matches!(self.ui.mode.as_str(), "standard" | "declarative" | "custom") {
            return Err("workflow ui mode is invalid".to_string());
        }
        for surface in &self.ui.surfaces {
            if !matches!(surface.as_str(), "agent" | "dashboard" | "mcp") {
                return Err(format!("unsupported workflow ui surface: {surface}"));
            }
        }
        if self.ui.mode == "custom" {
            if self.ui.entry.trim().is_empty() {
                return Err("custom workflow ui requires an entry".to_string());
            }
            validate_relative_asset_path(&self.ui.entry)?;
        }
        Ok(())
    }
}

pub(crate) fn load_from_directory(root: &Path) -> Result<WorkflowPackage, Box<dyn Error>> {
    let path = root.join("workflow.json");
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("read workflow package {}: {error}", path.display()))?;
    let mut package: WorkflowPackage = serde_json::from_str(&source)?;
    package.validate().map_err(std::io::Error::other)?;
    package.source_root = root.canonicalize()?;
    package.connectors =
        load_connector_manifests(&package.source_root, &package.dependencies.connectors)?;
    validate_package_assets(root, &package)?;
    Ok(package)
}

pub(crate) fn package_dir(root: &Path, package_id: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_workflow_id(package_id).map_err(std::io::Error::other)?;
    Ok(root.join(package_id))
}

fn validate_workflow_id(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid workflow identifier: {value}"));
    }
    Ok(())
}

fn default_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn default_true() -> bool {
    true
}

fn default_git_source() -> String {
    "git".to_string()
}

fn default_execution_policy() -> String {
    "strict".to_string()
}

fn default_artifact_max_bytes() -> u64 {
    16 * 1024 * 1024
}

fn validate_execution_contract(
    package: &WorkflowPackage,
    step_ids: &HashSet<String>,
) -> Result<(), String> {
    let policy = package.execution_policy.trim();
    if !matches!(policy, "strict" | "segmented" | "flexible") {
        return Err(format!(
            "workflow execution_policy is invalid: {}",
            package.execution_policy
        ));
    }
    if policy == "strict" {
        if !package.entrypoints.is_empty() || !package.exits.is_empty() {
            return Err("strict workflow cannot declare custom entrypoints or exits".to_string());
        }
        return Ok(());
    }
    if package.entrypoints.is_empty() {
        return Err(format!(
            "{policy} workflow requires at least one entrypoint"
        ));
    }
    if package.exits.is_empty() {
        return Err(format!("{policy} workflow requires at least one exit"));
    }
    validate_endpoint_scope("entrypoint", &package.entrypoints, step_ids)?;
    validate_endpoint_scope("exit", &package.exits, step_ids)?;
    // 默认入口/出口必须指向真实声明的端点，否则等于给调用方埋了一个必失败路径。
    for (label, value, endpoints) in [
        ("default_entrypoint", package.default_entrypoint.trim(), &package.entrypoints),
        ("default_exitpoint", package.default_exitpoint.trim(), &package.exits),
    ] {
        if value.is_empty() {
            continue;
        }
        if !endpoints.iter().any(|endpoint| endpoint.id == value) {
            return Err(format!(
                "workflow {label} is not a declared endpoint: {value}"
            ));
        }
    }
    Ok(())
}

fn validate_endpoint_scope(
    name: &str,
    endpoints: &[WorkflowEndpoint],
    step_ids: &HashSet<String>,
) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut steps = HashSet::new();
    for endpoint in endpoints {
        validate_workflow_id(&endpoint.id)?;
        if !ids.insert(endpoint.id.as_str()) {
            return Err(format!("duplicate workflow {name} id: {}", endpoint.id));
        }
        if endpoint.at_step.trim().is_empty() || !step_ids.contains(endpoint.at_step.as_str()) {
            return Err(format!(
                "workflow {name} {} references unknown step: {}",
                endpoint.id, endpoint.at_step
            ));
        }
        if !steps.insert(endpoint.at_step.as_str()) {
            return Err(format!(
                "workflow {name} step cannot be declared twice: {}",
                endpoint.at_step
            ));
        }
        validate_unique_text(&format!("{name} requirement"), &endpoint.requires)?;
        validate_unique_text(&format!("{name} output"), &endpoint.produces)?;
    }
    Ok(())
}

fn validate_unique_text(name: &str, values: &[String]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(format!("{name} entry cannot be empty"));
        }
        if !seen.insert(value.as_str()) {
            return Err(format!("duplicate {name}: {value}"));
        }
    }
    Ok(())
}

fn validate_relative_asset_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!("workflow asset path escapes package: {value}"));
    }
    Ok(())
}

fn validate_package_assets(root: &Path, package: &WorkflowPackage) -> Result<(), Box<dyn Error>> {
    let canonical_root = root.canonicalize()?;
    let mut assets = package
        .artifacts
        .iter()
        .map(|artifact| artifact.schema.as_str())
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    collect_runtime_schema_assets(&package.steps, &mut assets);
    if !package.ui.entry.trim().is_empty() {
        assets.push(package.ui.entry.as_str());
    }
    let mut validated_view = false;
    for asset in assets {
        let target = canonical_root.join(asset);
        let canonical_target = target
            .canonicalize()
            .map_err(|error| format!("workflow asset is unavailable: {asset}: {error}"))?;
        if !canonical_target.starts_with(&canonical_root) || !canonical_target.is_file() {
            return Err(format!("workflow asset escapes package or is not a file: {asset}").into());
        }
        if !package.ui.entry.trim().is_empty() && asset == package.ui.entry && !validated_view {
            validate_workflow_view(root, &package.ui.entry)?;
            validated_view = true;
        }
    }
    Ok(())
}

fn validate_workflow_view(root: &Path, entry: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(entry);
    let view: WorkflowViewManifest = serde_json::from_slice(&fs::read(&path)?)?;
    if view.schema_version != "workflow_view.v1" {
        return Err("workflow view schema_version is invalid".into());
    }
    if view.title.trim().is_empty() {
        return Err("workflow view title is required".into());
    }
    let mut section_ids = HashSet::new();
    let mut field_ids = HashSet::new();
    for section in &view.sections {
        validate_workflow_id(&section.id).map_err(std::io::Error::other)?;
        if section.title.trim().is_empty() {
            return Err(format!("workflow view section {} title is required", section.id).into());
        }
        if !section_ids.insert(section.id.as_str()) {
            return Err(format!("duplicate workflow view section: {}", section.id).into());
        }
        for field in &section.fields {
            let definition = match field {
                WorkflowViewField::Id(id) => {
                    validate_workflow_id(id).map_err(std::io::Error::other)?;
                    if !field_ids.insert(id.as_str()) {
                        return Err(format!("duplicate workflow view field: {id}").into());
                    }
                    continue;
                }
                WorkflowViewField::Definition(definition) => definition,
            };
            validate_workflow_id(&definition.id).map_err(std::io::Error::other)?;
            if !field_ids.insert(definition.id.as_str()) {
                return Err(format!("duplicate workflow view field: {}", definition.id).into());
            }
            if !matches!(
                definition.field_type.as_str(),
                "text"
                    | "textarea"
                    | "number"
                    | "boolean"
                    | "select"
                    | "list"
                    | "json"
                    | "credential"
            ) {
                return Err(
                    format!("workflow view field {} type is invalid", definition.id).into(),
                );
            }
            if definition.field_type == "select" && definition.options.is_empty() {
                return Err(format!(
                    "workflow view select field {} requires options",
                    definition.id
                )
                .into());
            }
            if definition.field_type == "credential" && definition.target.trim().is_empty() {
                return Err(format!(
                    "workflow view credential field {} requires a target",
                    definition.id
                )
                .into());
            }
            if !matches!(definition.picker.as_str(), "" | "directory") {
                return Err(format!(
                    "workflow view field {} picker is invalid",
                    definition.id
                )
                .into());
            }
            if !matches!(definition.span.as_str(), "" | "full") {
                return Err(format!(
                    "workflow view field {} span is invalid",
                    definition.id
                )
                .into());
            }
        }
    }
    let mut actions = HashSet::new();
    for action in &view.actions {
        validate_workflow_id(action).map_err(std::io::Error::other)?;
        if !actions.insert(action.as_str()) {
            return Err(format!("duplicate workflow view action: {action}").into());
        }
    }
    Ok(())
}

fn default_workflow_view_field_type() -> String {
    "text".to_string()
}

fn collect_runtime_schema_assets<'a>(steps: &'a [WorkflowStep], assets: &mut Vec<&'a str>) {
    for step in steps {
        if let Some(runtime) = step.runtime.as_ref() {
            if !runtime.result_schema.trim().is_empty() {
                assets.push(runtime.result_schema.as_str());
            }
        }
        if let Some(loop_config) = step.loop_config.as_ref() {
            collect_runtime_schema_assets(&loop_config.steps, assets);
        }
    }
}

fn validate_step_scope(
    steps: &[WorkflowStep],
    global_step_ids: &mut HashSet<String>,
    depth: usize,
) -> Result<(), String> {
    if steps.is_empty() {
        return Err("workflow step scope must contain at least one step".to_string());
    }
    if depth > 8 {
        return Err("workflow loop nesting exceeds 8 levels".to_string());
    }
    let mut scope_ids = HashSet::new();
    for step in steps {
        validate_workflow_id(&step.id)?;
        if !global_step_ids.insert(step.id.clone()) {
            return Err(format!("duplicate workflow step id: {}", step.id));
        }
        if step.title.trim().is_empty() {
            return Err(format!("workflow step {} title is required", step.id));
        }
        if !matches!(
            step.execution_mode.as_str(),
            "sync" | "long_running" | "provider_defined"
        ) {
            return Err(format!(
                "workflow step {} execution_mode is invalid",
                step.id
            ));
        }
        if !matches!(step.on_failure.as_str(), "" | "fail" | "continue") {
            return Err(format!(
                "workflow step {} on_failure must be fail or continue",
                step.id
            ));
        }
        if !step.input.is_object() {
            return Err(format!("workflow step {} input must be an object", step.id));
        }
        if step.approval_required && step.risk_level.trim().is_empty() {
            return Err(format!(
                "workflow step {} approval requires a risk level",
                step.id
            ));
        }
        let kind = normalized_step_kind(step)?;
        match kind {
            "capability" => {
                if step.capability_id.trim().is_empty() {
                    return Err(format!(
                        "workflow capability step {} requires capability_id",
                        step.id
                    ));
                }
                if step.runtime.is_some() || step.loop_config.is_some() {
                    return Err(format!(
                        "workflow capability step {} cannot declare runtime or loop",
                        step.id
                    ));
                }
            }
            "runtime" => {
                if !step.capability_id.trim().is_empty() {
                    return Err(format!(
                        "workflow runtime step {} cannot declare capability_id",
                        step.id
                    ));
                }
                validate_runtime_step(step)?;
                if step.loop_config.is_some() {
                    return Err(format!(
                        "workflow runtime step {} cannot declare a loop",
                        step.id
                    ));
                }
            }
            "loop" => {
                if !step.capability_id.trim().is_empty() || step.runtime.is_some() {
                    return Err(format!(
                        "workflow loop step {} cannot declare capability_id or runtime",
                        step.id
                    ));
                }
                validate_loop_step(step, global_step_ids, depth + 1)?;
            }
            "manual" | "provider_defined" => {
                if !step.capability_id.trim().is_empty() {
                    return Err(format!(
                        "workflow {} step {} cannot declare capability_id",
                        kind, step.id
                    ));
                }
                if step.runtime.is_some() || step.loop_config.is_some() {
                    return Err(format!(
                        "workflow {} step {} cannot declare runtime or loop",
                        kind, step.id
                    ));
                }
            }
            _ => unreachable!(),
        }
        if let Some(condition) = step.when.as_ref() {
            validate_condition(condition, 0)?;
        }
        if let Some(condition) = step.fail_when.as_ref() {
            validate_condition(condition, 0)?;
        }
        if !matches!(step.candidate_action.as_str(), "" | "freeze" | "require") {
            return Err(format!(
                "workflow step {} candidate_action is invalid",
                step.id
            ));
        }
        scope_ids.insert(step.id.as_str());
    }
    for step in steps {
        for dependency in &step.depends_on {
            if dependency == &step.id {
                return Err(format!("workflow step {} depends on itself", step.id));
            }
            if !scope_ids.contains(dependency.as_str()) {
                return Err(format!(
                    "workflow step {} references missing dependency {}",
                    step.id, dependency
                ));
            }
        }
    }
    validate_step_graph(steps)?;
    Ok(())
}

fn normalized_step_kind(step: &WorkflowStep) -> Result<&'static str, String> {
    if !step.kind.trim().is_empty() {
        return match step.kind.trim() {
            "capability" => Ok("capability"),
            "runtime" => Ok("runtime"),
            "loop" => Ok("loop"),
            "manual" => Ok("manual"),
            "provider_defined" => Ok("provider_defined"),
            value => Err(format!(
                "workflow step {} kind is invalid: {value}",
                step.id
            )),
        };
    }
    if step.loop_config.is_some() {
        return Ok("loop");
    }
    if step.runtime.is_some() {
        return Ok("runtime");
    }
    if !step.capability_id.trim().is_empty() {
        return Ok("capability");
    }
    if step.approval_required {
        return Ok("manual");
    }
    if step.execution_mode == "provider_defined" {
        return Ok("provider_defined");
    }
    Ok("manual")
}

fn validate_runtime_step(step: &WorkflowStep) -> Result<(), String> {
    let runtime = step
        .runtime
        .as_ref()
        .ok_or_else(|| format!("workflow runtime step {} requires runtime", step.id))?;
    if runtime.provider.trim().is_empty() {
        return Err(format!(
            "workflow runtime step {} provider is required",
            step.id
        ));
    }
    if runtime.prompt.trim().is_empty() {
        return Err(format!(
            "workflow runtime step {} prompt is required",
            step.id
        ));
    }
    if !runtime.result_schema.trim().is_empty() {
        validate_relative_asset_path(&runtime.result_schema)?;
    }
    if !matches!(runtime.tool_policy.as_str(), "" | "default" | "none") {
        return Err(format!(
            "workflow runtime step {} tool_policy must be default or none",
            step.id
        ));
    }
    let mut seen_artifacts = HashSet::new();
    for artifact_id in &runtime.input_artifacts {
        let artifact_id = artifact_id.trim();
        if artifact_id.is_empty() {
            return Err(format!(
                "workflow runtime step {} input_artifacts contains an empty id",
                step.id
            ));
        }
        if !seen_artifacts.insert(artifact_id.to_string()) {
            return Err(format!(
                "workflow runtime step {} input_artifacts contains duplicate id: {}",
                step.id, artifact_id
            ));
        }
    }
    if !runtime.input_artifacts.is_empty() && runtime.tool_policy.trim() == "none" {
        // 读文件需要工具；声明矛盾时直接拒绝，避免运行时必然失败。
        return Err(format!(
            "workflow runtime step {} cannot combine input_artifacts with tool_policy=none",
            step.id
        ));
    }
    if runtime.timeout_seconds > 86_400 {
        return Err(format!(
            "workflow runtime step {} timeout_seconds must not exceed 86400",
            step.id
        ));
    }
    Ok(())
}

fn validate_runtime_declarations(
    steps: &[WorkflowStep],
    supported_runtimes: &[String],
) -> Result<(), String> {
    fn walk(steps: &[WorkflowStep], supported_runtimes: &[String]) -> Result<(), String> {
        for step in steps {
            if let Some(runtime) = step.runtime.as_ref() {
                let provider = runtime.provider.trim();
                if provider != "auto" && !supported_runtimes.iter().any(|item| item == provider) {
                    return Err(format!(
                        "workflow runtime step {} uses undeclared provider {}",
                        step.id, provider
                    ));
                }
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(&loop_config.steps, supported_runtimes)?;
            }
        }
        Ok(())
    }
    walk(steps, supported_runtimes)
}

fn validate_loop_step(
    step: &WorkflowStep,
    global_step_ids: &mut HashSet<String>,
    depth: usize,
) -> Result<(), String> {
    let loop_config = step
        .loop_config
        .as_ref()
        .ok_or_else(|| format!("workflow loop step {} requires loop", step.id))?;
    if !(1..=20).contains(&loop_config.max_iterations) {
        return Err(format!(
            "workflow loop step {} max_iterations must be between 1 and 20",
            step.id
        ));
    }
    if loop_config.continue_when.is_none() && loop_config.exit_when.is_none() {
        return Err(format!(
            "workflow loop step {} requires continue_when or exit_when",
            step.id
        ));
    }
    if let Some(condition) = loop_config.continue_when.as_ref() {
        validate_condition(condition, 0)?;
    }
    if let Some(condition) = loop_config.exit_when.as_ref() {
        validate_condition(condition, 0)?;
    }
    if loop_config.steps.iter().any(step_scope_requires_approval) {
        return Err(format!(
            "workflow loop step {} cannot contain approval-required steps; approvals belong outside the loop",
            step.id
        ));
    }
    if loop_config
        .steps
        .iter()
        .any(step_scope_has_candidate_action)
    {
        return Err(format!(
            "workflow loop step {} cannot freeze or require a candidate; candidate binding belongs outside the loop",
            step.id
        ));
    }
    validate_step_scope(&loop_config.steps, global_step_ids, depth)
}

fn step_scope_requires_approval(step: &WorkflowStep) -> bool {
    step.approval_required
        || step
            .loop_config
            .as_ref()
            .is_some_and(|loop_config| loop_config.steps.iter().any(step_scope_requires_approval))
}

fn step_scope_has_candidate_action(step: &WorkflowStep) -> bool {
    !step.candidate_action.is_empty()
        || step.loop_config.as_ref().is_some_and(|loop_config| {
            loop_config
                .steps
                .iter()
                .any(step_scope_has_candidate_action)
        })
}

fn validate_condition(condition: &WorkflowCondition, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("workflow condition nesting exceeds 8 levels".to_string());
    }
    match condition.operator.as_str() {
        "all" | "any" => {
            if condition.conditions.is_empty() {
                return Err(format!(
                    "workflow condition {} requires conditions",
                    condition.operator
                ));
            }
            for child in &condition.conditions {
                validate_condition(child, depth + 1)?;
            }
        }
        "not" => {
            if condition.conditions.len() != 1 {
                return Err("workflow not condition requires exactly one child".to_string());
            }
            validate_condition(&condition.conditions[0], depth + 1)?;
        }
        "equals" | "not_equals" | "exists" | "contains" | "gt" | "gte" | "lt" | "lte" => {
            if condition.path.trim().is_empty() {
                return Err(format!(
                    "workflow condition {} requires path",
                    condition.operator
                ));
            }
        }
        value => return Err(format!("workflow condition operator is invalid: {value}")),
    }
    Ok(())
}

fn validate_candidate_actions(
    steps: &[WorkflowStep],
    candidate: Option<&WorkflowCandidatePolicy>,
) -> Result<(), String> {
    fn walk(
        steps: &[WorkflowStep],
        candidate_present: bool,
        freeze_count: &mut u32,
    ) -> Result<(), String> {
        for step in steps {
            match step.candidate_action.as_str() {
                "" => {}
                "freeze" => {
                    if !candidate_present {
                        return Err(format!(
                            "workflow step {} freezes an undeclared candidate",
                            step.id
                        ));
                    }
                    *freeze_count += 1;
                }
                "require" => {
                    if !candidate_present {
                        return Err(format!(
                            "workflow step {} requires an undeclared candidate",
                            step.id
                        ));
                    }
                }
                _ => {}
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(&loop_config.steps, candidate_present, freeze_count)?;
            }
        }
        Ok(())
    }
    let mut freeze_count = 0;
    walk(steps, candidate.is_some(), &mut freeze_count)?;
    if candidate.is_some_and(|candidate| candidate.required) && freeze_count != 1 {
        return Err("workflow candidate requires exactly one freeze step".to_string());
    }
    Ok(())
}

fn validate_step_graph(steps: &[WorkflowStep]) -> Result<(), String> {
    let dependencies = steps
        .iter()
        .map(|step| (step.id.as_str(), step.depends_on.as_slice()))
        .collect::<HashMap<_, _>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for step in steps {
        visit_step(step.id.as_str(), &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_step<'a>(
    step_id: &'a str,
    dependencies: &HashMap<&'a str, &'a [String]>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> Result<(), String> {
    if visited.contains(step_id) {
        return Ok(());
    }
    if !visiting.insert(step_id) {
        return Err(format!("workflow step graph contains a cycle at {step_id}"));
    }
    if let Some(step_dependencies) = dependencies.get(step_id) {
        for dependency in *step_dependencies {
            visit_step(dependency.as_str(), dependencies, visiting, visited)?;
        }
    }
    visiting.remove(step_id);
    visited.insert(step_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_wechat_miniprogram_delivery_package() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let package = load_from_directory(&root).unwrap();
        assert_eq!(
            package.id,
            "com.himind.workflow.wechat-miniprogram-delivery"
        );
        assert!(package.steps.iter().any(|step| step.id == "DEV-LOOP"));
        assert!(package.steps.iter().any(|step| step.id == "WX-CANDIDATE"));
        assert!(package
            .capabilities
            .contains(&"workflow.candidate.freeze".to_string()));
    }

    #[test]
    fn workflow_package_v1_legacy_fixture_remains_compatible() {
        let package: WorkflowPackage = serde_json::from_str(include_str!(
            "../../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"
        ))
        .unwrap();
        package.validate().unwrap();
        assert_eq!(package.steps[0].kind, "provider_defined");
        assert_eq!(package.steps[0].execution_mode, "provider_defined");
        assert_eq!(package.ui.mode, "standard");
    }

    #[test]
    fn workflow_package_v1_current_product_remains_compatible() {
        let package: WorkflowPackage = serde_json::from_str(include_str!(
            "../../workflows/wechat-miniprogram-delivery/workflow.json"
        ))
        .unwrap();
        package.validate().unwrap();
        assert!(package.steps.iter().any(|step| step.kind == "loop"));
        assert!(package
            .steps
            .iter()
            .any(|step| step.candidate_action == "freeze"));
    }

    #[test]
    fn segmented_workflow_requires_declared_entry_and_exit_steps() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        package.execution_policy = "segmented".to_string();
        package.entrypoints = vec![WorkflowEndpoint {
            id: "develop".to_string(),
            at_step: "DEV-LOOP".to_string(),
            label: "开发".to_string(),
            requires: vec!["project".to_string()],
            produces: Vec::new(),
        }];
        package.exits = vec![WorkflowEndpoint {
            id: "checkpoint".to_string(),
            at_step: "WX-CANDIDATE".to_string(),
            label: "候选冻结".to_string(),
            requires: Vec::new(),
            produces: vec!["candidate".to_string()],
        }];
        package.validate().unwrap();

        package.entrypoints[0].at_step = "UNKNOWN".to_string();
        assert!(package
            .validate()
            .unwrap_err()
            .contains("references unknown step"));
    }

    #[test]
    fn strict_workflow_rejects_custom_entrypoints_and_exits() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        package.entrypoints = vec![WorkflowEndpoint {
            id: "develop".to_string(),
            at_step: "DEV-LOOP".to_string(),
            label: String::new(),
            requires: Vec::new(),
            produces: Vec::new(),
        }];
        assert!(package
            .validate()
            .unwrap_err()
            .contains("strict workflow cannot declare"));
    }

    #[test]
    fn workflow_package_future_schema_version_fails_closed() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"
        ))
        .unwrap();
        value["schema_version"] = Value::String("workflow_package.v2".to_string());
        let package: WorkflowPackage = serde_json::from_value(value).unwrap();
        assert_eq!(
            package.validate().unwrap_err(),
            "workflow package schema_version is invalid"
        );
    }

    #[test]
    fn workflow_package_unknown_fields_fail_closed() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../contracts/agent-core/v1/examples/workflow-package.legacy-v1.json"
        ))
        .unwrap();
        value["future_extension"] = Value::Bool(true);
        let error = serde_json::from_value::<WorkflowPackage>(value).unwrap_err();
        assert!(error
            .to_string()
            .contains("unknown field `future_extension`"));
    }

    #[test]
    fn rejects_step_dependency_cycles() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let first = package.steps[0].id.clone();
        let second = package.steps[1].id.clone();
        package.steps[0].depends_on.push(second);
        package.steps[1].depends_on.push(first);
        assert!(package.validate().unwrap_err().contains("cycle"));
    }

    #[test]
    fn rejects_custom_ui_path_escape() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        package.ui.mode = "custom".to_string();
        package.ui.entry = "../outside.html".to_string();
        assert!(package.validate().unwrap_err().contains("escapes"));
    }

    #[test]
    fn rejects_missing_asset() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let temp = std::env::temp_dir().join(format!(
            "himind-workflow-missing-asset-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        copy_dir(&root, &temp).unwrap();
        fs::remove_file(temp.join("ui/workflow-view.json")).unwrap();
        assert!(load_from_directory(&temp)
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn rejects_invalid_declarative_ui_field_type() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let temp = std::env::temp_dir().join(format!(
            "himind-workflow-invalid-view-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        copy_dir(&root, &temp).unwrap();
        let view_path = temp.join("ui/workflow-view.json");
        let mut view: Value = serde_json::from_slice(&fs::read(&view_path).unwrap()).unwrap();
        view["sections"][0]["fields"][0]["type"] = Value::String("script".to_string());
        fs::write(&view_path, serde_json::to_vec_pretty(&view).unwrap()).unwrap();
        assert!(load_from_directory(&temp)
            .unwrap_err()
            .to_string()
            .contains("type is invalid"));
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn accepts_runtime_step_and_condition_contracts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "runtime".to_string();
        step.capability_id.clear();
        step.runtime = Some(WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Implement the next change from the current feedback.".to_string(),
            workspace_path: "input.project_root".to_string(),
            result_schema: String::new(),
            allow_network: false,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            timeout_seconds: 1_800,
        });
        step.when = Some(WorkflowCondition {
            operator: "all".to_string(),
            path: String::new(),
            value: Value::Null,
            conditions: vec![WorkflowCondition {
                operator: "equals".to_string(),
                path: "input.mode".to_string(),
                value: Value::String("develop".to_string()),
                conditions: Vec::new(),
            }],
        });
        package.validate().unwrap();
    }

    #[test]
    fn rejects_runtime_step_timeout_above_one_day() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let loop_step = package
            .steps
            .iter_mut()
            .find(|step| step.id == "DEV-LOOP")
            .unwrap();
        loop_step
            .loop_config
            .as_mut()
            .unwrap()
            .steps
            .iter_mut()
            .find(|step| step.id == "DEV-CODE")
            .unwrap()
            .runtime
            .as_mut()
            .unwrap()
            .timeout_seconds = 86_401;
        assert!(package
            .validate()
            .unwrap_err()
            .contains("timeout_seconds must not exceed 86400"));
    }

    #[test]
    fn accepts_bounded_development_loop() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "loop".to_string();
        step.capability_id.clear();
        step.loop_config = Some(Box::new(WorkflowLoop {
            max_iterations: 4,
            pause_for_feedback: false,
            continue_when: Some(WorkflowCondition {
                operator: "not_equals".to_string(),
                path: "loops.DEV.latest.feedback".to_string(),
                value: Value::String("accepted".to_string()),
                conditions: Vec::new(),
            }),
            exit_when: Some(WorkflowCondition {
                operator: "equals".to_string(),
                path: "loops.DEV.latest.feedback".to_string(),
                value: Value::String("accepted".to_string()),
                conditions: Vec::new(),
            }),
            steps: vec![WorkflowStep {
                id: "DEV-BODY".to_string(),
                title: "Runtime development iteration".to_string(),
                kind: "provider_defined".to_string(),
                capability_id: String::new(),
                runtime: None,
                loop_config: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: default_object(),
                execution_mode: "provider_defined".to_string(),
                risk_level: "local_write".to_string(),
                approval_required: false,
                on_failure: String::new(),
                depends_on: Vec::new(),
            }],
        }));
        package.validate().unwrap();
    }

    #[test]
    fn rejects_loop_without_iteration_condition() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "loop".to_string();
        step.capability_id.clear();
        step.loop_config = Some(Box::new(WorkflowLoop {
            max_iterations: 4,
            pause_for_feedback: false,
            continue_when: None,
            exit_when: None,
            steps: vec![WorkflowStep {
                id: "DEV-BODY".to_string(),
                title: "Body".to_string(),
                kind: "manual".to_string(),
                capability_id: String::new(),
                runtime: None,
                loop_config: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: default_object(),
                execution_mode: "sync".to_string(),
                risk_level: "read_only".to_string(),
                approval_required: false,
                on_failure: String::new(),
                depends_on: Vec::new(),
            }],
        }));
        assert!(package
            .validate()
            .unwrap_err()
            .contains("requires continue_when or exit_when"));
    }

    #[test]
    fn candidate_requires_one_freeze_step() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        package.candidate = Some(WorkflowCandidatePolicy {
            required: true,
            artifact_id: "release-record".to_string(),
            source: "git".to_string(),
            allow_dirty: false,
        });
        package
            .steps
            .iter_mut()
            .find(|step| step.id == "WX-CANDIDATE")
            .unwrap()
            .candidate_action
            .clear();
        assert!(package
            .validate()
            .unwrap_err()
            .contains("exactly one freeze step"));
    }

    fn copy_dir(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let target_path = target.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &target_path)?;
            } else {
                fs::copy(entry.path(), target_path)?;
            }
        }
        Ok(())
    }
}
