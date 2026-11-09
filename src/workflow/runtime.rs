use serde_json::{json, Value};
use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};

use super::{WorkflowPackage, WorkflowStep, WorkflowStepExecution};
use crate::runtime::{codex, copilot, process};
use crate::Options;

const RUNTIME_OUTPUT_LIMIT: usize = 64 * 1024;
const DEFAULT_RUNTIME_TIMEOUT_SECONDS: u64 = 2 * 60 * 60;

pub(crate) fn execute_runtime_step(
    package: &WorkflowPackage,
    step: &WorkflowStep,
    input: &Value,
    options: Option<&Options>,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<WorkflowStepExecution, Box<dyn Error>> {
    let runtime = step
        .runtime
        .as_ref()
        .ok_or_else(|| format!("workflow runtime step {} is missing runtime", step.id))?;
    // 角色可以覆盖 provider / prompt / tool_policy；未覆盖的沿用步骤声明。
    // 这不是「换了个 Agent」——同一条运行、同一份台账，只是这一段的执行配置不同。
    let role = resolve_role(package, step);
    let provider_name = role
        .map(|role| role.runtime_provider.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(runtime.provider.trim());
    let tool_policy = role
        .map(|role| role.tool_policy.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(runtime.tool_policy.trim());
    let base_prompt = role
        .map(|role| role.prompt.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(runtime.prompt.trim());
    // 模型覆盖与 provider / tool_policy 同一优先级：角色优先于步骤。
    // 两者都声明时角色说了算，否则会出现「角色把 provider 换到 builtin、模型却
    // 还留在 acp 命名空间」这种谁也执行不了组合。
    let model_alias = role
        .map(|role| role.model.trim())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = runtime.model.trim();
            (!value.is_empty()).then_some(value)
        });
    // auto 的含义是「挑一个能干这活的本机 Runtime」——干不了的就不该成为候选。
    // 传 tool_policy 进去，让 none 约束参与候选筛选，而不是先挑中再报错。
    let provider = resolve_provider(provider_name, tool_policy)?;
    enforce_runtime_network_policy(&provider, runtime.allow_network)?;
    let disable_tools = enforce_runtime_tool_policy(&provider, tool_policy)?;
    enforce_model_namespace(&provider, model_alias)?;
    let workspace = resolve_workspace(&runtime.workspace_path, input)?;
    // 声明了 input_artifacts 的步骤不从提示词里带上游输出：数据已经以文件路径
    // 注入 input_artifacts，提示词长度因此与 Artifact 体量无关。
    let mut prompt_input = if runtime.input_artifacts.is_empty() {
        input.clone()
    } else {
        without_step_outputs(input)
    };
    if disable_tools && !runtime.input_artifacts.is_empty() {
        inline_input_artifacts(&mut prompt_input)?;
    }
    let prompt = match role {
        Some(role) => build_prompt(&format!("{}{}", role_prompt_prefix(role), base_prompt), &prompt_input),
        None => build_prompt(base_prompt, &prompt_input),
    };
    // 运行时提示是通过命令行传给 Runtime 的，Windows 上整条命令行有 ~32KB 上限；
    // 超了会以 "os error 206" 这种看不出原因的方式失败，这里提前给出可读错误。
    const MAX_RUNTIME_PROMPT_CHARS: usize = 24_000;
    if prompt.chars().count() > MAX_RUNTIME_PROMPT_CHARS {
        return Err(format!(
            "workflow runtime step {} prompt is too long ({} chars, limit {}): shrink the step input or let the upstream step emit a smaller artifact",
            step.id,
            prompt.chars().count(),
            MAX_RUNTIME_PROMPT_CHARS
        )
        .into());
    }
    let timeout_seconds = if runtime.timeout_seconds == 0 {
        DEFAULT_RUNTIME_TIMEOUT_SECONDS
    } else {
        runtime.timeout_seconds
    };
    let run_id = input
        .pointer("/workflow_context/run/run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut runtime_facts: Option<(String, String, String)> = None;
    let mut acp_facts: Option<Value> = None;
    let output = match provider.as_str() {
        "personal.codex" => {
            // `cli:codex:<id>` 里的 id 原样转给 Codex；没声明就沿用 Codex 自己的默认模型。
            let cli_model = strip_cli_model_prefix(model_alias);
            let text = execute_codex(
                &workspace,
                &prompt,
                timeout_seconds,
                cli_model,
                is_canceled,
            )?;
            if let Some(model) = cli_model {
                runtime_facts = Some((
                    model.to_string(),
                    "cli".to_string(),
                    String::new(),
                ));
            }
            text
        }
        "personal.github-copilot" => {
            let cli_model = strip_cli_model_prefix(model_alias);
            let text = execute_copilot(
                &workspace,
                &prompt,
                timeout_seconds,
                cli_model,
                is_canceled,
            )?;
            if let Some(model) = cli_model {
                runtime_facts = Some((
                    model.to_string(),
                    "cli".to_string(),
                    String::new(),
                ));
            }
            text
        }
        "himind.fixture" => fixture_runtime_output(&step.id, &workspace, input)?,
        "himind.builtin" => {
            let outcome = crate::runtime::deepseek_harness::execute_workflow_in_run_home(
                options
                    .ok_or("himind.builtin runtime step is unavailable without Agent options")?,
                &workspace,
                &prompt,
                timeout_seconds,
                disable_tools,
                run_id,
                step.id.as_str(),
                model_alias,
                is_canceled,
            )?;
            // 把“实际用了哪个模型/哪类服务”写进步骤输出，随运行事实一起留痕。
            runtime_facts = Some((
                outcome.model.clone(),
                outcome.service_source.to_string(),
                outcome.endpoint.clone(),
            ));
            outcome.text
        }
        provider if crate::runtime::acp::is_provider(provider) => {
            // acp 别名形如 `acp:<provider>:<model>`，只把 model 段交给会话；
            // provider 段必须与当前步骤的 provider 一致，否则是包写错了。
            let acp_model = strip_acp_model_prefix(provider, model_alias)?;
            let execution = crate::runtime::acp::execute_workflow(
                provider,
                &workspace,
                &prompt,
                timeout_seconds,
                run_id,
                acp_model,
                options,
                is_canceled,
            )?;
            // ACP 步骤把真实会话事实留痕：工作台需要能回答“这一步由哪个 ACP 会话
            // 执行、真实 Agent 发起了多少次工具调用与权限请求”，否则链路不可审计。
            if !execution.model.trim().is_empty() {
                runtime_facts = Some((
                    execution.model.clone(),
                    "acp".to_string(),
                    String::new(),
                ));
            }
            acp_facts = Some(json!({
                "provider": provider,
                "model": execution.model,
                "session_id": execution.session_id,
                "stop_reason": execution.stop_reason,
                "update_count": execution.update_count,
                "tool_call_count": execution.tool_call_count,
                "permission_request_count": execution.permission_requests.len(),
                "denied_client_methods": execution.denied_client_methods,
                "log_path": execution.log_path,
            }));
            execution.final_text
        }
        value => return Err(format!("unsupported workflow runtime provider: {value}").into()),
    };
    let structured = parse_runtime_payload(&output);
    let mut result = json!({
        "schema_version": "workflow_runtime_result.v1",
        "provider": provider,
        "workspace": workspace,
        "summary": output,
    });
    if let Some((model, service_source, endpoint)) = runtime_facts {
        if let Some(object) = result.as_object_mut() {
            object.insert("model".to_string(), json!(model));
            object.insert("service_source".to_string(), json!(service_source));
            object.insert("endpoint".to_string(), json!(endpoint));
        }
    }
    if let Some(facts) = acp_facts {
        if let Some(object) = result.as_object_mut() {
            object.insert("acp".to_string(), facts);
        }
    }
    if let Some(structured) = structured {
        if let (Some(result), Some(structured)) = (result.as_object_mut(), structured.as_object()) {
            for (key, value) in structured {
                result.insert(key.clone(), value.clone());
            }
        }
    }
    validate_runtime_output(package, runtime.result_schema.trim(), &result)?;
    Ok(WorkflowStepExecution::output(result))
}

fn parse_runtime_payload(output: &str) -> Option<Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 模型习惯把 JSON 包在 ``` 代码块里，这属于格式差异而不是内容错误：
    // 先剥掉围栏再解析，避免“答案正确但被判成失败”。
    let candidates = std::iter::once(trimmed)
        .chain(strip_code_fence(trimmed))
        .chain(
            trimmed
                .lines()
                .rev()
                .map(str::trim)
                .filter(|line| line.starts_with('{')),
        );
    candidates
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<Value>(candidate).ok())
        .filter(Value::is_object)
}

/// 去掉 ```json … ``` 围栏（含无语言标记的 ``` … ```），返回其中的内容。
fn strip_code_fence(value: &str) -> Option<&str> {
    let without_open = value
        .strip_prefix("```json")
        .or_else(|| value.strip_prefix("```JSON"))
        .or_else(|| value.strip_prefix("```Json"))
        .or_else(|| value.strip_prefix("```"))?;
    let inner = without_open.trim_start_matches(['\r', '\n', ' ', '\t']);
    let end = inner.rfind("```")?;
    Some(inner[..end].trim())
}

fn validate_runtime_output(
    package: &WorkflowPackage,
    schema: &str,
    output: &Value,
) -> Result<(), Box<dyn Error>> {
    if schema.is_empty() {
        return Ok(());
    }
    let root = package.source_root.canonicalize()?;
    let schema_path = root.join(schema).canonicalize()?;
    if !schema_path.starts_with(&root) {
        return Err("workflow runtime result schema escapes the package".into());
    }
    let schema_value: Value = serde_json::from_slice(&std::fs::read(schema_path)?)?;
    let validator = jsonschema::validator_for(&schema_value)?;
    let errors = validator
        .iter_errors(output)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        return Ok(());
    }
    Err(format!(
        "workflow runtime result schema validation failed: {}",
        errors.join("; ")
    )
    .into())
}

fn resolve_provider(provider: &str, tool_policy: &str) -> Result<String, Box<dyn Error>> {
    let provider = provider.trim();
    if provider != "auto" {
        return Ok(provider.to_string());
    }
    // 步骤要求不挂工具时，只有能强制这一点的 Runtime 才是候选。
    // 否则会出现「auto 挑中 codex，运行到一半报 cannot enforce tool_policy=none」——
    // 那是最难排查的一类失败：预检说能动、真跑才炸。
    let require_no_tools = tool_policy.trim() == "none";
    let candidate_ok = |name: &str| !require_no_tools || super::runtime_supports_tool_none(name);

    if std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1")
        && candidate_ok("himind.fixture")
    {
        return Ok("himind.fixture".to_string());
    }
    if candidate_ok("personal.codex") && codex::probe().status == "ready" {
        return Ok("personal.codex".to_string());
    }
    if candidate_ok("personal.github-copilot") && copilot::probe().status == "ready" {
        return Ok("personal.github-copilot".to_string());
    }
    if candidate_ok("himind.builtin") && crate::runtime::deepseek_harness::probe().status == "ready" {
        return Ok("himind.builtin".to_string());
    }
    if require_no_tools {
        return Err(
            "no local Runtime can enforce tool_policy=none for an auto step; set provider to himind.builtin"
                .into(),
        );
    }
    Err("no local AI Runtime Provider is available for runtime step".into())
}

/// 模型别名必须与解析出的 Runtime 属于同一命名空间。
///
/// 这条检查存在的意义：`himind:` 别名只有 Agent 内置 Runtime 认，`acp:` 别名
/// 只有外部 ACP 客户端认。若把两者配错，运行期的结果不是「换了个模型」，而是
/// 「这个 Runtime 上没有这个模型」——那类失败在错误信息里看起来像环境问题，
/// 实际是包写错了。所以在启动子进程之前就拒绝。
/// 从 `acp:<provider>:<model>` 里取出 `<model>`，并校验 provider 段与步骤一致。
///
/// provider 段的比对归一化：`acp:claude:...` 与步骤写 `acp.claude` 视为同一家，
/// 因为 profile id 带点、别名写法带冒号，两种都出现在包作者的自然写法里。
fn strip_acp_model_prefix<'a>(
    provider: &str,
    model_alias: Option<&'a str>,
) -> Result<Option<&'a str>, Box<dyn Error>> {
    let Some(alias) = model_alias.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let rest = alias.strip_prefix("acp:").ok_or_else(|| {
        format!("acp runtime step received a non-acp model alias: {alias}")
    })?;
    let (alias_provider, model) = rest.split_once(':').ok_or_else(|| {
        format!("acp model alias requires <provider>:<model>: {alias}")
    })?;
    let normalize = |value: &str| {
        value
            .trim()
            .strip_prefix("acp.")
            .unwrap_or(value.trim())
            .to_ascii_lowercase()
    };
    if normalize(alias_provider) != normalize(provider) {
        return Err(format!(
            "workflow runtime {provider} cannot execute model alias {alias}: provider segment does not match the step's ACP client"
        )
        .into());
    }
    let model = model.trim();
    if model.is_empty() {
        return Err(format!("acp model alias has an empty model segment: {alias}").into());
    }
    Ok(Some(model))
}

