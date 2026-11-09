use semver::Version;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::extension_contracts::DistributionTarget;

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
    ensure_release_preflight_ready, list as list_authoring_drafts,
    mark_submitted as mark_workflow_candidate_submitted, read as read_authoring_draft,
    save_from_source as save_authoring_candidate, submit as submit_authoring_candidate,
    test as test_authoring_candidate,
    test_with_capabilities as test_authoring_candidate_with_capabilities,
    validate_release_dependency_versions, WorkflowDraft,
};
#[allow(unused_imports)]
pub(crate) use candidate::{freeze_candidate, read_candidate};
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
pub(crate) use presets::{
    delete as delete_run_preset, find as find_run_preset, list as list_run_presets,
    load as load_run_presets, merge_override as merge_run_preset_input, set as set_run_preset,
    WorkflowRunPreset,
};
#[allow(unused_imports)]
pub(crate) use runner::{
    collect_step_evidence, verify_run, SharedStepExecutor, WorkflowArtifactOutput,
    WorkflowArtifactVerification, WorkflowRunOutcome, WorkflowRunVerification, WorkflowRunner,
    WorkflowStepExecution, WorkflowStepExecutor,
};
pub(crate) use runtime::execute_runtime_step;
pub(crate) use store::{InstalledWorkflow, WorkflowStore};

pub(crate) const WORKFLOW_PACKAGE_SCHEMA_VERSION: &str = "workflow_package.v1";

/// 运行期步参数覆盖在 `input` 里的键。
///
/// 工作流包是签名制品，结构改动必须走版本化路径；但「这次要多一层批准」
/// 「最多重试几轮」属于执行策略，用户应当在启动前就能调，不必为此发新版本。
/// 覆盖只放在本次运行的输入里，所以它天然随 interaction 存档、随恢复回放。
pub(crate) const EXECUTION_OVERRIDES_KEY: &str = "execution_overrides";

/// 应用运行期步参数覆盖。
///
/// 只接受「收紧」这一个方向：
/// - 可以给一步加审批闸门，不能撤掉包里已有的；
/// - 可以把循环轮数调小，不能调大；
/// - 只能把 `on_failure` 改成 `fail`，不能改成 `continue` 把失败吞掉。
///
/// 反方向等于用运行参数绕过签名包声明的风险边界，所以直接拒绝而不是静默忽略。
pub(crate) fn apply_execution_overrides(
    package: &WorkflowPackage,
    input: &Value,
) -> Result<WorkflowPackage, Box<dyn Error>> {
    let Some(overrides) = input
        .get(EXECUTION_OVERRIDES_KEY)
        .and_then(Value::as_object)
    else {
        return Ok(package.clone());
    };
    let mut patched = package.clone();
    for (step_id, spec) in overrides {
        let spec = spec
            .as_object()
            .ok_or_else(|| format!("execution override for {step_id} must be an object"))?;
        let step = find_step_mut(&mut patched.steps, step_id)
            .ok_or_else(|| format!("execution override references unknown step: {step_id}"))?;
        if let Some(approval) = spec.get("approval_required").and_then(Value::as_bool) {
            if !approval && step.approval_required {
                return Err(format!(
                    "execution override cannot remove the approval gate on {step_id}"
                )
                .into());
            }
            if approval {
                if step.risk_level.trim().is_empty() {
                    return Err(format!(
                        "workflow step {step_id} approval requires a risk level"
                    )
                    .into());
                }
                step.approval_required = true;
            }
        }
        if let Some(rounds) = spec.get("max_iterations").and_then(Value::as_u64) {
            let rounds = u32::try_from(rounds).unwrap_or(u32::MAX);
            if rounds == 0 {
                return Err(format!("execution override max_iterations must be >= 1: {step_id}").into());
            }
            let loop_config = step.loop_config.as_mut().ok_or_else(|| {
                format!("execution override max_iterations requires a loop step: {step_id}")
            })?;
            // 可调范围是 `[1, iteration_limit]`，不是「只能降」。
            // 包作者用 `max_iterations_limit` 声明成本边界；没声明的老包上限即默认值，
            // 所以行为与以前完全一致。
            let limit = loop_config.iteration_limit();
            if rounds > limit {
                return Err(format!(
                    "execution override max_iterations exceeds the declared limit on {step_id} (limit {limit}, requested {rounds})"
                )
                .into());
            }
            loop_config.max_iterations = rounds;
        }
        if let Some(on_failure) = spec.get("on_failure").and_then(Value::as_str) {
            if on_failure.trim() != "fail" {
                return Err(format!(
                    "execution override on_failure only accepts \"fail\": {step_id}"
                )
                .into());
            }
            step.on_failure = "fail".to_string();
        }
        // 换 Runtime 只允许在主清单 + 可选清单内进行。
        //
        // 这是「不重新打包也能换外部 AI 客户端」的唯一入口：包作者用
        // `optional_providers` 声明能接受的替代实现，用户启动时选其中一个。
        // 白名单之外的 provider 一律拒绝——运行参数不能成为绕过包声明的后门。
        if let Some(provider) = spec.get("runtime_provider").and_then(Value::as_str) {
            let provider = provider.trim();
            if provider.is_empty() {
                return Err(format!(
                    "execution override runtime_provider cannot be empty: {step_id}"
                )
                .into());
            }
            if !package.accepts_provider(provider) {
                return Err(format!(
                    "execution override runtime_provider is not declared by the package: {step_id} -> {provider}"
                )
                .into());
            }
            let runtime = step.runtime.as_mut().ok_or_else(|| {
                format!(
                    "execution override runtime_provider requires a runtime step: {step_id}"
                )
            })?;
            // 换 provider 不能把「强制隔离」换成「做不到隔离」。
            // 这条和预检的 tool_policy 检查是同一个约束，但必须在这里也拦一次：
            // 用户可能在预检通过后才提交覆盖，而覆盖是启动路径上最后一处可写点。
            if runtime.tool_policy.trim() == "none" && !runtime_supports_tool_none(provider) {
                return Err(format!(
                    "execution override cannot move {step_id} to {provider}: that provider cannot enforce tool_policy=none"
                )
                .into());
            }
            runtime.provider = provider.to_string();
        }
        // 换模型：与换 provider 同一道门。空串表示「清掉声明、回到 Runtime 默认」，
        // 因此必须是显式 null/空串才生效，避免前端漏字段时误清。
        if let Some(model) = spec.get("model").and_then(Value::as_str) {
            let model = model.trim();
            let runtime = step.runtime.as_mut().ok_or_else(|| {
                format!("execution override model requires a runtime step: {step_id}")
            })?;
            if model.is_empty() {
                runtime.model = String::new();
                runtime.model_fallbacks.clear();
            } else {
                validate_model_alias(model)
                    .map_err(|error| format!("execution override model on {step_id}: {error}"))?;
                model_alias_matches_provider(&runtime.provider, model)
                    .map_err(|error| format!("execution override model on {step_id}: {error}"))?;
                runtime.model = model.to_string();
                // 换了主模型，旧降级序列（可能属于另一命名空间）就不再成立。
                runtime.model_fallbacks.clear();
            }
        }
    }
    // 覆盖后必须仍然是一个合法包：例如给 runtime 步骤加审批会撞上能力步骤的约束。
    patched.validate().map_err(std::io::Error::other)?;
    Ok(patched)
}

fn find_step_mut<'a>(steps: &'a mut [WorkflowStep], step_id: &str) -> Option<&'a mut WorkflowStep> {
    for step in steps.iter_mut() {
        if step.id == step_id {
            return Some(step);
        }
        if let Some(loop_config) = step.loop_config.as_mut() {
            if let Some(found) = find_step_mut(&mut loop_config.steps, step_id) {
                return Some(found);
            }
        }
    }
    None
}

/// 能力在当下是否可用：控制面能力只有接上工作台（dashboard_provider）才算可用。
///
/// 预检与运行期必须共用这一条判据，否则会出现「预检说可用、运行期调不到」。
pub(crate) fn capability_is_usable(
    capability: &crate::capability::types::CapabilityDescriptor,
) -> bool {
    capability.availability != crate::capability::types::CapabilityAvailability::ControlPlane
        || capability.dashboard_provider
}

/// 哪些 Runtime 能真正执行 `tool_policy = "none"`。
///
/// 判据只能来自 Runtime 自身的能力，不能靠「名字看起来像内建的」。目前只有
/// 内核自带的两个 Runtime 能在不挂载模型可见工具的前提下跑一步：
/// - `himind.builtin`：由 Agent 直接控制进程与工具挂载；
/// - `himind.fixture`：测试替身，本来就不挂工具。
///
/// 其余 Provider（personal.codex / personal.github-copilot / ACP）由外部 CLI 掌管
/// 工具面，Agent 只能提要求、不能强制，所以拒绝而不是静默降级——
/// 「审查角色读不到实现过程」是这个策略存在的唯一理由，做不到就不该假装做到。
pub(crate) const TOOL_NONE_CAPABLE_RUNTIMES: &[&str] = &["himind.builtin", "himind.fixture"];

/// 这个 Runtime 能否执行 `tool_policy = "none"`。
pub(crate) fn runtime_supports_tool_none(provider: &str) -> bool {
    TOOL_NONE_CAPABLE_RUNTIMES.contains(&provider.trim())
}

pub(crate) fn workflow_approval_id(run_id: &str, step_id: &str) -> String {
    format!("workflow:{}:{}", run_id.trim(), step_id.trim())
}