fn enforce_model_namespace(provider: &str, model_alias: Option<&str>) -> Result<(), Box<dyn Error>> {
    let Some(alias) = model_alias.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let namespace = super::model_alias_namespace(alias).ok_or_else(|| {
        format!(
            "workflow runtime model alias must start with `himind:` or `acp:` (got {alias})"
        )
    })?;
    // 与契约层同一判据：这里复用 `model_alias_matches_provider`，避免两处规则走偏。
    super::model_alias_matches_provider(provider, alias)
        .map_err(|error| format!("workflow runtime: {error}"))?;
    Ok(())
}

/// 从 `cli:<target>:<model>` 取出要原样转给 CLI 的模型 id。
fn strip_cli_model_prefix<'a>(alias: Option<&'a str>) -> Option<&'a str> {
    let alias = alias.map(str::trim).filter(|value| !value.is_empty())?;
    alias
        .strip_prefix("cli:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, model)| model.trim())
        .filter(|model| !model.is_empty())
}

fn enforce_runtime_network_policy(
    provider: &str,
    allow_network: Option<bool>,
) -> Result<(), Box<dyn Error>> {
    // 只有显式声明 `allow_network: false` 的步骤才要求 Runtime 提供网络隔离；
    // 不声明表示作者没有提出这项约束。
    if allow_network != Some(false) || provider == "himind.fixture" {
        return Ok(());
    }
    Err(format!(
        "workflow runtime provider {provider} cannot enforce allow_network=false; configure a Runtime that advertises network isolation or explicitly allow network access"
    )
    .into())
}