/// 模型别名的两种命名空间。
///
/// 别名必须带来源前缀：`deepseek-chat` 这个名字可能同时存在于两个服务账号下，
/// 裸 id 无法决定走哪条路由。
const MODEL_ALIAS_HIMIND_PREFIX: &str = "himind:";
const MODEL_ALIAS_ACP_PREFIX: &str = "acp:";
/// 透传给外部 CLI 的模型 id（`cli:codex:<id>` / `cli:github-copilot:<id>`）。
///
/// 与前两个命名空间的区别：`himind:` 与 `acp:` 都是**逻辑别名**，由平台解析；
/// `cli:` 是**原样透传**，因为 Codex / Copilot 的模型 id 属于它们自己的协议，
/// 平台不做映射，也无法校验是否存在。
const MODEL_ALIAS_CLI_PREFIX: &str = "cli:";

/// 这条模型别名是否语法合法。
///
/// 只校验形状，不校验「这个模型在这台机器上装没装」——后者是预检的事，
/// 需要探测本机 AI 服务与 ACP profile，契约校验阶段拿不到。
///
/// 约束：
/// - `himind:<service_id>:<model>`，两段都非空；
/// - `acp:<provider>:<model>`，provider 可带 `acp.` 前缀也可不带（归一后比较）。
pub(crate) fn validate_model_alias(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > 300 {
        return Err(format!("model alias is too long: {} chars", value.len()));
    }
    if value.chars().any(char::is_control) || value.contains(char::is_whitespace) {
        return Err(format!("model alias cannot contain whitespace or control chars: {value}"));
    }
    if let Some(rest) = value.strip_prefix(MODEL_ALIAS_HIMIND_PREFIX) {
        let (service, model) = rest
            .split_once(':')
            .ok_or_else(|| format!("himind model alias requires <service>:<model>: {value}"))?;
        if service.trim().is_empty() || model.trim().is_empty() {
            return Err(format!("himind model alias has an empty segment: {value}"));
        }
        return Ok(());
    }
    if let Some(rest) = value.strip_prefix(MODEL_ALIAS_ACP_PREFIX) {
        let (provider, model) = rest
            .split_once(':')
            .ok_or_else(|| format!("acp model alias requires <provider>:<model>: {value}"))?;
        let provider = provider.trim().strip_prefix("acp.").unwrap_or(provider.trim());
        if provider.is_empty() || model.trim().is_empty() {
            return Err(format!("acp model alias has an empty segment: {value}"));
        }
        return Ok(());
    }
    if let Some(rest) = value.strip_prefix(MODEL_ALIAS_CLI_PREFIX) {
        let (provider, model) = rest
            .split_once(':')
            .ok_or_else(|| format!("cli model alias requires <provider>:<model>: {value}"))?;
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(format!("cli model alias has an empty segment: {value}"));
        }
        return Ok(());
    }
    Err(format!(
        "model alias must start with `himind:`, `acp:` or `cli:` (got {value}); a bare model id cannot decide the route"
    ))
}

impl WorkflowPackage {
    /// 这个包是否接受某个 Runtime：主清单与可选清单都算。
    ///
    /// 预检、运行期覆盖、角色/步骤校验必须共用这一条，否则会出现
    /// 「声明在 optional_providers 里、预检也列出来了，运行时却说未声明」。
    pub(crate) fn accepts_provider(&self, provider: &str) -> bool {
        let provider = provider.trim();
        !provider.is_empty()
            && (self
                .supported_runtimes
                .iter()
                .any(|item| item.trim() == provider)
                || self
                    .optional_providers
                    .iter()
                    .any(|item| item.trim() == provider))
    }
}

/// 模型别名与某个具体 Provider 是否匹配。
///
/// `auto` 不接受任何声明模型：auto 的语义是「运行时自己挑一个能干的」，
/// 它可能挑到不认这个命名空间的 Runtime，所以在契约阶段就拒绝，
/// 让作者显式写出 provider。
pub(crate) fn model_alias_matches_provider(provider: &str, alias: &str) -> Result<(), String> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Ok(());
    }
    let provider = provider.trim();
    if provider.is_empty() {
        return Err("declaring model requires an explicit runtime provider".to_string());
    }
    if provider == "auto" {
        return Err(
            "declaring model requires an explicit runtime provider, not `auto`".to_string(),
        );
    }
    let namespace = model_alias_namespace(alias).ok_or_else(|| {
        format!("model alias must start with `himind:`, `acp:` or `cli:` (got {alias})")
    })?;
    let provider_is_acp = provider == "acp.stdio" || provider.starts_with("acp.");
    let provider_is_cli = matches!(provider, "personal.codex" | "personal.github-copilot");
    match (namespace, provider_is_acp, provider_is_cli) {
        ("himind", false, false) => Ok(()),
        ("acp", true, false) => Ok(()),
        ("cli", false, true) => {
            // 透传命名空间还要核对「给谁用」：`cli:codex:` 不能喂给 Copilot。
            let rest = alias.trim_start_matches(MODEL_ALIAS_CLI_PREFIX);
            let target = rest.split_once(':').map(|(name, _)| name.trim()).unwrap_or_default();
            if target.eq_ignore_ascii_case(cli_target_for_provider(provider)) {
                Ok(())
            } else {
                Err(format!(
                    "runtime {provider} cannot execute `cli:{target}:` model alias {alias}; the cli target must be {}",
                    cli_target_for_provider(provider)
                ))
            }
        }
        ("himind", true, _) | ("himind", _, true) => Err(format!(
            "runtime {provider} cannot execute `himind:` model alias {alias}; `himind:` aliases are only resolved by the built-in runtime"
        )),
        ("acp", _, _) => Err(format!(
            "runtime {provider} cannot execute `acp:` model alias {alias}; `acp:` aliases belong to an ACP client"
        )),
        ("cli", _, _) => Err(format!(
            "runtime {provider} cannot execute `cli:` model alias {alias}; `cli:` aliases are only forwarded to a local CLI runtime"
        )),
        _ => Err(format!("model alias has an unsupported namespace: {alias}")),
    }
}

/// `cli:` 别名里的目标名与 provider 的稳定映射。
fn cli_target_for_provider(provider: &str) -> &'static str {
    match provider.trim() {
        "personal.codex" => "codex",
        "personal.github-copilot" => "github-copilot",
        _ => "",
    }
}

/// 别名归属哪个 Runtime 命名空间。
pub(crate) fn model_alias_namespace(value: &str) -> Option<&'static str> {
    let value = value.trim();
    if value.starts_with(MODEL_ALIAS_HIMIND_PREFIX) {
        return Some("himind");
    }
    if value.starts_with(MODEL_ALIAS_ACP_PREFIX) {
        return Some("acp");
    }
    if value.starts_with(MODEL_ALIAS_CLI_PREFIX) {
        return Some("cli");
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowPackage {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    /// 作者声明的分发落点（`workbench` / `github`）。
    ///
    /// 属于发布元数据，运行期不参与执行：Agent 读它来判断这个制品允许发到哪里。
    /// 声明即硬约束，本机设置只能在声明范围内收窄，不能扩权。
    #[serde(default)]
    pub distribution_targets: Vec<DistributionTarget>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Optional package-local PNG/WebP path used as the extension brand icon.
    #[serde(default)]
    pub icon: String,
    /// 本版本更新说明，用于市场与更新日志展示。
    ///
    /// 与 `distribution_targets` 一样属于发布元数据，运行期不参与执行：Agent 在
    /// 发布拓展时把它写进发布清单，市场据此渲染版本更新说明。
    #[serde(default)]
    pub release_notes: String,
    pub min_agent_version: String,
    #[serde(default = "default_object")]
    pub local_requirements: Value,
    #[serde(default)]
    pub optional_providers: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 执行角色及其执行配置。
    ///
    /// 步骤通过 `role_id` 引用这里的键。角色只描述「谁在做、怎么做」，
    /// 不改变能力路由：一个任务里不同步骤分属不同角色，但都跑在同一条运行台账上。
    /// 未声明的角色 ID 允许存在于步骤上（老包渐进迁移），只是不获得任何覆盖。
    #[serde(default)]
    pub roles: std::collections::BTreeMap<String, WorkflowRole>,
    /// 声明能力的候选提供方：主能力不可用时，任一候选可用即视为满足。
    ///
    /// 典型用途是控制面/独立模式的差异：软件发布工作流声明
    /// `distribution.release.publish`（工作台发布，独立模式下不可用），同时把
    /// `distribution.release.publish.github`（仓库级发布，Local）列为候选 —— 这样
    /// 独立模式下预检不再把整条工作流拦死。预检会明确写出实际使用的是候选提供方，
    /// 不静默降级。
    #[serde(default)]
    pub capability_alternatives: std::collections::BTreeMap<String, Vec<String>>,
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
    pub skills: Vec<WorkflowDependencyRef>,
    #[serde(default)]
    pub plugins: Vec<WorkflowDependencyRef>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

/// 工作流依赖里的一条声明。
///
/// 字符串写法表示「本分发内的必需依赖」；对象写法可以补 `required` 与
/// `min_version`，用于跨分发依赖（依赖由别的分发发布，这里解析不到确定制品）
/// 或需要声明版本下限的场景。只声明 ID 的依赖序列化回字符串，简单声明不被
/// 扩写成对象，源码清单保持可读。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkflowDependencyRef {
    pub id: String,
    pub required: bool,
    pub min_version: String,
}

impl WorkflowDependencyRef {
    pub(crate) fn required(id: impl Into<String>) -> Self {
        Self {
            id: id.into().trim().to_string(),
            required: true,
            min_version: String::new(),
        }
    }

    /// 运行期只关心「这条依赖的 ID」时用这个取值，避免到处写 `.id`。
    pub(crate) fn id(&self) -> &str {
        self.id.as_str()
    }
}

impl<'de> Deserialize<'de> for WorkflowDependencyRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(id) => {
                let id = id.trim();
                if id.is_empty() {
                    return Err(serde::de::Error::custom("workflow dependency id is empty"));
                }
                Ok(Self::required(id))
            }
            Value::Object(map) => {
                let text = |key: &str| {
                    map.get(key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string()
                };
                let id = ["plugin_id", "skill_id", "id"]
                    .into_iter()
                    .map(text)
                    .find(|candidate| !candidate.is_empty())
                    .ok_or_else(|| {
                        serde::de::Error::custom("workflow dependency object needs an id")
                    })?;
                let required = match map.get("required") {
                    None | Some(Value::Null) => true,
                    Some(Value::Bool(flag)) => *flag,
                    Some(_) => {
                        return Err(serde::de::Error::custom(format!(
                            "workflow dependency {id} required must be a boolean"
                        )))
                    }
                };
                Ok(Self {
                    id,
                    required,
                    min_version: text("min_version"),
                })
            }
            _ => Err(serde::de::Error::custom(
                "workflow dependency must be a string id or an object",
            )),
        }
    }
}