/// 解析 `runtime.tool_policy` 并判断当前 Provider 能否兑现。
///
/// 只有能真正控制模型可见工具的 Provider 才允许声明 `none`：拿不到保证时
/// 直接失败，而不是让“禁用工具”退化成一句 prompt 约定。
fn enforce_runtime_tool_policy(provider: &str, tool_policy: &str) -> Result<bool, Box<dyn Error>> {
    match tool_policy.trim() {
        "" | "default" => Ok(false),
        "none" => {
            // 与预检共用同一份判据：预检说能跑、运行期才拒绝，是最难排查的一类失败。
            if super::runtime_supports_tool_none(provider) {
                Ok(true)
            } else {
                Err(format!(
                    "workflow runtime provider {provider} cannot enforce tool_policy=none; use a Runtime that can mount the step without model-facing tools"
                )
                .into())
            }
        }
        other => Err(format!("unsupported workflow runtime tool_policy: {other}").into()),
    }
}

fn fixture_runtime_output(
    step_id: &str,
    workspace: &str,
    input: &Value,
) -> Result<String, Box<dyn Error>> {
    let iteration = input
        .get("workflow_context")
        .and_then(|context| context.get("loops"))
        .and_then(|loops| loops.get("DEV-LOOP"))
        .and_then(|loop_state| loop_state.get("iteration"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if step_id.contains("REVIEW") {
        return Ok(serde_json::json!({
            "feedback": {
                "decision": if iteration >= 2 { "accepted" } else { "rejected" },
                "reason": if iteration >= 2 {
                    "fixture review accepted the current change"
                } else {
                    "fixture review requests another iteration"
                }
            }
        })
        .to_string());
    }
    let state_dir = std::path::Path::new(workspace).join(".himind-workflow-fixture");
    std::fs::create_dir_all(&state_dir)?;
    let relative_state_path =
        std::path::Path::new(".himind-workflow-fixture").join("development.json");
    let state_path = state_dir.join("development.json");
    let state = serde_json::json!({
        "schema_version": "workflow_runtime_fixture_state.v1",
        "step_id": step_id,
        "iteration": iteration,
        "applied_at": crate::approval::manager::unix_now(),
    });
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state)?)?;
    Ok(serde_json::json!({
        "summary": "fixture runtime applied the requested development change",
        "changes": [format!("updated {}", relative_state_path.to_string_lossy())],
        "verification": [{"name": "fixture", "status": "passed"}],
        "fixture_iteration": iteration,
        "fixture_state_path": relative_state_path.to_string_lossy(),
    })
    .to_string())
}

fn resolve_workspace(template: &str, input: &Value) -> Result<String, Box<dyn Error>> {
    let template = template.trim();
    // 工作区按优先级回退：显式声明 → run 输入里的项目/工作区 → Agent 当前工作区 → Agent 目录。
    // 这样“没有可省略参数就起不来”的问题不会再把简单工作流的启动成本抬高；
    // 实际使用的目录会写进步骤输出，事后可核对。
    let explicit = if template.is_empty() {
        None
    } else if let Some(key) = template.strip_prefix("input.") {
        input.get(key).and_then(Value::as_str)
    } else if let Some(key) = template.strip_prefix("workflow_context.") {
        input
            .get("workflow_context")
            .and_then(|context| context.get(key))
            .and_then(Value::as_str)
    } else {
        Some(template)
    };
    let candidate = explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            input
                .get("project_root")
                .or_else(|| input.get("workspace_root"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            crate::extension_projects::current_workspace_path()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| {
            crate::store::paths::agent_home()
                .to_string_lossy()
                .to_string()
        });
    let workspace = process::canonical_workspace(&candidate)?;
    Ok(workspace.to_string_lossy().to_string())
}

/// 去掉 `workflow_context.steps`（上游步骤输出），保留运行元数据与 input_artifacts。
///
/// 上游输出可能很大（几十 KB 的 Artifact），把它塞进提示词等于用命令行当数据通道；
/// 需要数据的步骤应该声明 `input_artifacts` 并读文件。
fn without_step_outputs(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(context) = value
        .get_mut("workflow_context")
        .and_then(Value::as_object_mut)
    {
        context.remove("steps");
    }
    value
}

/// 解析步骤实际生效的角色配置。
///
/// 角色在 package.roles 里按 ID 声明；步骤只带 ID。找不到就返回 None——
/// 允许「步骤写了角色但包没定义」的中间状态，那是渐进迁移，不该让运行失败。
fn resolve_role<'a>(package: &'a WorkflowPackage, step: &WorkflowStep) -> Option<&'a super::WorkflowRole> {
    let role_id = step.role_id.trim();
    if role_id.is_empty() {
        return None;
    }
    package.roles.get(role_id).filter(|role| {
        // 空配置的角色等价于「只标注负责人」，不产生任何覆盖。
        !role.brief.trim().is_empty()
            || !role.runtime_provider.trim().is_empty()
            || !role.prompt.trim().is_empty()
            || !role.tool_policy.trim().is_empty()
    })
}