impl Serialize for WorkflowDependencyRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.required && self.min_version.trim().is_empty() {
            return serializer.serialize_str(self.id.trim());
        }
        let mut object = serde_json::Map::new();
        object.insert("id".to_string(), Value::String(self.id.trim().to_string()));
        object.insert("required".to_string(), Value::Bool(self.required));
        if !self.min_version.trim().is_empty() {
            object.insert(
                "min_version".to_string(),
                Value::String(self.min_version.trim().to_string()),
            );
        }
        Value::Object(object).serialize(serializer)
    }
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
    /// 把这一步拆成多个并行子任务，各起一条子运行。
    ///
    /// 与 `loop` 的区别是语义：loop 是「同一步重做」，delegate 是「分给不同角色做」。
    /// 两者都产生子运行，但统计口径不同（返工 vs 分工），所以是独立的步骤配置。
    #[serde(default)]
    pub delegate: Option<Box<WorkflowDelegate>>,
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
    /// 执行这一步的角色（如 `software.implementer`）。
    ///
    /// 只描述「谁在做」，不参与能力路由：一个任务里不同步骤可以分属不同角色，
    /// 但都跑在同一条运行台账上。留空表示未声明角色，界面不显示角色徽标。
    #[serde(default)]
    pub role_id: String,
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
    /// 网络约束：`null`（未声明）表示不额外要求，`true` 表示允许联网，
    /// `false` 表示要求 Runtime 提供可证明的网络隔离。
    ///
    /// 只有显式 `false` 才会在预检里要求 `network_isolated=true` 的 Runtime；
    /// 不声明不会把「无法证明隔离」变成拒绝服务。
    #[serde(default)]
    pub allow_network: Option<bool>,
    /// 该步骤需要的上游 Artifact（按 package 里声明的 id）。
    ///
    /// 平台把 Artifact 以**文件路径**注入 `input.input_artifacts`，并把上游步骤输出
    /// 从提示词里移除：数据走文件，提示词只描述任务。这样提示词长度与 Artifact
    /// 体量无关，也不再把命令行当数据通道。
    #[serde(default)]
    pub input_artifacts: Vec<String>,
    /// 允许该步骤使用哪些工具：`default`（沿用 Runtime 默认）或 `none`（不挂载任何
    /// 模型可见工具）。`none` 由 Runtime Provider 强制，无法保证时预检失败。
    /// 选择 `none` 时，平台会把声明的 input_artifacts 作为只读内容注入提示上下文，
    /// 不需要模型再调用文件工具。
    #[serde(default)]
    pub tool_policy: String,
    /// 这一步用哪个模型。留空表示「沿用 Runtime 的当前配置」（历史行为）。
    ///
    /// 取值是**逻辑别名**而不是裸模型 id：
    /// - `himind:<service_id>:<model>`：本机 AI 服务里的某个模型，由 Agent 内的
    ///   推理网关按别名路由（见 `app::builtin_ai_router::model_alias`）。
    /// - `acp:<provider>:<model>`：外部 ACP 客户端声明的模型，映射到该会话
    ///   `config_options` 里 `category=model` 的可选值。
    ///
    /// 为什么不写裸 id：同名的 `deepseek-chat` 可能来自两个不同的服务账号，
    /// 裸 id 在网关里无法区分；别名已经带上了来源。
    #[serde(default)]
    pub model: String,
    /// 主模型不可用时的降级序列，按顺序尝试；全部不可用则 fail-closed。
    #[serde(default)]
    pub model_fallbacks: Vec<String>,
    #[serde(default)]
    pub timeout_seconds: u64,
}

/// 一个执行角色的配置。
///
/// 步骤上的 `role_id` 指向这里。所有字段都是可选的：只声明一个角色 ID 而不写
/// 任何配置，等价于「这一步由某人负责」，不改变任何执行行为。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowRole {
    #[serde(default)]
    pub label: String,
    /// 该角色负责步骤的职责说明；会拼进提示词前缀，让同一 Runtime 按角色行事。
    #[serde(default)]
    pub brief: String,
    /// 覆盖步骤声明的 Runtime Provider。留空沿用步骤自己的。
    #[serde(default)]
    pub runtime_provider: String,
    /// 覆盖步骤的提示词模板。留空沿用步骤自己的（角色 brief 仍会追加）。
    #[serde(default)]
    pub prompt: String,
    /// 覆盖步骤的工具策略：`default` / `none`。留空沿用步骤自己的。
    ///
    /// 这是角色隔离的实质手段之一：审查角色挂 `none` 就看不到上游推理过程，
    /// 只能依据显式注入的 Artifact（diff、测试报告）判断。
    #[serde(default)]
    pub tool_policy: String,
    /// 覆盖步骤声明的模型别名。留空沿用步骤自己的。
    ///
    /// 与 `tool_policy` 正交：需要「隔离审查 + 轻模型」时，把 `tool_policy`
    /// 设成 `none`（锁 builtin）同时在这里指定便宜的模型，不必二选一。
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowDelegate {
    /// 分出去的子任务。每个分支起一条**独立子运行**，挂在父运行下。
    pub branches: Vec<WorkflowDelegateBranch>,
    /// 至少要有几个分支成功，这一步才算成功。
    ///
    /// 留 0 表示「全部分支都要成功」；显式写 1 就是「任一成功即可」。
    /// 审查与测试并行时通常要全部成功，所以默认从严。
    #[serde(default)]
    pub min_succeeded: u32,
}

/// 一条被委派出去的子任务。
///
/// 分支自带 role_id 与 runtime：它要能独立执行，不能依赖父步骤的配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowDelegateBranch {
    pub id: String,
    pub title: String,
    /// 执行这一分支的角色。留空表示不指定角色，但委派语义仍然成立。
    #[serde(default)]
    pub role_id: String,
    #[serde(default)]
    pub runtime: Option<WorkflowRuntimeStep>,
    #[serde(default = "default_object")]
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowLoop {
    /// 默认轮数。执行时若没有覆盖就用它。
    pub max_iterations: u32,
    /// 轮数硬上限，由包作者声明。
    ///
    /// 与 `max_iterations` 分离是必要的：如果只有默认值，它同时也是天花板，
    /// 用户就永远无法把「这次多打磨两轮」表达出来。留 0 表示等于 `max_iterations`
    /// （老包不必改），这样既有包的行为完全不变。
    #[serde(default)]
    pub max_iterations_limit: u32,
    #[serde(default)]
    pub pause_for_feedback: bool,
    #[serde(default)]
    pub continue_when: Option<WorkflowCondition>,
    #[serde(default)]
    pub exit_when: Option<WorkflowCondition>,
    pub steps: Vec<WorkflowStep>,
}

impl WorkflowLoop {
    /// 生效的轮数上限。
    ///
    /// `max_iterations_limit` 留 0 时退回 `max_iterations`——这正是旧包的行为，
    /// 因此不必迁移既有工作流。
    pub(crate) fn iteration_limit(&self) -> u32 {
        if self.max_iterations_limit == 0 {
            self.max_iterations
        } else {
            self.max_iterations_limit.max(self.max_iterations)
        }
    }
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

/// 下拉/列表字段的可选值。
///
/// 只写字符串时值与显示文本相同（老包继续可用）；写成 `{ value, label }`
/// 时提交给能力的是 `value`，用户看到的是 `label`——展馆这种
/// 「机器标识是 szkjg、人话是随州科技馆」的取值就靠它区分，
/// 否则界面只能把机器标识直接怼给用户。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WorkflowViewOption {
    Value(String),
    Labeled {
        value: String,
        #[serde(default)]
        label: String,
        #[serde(default)]
        topics: Vec<String>,
    },
}