/// 角色 brief 拼成提示词前缀。
///
/// 用独立段落而不是塞进 base prompt：Runtime 看到的是「你是这个角色」+「这一步要做
/// 什么」两段，角色职责不会因为步骤提示词被改而丢失。
fn role_prompt_prefix(role: &super::WorkflowRole) -> String {
    let brief = role.brief.trim();
    if brief.is_empty() {
        return String::new();
    }
    format!("【你的角色】{brief}\n\n")
}

fn build_prompt(base: &str, input: &Value) -> String {
    let mut prompt = base.trim().to_string();
    prompt.push_str("\n\nWorkflow context (JSON):\n");
    // 紧凑 JSON：提示词最终要通过命令行传给 Runtime（Windows 整条命令行 ~32KB），
    // 缩进展开会让大 Artifact 白白多花三成体积。
    prompt.push_str(&serde_json::to_string(&redact_runtime_context(input)).unwrap_or_default());
    prompt
}

/// 无工具 Runtime 不能再通过文件工具读取 Artifact，因此由 Agent 在启动前
/// 以只读方式把声明过的 JSON Artifact 注入上下文。对科技雷达这类列表型
/// Artifact 只保留提示词真正需要的字段，避免 Windows 命令行长度上限。
fn inline_input_artifacts(input: &mut Value) -> Result<(), Box<dyn Error>> {
    const MAX_INLINE_ARTIFACT_CHARS: usize = 16_000;
    let Some(artifacts) = input
        .get_mut("input_artifacts")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for (artifact_id, value) in artifacts.iter_mut() {
        let Some(path) = value
            .as_str()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let raw = std::fs::read_to_string(path).map_err(|error| {
            format!("workflow runtime cannot inline artifact {artifact_id}: {error}")
        })?;
        let parsed = serde_json::from_str::<Value>(&raw).map_err(|error| {
            format!("workflow runtime artifact {artifact_id} is not valid JSON: {error}")
        })?;
        let compact = compact_artifact_for_prompt(&parsed);
        let encoded = serde_json::to_string(&compact)?;
        if encoded.chars().count() > MAX_INLINE_ARTIFACT_CHARS {
            return Err(format!(
                "workflow runtime artifact {artifact_id} is too large for no-tools execution ({} chars, limit {}); reduce the upstream artifact or use an interactive Runtime",
                encoded.chars().count(),
                MAX_INLINE_ARTIFACT_CHARS
            )
            .into());
        }
        *value = compact;
    }
    Ok(())
}

fn compact_artifact_for_prompt(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    // Tech Radar 快照包含 URL、主题和趋势辅助字段；这些字段对解读不是必需，
    // 且会把原本可控的 headless prompt 撑过 Windows 命令行限制。
    if let Some(entries) = object.get("entries").and_then(Value::as_array) {
        let mut result = serde_json::Map::new();
        for key in [
            "generated_at",
            "rule_version",
            "window_days",
            "domains",
            "lanes",
            "min_stars",
            "new_min_stars",
        ] {
            if let Some(value) = object.get(key) {
                result.insert(key.to_string(), value.clone());
            }
        }
        let entries = entries
            .iter()
            .map(|entry| {
                let Some(entry) = entry.as_object() else {
                    return entry.clone();
                };
                let mut compact = serde_json::Map::new();
                for key in [
                    "entry_id",
                    "full_name",
                    "description",
                    "language",
                    "stars",
                    "stars_gained",
                    "pushed_at",
                    "license",
                    "domains",
                    "lanes",
                    "is_new",
                ] {
                    if let Some(value) = entry.get(key) {
                        compact.insert(key.to_string(), value.clone());
                    }
                }
                Value::Object(compact)
            })
            .collect();
        result.insert("entries".to_string(), Value::Array(entries));
        return Value::Object(result);
    }
    value.clone()
}

fn redact_runtime_context(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    if normalized.contains("secret")
                        || normalized.contains("password")
                        || normalized.contains("token")
                        || normalized.contains("private_key")
                        || normalized.contains("apikey")
                        || normalized.contains("api_key")
                    {
                        (key.clone(), Value::String("[redacted]".to_string()))
                    } else {
                        (key.clone(), redact_runtime_context(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_runtime_context).collect()),
        _ => value.clone(),
    }
}

fn execute_codex(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let (executable, _) = codex::resolve_codex_executable()?;
    let result_path = process::safe_temp_path(
        &format!("workflow-codex-{}", rand::random::<u64>()),
        "result.txt",
    )?;
    process::remove_file_if_present(&result_path);
    let mut command = Command::new(executable);
    command.args([
        "-C",
        workspace,
        "-s",
        "workspace-write",
        "-a",
        "never",
    ]);
    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        command.arg("--model").arg(model);
    }
    command
        .args([
            "exec",
            "--json",
            "--color",
            "never",
            "--skip-git-repo-check",
            "-o",
        ])
        .arg(&result_path)
        .arg("-")
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    process::configure_hidden_process(&mut command);
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(prompt.as_bytes()) {
            process::terminate_process_tree(&mut child);
            process::remove_file_if_present(&result_path);
            return Err(format!("failed to send workflow instruction to Codex: {error}").into());
        }
    } else {
        process::terminate_process_tree(&mut child);
        process::remove_file_if_present(&result_path);
        return Err("Codex runtime stdin was not available".into());
    }
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child_with_timeout_and_cancel(
        &mut child,
        "HIMIND_CODEX_TIMEOUT_SECONDS",
        timeout_seconds,
        "Codex workflow runtime",
        || is_canceled(),
    );
    let stdout = process::join_output(stdout);
    let stderr = process::join_output(stderr);
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            process::remove_file_if_present(&result_path);
            return Err(error);
        }
    };
    if !status.success() {
        process::remove_file_if_present(&result_path);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!(
            "Codex runtime step failed: {}",
            process::summarize_output(detail.trim(), RUNTIME_OUTPUT_LIMIT)
        )
        .into());
    }
    if result_path.is_file() {
        let result = std::fs::read_to_string(&result_path)?;
        process::remove_file_if_present(&result_path);
        return Ok(process::summarize_output(
            result.trim(),
            RUNTIME_OUTPUT_LIMIT,
        ));
    }
    process::remove_file_if_present(&result_path);
    Ok(process::summarize_output(
        stdout.trim(),
        RUNTIME_OUTPUT_LIMIT,
    ))
}