impl WorkflowViewOption {
    fn value(&self) -> &str {
        match self {
            WorkflowViewOption::Value(value) => value.as_str(),
            WorkflowViewOption::Labeled { value, .. } => value.as_str(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowViewOptionSource {
    #[serde(default)]
    pub(crate) kind: String,
    pub(crate) capability_id: String,
    #[serde(default)]
    pub(crate) input: Value,
    #[serde(default)]
    pub(crate) items_path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowViewBudget {
    #[serde(default)]
    max_queries: Option<u64>,
    #[serde(default)]
    interval_ms: Option<u64>,
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
    options: Vec<WorkflowViewOption>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    option_source: Option<WorkflowViewOptionSource>,
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
    // 主题字段可声明查询预算；前端据此在保存前给出预估并阻止超限组合。
    #[serde(default)]
    budget: WorkflowViewBudget,
    // span 控制栅格占位：full 表示整行，其它值按单列处理。
    #[serde(default)]
    span: String,
    /// 该字段不是步骤输入，而是一条运行期覆盖（如循环轮数）。
    ///
    /// 声明后前端把值写进 `execution_overrides`，而不是步骤输入——
    /// 否则「这次跑几轮」会被当成业务参数传给模型。
    #[serde(default, rename = "override")]
    override_target: Option<WorkflowViewFieldOverride>,
}

/// 视图字段指向的运行期覆盖目标。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowViewFieldOverride {
    #[serde(rename = "stepId")]
    step_id: String,
    key: String,
}

impl WorkflowPackage {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.icon.trim().is_empty() {
            crate::app::extension_icons::validate_icon_reference(&self.icon)?;
        }
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
        for (primary, alternatives) in &self.capability_alternatives {
            if !self.capabilities.iter().any(|item| item == primary) {
                return Err(format!(
                    "capability alternative declares an undeclared primary: {primary}"
                ));
            }
            validate_unique_text("capability alternative", alternatives)?;
            if alternatives.iter().any(|item| item == primary) {
                return Err(format!(
                    "capability alternative cannot point at its primary: {primary}"
                ));
            }
        }
        validate_unique_text("optional provider", &self.optional_providers)?;
        validate_unique_text("supported runtime", &self.supported_runtimes)?;
        // 角色键必须能被步骤的 role_id 引用到，所以用同一套字符集校验；
        // 角色里的 provider 也必须是包已声明的 runtime，否则预检会放行一个跑不起来的包。
        for (role_id, role) in &self.roles {
            validate_workflow_id(role_id)
                .map_err(|error| format!("workflow role {role_id}: {error}"))?;
            if !matches!(role.tool_policy.trim(), "" | "default" | "none") {
                return Err(format!(
                    "workflow role {role_id} tool_policy must be default or none"
                ));
            }
            let provider = role.runtime_provider.trim();
            if !provider.is_empty() && !self.accepts_provider(provider) {
                return Err(format!(
                    "workflow role {role_id} runtime_provider is not declared in supported_runtimes or optional_providers: {provider}"
                ));
            }
            validate_model_alias(&role.model)
                .map_err(|error| format!("workflow role {role_id} model: {error}"))?;
        }
        validate_dependency_refs("skill", &self.dependencies.skills)?;
        validate_dependency_refs("plugin", &self.dependencies.plugins)?;
        let declared_targets = self
            .distribution_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        validate_unique_text("distribution target", &declared_targets)?;

        if let Some(candidate) = self.candidate.as_ref() {
            validate_workflow_id(&candidate.artifact_id)?;
            if candidate.source != "git" {
                return Err("workflow candidate source must be git".to_string());
            }
        }
        let mut step_ids = HashSet::new();
        validate_step_scope(&self.steps, &mut step_ids, 0)?;
        validate_execution_contract(self, &step_ids)?;
        validate_runtime_declarations(self, &self.steps)?;
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

/// 启动表单声明的一个字段。
///
/// `guaranteed` 表示这次启动一定会带上该字段：要么表单必填，要么有默认值。
/// 预检查据此区分「Workflow 忘了声明参数」和「参数由启动表单提供」，
/// 避免把表单字段误判成缺失的能力入参。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkflowLaunchField {
    pub id: String,
    /// credential 之类的字段在运行期由 connector 解析后注入到 `target` 名下，
    /// 因此「启动时一定有值」的属性名既可能是 id，也可能是 target。
    pub target: String,
    pub guaranteed: bool,
    /// 表单默认值：预检按它推导「用户不改任何字段时这一步会不会执行」。
    pub default: Value,
    /// 凭据字段默认值是「句柄名」而不是明文，必须按 `id: { target: handle }` 的
    /// 结构并入输入，和界面上提交凭据的方式保持一致。
    pub credential: bool,
}

/// 读取声明式启动表单的字段清单；没有表单或表单不可读时返回空集。
pub(crate) fn launch_form_fields(package: &WorkflowPackage) -> Vec<WorkflowLaunchField> {
    let entry = package.ui.entry.trim();
    if entry.is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = fs::read(package.source_root.join(entry)) else {
        return Vec::new();
    };
    let Ok(view) = serde_json::from_slice::<WorkflowViewManifest>(&bytes) else {
        return Vec::new();
    };
    view.sections
        .iter()
        .flat_map(|section| section.fields.iter())
        .map(|field| match field {
            WorkflowViewField::Id(id) => WorkflowLaunchField {
                id: id.clone(),
                target: String::new(),
                guaranteed: false,
                default: Value::Null,
                credential: false,
            },
            WorkflowViewField::Definition(definition) => WorkflowLaunchField {
                guaranteed: definition.required || !definition.default.is_null(),
                id: definition.id.clone(),
                target: definition.target.clone(),
                default: definition.default.clone(),
                credential: definition.field_type == "credential",
            },
        })
        .collect()
}

/// 把声明式启动表单里的默认值补进运行输入。
///
/// 启动表单的默认值此前只服务于界面预填：从定时计划、命令行走同一条工作流时，
/// 用户不填就会退回能力自身的兜底，容易在运行中途才暴露缺参。
/// 这里统一在运行入口补齐缺失字段，让「不填也能按默认值跑」成为所有入口的一致行为。
/// 从已安装工作流的声明式视图中取字段的动态选项来源。
/// 动态来源必须由包自己声明，不能由前端临时传入任意 capability。
pub(crate) fn view_option_source(
    package: &WorkflowPackage,
    field_id: &str,
) -> Result<Option<WorkflowViewOptionSource>, Box<dyn std::error::Error>> {
    let entry = package.ui.entry.trim();
    if entry.is_empty() {
        return Ok(None);
    }
    let bytes = fs::read(package.source_root.join(entry))?;
    let view: WorkflowViewManifest = serde_json::from_slice(&bytes)?;
    for section in view.sections {
        for field in section.fields {
            if let WorkflowViewField::Definition(definition) = field {
                if definition.id == field_id {
                    return Ok(definition.option_source);
                }
            }
        }
    }
    Ok(None)
}
/// 只填补缺失或显式 `null` 的键，用户主动填的空串（例如「版本号留空按日期生成」）不会被覆盖。
pub(crate) fn with_launch_defaults(package: &WorkflowPackage, input: &Value) -> Value {
    let mut merged = match input {
        Value::Object(object) => object.clone(),
        _ => serde_json::Map::new(),
    };
    for field in launch_form_fields(package) {
        if field.default.is_null() {
            continue;
        }
        // 已显式给值的字段保持用户输入，只补缺失或显式 null 的键。
        if merged.get(&field.id).is_some_and(|value| !value.is_null()) {
            continue;
        }
        if field.credential {
            // 凭据以句柄结构提交，明文和句柄不能混在同一个键上。
            let target = if field.target.trim().is_empty() {
                "value"
            } else {
                field.target.trim()
            };
            let mut handle = serde_json::Map::new();
            handle.insert(target.to_string(), field.default.clone());
            merged.insert(field.id.clone(), Value::Object(handle));
        } else {
            merged.insert(field.id.clone(), field.default.clone());
        }
    }
    Value::Object(merged)
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

/// 校验一条工作流依赖声明：ID 非空且不重复、最低版本是规范版本号。
fn validate_dependency_refs(kind: &str, refs: &[WorkflowDependencyRef]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for item in refs {
        let id = item.id();
        if id.is_empty() {
            return Err(format!("workflow {kind} dependency must declare an id"));
        }
        validate_workflow_id(id)?;
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate workflow {kind} dependency: {id}"));
        }
        let min_version = item.min_version.trim();
        if !min_version.is_empty() {
            Version::parse(min_version).map_err(|error| {
                format!("invalid workflow {kind} dependency min_version: {error}")
            })?;
        }
    }
    Ok(())
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
        (
            "default_entrypoint",
            package.default_entrypoint.trim(),
            &package.entrypoints,
        ),
        (
            "default_exitpoint",
            package.default_exitpoint.trim(),
            &package.exits,
        ),
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
                    | "theme-list"
                    | "tag-list"
                    | "topic-groups"
                    | "json"
                    | "credential"
                    // 运行环境 / 模型选择：与 select 同形，但语义上专指
                    // 「这一步用哪个 Runtime / 哪个模型」。选项来自预检结果
                    // 或包声明，值写进 execution_overrides 而不是步骤输入。
                    | "runtime-provider"
                    | "model"
            ) {
                return Err(
                    format!("workflow view field {} type is invalid", definition.id).into(),
                );
            }
            if matches!(
                definition.field_type.as_str(),
                "select" | "runtime-provider" | "model"
            ) && definition.options.is_empty()
                && definition.option_source.is_none()
            {
                return Err(format!(
                    "workflow view select field {} requires options or option_source",
                    definition.id
                )
                .into());
            }
            if matches!(
                definition.field_type.as_str(),
                "theme-list" | "topic-groups"
            ) && definition.options.is_empty()
                && definition.option_source.is_none()
            {
                return Err(format!(
                    "workflow view theme-list field {} requires options or option_source",
                    definition.id
                )
                .into());
            }
            let mut alias_values = HashSet::new();
            for alias in &definition.aliases {
                validate_workflow_id(alias).map_err(std::io::Error::other)?;
                if alias == &definition.id || !alias_values.insert(alias.as_str()) {
                    return Err(format!(
                        "workflow view field {} has an invalid or duplicate alias: {}",
                        definition.id, alias
                    )
                    .into());
                }
            }
            if let Some(source) = definition.option_source.as_ref() {
                if !matches!(
                    source.kind.as_str(),
                    "" | "plugin_capability" | "capability"
                ) {
                    return Err(format!(
                        "workflow view field {} option_source kind is invalid",
                        definition.id
                    )
                    .into());
                }
                if source.capability_id.trim().is_empty() {
                    return Err(format!(
                        "workflow view field {} option_source capability_id is required",
                        definition.id
                    )
                    .into());
                }
            }
            // 选项值重复或为空会让下拉出现两个「看起来一样、提交后不一样」的项，
            // 属于包写错了，直接在安装校验里挡掉。
            let mut option_values = HashSet::new();
            for option in &definition.options {
                let value = option.value().trim();
                if value.is_empty() {
                    return Err(format!(
                        "workflow view field {} has an empty option value",
                        definition.id
                    )
                    .into());
                }
                if !option_values.insert(value.to_string()) {
                    return Err(format!(
                        "workflow view field {} has a duplicate option value: {}",
                        definition.id, value
                    )
                    .into());
                }
            }
            if definition.field_type == "credential" && definition.target.trim().is_empty() {
                return Err(format!(
                    "workflow view credential field {} requires a target",
                    definition.id
                )
                .into());
            }
            if !matches!(definition.picker.as_str(), "" | "directory") {
                return Err(
                    format!("workflow view field {} picker is invalid", definition.id).into(),
                );
            }
            if !matches!(definition.span.as_str(), "" | "full") {
                return Err(
                    format!("workflow view field {} span is invalid", definition.id).into(),
                );
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
        // 角色 ID 与步骤 ID 用同一套字符集：它会被写进运行台账并按角色聚合，
        // 出现空格或中文会让前端徽标和统计分组各说一套。
        if !step.role_id.trim().is_empty() {
            validate_workflow_id(step.role_id.trim())
                .map_err(|error| format!("workflow step {} role_id: {error}", step.id))?;
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
                if step.delegate.is_some() {
                    return Err(format!(
                        "workflow loop step {} cannot declare delegate",
                        step.id
                    ));
                }
                validate_loop_step(step, global_step_ids, depth + 1)?;
            }
            "delegate" => {
                if !step.capability_id.trim().is_empty() || step.runtime.is_some() {
                    return Err(format!(
                        "workflow delegate step {} cannot declare capability_id or runtime",
                        step.id
                    ));
                }
                if step.loop_config.is_some() {
                    return Err(format!(
                        "workflow delegate step {} cannot declare loop",
                        step.id
                    ));
                }
                validate_delegate_step(step, depth)?;
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
            "delegate" => Ok("delegate"),
            "manual" => Ok("manual"),
            "provider_defined" => Ok("provider_defined"),
            value => Err(format!(
                "workflow step {} kind is invalid: {value}",
                step.id
            )),
        };
    }
    if step.delegate.is_some() {
        return Ok("delegate");
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
    if runtime.timeout_seconds > 86_400 {
        return Err(format!(
            "workflow runtime step {} timeout_seconds must not exceed 86400",
            step.id
        ));
    }
    validate_model_alias(&runtime.model)
        .map_err(|error| format!("workflow runtime step {} model: {error}", step.id))?;
    // 声明了模型就必须写明确 provider：auto 可能解析到不认这个命名空间的 Runtime。
    model_alias_matches_provider(&runtime.provider, &runtime.model)
        .map_err(|error| format!("workflow runtime step {}: {error}", step.id))?;
    for alias in &runtime.model_fallbacks {
        model_alias_matches_provider(&runtime.provider, alias)
            .map_err(|error| format!("workflow runtime step {}: {error}", step.id))?;
    }
    let mut seen_models = HashSet::new();
    for alias in &runtime.model_fallbacks {
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(format!(
                "workflow runtime step {} model_fallbacks contains an empty alias",
                step.id
            ));
        }
        if !seen_models.insert(alias.to_string()) {
            return Err(format!(
                "workflow runtime step {} model_fallbacks contains a duplicate alias: {alias}",
                step.id
            ));
        }
        validate_model_alias(alias).map_err(|error| {
            format!(
                "workflow runtime step {} model_fallbacks: {error}",
                step.id
            )
        })?;
    }
    // 降级序列必须与主模型同一命名空间：himind 别名换到 acp 客户端上去
    // 没有意义，而且会让「主模型不可用」的判定变得不可预测。
    if !runtime.model.trim().is_empty() {
        let primary = model_alias_namespace(&runtime.model);
        for alias in &runtime.model_fallbacks {
            if model_alias_namespace(alias) != primary {
                return Err(format!(
                    "workflow runtime step {} model_fallbacks must stay in the same namespace as model",
                    step.id
                ));
            }
        }
    }
    Ok(())
}

/// 每个 Runtime 步骤的 provider 都必须在本包里声明过。
///
/// 「声明过」= `supported_runtimes`（本包主推）∪ `optional_providers`（本包接受、
/// 由用户按需选择）。两侧必须同一判据：预检按这个集合生成候选，运行期覆盖也按
/// 这个集合放行；只要有一处更严格，就会出现「预检说能换、运行时不认」。
fn validate_runtime_declarations(
    package: &WorkflowPackage,
    steps: &[WorkflowStep],
) -> Result<(), String> {
    fn walk(
        package: &WorkflowPackage,
        steps: &[WorkflowStep],
    ) -> Result<(), String> {
        for step in steps {
            if let Some(runtime) = step.runtime.as_ref() {
                let provider = runtime.provider.trim();
                if provider != "auto" && !package.accepts_provider(provider) {
                    return Err(format!(
                        "workflow runtime step {} uses undeclared provider {}",
                        step.id, provider
                    ));
                }
            }
            if let Some(loop_config) = step.loop_config.as_ref() {
                walk(package, &loop_config.steps)?;
            }
            if let Some(delegate) = step.delegate.as_ref() {
                for branch in &delegate.branches {
                    let Some(runtime) = branch.runtime.as_ref() else {
                        continue;
                    };
                    let provider = runtime.provider.trim();
                    if provider != "auto" && !package.accepts_provider(provider) {
                        return Err(format!(
                            "workflow delegate branch {} uses undeclared provider {}",
                            branch.id, provider
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    walk(package, steps)
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
    // 上限同样限制在 1..=20，且不得小于默认值——否则「默认轮数」跑不满自己声明的上限，
    // 用户看到的两个数字会自相矛盾。
    if loop_config.max_iterations_limit != 0
        && !(loop_config.max_iterations..=20).contains(&loop_config.max_iterations_limit)
    {
        return Err(format!(
            "workflow loop step {} max_iterations_limit must be between max_iterations and 20",
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

/// 委派步骤的校验。
///
/// 与 loop 的差别：loop 是一组步骤反复执行，delegate 是一组**独立分支**各起一条子运行。
/// 分支必须自带 id/title 和独立 runtime，否则子运行起不来；分支内不允许再嵌套委派或审批，
/// 避免出现「子运行里还要再请示父运行」的循环依赖。
fn validate_delegate_step(step: &WorkflowStep, depth: usize) -> Result<(), String> {
    let delegate = step
        .delegate
        .as_ref()
        .ok_or_else(|| format!("workflow delegate step {} requires delegate", step.id))?;
    if depth > 4 {
        return Err("workflow delegate nesting exceeds 4 levels".to_string());
    }
    if delegate.branches.len() < 2 {
        return Err(format!(
            "workflow delegate step {} requires at least two branches; a single branch is just a step",
            step.id
        ));
    }
    if delegate.branches.len() > 8 {
        return Err(format!(
            "workflow delegate step {} cannot fan out to more than 8 branches",
            step.id
        ));
    }
    let mut ids = HashSet::new();
    for branch in &delegate.branches {
        validate_workflow_id(&branch.id)
            .map_err(|error| format!("workflow delegate branch {}: {error}", branch.id))?;
        if !ids.insert(branch.id.as_str()) {
            return Err(format!(
                "duplicate workflow delegate branch id: {}",
                branch.id
            ));
        }
        if branch.title.trim().is_empty() {
            return Err(format!(
                "workflow delegate branch {} title is required",
                branch.id
            ));
        }
        let runtime = branch.runtime.as_ref().ok_or_else(|| {
            format!(
                "workflow delegate branch {} requires runtime; a branch must be runnable on its own",
                branch.id
            )
        })?;
        if runtime.provider.trim().is_empty() {
            return Err(format!(
                "workflow delegate branch {} runtime provider is required",
                branch.id
            ));
        }
        if runtime.prompt.trim().is_empty() {
            return Err(format!(
                "workflow delegate branch {} runtime prompt is required",
                branch.id
            ));
        }
        if !matches!(runtime.tool_policy.trim(), "" | "default" | "none") {
            return Err(format!(
                "workflow delegate branch {} tool_policy must be default or none",
                branch.id
            ));
        }
        validate_model_alias(&runtime.model).map_err(|error| {
            format!("workflow delegate branch {} model: {error}", branch.id)
        })?;
        model_alias_matches_provider(&runtime.provider, &runtime.model).map_err(|error| {
            format!("workflow delegate branch {}: {error}", branch.id)
        })?;
        for alias in &runtime.model_fallbacks {
            if alias.trim().is_empty() {
                return Err(format!(
                    "workflow delegate branch {} model_fallbacks contains an empty alias",
                    branch.id
                ));
            }
            validate_model_alias(alias).map_err(|error| {
                format!(
                    "workflow delegate branch {} model_fallbacks: {error}",
                    branch.id
                )
            })?;
        }
        if !branch.role_id.trim().is_empty() {
            validate_workflow_id(branch.role_id.trim()).map_err(|error| {
                format!("workflow delegate branch {} role_id: {error}", branch.id)
            })?;
        }
        if !branch.input.is_object() {
            return Err(format!(
                "workflow delegate branch {} input must be an object",
                branch.id
            ));
        }
    }
    if delegate.min_succeeded as usize > delegate.branches.len() {
        return Err(format!(
            "workflow delegate step {} min_succeeded exceeds branch count",
            step.id
        ));
    }
    Ok(())
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
    fn workflow_view_accepts_theme_metadata_and_query_budget() {
        let view: WorkflowViewManifest = serde_json::from_str(
            r#"{
                "schema_version": "workflow_view.v1",
                "title": "开源热榜日报",
                "sections": [{
                    "id": "scope",
                    "title": "关注范围",
                    "fields": [{
                        "id": "domains",
                        "label": "关注主题",
                        "type": "theme-list",
                        "default": ["ai-video"],
                        "options": [{
                            "value": "ai-video",
                            "label": "AI 视频生成",
                            "topics": ["text-to-video", "video-generation"]
                        }],
                        "budget": {"max_queries": 24, "interval_ms": 7000}
                    }]
                }]
            }"#,
        )
        .unwrap();
        let WorkflowViewField::Definition(field) = &view.sections[0].fields[0] else {
            panic!("expected structured field");
        };
        assert_eq!(field.field_type, "theme-list");
        assert_eq!(field.budget.max_queries, Some(24));
        assert_eq!(field.budget.interval_ms, Some(7000));
        match &field.options[0] {
            WorkflowViewOption::Labeled { topics, .. } => {
                assert_eq!(topics, &["text-to-video", "video-generation"]);
            }
            WorkflowViewOption::Value(_) => panic!("expected labeled theme option"),
        }
    }

    #[test]
    fn loads_wechat_experience_upload_package() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let package = load_from_directory(&root).unwrap();
        assert_eq!(package.id, "com.himind.workflow.wechat-experience-upload");
        assert!(package.steps.iter().any(|step| step.id == "WX-BUILD"));
        assert!(package.steps.iter().any(|step| step.id == "WX-CANDIDATE"));
        assert!(package
            .capabilities
            .contains(&"workflow.candidate.freeze".to_string()));
        // 该工作流只做构建与上传，不再内嵌 AI 开发 Loop。
        assert!(package.steps.iter().all(|step| step.kind != "loop"));
        assert!(package.supported_runtimes.is_empty());
    }

    #[test]
    fn workflow_dependency_declarations_accept_string_and_object_forms() {
        // 字符串 = 本分发内的必需依赖；对象可以补 required 与 min_version。
        let dependencies: WorkflowDependencies = serde_json::from_str(
            r#"{
                "skills": [
                    "develop-himind-skills",
                    {"skill_id": "wechatide-skill", "required": false, "min_version": "0.4.0"}
                ],
                "plugins": [
                    {"id": "com.himind.software-distribution", "min_version": "1.2.0"},
                    "com.himind.wechat-miniprogram-tools"
                ],
                "connectors": [],
                "runtimes": []
            }"#,
        )
        .unwrap();
        assert_eq!(dependencies.skills[0].id(), "develop-himind-skills");
        assert!(dependencies.skills[0].required);
        assert_eq!(dependencies.skills[1].id(), "wechatide-skill");
        assert!(!dependencies.skills[1].required);
        assert_eq!(dependencies.skills[1].min_version, "0.4.0");
        assert_eq!(
            dependencies.plugins[0].id(),
            "com.himind.software-distribution"
        );
        assert_eq!(dependencies.plugins[0].min_version, "1.2.0");
        assert!(dependencies.plugins[1].required);

        // 只声明 ID 的依赖写回字符串，带约束的才展开成对象。
        let encoded = serde_json::to_value(&dependencies).unwrap();
        assert_eq!(
            encoded["skills"][0],
            Value::String("develop-himind-skills".to_string())
        );
        assert_eq!(encoded["skills"][1]["required"], Value::Bool(false));
        assert_eq!(
            encoded["skills"][1]["min_version"],
            Value::String("0.4.0".to_string())
        );
        assert_eq!(
            encoded["plugins"][0]["id"],
            Value::String("com.himind.software-distribution".to_string())
        );
    }

    #[test]
    fn workflow_dependency_declarations_reject_invalid_entries() {
        let duplicate: WorkflowDependencies = serde_json::from_str(
            r#"{"skills": ["same-skill", {"id": "same-skill"}], "plugins": []}"#,
        )
        .unwrap();
        assert!(validate_dependency_refs("skill", &duplicate.skills)
            .unwrap_err()
            .contains("duplicate"));

        let bad_version: WorkflowDependencies = serde_json::from_str(
            r#"{"skills": [{"id": "some-skill", "min_version": "0.4"}], "plugins": []}"#,
        )
        .unwrap();
        assert!(validate_dependency_refs("skill", &bad_version.skills)
            .unwrap_err()
            .contains("min_version"));

        assert!(serde_json::from_str::<WorkflowDependencies>(r#"{"skills": [42]}"#).is_err());
        assert!(serde_json::from_str::<WorkflowDependencies>(
            r#"{"skills": [{"required": false}]}"#
        )
        .is_err());
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
    fn workflow_icon_is_optional_and_package_local() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        package.icon = "assets/icon.webp".to_string();
        assert!(package.validate().is_ok());
        package.icon = "../icon.webp".to_string();
        assert!(package.validate().is_err());
    }

    #[test]
    fn workflow_package_v1_current_product_remains_compatible() {
        let package: WorkflowPackage = serde_json::from_str(include_str!(
            "../../workflows/wechat-experience-upload/workflow.json"
        ))
        .unwrap();
        package.validate().unwrap();
        assert!(package.steps.iter().all(|step| step.kind != "loop"));
        assert!(package.steps.iter().any(|step| step.id == "WX-BUILD"));
        assert!(package
            .steps
            .iter()
            .any(|step| step.candidate_action == "freeze"));
    }

    #[test]
    fn shipped_engine_build_workflow_waits_for_the_build() {
        // 引擎构建是长任务：步骤必须显式要求阻塞等待，否则运行会「秒完成」，
        // 用户看到成功而构建还在后台跑。
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("engine-build");
        let package = load_from_directory(&root).unwrap();
        assert_eq!(package.id, "com.himind.workflow.engine-build");
        let step = package
            .steps
            .iter()
            .find(|step| step.id == "ENGINE-BUILD")
            .expect("engine build step");
        assert_eq!(step.capability_id, "workspace.exhibit.build");
        assert_eq!(step.execution_mode, "long_running");
        assert_eq!(step.input["wait"], serde_json::json!(true));
        assert_eq!(package.ui.mode, "declarative");
    }

    #[test]
    fn segmented_workflow_requires_declared_entry_and_exit_steps() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        package.execution_policy = "segmented".to_string();
        package.default_entrypoint = "build".to_string();
        package.default_exitpoint = "checkpoint".to_string();
        package.entrypoints = vec![WorkflowEndpoint {
            id: "build".to_string(),
            at_step: "WX-BUILD".to_string(),
            label: "构建".to_string(),
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
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        // 该 fixture 现在按分段执行发布，这条用例只验证 strict 契约本身。
        package.execution_policy = "strict".to_string();
        package.exits = Vec::new();
        package.entrypoints = vec![WorkflowEndpoint {
            id: "build".to_string(),
            at_step: "WX-BUILD".to_string(),
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
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let first = package.steps[0].id.clone();
        let second = package.steps[1].id.clone();
        package.steps[0].depends_on.push(second);
        package.steps[1].depends_on.push(first);
        assert!(package.validate().unwrap_err().contains("cycle"));
    }

    #[test]
    fn role_id_is_optional_but_validated() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        // 不声明角色是合法的：老包没有这个字段，读出来就是空串。
        assert!(package.steps.iter().all(|step| step.role_id.is_empty()));
        package.validate().unwrap();

        package.steps[0].role_id = "software.implementer".to_string();
        package.validate().unwrap();

        // 角色 ID 会被写进台账并按角色聚合，带空格或中文会让统计分组各说一套。
        package.steps[0].role_id = "software implementer".to_string();
        let error = package.validate().unwrap_err();
        assert!(error.contains("role_id"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_custom_ui_path_escape() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        package.ui.mode = "custom".to_string();
        package.ui.entry = "../outside.html".to_string();
        assert!(package.validate().unwrap_err().contains("escapes"));
    }

    #[test]
    fn rejects_missing_asset() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
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
            .join("wechat-experience-upload");
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
    fn launch_defaults_fill_missing_input_for_every_entrypoint() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let package = load_from_directory(&root).unwrap();

        // 定时计划或命令行只给了部分参数时，其余按启动表单默认值补齐。
        let merged = with_launch_defaults(&package, &serde_json::json!({ "version": "" }));
        assert_eq!(merged["venue"], "hdcybwg");
        assert_eq!(merged["environment"], "development");
        assert_eq!(merged["workspace_root"], "F:\\WebProjects\\kerun_user");
        // 凭据默认值是句柄，必须按界面同样的结构并入，不能被当作明文塞进 target 键。
        assert_eq!(
            merged["credential_handles"]["private_key_path"],
            "wechat-upload-private-key"
        );
        // 显式空串代表「版本号留空，按日期生成」，不能被默认值覆盖。
        assert_eq!(merged["version"], "");

        // 显式传值永远优先于默认值。
        let merged = with_launch_defaults(&package, &serde_json::json!({ "venue": "szkjg" }));
        assert_eq!(merged["venue"], "szkjg");

        // 非对象输入退化为「全部使用默认值」，而不是直接失败。
        let merged = with_launch_defaults(&package, &Value::Null);
        assert_eq!(merged["venue"], "hdcybwg");
    }

    #[test]
    fn accepts_runtime_step_and_condition_contracts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "runtime".to_string();
        step.capability_id.clear();
        // 首步是普通能力步骤，转成 Runtime Step 前先确认它没有 loop 声明。
        step.loop_config = None;
        // 声明式 Runtime Provider 必须先在本包声明，否则 validate 直接拒绝。
        package
            .supported_runtimes
            .push("personal.codex".to_string());
        step.runtime = Some(WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Implement the next change from the current feedback.".to_string(),
            workspace_path: "input.project_root".to_string(),
            result_schema: String::new(),
            allow_network: None,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            model: String::new(),
            model_fallbacks: Vec::new(),
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
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "runtime".to_string();
        step.capability_id.clear();
        package
            .supported_runtimes
            .push("personal.codex".to_string());
        step.runtime = Some(WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Run one bounded iteration.".to_string(),
            workspace_path: "input.workspace_root".to_string(),
            result_schema: String::new(),
            allow_network: None,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            model: String::new(),
            model_fallbacks: Vec::new(),
            timeout_seconds: 86_401,
        });
        assert!(package
            .validate()
            .unwrap_err()
            .contains("timeout_seconds must not exceed 86400"));
    }

    /// 模型别名必须带命名空间：裸 id 无法决定走哪条路由，应当在契约阶段拒绝。
    #[test]
    fn model_alias_requires_a_namespace_prefix() {
        assert!(validate_model_alias("").is_ok(), "留空表示沿用 Runtime 配置");
        assert!(validate_model_alias("himind:svc-a:deepseek-chat").is_ok());
        assert!(validate_model_alias("acp:claude:claude-sonnet-4-5").is_ok());
        assert!(validate_model_alias("acp:acp.claude:claude-sonnet-4-5").is_ok());
        assert_eq!(model_alias_namespace("himind:svc-a:x"), Some("himind"));
        assert_eq!(model_alias_namespace("acp:claude:x"), Some("acp"));
        assert_eq!(model_alias_namespace("deepseek-chat"), None);

        let bare = validate_model_alias("deepseek-chat").unwrap_err();
        assert!(bare.contains("himind:"), "{bare}");
        assert!(validate_model_alias("himind:svc-a").is_err(), "缺 model 段");
        assert!(validate_model_alias("himind::m").is_err(), "service 为空");
        assert!(validate_model_alias("acp:claude:").is_err(), "model 为空");
        assert!(validate_model_alias("himind:svc:a b").is_err(), "含空白");
    }

    /// 步骤声明了非法模型别名时整包校验必须失败，而不是留到运行时。
    #[test]
    fn rejects_runtime_step_with_invalid_model_alias() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "runtime".to_string();
        step.capability_id.clear();
        package
            .supported_runtimes
            .extend(["personal.codex".to_string(), "himind.builtin".to_string()]);
        step.runtime = Some(WorkflowRuntimeStep {
            provider: "personal.codex".to_string(),
            prompt: "Do work.".to_string(),
            workspace_path: "input.workspace_root".to_string(),
            result_schema: String::new(),
            allow_network: None,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            model: "deepseek-chat".to_string(),
            model_fallbacks: Vec::new(),
            timeout_seconds: 60,
        });
        let error = package.validate().unwrap_err();
        assert!(error.contains("model alias must start with"), "{error}");

        // 换成合法别名后通过——但 `himind:` 别名只归内置 Runtime，所以 provider 一起换。
        package.steps[0].runtime.as_mut().unwrap().provider = "himind.builtin".to_string();
        package.steps[0].runtime.as_mut().unwrap().model = "himind:svc-a:deepseek-chat".to_string();
        package.validate().unwrap();

        // `himind:` 别名配外部 CLI 必须被拒绝：CLI 没有这条路由。
        package.steps[0].runtime.as_mut().unwrap().provider = "personal.codex".to_string();
        let error = package.validate().unwrap_err();
        assert!(error.contains("`himind:` aliases are only resolved"), "{error}");
    }

    /// 降级序列不能跨命名空间：himind 主模型降级到 acp 客户端没有语义。
    ///
    /// 现在的拒绝可能来自两处（provider 不认该命名空间 / 降级序列与主模型不同源），
    /// 两者都是对的；这里只断言「被拒绝且指向 acp 别名」，不锁具体措辞。
    #[test]
    fn rejects_model_fallbacks_across_namespaces() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "runtime".to_string();
        step.capability_id.clear();
        package
            .supported_runtimes
            .extend(["personal.codex".to_string(), "himind.builtin".to_string()]);
        // 主模型与 provider 匹配，只有降级序列跨了命名空间：
        // provider 用内置，这样失败点唯一落在「降级序列与主模型不同源」上。
        step.runtime = Some(WorkflowRuntimeStep {
            provider: "himind.builtin".to_string(),
            prompt: "Do work.".to_string(),
            workspace_path: "input.workspace_root".to_string(),
            result_schema: String::new(),
            allow_network: None,
            input_artifacts: Vec::new(),
            tool_policy: String::new(),
            model: "himind:svc-a:deepseek-chat".to_string(),
            model_fallbacks: vec!["acp:claude:claude-sonnet-4-5".to_string()],
            timeout_seconds: 60,
        });
        let error = package.validate().unwrap_err();
        assert!(
            error.contains("same namespace") || error.contains("acp:"),
            "{error}"
        );

        // 主模型与降级序列同源、且 provider 认这个命名空间时通过。
        // 注意：`himind:` 别名只归内置 Runtime，所以 provider 也要一起换。
        let runtime = package.steps[0].runtime.as_mut().unwrap();
        runtime.provider = "himind.builtin".to_string();
        runtime.model_fallbacks = vec!["himind:svc-b:deepseek-reasoner".to_string()];
        package.validate().unwrap_or_else(|error| {
            panic!("合法的 himind 主模型 + 同源降级应通过，实际: {error}")
        });
    }

    /// 角色上的模型别名同样受校验：它是步骤模型的覆盖来源。
    #[test]
    fn rejects_role_with_invalid_model_alias() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("ui-craft-team");
        let mut package = load_from_directory(&root).unwrap();
        let role = package
            .roles
            .get_mut("ui.guard")
            .expect("shipped package declares ui.guard");
        role.model = "claude-sonnet".to_string();
        let error = package.validate().unwrap_err();
        assert!(error.contains("workflow role ui.guard model"), "{error}");
    }

    #[test]
    fn accepts_bounded_development_loop() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "loop".to_string();
        step.capability_id.clear();
        step.loop_config = Some(Box::new(WorkflowLoop {
            max_iterations: 4,
            max_iterations_limit: 0,
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
                delegate: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: default_object(),
                execution_mode: "provider_defined".to_string(),
                role_id: String::new(),
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
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        let step = &mut package.steps[0];
        step.kind = "loop".to_string();
        step.capability_id.clear();
        step.loop_config = Some(Box::new(WorkflowLoop {
            max_iterations: 4,
            max_iterations_limit: 0,
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
                delegate: None,
                when: None,
                fail_when: None,
                candidate_action: String::new(),
                input: default_object(),
                execution_mode: "sync".to_string(),
                role_id: String::new(),
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
            .join("wechat-experience-upload");
        let mut package = load_from_directory(&root).unwrap();
        package.candidate = Some(WorkflowCandidatePolicy {
            required: true,
            artifact_id: "candidate".to_string(),
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
    #[test]
    fn execution_override_can_only_tighten_risk_boundaries() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("software-delivery");
        let package = load_from_directory(&root).unwrap();

        // 收紧：给实现步骤加审批闸门。
        let tightened = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": {
                    "IMPLEMENT": { "approval_required": true }
                }
            }),
        )
        .unwrap();
        let step = tightened.steps.iter().find(|s| s.id == "IMPLEMENT").unwrap();
        assert!(step.approval_required);

        // 放宽：撤掉包里已有的闸门必须被拒绝，而不是静默忽略。
        let error = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "CONFIRM": { "approval_required": false } }
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot remove the approval gate"));
    }

    /// 运行期换 Runtime 只能在包声明过的 provider 内进行。
    ///
    /// 这是「不重新打包也能换外部 AI 客户端」的唯一入口；白名单之外必须拒绝，
    /// 否则运行参数就成了绕过包声明的后门。
    #[test]
    fn runtime_provider_override_respects_declared_providers() {
        let mut package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.provider-override",
            "version": "1.0.0",
            "name": "Provider Override",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "supported_runtimes": ["himind.builtin"],
            "optional_providers": ["acp.claude"],
            "steps": [{
                "id": "WORK",
                "title": "Work",
                "kind": "runtime",
                "execution_mode": "sync",
                "risk_level": "local_write",
                "runtime": {
                    "provider": "himind.builtin",
                    "prompt": "do work",
                    "tool_policy": "default"
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" }
        }))
        .unwrap();
        package.validate().unwrap();
        assert!(package.accepts_provider("himind.builtin"));
        assert!(package.accepts_provider("acp.claude"), "可选清单也算接受");
        assert!(!package.accepts_provider("acp.codex"));

        // 主清单 -> 可选清单：允许。
        let switched = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "runtime_provider": "acp.claude" } }
            }),
        )
        .unwrap();
        let step = switched.steps.iter().find(|s| s.id == "WORK").unwrap();
        assert_eq!(step.runtime.as_ref().unwrap().provider, "acp.claude");

        // 未声明的 provider：拒绝。
        let error = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "runtime_provider": "acp.codex" } }
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not declared by the package"), "{error}");

        // 空值：拒绝（不能用来清空 provider）。
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "runtime_provider": "  " } }
            }),
        )
        .is_err());

        // 非 runtime 步骤：拒绝。
        package.steps[0].kind = "manual".to_string();
        package.steps[0].runtime = None;
        package.steps[0].approval_required = true;
        package.steps[0].risk_level = "R3".to_string();
        let error = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "runtime_provider": "acp.claude" } }
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires a runtime step"), "{error}");
    }

    /// 启动时换模型：同命名空间允许，跨命名空间或裸 id 拒绝；
    /// 换 provider 后同步换模型也必须一致。
    #[test]
    fn model_override_follows_the_declared_provider_namespace() {
        let package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.model-override",
            "version": "1.0.0",
            "name": "Model Override",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "supported_runtimes": ["himind.builtin"],
            "optional_providers": ["acp.claude"],
            "steps": [{
                "id": "WORK",
                "title": "Work",
                "kind": "runtime",
                "execution_mode": "sync",
                "risk_level": "local_write",
                "runtime": {
                    "provider": "himind.builtin",
                    "prompt": "do work",
                    "model": "himind:svc-a:deepseek-chat",
                    "model_fallbacks": ["himind:svc-a:deepseek-reasoner"]
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" }
        }))
        .unwrap();
        package.validate().unwrap();

        // 同命名空间换模型：允许，且旧降级序列被清掉（它属于旧主模型）。
        let switched = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "model": "himind:svc-b:qwen-max" } }
            }),
        )
        .unwrap();
        let runtime = switched.steps[0].runtime.as_ref().unwrap();
        assert_eq!(runtime.model, "himind:svc-b:qwen-max");
        assert!(runtime.model_fallbacks.is_empty());

        // 跨命名空间：拒绝。
        let error = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "model": "acp:claude:claude-sonnet-4-5" } }
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot execute `acp:`"), "{error}");

        // 裸 id：拒绝。
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "model": "deepseek-chat" } }
            }),
        )
        .is_err());

        // 先换 provider 再换模型：两个覆盖一起提交时按 provider 先行，合法。
        let both = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": {
                    "WORK": {
                        "runtime_provider": "acp.claude",
                        "model": "acp:claude:claude-sonnet-4-5"
                    }
                }
            }),
        )
        .unwrap();
        let runtime = both.steps[0].runtime.as_ref().unwrap();
        assert_eq!(runtime.provider, "acp.claude");
        assert_eq!(runtime.model, "acp:claude:claude-sonnet-4-5");

        // 空串表示回到 Runtime 默认：允许，且降级序列一起清空。
        let cleared = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "model": "" } }
            }),
        )
        .unwrap();
        let runtime = cleared.steps[0].runtime.as_ref().unwrap();
        assert!(runtime.model.is_empty());
        assert!(runtime.model_fallbacks.is_empty());

        // 非 runtime 步骤：拒绝。
        let mut manual = package.clone();
        manual.steps[0].kind = "manual".to_string();
        manual.steps[0].runtime = None;
        manual.steps[0].approval_required = true;
        manual.steps[0].risk_level = "R3".to_string();
        let error = apply_execution_overrides(
            &manual,
            &serde_json::json!({
                "execution_overrides": { "WORK": { "model": "himind:svc-a:deepseek-chat" } }
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires a runtime step"), "{error}");
    }

    /// 声明了 model 就必须写死 provider：auto 可能挑到不认这个命名空间的 Runtime。
    #[test]
    fn declaring_model_requires_an_explicit_provider() {
        let mut package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.model-needs-provider",
            "version": "1.0.0",
            "name": "Model Needs Provider",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "supported_runtimes": ["himind.builtin", "acp.claude"],
            "steps": [{
                "id": "WORK",
                "title": "Work",
                "kind": "runtime",
                "execution_mode": "sync",
                "risk_level": "local_write",
                "runtime": {
                    "provider": "auto",
                    "prompt": "do work",
                    "model": "himind:svc-a:deepseek-chat"
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" }
        }))
        .unwrap();
        let error = package.validate().unwrap_err();
        assert!(error.contains("not `auto`"), "{error}");

        package.steps[0].runtime.as_mut().unwrap().provider = "acp.claude".to_string();
        let error = package.validate().unwrap_err();
        assert!(error.contains("cannot execute `himind:`"), "{error}");

        package.steps[0].runtime.as_mut().unwrap().provider = "himind.builtin".to_string();
        package.validate().unwrap();
    }

    /// 覆盖测试自带一个最小包：直接依赖某个已发布包的形状，会让「包改了」误伤
    /// 「覆盖逻辑坏了」这两件事分不开。
    fn loop_override_package(rounds: u32) -> WorkflowPackage {
        let mut package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.override-loop",
            "version": "1.0.0",
            "name": "Override Loop",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "steps": [{
                "id": "VERIFY",
                "title": "Verify",
                "kind": "loop",
                "execution_mode": "long_running",
                "risk_level": "local_write",
                "loop": {
                    "max_iterations": rounds,
                    "exit_when": { "operator": "equals", "path": "loops.VERIFY.latest.passed", "value": true },
                    "steps": [{
                        "id": "BODY",
                        "title": "Body",
                        "kind": "capability",
                        "capability_id": "test.body",
                        "execution_mode": "sync",
                        "risk_level": "read_only"
                    }]
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" },
            "capabilities": ["test.body"]
        }))
        .unwrap();
        package.source_root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        package.validate().unwrap();
        package
    }

    #[test]
    fn execution_override_can_lower_loop_rounds_but_not_raise_them() {
        let package = loop_override_package(3);

        let lowered = apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "VERIFY": { "max_iterations": 1 } }
            }),
        )
        .unwrap();
        let rounds = lowered
            .steps
            .iter()
            .find(|s| s.id == "VERIFY")
            .and_then(|s| s.loop_config.as_ref())
            .unwrap()
            .max_iterations;
        assert_eq!(rounds, 1);

        // 调大轮数会突破包里声明的重试预算，必须拒绝。
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({
                "execution_overrides": { "VERIFY": { "max_iterations": 8 } }
            }),
        )
        .is_err());
    }

    /// 带显式上限的循环包：默认 3 轮，允许上调到 6。
    fn loop_override_package_with_limit(default: u32, limit: u32) -> WorkflowPackage {
        let mut package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.override-loop-limit",
            "version": "1.0.0",
            "name": "Override Loop Limit",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "steps": [{
                "id": "VERIFY",
                "title": "Verify",
                "kind": "loop",
                "execution_mode": "long_running",
                "risk_level": "local_write",
                "loop": {
                    "max_iterations": default,
                    "max_iterations_limit": limit,
                    "exit_when": { "operator": "equals", "path": "loops.VERIFY.latest.passed", "value": true },
                    "steps": [{
                        "id": "BODY",
                        "title": "Body",
                        "kind": "capability",
                        "capability_id": "test.body",
                        "execution_mode": "sync",
                        "risk_level": "read_only"
                    }]
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" },
            "capabilities": ["test.body"]
        }))
        .unwrap();
        package.source_root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        package.validate().unwrap();
        package
    }

    #[test]
    fn execution_override_can_raise_rounds_up_to_the_declared_limit() {
        let package = loop_override_package_with_limit(3, 6);
        // 上调：默认 3，上限 6，用户可以要求 5。
        let raised = apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "VERIFY": { "max_iterations": 5 } } }),
        )
        .unwrap();
        let rounds = raised
            .steps
            .iter()
            .find(|s| s.id == "VERIFY")
            .and_then(|s| s.loop_config.as_ref())
            .unwrap()
            .max_iterations;
        assert_eq!(rounds, 5, "默认值与上限分离后，必须能上调");
    }

    #[test]
    fn execution_override_rejects_rounds_above_the_declared_limit() {
        let package = loop_override_package_with_limit(3, 6);
        let error = apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "VERIFY": { "max_iterations": 7 } } }),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("exceeds the declared limit"),
            "超限必须给出明确原因，实际：{error}"
        );
    }

    #[test]
    fn legacy_loop_without_limit_keeps_lowering_only() {
        // 老包没有 max_iterations_limit：上限等于默认值，行为与改动前一致。
        let package = loop_override_package(3);
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "VERIFY": { "max_iterations": 4 } } }),
        )
        .is_err(), "老包不能上调轮数");
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "VERIFY": { "max_iterations": 2 } } }),
        )
        .is_ok(), "老包仍可下调轮数");
    }

    #[test]
    fn loop_limit_must_not_be_below_default() {
        let error = serde_json::from_value::<WorkflowPackage>(serde_json::json!({
            "schema_version": WORKFLOW_PACKAGE_SCHEMA_VERSION,
            "id": "test.bad-limit",
            "version": "1.0.0",
            "name": "Bad Limit",
            "min_agent_version": "0.3.90",
            "execution_policy": "strict",
            "steps": [{
                "id": "VERIFY",
                "title": "Verify",
                "kind": "loop",
                "execution_mode": "long_running",
                "risk_level": "local_write",
                "loop": {
                    "max_iterations": 5,
                    "max_iterations_limit": 2,
                    "exit_when": { "operator": "equals", "path": "x", "value": true },
                    "steps": [{
                        "id": "BODY",
                        "title": "Body",
                        "kind": "capability",
                        "capability_id": "test.body",
                        "execution_mode": "sync",
                        "risk_level": "read_only"
                    }]
                }
            }],
            "artifacts": [],
            "ui": { "mode": "standard" }
        }))
        .unwrap();
        let err = error.validate().unwrap_err();
        assert!(err.contains("max_iterations_limit"), "上限低于默认值时必须拒绝：{err}");
    }

    #[test]
    fn execution_override_rejects_unknown_steps_and_bad_values() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("software-delivery");
        let package = load_from_directory(&root).unwrap();
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "NOPE": { "approval_required": true } } }),
        )
        .is_err());
        // 只有 loop 步骤能配轮数。
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "IMPLEMENT": { "max_iterations": 2 } } }),
        )
        .is_err());
        // on_failure 只接受 fail：改成 continue 会把失败吞掉。
        assert!(apply_execution_overrides(
            &package,
            &serde_json::json!({ "execution_overrides": { "IMPLEMENT": { "on_failure": "continue" } } }),
        )
        .is_err());
    }
}