fn execute_copilot(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let (executable, _) = copilot::resolve_copilot_executable()?;
    let mut command = Command::new(executable);
    command.args(["-p", prompt, "-s", "--no-ask-user", "--allow-all-tools"]);
    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        command.arg("--model").arg(model);
    }
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    process::configure_hidden_process(&mut command);
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child_with_timeout_and_cancel(
        &mut child,
        "HIMIND_GITHUB_COPILOT_TIMEOUT_SECONDS",
        timeout_seconds,
        "GitHub Copilot workflow runtime",
        || is_canceled(),
    )?;
    let stdout = process::join_output(stdout);
    let stderr = process::join_output(stderr);
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!(
            "GitHub Copilot runtime step failed: {}",
            process::summarize_output(detail.trim(), RUNTIME_OUTPUT_LIMIT)
        )
        .into());
    }
    Ok(process::summarize_output(
        stdout.trim(),
        RUNTIME_OUTPUT_LIMIT,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_payload_parses_fenced_json() {
        // 模型常把 JSON 包在代码块里；围栏不能当成内容错误。
        let fenced = "```json\n{\"insights\":[{\"entry_id\":\"a/b\"}],\"summary\":\"ok\"}\n```";
        let parsed = parse_runtime_payload(fenced).expect("fenced JSON must parse");
        assert_eq!(parsed["summary"], json!("ok"));
        assert!(parse_runtime_payload("```\n{\"summary\":\"ok\"}\n```").is_some());
        // 非对象或没有 JSON 时仍然返回 None，不猜。
        assert!(parse_runtime_payload("[1,2,3]").is_none());
        assert!(parse_runtime_payload("抱歉，我无法完成。").is_none());
    }

    #[test]
    fn steps_with_input_artifacts_drop_upstream_outputs_from_the_prompt() {
        let input = json!({
            "workspace_root": "C:\\work",
            "input_artifacts": {"tech-radar-snapshot": "C:\\data\\snapshots\\2026-09-20.json"},
            "workflow_context": {
                "run": {"run_id": "run-1"},
                "step_id": "TR-INSIGHT",
                "steps": {"TR-COLLECT": {"snapshot": {"entries": [{"entry_id": "a/b"}]}}},
            },
        });
        let reduced = without_step_outputs(&input);
        // 数据走文件引用，提示词不再背着上游 Artifact。
        assert!(reduced.pointer("/workflow_context/steps").is_none());
        assert_eq!(
            reduced
                .pointer("/input_artifacts/tech-radar-snapshot")
                .and_then(Value::as_str),
            Some("C:\\data\\snapshots\\2026-09-20.json")
        );
        assert_eq!(
            reduced
                .pointer("/workflow_context/run/run_id")
                .and_then(Value::as_str),
            Some("run-1")
        );
        let prompt = build_prompt("任务说明", &reduced);
        assert!(prompt.contains("tech-radar-snapshot"));
        assert!(!prompt.contains("entry_id"));
    }

    #[test]
    fn tool_policy_none_requires_a_provider_that_can_honor_it() {
        assert_eq!(
            enforce_runtime_tool_policy("himind.builtin", "none").unwrap(),
            true
        );
        assert_eq!(
            enforce_runtime_tool_policy("himind.builtin", "default").unwrap(),
            false
        );
        assert_eq!(
            enforce_runtime_tool_policy("himind.builtin", "").unwrap(),
            false
        );
        // 无法保证禁用工具的 Provider 必须 fail closed，而不是静默降级成“尽力而为”。
        assert!(enforce_runtime_tool_policy("personal.codex", "none").is_err());
        assert!(enforce_runtime_tool_policy("himind.builtin", "read-only").is_err());
    }

    #[test]
    fn runtime_context_redacts_secret_like_values() {
        let input = json!({
            "project_root": "C:\\work",
            "private_key_path": "C:\\secret.key",
            "nested": {"access_token": "secret"}
        });
        let redacted = redact_runtime_context(&input);
        assert_eq!(redacted["private_key_path"], "[redacted]");
        assert_eq!(redacted["nested"]["access_token"], "[redacted]");
        assert_eq!(redacted["project_root"], "C:\\work");
    }

    #[test]
    fn resolves_runtime_workspace_from_input_path() {
        let root = std::env::temp_dir();
        let input = json!({"project_root": root});
        let workspace = resolve_workspace("input.project_root", &input).unwrap();
        assert!(!workspace.is_empty());
    }

    #[test]
    fn fixture_runtime_records_workspace_mutation_per_iteration() {
        let root = std::env::temp_dir().join(format!(
            "himind-runtime-fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let input = json!({
            "workflow_context": {
                "loops": {
                    "DEV-LOOP": {"iteration": 2}
                }
            }
        });
        let output = fixture_runtime_output("DEV-CODE", root.to_str().unwrap(), &input).unwrap();
        let state: Value = serde_json::from_slice(
            &std::fs::read(root.join(".himind-workflow-fixture/development.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["iteration"], 2);
        assert!(output.contains("fixture_iteration"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// 关键回归：auto + tool_policy=none 必须挑到能执行它的 Runtime，
    /// 而不是先挑中 codex 再在运行期报 cannot enforce。
    #[test]
    fn auto_never_resolves_to_a_runtime_that_cannot_enforce_tool_none() {
        // fixture 模式是唯一可确定的环境；它本来就支持 none，理应被选中。
        std::env::set_var("HIMIND_WORKFLOW_RUNTIME_FIXTURE", "1");
        let resolved = resolve_provider("auto", "none").unwrap();
        assert!(
            super::super::runtime_supports_tool_none(&resolved),
            "auto 为 none 步骤挑出的 provider 必须支持 none，实际：{resolved}"
        );
        // 非 none 的步骤不受影响：fixture 优先仍然是既有行为。
        assert_eq!(resolve_provider("auto", "default").unwrap(), "himind.fixture");
        std::env::remove_var("HIMIND_WORKFLOW_RUNTIME_FIXTURE");
    }

    #[test]
    fn explicit_provider_is_never_rewritten_by_tool_policy() {
        // 显式声明不改写：用户写了 personal.codex，就该报错而不是悄悄换成 builtin。
        assert_eq!(
            resolve_provider("personal.codex", "none").unwrap(),
            "personal.codex"
        );
    }

    #[test]
    fn rejects_network_disabled_policy_for_unisolated_provider() {
        assert!(enforce_runtime_network_policy("personal.codex", Some(false)).is_err());
        assert!(enforce_runtime_network_policy("personal.github-copilot", Some(false)).is_err());
        assert!(enforce_runtime_network_policy("himind.fixture", Some(false)).is_ok());
        assert!(enforce_runtime_network_policy("personal.codex", Some(true)).is_ok());
        // 未声明网络约束时不得把「无法证明隔离」当成拒绝服务的理由。
        assert!(enforce_runtime_network_policy("personal.codex", None).is_ok());
    }

    fn role_package(roles: &[(&str, super::super::WorkflowRole)]) -> WorkflowPackage {
        let mut package: WorkflowPackage = serde_json::from_value(serde_json::json!({
            "schema_version": "workflow_package.v1",
            "id": "test.roles",
            "version": "1.0.0",
            "name": "Roles",
            "min_agent_version": "0.3.90",
            "steps": [],
            "artifacts": [],
            "ui": { "mode": "standard" }
        }))
        .unwrap();
        for (id, role) in roles {
            package.roles.insert(id.to_string(), role.clone());
        }
        package
    }

    fn role_step(role_id: &str, provider: &str, tool_policy: &str, prompt: &str) -> WorkflowStep {
        serde_json::from_value(serde_json::json!({
            "id": "STEP",
            "title": "Step",
            "kind": "runtime",
            "execution_mode": "sync",
            "role_id": role_id,
            "runtime": {
                "provider": provider,
                "prompt": prompt,
                "tool_policy": tool_policy
            }
        }))
        .unwrap()
    }

    /// `cli:` 别名要按目标名分发，并只把 model 段原样转给 CLI。
    #[test]
    fn cli_model_alias_is_forwarded_to_the_matching_cli() {
        assert_eq!(
            strip_cli_model_prefix(Some("cli:codex:gpt-5-codex")),
            Some("gpt-5-codex")
        );
        assert_eq!(
            strip_cli_model_prefix(Some("cli:github-copilot:claude-sonnet-4-5")),
            Some("claude-sonnet-4-5")
        );
        // 未声明 / 非 cli 命名空间 -> 不透传。
        assert_eq!(strip_cli_model_prefix(None), None);
        assert_eq!(strip_cli_model_prefix(Some("himind:svc:x")), None);

        // 目标名写错要在启动前拒绝：`cli:codex:` 不能喂给 Copilot。
        let error =
            enforce_model_namespace("personal.github-copilot", Some("cli:codex:gpt-5")).unwrap_err();
        assert!(error.to_string().contains("cli target must be"), "{error}");
        assert!(enforce_model_namespace("personal.github-copilot", Some("cli:github-copilot:gpt-4.1")).is_ok());
        assert!(enforce_model_namespace("personal.codex", Some("cli:codex:gpt-5-codex")).is_ok());
        // himind 别名不能发给 CLI。
        assert!(enforce_model_namespace("personal.codex", Some("himind:svc:x")).is_err());
    }

    #[test]
    fn acp_model_alias_is_split_and_provider_matched() {
        let extracted = strip_acp_model_prefix(
            "acp.claude",
            Some("acp:claude:claude-sonnet-4-5"),
        )
        .unwrap();
        assert_eq!(extracted, Some("claude-sonnet-4-5"));

        // 别名里的 provider 段带不带 `acp.` 都接受。
        assert_eq!(
            strip_acp_model_prefix("acp.claude", Some("acp:acp.claude:claude-opus-4-1")).unwrap(),
            Some("claude-opus-4-1")
        );
        // 未声明 -> 不切换。
        assert_eq!(strip_acp_model_prefix("acp.claude", None).unwrap(), None);

        // provider 段与步骤不一致：包写错了，必须拒绝。
        let mismatch = strip_acp_model_prefix("acp.claude", Some("acp:codex:gpt-5")).unwrap_err();
        assert!(mismatch.to_string().contains("does not match"), "{mismatch}");
        // 非 acp 命名空间不能进 acp 分支。
        assert!(strip_acp_model_prefix("acp.claude", Some("himind:svc:x")).is_err());
        // 缺 model 段。
        assert!(strip_acp_model_prefix("acp.claude", Some("acp:claude:")).is_err());
    }

    #[test]
    fn model_namespace_must_match_the_runtime() {
        assert!(enforce_model_namespace("himind.builtin", Some("himind:svc:x")).is_ok());
        assert!(enforce_model_namespace("acp.claude", Some("acp:claude:x")).is_ok());
        assert!(enforce_model_namespace("himind.builtin", None).is_ok());

        let builtin_with_acp =
            enforce_model_namespace("himind.builtin", Some("acp:claude:x")).unwrap_err();
        assert!(builtin_with_acp.to_string().contains("acp:"), "{builtin_with_acp}");
        let acp_with_himind =
            enforce_model_namespace("acp.claude", Some("himind:svc:x")).unwrap_err();
        assert!(acp_with_himind.to_string().contains("himind:"), "{acp_with_himind}");
        assert!(enforce_model_namespace("himind.builtin", Some("bare-model")).is_err());
    }

    #[test]
    fn role_overrides_step_runtime_settings() {
        let package = role_package(&[(
            "software.reviewer",
            super::super::WorkflowRole {
                label: "代码审查".to_string(),
                brief: "只依据 diff 判断".to_string(),
                runtime_provider: "himind.fixture".to_string(),
                prompt: "审查这次改动".to_string(),
                tool_policy: "none".to_string(),
                model: String::new(),
            },
        )]);
        let step = role_step("software.reviewer", "personal.codex", "default", "步骤原始提示");
        let role = resolve_role(&package, &step).expect("declared role must resolve");
        // 角色声明的三项都优先于步骤声明：这是角色隔离的实质。
        assert_eq!(role.runtime_provider, "himind.fixture");
        assert_eq!(role.tool_policy, "none");
        assert_eq!(role.prompt, "审查这次改动");
        // brief 走独立段落，不混进 base prompt。
        assert!(role_prompt_prefix(role).contains("只依据 diff 判断"));
        assert!(role_prompt_prefix(role).starts_with("【你的角色】"));
    }

    #[test]
    fn role_without_config_falls_back_to_step_settings() {
        // 只声明 label 的角色是「标注负责人」，不该改变任何执行行为。
        let package = role_package(&[(
            "software.analyst",
            super::super::WorkflowRole {
                label: "需求分析".to_string(),
                ..Default::default()
            },
        )]);
        let step = role_step("software.analyst", "himind.fixture", "default", "原始提示");
        assert!(resolve_role(&package, &step).is_none());
    }

    #[test]
    fn undeclared_role_id_does_not_break_execution() {
        // 老包可能逐步迁移：步骤写了角色但包还没定义，此时按无角色处理而不是报错。
        let package = role_package(&[]);
        let step = role_step("software.mystery", "himind.fixture", "default", "原始提示");
        assert!(resolve_role(&package, &step).is_none());
        assert_eq!(role_prompt_prefix(&super::super::WorkflowRole::default()), "");
    }
}
