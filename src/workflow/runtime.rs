use serde_json::{json, Value};
use std::error::Error;
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
    execute_runtime_step_with_resources(package, step, input, options, is_canceled, None)
}

pub(crate) fn execute_runtime_step_with_resources(
    package: &WorkflowPackage,
    step: &WorkflowStep,
    input: &Value,
    options: Option<&Options>,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    resources: Option<&crate::mission::CheckRuntimeResources>,
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
    let provider = resolve_provider(if runtime.permission_mode == "full_access" && provider_name == "auto" { "himind.builtin" } else { provider_name }, tool_policy)?;
    super::validate_runtime_permission_mode(&provider, &runtime.permission_mode)?;
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
    validate_prompt_transport(&provider, &step.id, &prompt)?;
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
    let mut codex_facts: Option<Value> = None;
    let mut usage = None;
    let output = match provider.as_str() {
        "personal.codex" => {
            // `cli:codex:<id>` 里的 id 原样转给 Codex；没声明就沿用 Codex 自己的默认模型。
            let cli_model = strip_cli_model_prefix(model_alias);
            let outcome = execute_codex_outcome_with_resources(
                &workspace,
                &prompt,
                timeout_seconds,
                cli_model,
                workflow_access_with_permission(&step.risk_level, tool_policy, &runtime.permission_mode),
                is_canceled,
                std::time::Duration::from_secs(15),
                resources,
                Some(&format!("{run_id}:{}:{}",step.id,input.pointer("/workflow_context/runtime_attempt").and_then(Value::as_u64).unwrap_or(1))),
            )?;
            if let Some(model) = cli_model {
                runtime_facts = Some((
                    model.to_string(),
                    "cli".to_string(),
                    String::new(),
                ));
            }
            usage = outcome.usage;
            codex_facts = Some(json!({"provider":"personal.codex","session_id":outcome.session_id,
                "version":outcome.version,"usage_receipt":outcome.usage_receipt,
                "model_source":if cli_model.is_some() {"explicit_cli"} else {"personal_cli_config"},
                "cost":null,"cost_status":"unknown"}));
            outcome.text
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
            let outcome = crate::runtime::deepseek_harness::execute_workflow_in_run_home_with_resources(
                options
                    .ok_or("himind.builtin runtime step is unavailable without Agent options")?,
                &workspace,
                &prompt,
                timeout_seconds,
                disable_tools,
                run_id,
                step.id.as_str(),
                model_alias,
                workflow_access_with_permission(&step.risk_level, tool_policy, &runtime.permission_mode),
                is_canceled,
                resources,
            )?;
            // 把“实际用了哪个模型/哪类服务”写进步骤输出，随运行事实一起留痕。
            runtime_facts = Some((
                outcome.model.clone(),
                outcome.service_source.to_string(),
                outcome.endpoint.clone(),
            ));
            usage = outcome.usage;
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
                &runtime.permission_mode,
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
            usage = execution.usage;
            execution.final_text
        }
        value => return Err(format!("unsupported workflow runtime provider: {value}").into()),
    };
    // Mission's declared final is a JSON report. Do not salvage an inner/last
    // line from a truncated outer object using the text-friendly Workflow
    // fallback. Ordinary workflows retain their existing text semantics.
    let structured = if package.id.starts_with("com.himind.mission.") {
        let raw = output.trim();
        let json = if raw.starts_with("```") && raw.ends_with("```") {
            strip_code_fence(raw).unwrap_or(raw)
        } else { raw };
        let report = serde_json::from_str::<Value>(json).ok().filter(|value|
            value.is_object() && value.get("passed").and_then(Value::as_bool).is_some());
        if report.is_none() {
            return Err(Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
                provenance:codex_facts,
                message: "Mission runtime requires one complete JSON report with boolean passed".into(),
                usage, report: None,
            }));
        }
        report
    } else { parse_runtime_payload(&output) };
    if (!runtime.result_schema.trim().is_empty() && structured.is_none())
        || structured.as_ref().is_some_and(|value| value.get("passed").and_then(Value::as_bool) == Some(false)) {
        return Err(Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
            provenance:codex_facts,
            message: "Runtime returned an invalid structured result or passed=false".into(),
            usage, report: structured,
        }));
    }
    let mut result = json!({
        "schema_version": "workflow_runtime_result.v1",
        "provider": provider,
        "workspace": workspace,
        "summary": output,
        "final_message": output,
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
    if let Some(facts) = codex_facts {
        result.as_object_mut().unwrap().insert("codex".to_string(), facts);
    }
    if let Some(structured) = structured {
        if let (Some(result), Some(structured)) = (result.as_object_mut(), structured.as_object()) {
            for (key, value) in structured {
                if ["schema_version","provider","workspace","model","service_source","endpoint","acp","codex","final_message"].contains(&key.as_str()) { continue; }
                result.insert(key.clone(), value.clone());
            }
        }
    }
    validate_runtime_output(package, runtime.result_schema.trim(), &result).map_err(|error|
        Box::new(crate::runtime::deepseek_harness::WorkflowRuntimeFailure {
            message:error.to_string(),report:None,usage:usage.clone(),
            provenance:Some(json!({"provider":provider,"codex":result.get("codex"),"model":result.get("model")})),
        }) as Box<dyn Error>)?;
    Ok(WorkflowStepExecution {
        output: result,
        artifacts: Vec::new(),
        usage,
    })
}

fn validate_prompt_transport(provider: &str, step_id: &str, prompt: &str) -> Result<(), Box<dyn Error>> {
    // Only Copilot puts the prompt in argv (-p). Codex and builtin use stdin;
    // ACP sends a protocol message. Windows counts UTF-16 code units.
    const MAX_ARGV_PROMPT_UNITS: usize = 24_000;
    if provider == "personal.github-copilot" {
        let units = prompt.encode_utf16().count();
        if units > MAX_ARGV_PROMPT_UNITS {
            return Err(format!("workflow runtime step {step_id} prompt exceeds the {provider} command-line budget ({units} UTF-16 units, limit {MAX_ARGV_PROMPT_UNITS}): use input artifacts or a runtime with stdin/protocol input").into());
        }
    }
    Ok(())
}

pub(crate) fn parse_runtime_payload(output: &str) -> Option<Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 模型习惯把 JSON 包在 ``` 代码块里，这属于格式差异而不是内容错误：
    // 先剥掉围栏再解析，避免“答案正确但被判成失败”。
    serde_json::from_str::<Value>(strip_code_fence(trimmed).unwrap_or(trimmed))
        .ok().filter(Value::is_object)
}

/// 去掉 ```json … ``` 围栏（含无语言标记的 ``` … ```），返回其中的内容。
fn strip_code_fence(value: &str) -> Option<&str> {
    let (opening, rest) = value.split_once('\n')?;
    if !["```", "```json", "```JSON", "```Json"].contains(&opening.trim_end_matches('\r')) { return None; }
    Some(rest.trim_end().strip_suffix("```")?.trim())
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
    let candidate_ok = |name: &str| {
        if tool_policy.trim() == "files_only" { name == "himind.builtin" }
        else { !require_no_tools || super::runtime_supports_tool_none(name) }
    };

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

/// 步骤风险等级 -> 实际需要的文件访问级别。
///
/// 只读步骤必须走 DSH 的 `read-only`：它不创建写授权，也就不要求调用方拥有
/// 目标目录的 WRITE_DAC / WRITE_OWNER。把只读步骤也按 workspace-write 跑，
/// 会让「只做审查」的任务在 ACL 受限的机器上直接失败。
///
/// 判定用白名单倒推：只有明确声明为只读的才降级，其余一律按可写处理——
/// 宁可多要权限，也不能让需要写盘的步骤静默变成只读。
fn workflow_access_for_risk(risk_level: &str) -> crate::runtime::deepseek_harness::WorkflowAccess {
    use crate::runtime::deepseek_harness::WorkflowAccess;
    match risk_level.trim().to_ascii_uppercase().as_str() {
        "READ_ONLY" | "R1" => WorkflowAccess::Read,
        _ => WorkflowAccess::Write,
    }
}

fn workflow_access_for_policy(risk_level: &str, tool_policy: &str) -> crate::runtime::deepseek_harness::WorkflowAccess {
    use crate::runtime::deepseek_harness::WorkflowAccess;
    match (workflow_access_for_risk(risk_level), tool_policy.trim()) {
        (WorkflowAccess::Write, "files_only") => WorkflowAccess::FilesOnly,
        (access, _) => access,
    }
}

fn workflow_access_with_permission(risk_level: &str, tool_policy: &str, permission_mode: &str) -> crate::runtime::deepseek_harness::WorkflowAccess {
    use crate::runtime::deepseek_harness::WorkflowAccess;
    match workflow_access_for_policy(risk_level, tool_policy) {
        WorkflowAccess::Write if permission_mode == "full_access" => WorkflowAccess::Full,
        access => access,
    }
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
        "files_only" if provider == "himind.builtin" => Ok(false),
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
    // Mission verification is a deterministic host-bound fixture. It echoes
    // only identities and host checks supplied in the run input; it can never
    // turn an arbitrary implementation into a pass.
    if step_id == "VERIFY" {
        let input_checks = input.get("host_checks").and_then(Value::as_array).cloned().unwrap_or_default();
        let checks = input_checks.iter().filter_map(|check| {
            let command = check.get("command")?.as_str()?;
            let exit_code = check.get("exit_code")?.as_i64()?;
            let digest = check.get("output_sha256")?.as_str().unwrap_or_default();
            Some(serde_json::json!({
                "command": command,
                "exit_code": exit_code,
                "detail": format!("source=host; output_sha256={digest}")
            }))
        }).collect::<Vec<_>>();
        let host_passed = !checks.is_empty() && checks.iter().all(|check| check["exit_code"] == 0);
        let candidate = input.get("candidate_before").cloned().unwrap_or(Value::Null);
        return Ok(serde_json::json!({
            "passed": host_passed && candidate.get("candidate_id").and_then(Value::as_str).is_some_and(|value| !value.is_empty()),
            "verified_task_ids": [input.get("task_id").cloned().unwrap_or(Value::Null)],
            "plan_sha256": input.get("expected_plan_sha256").cloned().unwrap_or(Value::Null),
            "candidate": candidate,
            "checks": checks,
            "reasons": if host_passed { Vec::<String>::new() } else { vec!["host checks incomplete or failed".to_string()] }
        }).to_string());
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
    let mut report = serde_json::json!({
        "summary": "fixture runtime applied the requested development change",
        "changes": [format!("updated {}", relative_state_path.to_string_lossy())],
        "verification": [{"name": "fixture", "status": "passed"}],
        "fixture_iteration": iteration,
        "fixture_state_path": relative_state_path.to_string_lossy(),
    });
    // The explicit fixture still has to satisfy Mission's strict final report
    // contract after its actual write succeeds. Ordinary workflow output keeps
    // its original shape; independent VERIFY remains host-bound above.
    if ["mission_id", "task_id"].iter().all(|key|
        input.get(*key).and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty())) {
        report["passed"] = json!(true);
    }
    Ok(report.to_string())
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

#[cfg(test)]
use crate::runtime::codex_transport::{codex_error_class, CodexCompletion};

fn execute_codex(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    access: crate::runtime::deepseek_harness::WorkflowAccess,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    execute_codex_with_shutdown_limit(workspace, prompt, timeout_seconds, model, access, is_canceled,
        std::time::Duration::from_secs(15))
}

fn execute_codex_with_shutdown_limit(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    access: crate::runtime::deepseek_harness::WorkflowAccess,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    shutdown_limit: std::time::Duration,
) -> Result<String, Box<dyn Error>> {
    execute_codex_with_resources(workspace, prompt, timeout_seconds, model, access, is_canceled, shutdown_limit, None)
}

fn execute_codex_with_resources(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    access: crate::runtime::deepseek_harness::WorkflowAccess,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    shutdown_limit: std::time::Duration,
    resources: Option<&crate::mission::CheckRuntimeResources>,
) -> Result<String, Box<dyn Error>> {
    execute_codex_outcome_with_resources(workspace, prompt, timeout_seconds, model, access,
        is_canceled, shutdown_limit, resources, None).map(|outcome| outcome.text)
}

fn execute_codex_outcome_with_resources(
    workspace: &str,
    prompt: &str,
    timeout_seconds: u64,
    model: Option<&str>,
    access: crate::runtime::deepseek_harness::WorkflowAccess,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    shutdown_limit: std::time::Duration,
    resources: Option<&crate::mission::CheckRuntimeResources>,
    run_identity: Option<&str>,
) -> Result<crate::runtime::codex_transport::CodexOutcome, Box<dyn Error>> {
    let (executable, version) = codex::resolve_codex_executable_with_cancel(is_canceled)?;
    let result_path = process::safe_temp_path(
        &format!("workflow-codex-{}", rand::random::<u64>()),
        "result.txt",
    )?;
    process::remove_file_if_present(&result_path);
    let sandbox = codex_workflow_sandbox(access)?;
    let mut command = Command::new(executable);
    command.args([
        "-C",
        workspace,
        "-s",
        sandbox,
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
    if let Some(resources) = resources { resources.apply(&mut command)?; }
    process::remove_himind_secret_environment(&mut command);
    process::configure_hidden_process(&mut command);
    let outcome = crate::runtime::codex_transport::execute(
        &mut command, prompt, &result_path, timeout_seconds, is_canceled, shutdown_limit,
        run_identity.unwrap_or(result_path.to_str().ok_or("Codex result identity is not UTF-8")?), "");
    process::remove_file_if_present(&result_path);
    outcome.map(|mut outcome| {outcome.version=version.trim().to_string();outcome})
}

fn codex_workflow_sandbox(access: crate::runtime::deepseek_harness::WorkflowAccess) -> Result<&'static str, Box<dyn Error>> {
    use crate::runtime::deepseek_harness::WorkflowAccess;
    match access {
        WorkflowAccess::Read => Ok("read-only"),
        WorkflowAccess::Write => Ok("workspace-write"),
        WorkflowAccess::Full => Ok("danger-full-access"),
        WorkflowAccess::FilesOnly => Err("Codex cannot enforce files_only access".into()),
    }
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
    let (mut child, _tree) = crate::runtime::owned_tree::OwnedTree::spawn(&mut command)?;
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child_with_timeout_and_cancel(
        &mut child,
        "HIMIND_GITHUB_COPILOT_TIMEOUT_SECONDS",
        timeout_seconds,
        "GitHub Copilot workflow runtime",
        || is_canceled(),
    )?;
    drop(_tree);
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
    fn executor_prompt_transport_and_strict_payload() {
        let prompt = "中文😀\"$()\\shell\n".repeat(9000);
        assert!(prompt.encode_utf16().count() > 32767);
        for provider in ["personal.codex","himind.builtin","acp.claude"] {
            validate_prompt_transport(provider,"step",&prompt).unwrap();
        }
        assert!(validate_prompt_transport("personal.github-copilot","step",&prompt).is_err());
        for bad in ["{\"passed\":false,\"details\":\n{\"passed\":true}",
            "```json\n{\"passed\":true}\n```\ntrailing", "prefix\n{\"passed\":true}"] {
            assert!(parse_runtime_payload(bad).is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn executor_workflow_large_input_no_reader_cancel_timeout_and_output_first() {
        use crate::runtime::process::codex_test_support::{Environment, ProcessProbe};
        let _lock = crate::store::paths::test_env_lock();
        let prompt = "中文😀\"$()\\shell\n".repeat(90000);
        for cancel in [false,true] {
            let env=Environment::new(); env.case(&json!({"no_reader":true,"output_before_input":true}));
            let root=env.root.clone();
            let probe=std::thread::spawn(move || {
                let parent=ProcessProbe::open(&root.join("root.pid"));
                let child=ProcessProbe::open(&root.join("child.pid"));
                parent.assert_exited(); child.assert_exited();
            });
            let started=std::time::Instant::now();
            let error=execute_codex(env.root.to_str().unwrap(),&prompt,1,None,
                crate::runtime::deepseek_harness::WorkflowAccess::Full,
                &|| Ok(cancel && env.root.join("root.pid").exists())).unwrap_err();
            assert!(started.elapsed()<std::time::Duration::from_secs(4),"{error}");
            assert!(error.to_string().contains(if cancel {"canceled"} else {"exceeded"}),"{error}");
            probe.join().unwrap();
        }
        let env=Environment::new(); env.case(&json!({"output_before_input":true}));
        execute_codex(env.root.to_str().unwrap(),&prompt,5,None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,&||Ok(false)).unwrap();
        assert_eq!(std::fs::read_to_string(env.root.join("stdin.txt")).unwrap(),prompt);
    }

    #[cfg(windows)]
    #[test]
    fn executor_discovery_cancel_and_timeout_do_not_launch_exec() {
        use crate::runtime::process::codex_test_support::{Environment, ProcessProbe};
        let _lock = crate::store::paths::test_env_lock();
        for cancel in [false,true] {
            let env=Environment::new(); env.case(&json!({"version_hang":true}));
            let root=env.root.clone();
            let probe=std::thread::spawn(move || ProcessProbe::open(&root.join("version.pid")).assert_exited());
            let exe=std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap();
            let started=std::time::Instant::now();
            let error=process::verify_command_with_cancel(&exe,&["--version"],std::time::Duration::from_millis(300),
                &||Ok(cancel && env.root.join("version.pid").exists())).unwrap_err();
            assert!(started.elapsed()<std::time::Duration::from_secs(3),"{error}");
            assert!(!env.root.join("root.pid").exists()); probe.join().unwrap();
        }
        let env=Environment::new();env.case(&json!({"version_tree":true}));
        let root=env.root.clone();
        let probe=std::thread::spawn(move ||ProcessProbe::open(&root.join("child.pid")).assert_exited());
        let error=execute_codex(env.root.to_str().unwrap(),"fixture",5,None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,&||Ok(false)).unwrap_err();
        assert!(error.to_string().contains("drain deadline"),"{error}");
        assert!(!env.root.join("root.pid").exists());probe.join().unwrap();
        let env=Environment::new();env.case(&json!({"version_hang":true}));
        let error=execute_codex(env.root.to_str().unwrap(),"fixture",5,None,
            crate::runtime::deepseek_harness::WorkflowAccess::Read,&||Ok(env.root.join("version.pid").exists())).unwrap_err();
        assert!(error.to_string().contains("canceled"),"{error}");
        assert!(!env.root.join("root.pid").exists());
    }

    #[test]
    fn codex_completion_error_notification_is_not_terminal() {
        let mut state = CodexCompletion::default();
        state.observe(br#"{"type":"turn.started"}"#);
        state.observe(br#"{"type":"error","message":"fixture retry notification"}"#);
        assert!(!state.failed, "a CLI notification is not turn.failed");
        assert!(state.terminal_at.is_none(), "a notification must not start the shutdown deadline");
        assert_eq!(state.error_notifications, 1);
        assert!(state.diagnostics().contains("cli_error_event=error; error_notifications=1; terminal_event=none"));
        assert!(!state.diagnostics().contains("fixture retry notification"));
        state.observe(br#"{"type":"item.completed","item":{"type":"agent_message","text":"valid final"}}"#);
        state.observe(br#"{"type":"turn.completed"}"#);
        assert!(state.completed && !state.failed);
        assert_eq!(state.final_message.as_deref(), Some("valid final"));
        assert!(state.terminal_at.is_some());
    }

    #[test]
    fn codex_completion_completed_requires_prior_final_for_shutdown() {
        let mut state = CodexCompletion::default();
        state.observe(br#"{"type":"turn.completed"}"#);
        assert!(state.terminal_at.is_none(), "completed without a final must not claim a bound terminal turn");
        state.observe(br#"{"type":"item.completed","item":{"type":"agent_message","text":"late final"}}"#);
        assert!(state.final_message.is_none(), "an out-of-order final must not retroactively bind completion");
        state.observe(br#"{"type":"turn.failed"}"#);
        assert!(state.failed && state.terminal_at.is_some());
    }

    #[test]
    fn codex_protocol_projects_large_tool_output_and_validates_its_tail() {
        let mut state = CodexCompletion::default();
        let line = json!({"type":"item.completed","item":{"type":"command_execution",
            "aggregated_output":format!("{}{}", "x".repeat(900*1024), r#"{"type":"turn.failed"}"#)}}).to_string();
        assert!(state.observe_reader(&mut line.as_bytes()));
        assert!(!state.failed && !state.completed && state.final_message.is_none());
        assert!(!state.observe_reader(&mut format!("{line}junk").as_bytes()));
        assert!(!state.observe_reader(&mut &line.as_bytes()[..line.len()-1]));
        assert!(!state.observe_reader(&mut br#"{"type":"turn.completed"}{}"#.as_slice()));
        assert!(!state.completed, "valid prefix of a malformed record cannot complete a turn");
        let oversized = json!({"type":"item.completed","item":{"type":"agent_message","text":"x".repeat(RUNTIME_OUTPUT_LIMIT+1)}}).to_string();
        assert!(state.observe_reader(&mut oversized.as_bytes()));
        assert!(state.final_message.is_none(), "spooling must not expand the final answer limit");
    }

    #[test]
    fn codex_failure_classification_retains_no_arbitrary_error_message() {
        let mut state = CodexCompletion::default();
        state.observe(br#"{"type":"error","message":"Reconnecting status 503 Service Unavailable, secret=fixture-private"}"#);
        assert!(state.diagnostics().contains("error_class=upstream_service_unavailable"));
        state.observe(br#"{"type":"turn.failed","error":{"message":"unexpected status 503 Service Unavailable, secret=fixture-private"}}"#);
        assert!(state.failed);
        assert!(!state.diagnostics().contains("fixture-private"));
        assert_eq!(codex_error_class("secret503"), "unclassified");
    }

    #[test]
    fn codex_control_protocol_requires_object_event_item_and_error() {
        let mut state = CodexCompletion::default();
        for invalid in [r#"["turn.completed",null,null,null]"#,
            r#"{"type":"item.completed","item":["agent_message",null,"final"]}"#,
            r#"{"type":"turn.failed","error":["failure"]}"#] {
            assert!(!state.observe_reader(&mut invalid.as_bytes()));
        }
        assert!(!state.failed && !state.completed && state.final_message.is_none());
    }

    #[cfg(windows)]
    fn recoverable_error_cli_fixture() -> crate::runtime::process::codex_test_support::Environment {
        // Event order/shapes bound to the installed Codex synthetic-upstream
        // probe stdout SHA256 15ed4d543a3b2d3f6315156b3542a3d6f82fa0d2e036a60be29776b6c8bbed1d
        // (control/codex-recoverable-cli-probe/1794494556233699500-ef98ee).
        // Test text/ids are synthetic; this is not repair1's missing stdout.
        use std::sync::LazyLock;
        static EXECUTABLE: LazyLock<std::path::PathBuf> = LazyLock::new(|| {
            let root = std::env::temp_dir().join(format!("codex-retry-fixture-{}", rand::random::<u64>()));
            std::fs::create_dir_all(&root).unwrap();
            let source = root.join("fake.rs");
            std::fs::write(&source, r##"
use std::{env,fs,io::{Read,Write},thread,time::Duration};
fn main() {
 let args:Vec<String>=env::args().collect();
 if args.iter().any(|a|a=="--version") {println!("codex-cli 0.167.0-retry-fixture");return;}
 let root=env::current_dir().unwrap();
 let case=fs::read_to_string(root.join("case.json")).unwrap();
 let has=|name:&str|case.contains(&format!("\"{name}\":true"));
 let mut input=String::new();std::io::stdin().read_to_string(&mut input).unwrap();
 fs::write(root.join("root.pid"),std::process::id().to_string()).unwrap();
 println!("{{\"type\":\"thread.started\",\"thread_id\":\"fixture-thread\"}}");
 for id in ["notice-1","notice-2"] {println!("{{\"type\":\"item.completed\",\"item\":{{\"id\":\"{id}\",\"type\":\"error\",\"message\":\"fixture notice\"}}}}");}
 println!("{{\"type\":\"turn.started\"}}");
 for n in 1..=2 {println!("{{\"type\":\"error\",\"message\":\"Reconnecting... {n}/3 (fixture synthetic stream closed)\"}}");}
 std::io::stdout().flush().unwrap();
 fs::write(root.join("notification"),b"ready").unwrap();
 if has("wait_for_cancel") {
  // The caller cancels after observing notification. Hold this boundary
  // until it actually terminates the owned process; scheduler/taskkill
  // latency must not turn the fixture's 900ms work into a completion race.
  let started=std::time::Instant::now();
  while !root.join("release_work").exists() {
   assert!(started.elapsed()<Duration::from_secs(8),"cancel was not delivered");
   thread::sleep(Duration::from_millis(5));
  }
 }
 if has("turn_failed") {
  println!("{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"fixture failed turn\"}}}}");
  std::io::stdout().flush().unwrap();thread::sleep(Duration::from_secs(8));return;
 }
 // Work for 900ms after notification, well past the test's 200ms shutdown
 // grace. These are complete protocol events, not a simulated state assertion.
 for _ in 0..if has("long_work"){40}else{12} {
  thread::sleep(Duration::from_millis(75));
  println!("{{\"type\":\"item.started\",\"item\":{{\"id\":\"working\",\"type\":\"command_execution\"}}}}");
  std::io::stdout().flush().unwrap();
 }
 let text=r#"{"passed":true,"summary":"fixture recovered"}"#;
 let path=&args.windows(2).find(|a|a[0]=="-o").unwrap()[1];
 if !has("no_final") {fs::write(path,format!("{text}\r\n")).unwrap();}
 let escaped=text.replace('"',"\\\"");
 if !has("no_final") {println!("{{\"type\":\"item.completed\",\"item\":{{\"id\":\"final\",\"type\":\"agent_message\",\"text\":\"{}\"}}}}",escaped);}
 // Complete fixture filesystem work before publishing the terminal event.
 // The real 200ms shutdown deadline must measure process shutdown, not a
 // post-terminal fixture disk write contending with other regression suites.
 fs::write(root.join("finished"),b"complete").unwrap();
 if !has("no_completed") {println!("{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}");}
 std::io::stdout().flush().unwrap();
}
"##).unwrap();
            let exe = root.join("fake-codex-retry.exe");
            let output = process::hidden_command("rustc").arg(&source)
                .args(["--edition=2021", "-C", "debuginfo=0", "-o"]).arg(&exe).output().unwrap();
            assert!(output.status.success(), "fixture compile: {}", String::from_utf8_lossy(&output.stderr));
            exe
        });
        let env = crate::runtime::process::codex_test_support::Environment::new();
        std::env::set_var("HIMIND_CODEX_EXECUTABLE", &*EXECUTABLE);
        env
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_recoverable_error_keeps_working_past_shutdown_grace() {
        let _lock = crate::store::paths::test_env_lock();
        let env = recoverable_error_cli_fixture();
        env.case(&json!({}));
        let started = std::time::Instant::now();
        let text = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
            std::time::Duration::from_millis(200)).unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(800));
        assert!(env.root.join("finished").is_file(), "CLI must reach its real final and zero exit");
        assert_eq!(parse_runtime_payload(&text).unwrap()["passed"], true);
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_turn_failed_after_notification_still_has_bounded_shutdown() {
        use crate::runtime::process::codex_test_support::ProcessProbe;
        let _lock = crate::store::paths::test_env_lock();
        let env = recoverable_error_cli_fixture();
        env.case(&json!({"turn_failed":true}));
        let root = env.root.clone();
        let probe = std::thread::spawn(move || {
            let process = ProcessProbe::open(&root.join("root.pid"));
            process.assert_exited();
        });
        let started = std::time::Instant::now();
        let error = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
            std::time::Duration::from_millis(200)).unwrap_err();
        probe.join().unwrap();
        println!("failed_notification_shutdown_elapsed_ms={}",started.elapsed().as_millis());
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert!(error.to_string().contains("shutdown deadline"), "{error}");
        assert!(error.to_string().contains("error_notifications=2; terminal_event=turn.failed"), "{error}");
        assert!(!env.root.join("finished").exists());
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_cancel_after_recoverable_notification_remains_canceled() {
        let _lock = crate::store::paths::test_env_lock();
        let env = recoverable_error_cli_fixture();
        env.case(&json!({"wait_for_cancel":true}));
        let error = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read,
            &|| Ok(env.root.join("notification").is_file()), std::time::Duration::from_millis(200)).unwrap_err();
        assert!(env.root.join("notification").exists());
        assert!(error.to_string().contains("was canceled"), "{error}");
        assert!(!env.root.join("finished").exists());
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_recoverable_notifications_zero_exit_without_bound_final_is_rejected() {
        let _lock = crate::store::paths::test_env_lock();
        for case in [json!({"no_final":true}), json!({"no_completed":true}),
            json!({"no_final":true,"no_completed":true})] {
            let env = recoverable_error_cli_fixture();
            env.case(&case);
            let error = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
                None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
                std::time::Duration::from_millis(200)).unwrap_err();
            assert!(env.root.join("finished").exists(), "CLI must finish normally, not be killed on notification");
            assert!(!error.to_string().contains("shutdown deadline"), "{error}");
            assert!(error.to_string().contains("error_notifications=2; terminal_event=none"), "{error}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_recoverable_notification_obeys_execution_timeout() {
        let _lock = crate::store::paths::test_env_lock();
        let env = recoverable_error_cli_fixture();
        env.case(&json!({"long_work":true}));
        let error = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 1,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
            std::time::Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("exceeded 1 seconds"), "{error}");
        assert!(error.to_string().contains("error_notifications=2; terminal_event=none"), "{error}");
        assert!(!env.root.join("finished").exists());
    }

    #[test]
    fn codex_terminal_protocol_requires_complete_agent_message_and_turn() {
        let mut state = CodexCompletion::default();
        state.observe(br#"{"type":"turn.completed"}"#);
        assert!(state.completed);
        assert!(state.final_message.is_none());
        let mut state = CodexCompletion::default();
        state.observe(br#"{"type":"turn.started"}"#);
        state.observe(br#"{"type":"item.completed","item":{"type":"agent_message","phase":"commentary","text":"not a final"}}"#);
        assert!(state.final_message.is_none());
        state.observe(br#"{"type":"item.completed","item":{"type":"agent_message","text":"final"}}"#);
        assert!(!state.completed);
        state.observe(br#"{"type":"turn.completed"}"#);
        assert_eq!(state.final_message.as_deref(), Some("final"));
        state.observe(br#"{"type":"turn.failed"}"#);
        assert!(state.failed);
        state.observe(br#"{"type":"turn.completed"}"#);
        assert!(state.failed, "a later completed event cannot reverse failure");
        state.observe(br#"{"type":"turn.started"}"#);
        assert!(state.failed, "another turn cannot erase the terminal failure");
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_final_protocol_and_shutdown_are_fail_closed() {
        use crate::runtime::process::codex_test_support::{Environment, ProcessProbe};
        let _lock = crate::store::paths::test_env_lock();
        for (name, case, expected) in [
            ("positive", json!({}), "positive"),
            ("negative", json!({"negative":true}), "negative"),
            ("missing", json!({"missing":true}), "error"),
            ("truncated", json!({"truncated":true}), "error"),
            ("nonzero", json!({"nonzero":true,"negative":true}), "error"),
            ("no-turn", json!({"no_turn":true}), "error"),
            ("no-agent", json!({"no_agent":true}), "error"),
            ("many-events", json!({"many_events":true}), "positive"),
            ("stderr-no-newline", json!({"stderr_no_newline":true}), "positive"),
            ("large-line", json!({"large_line":true}), "positive"),
            ("large-line-negative", json!({"large_line":true,"negative":true}), "negative"),
            ("malformed-large-line", json!({"bad_large_line":true}), "error"),
            ("large-line-missing-final", json!({"large_line":true,"missing":true}), "error"),
            ("truncated-event", json!({"truncated_event":true}), "error"),
            ("shutdown-hang", json!({"hang":true,"tree":true}), "error"),
        ] {
            let env = Environment::new();
            env.case(&case);
            let root = env.root.clone();
            let probes = if case["tree"] == true {
                Some(std::thread::spawn(move || {
                    let parent = ProcessProbe::open(&root.join("root.pid"));
                    let child = ProcessProbe::open(&root.join("child.pid"));
                    parent.assert_exited(); child.assert_exited();
                }))
            } else { None };
            let started = std::time::Instant::now();
            let result = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
                None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
                std::time::Duration::from_millis(500));
            if let Some(probes) = probes { probes.join().unwrap(); }
            assert!(started.elapsed() < std::time::Duration::from_secs(5), "{name}: unbounded shutdown");
            match expected {
                "positive" => assert_eq!(parse_runtime_payload(&result.unwrap()).unwrap()["passed"], true),
                "negative" => assert_eq!(parse_runtime_payload(&result.unwrap()).unwrap()["passed"], false),
                _ => {
                    let error = result.unwrap_err();
                    if name == "nonzero" {
                        let failure = error.downcast_ref::<crate::runtime::deepseek_harness::WorkflowRuntimeFailure>().unwrap();
                        assert_eq!(failure.report.as_ref().unwrap()["passed"], false);
                        assert!(failure.message.contains("exit=7"));
                    }
                    if name == "shutdown-hang" { assert!(error.to_string().contains("shutdown deadline")); }
                    if name == "shutdown-hang" { assert!(error.to_string().contains("terminal_event=turn.completed")); }
                    if name == "malformed-large-line" { assert!(error.to_string().contains("could not be read")); }
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_exited_root_with_inherited_pipes_does_not_block_dispatch() {
        use crate::runtime::process::codex_test_support::{Environment, ProcessProbe};
        let _lock = crate::store::paths::test_env_lock();
        let env = Environment::new();
        env.case(&json!({"tree":true,"inherit_writer":true}));
        let root = env.root.clone();
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = observed.clone();
        let probe = std::thread::spawn(move || {
            let parent = ProcessProbe::open(&root.join("root.pid"));
            let child = ProcessProbe::open(&root.join("child.pid"));
            parent.assert_exited();
            assert!(child.is_alive(), "test must hold both inherited pipe writers after root exit");
            signal.store(true, std::sync::atomic::Ordering::SeqCst);
            // Retain the exact descendant handle across product cleanup.
            child.assert_exited();
        });
        crate::runtime::codex_transport::AFTER_ROOT_EXIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            let started = std::time::Instant::now();
            while !observed.load(std::sync::atomic::Ordering::SeqCst) {
                assert!(started.elapsed() < std::time::Duration::from_secs(3), "descendant was not independently observed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })));
        let started = std::time::Instant::now();
        let result = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 6,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read, &|| Ok(false),
            std::time::Duration::from_millis(500));
        crate::runtime::codex_transport::AFTER_ROOT_EXIT.with(|hook| *hook.borrow_mut() = None);
        let error = result.unwrap_err();
        assert!(error.to_string().contains("drain deadline"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        probe.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn codex_cli_cancel_wins_over_emitted_final_and_exit() {
        use crate::runtime::process::codex_test_support::Environment;
        let _lock = crate::store::paths::test_env_lock();
        let env = Environment::new();
        env.case(&json!({}));
        let error = execute_codex_with_shutdown_limit(env.root.to_str().unwrap(), "fixture", 5,
            None, crate::runtime::deepseek_harness::WorkflowAccess::Read,
            &|| Ok(env.root.join("emitted").exists()), std::time::Duration::from_millis(500)).unwrap_err();
        assert!(env.root.join("emitted").is_file(), "the CLI must really have emitted its final");
        assert!(error.to_string().contains("was canceled"), "{error}");
    }

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
    fn files_only_policy_cannot_escalate_read_or_use_external_providers() {
        use crate::runtime::deepseek_harness::WorkflowAccess;
        assert_eq!(workflow_access_for_policy("R2", "files_only"), WorkflowAccess::FilesOnly);
        assert_eq!(workflow_access_for_policy("R1", "files_only"), WorkflowAccess::Read);
        assert_eq!(workflow_access_for_policy("R2", "default"), WorkflowAccess::Write);
        assert!(!enforce_runtime_tool_policy("himind.builtin", "files_only").unwrap());
        for provider in ["personal.codex", "personal.github-copilot", "acp.claude"] {
            assert!(enforce_runtime_tool_policy(provider, "files_only").is_err());
        }
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

    /// 只读步骤必须走 read-only 权限模式：它不创建写授权，也就不要求调用方
    /// 拥有目标目录的 WRITE_DAC / WRITE_OWNER。
    #[test]
    fn only_read_only_risk_steps_request_read_only_access() {
        use crate::runtime::deepseek_harness::WorkflowAccess;
        assert_eq!(workflow_access_for_risk("read_only"), WorkflowAccess::Read);
        assert_eq!(workflow_access_for_risk("READ_ONLY"), WorkflowAccess::Read);
        assert_eq!(workflow_access_for_risk("R1"), WorkflowAccess::Read);
        // 任何可能写盘或执行进程的等级都按可写处理：宁可多要权限，
        // 也不能让需要写盘的步骤静默退化成只读。
        for risk in ["local_write", "R2", "process", "network", "R3", "R4", "system", "", "unknown"] {
            assert_eq!(workflow_access_for_risk(risk), WorkflowAccess::Write, "risk={risk}");
        }
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
    fn codex_workflow_access_preserves_read_only_verification_and_explicit_full_access() {
        for (risk, mode, expected) in [
            ("local_write", "", "workspace-write"),
            ("local_write", "workspace_write", "workspace-write"),
            ("local_write", "full_access", "danger-full-access"),
            ("read_only", "full_access", "read-only"),
            ("R1", "", "read-only"),
        ] {
            assert_eq!(codex_workflow_sandbox(workflow_access_with_permission(risk, "default", mode)).unwrap(), expected);
        }
        assert!(codex_workflow_sandbox(workflow_access_with_permission("local_write", "files_only", "full_access")).is_err());
    }

    #[test]
    fn full_access_requires_explicit_mode_and_preserves_read_and_files_only_limits() {
        use crate::runtime::deepseek_harness::WorkflowAccess;
        assert_eq!(workflow_access_with_permission("R2","default","full_access"),WorkflowAccess::Full);
        assert_eq!(workflow_access_with_permission("R2","default",""),WorkflowAccess::Write);
        assert_eq!(workflow_access_with_permission("R1","default","full_access"),WorkflowAccess::Read);
        assert_eq!(workflow_access_with_permission("R2","files_only","full_access"),WorkflowAccess::FilesOnly);
        assert!(super::super::validate_runtime_permission_mode("personal.codex","full_access").is_ok());
        assert!(super::super::validate_runtime_permission_mode("personal.github-copilot","full_access").is_err());
        assert!(super::super::validate_runtime_permission_mode("acp.claude","full_access").is_ok());
        assert!(super::super::validate_runtime_permission_mode("acp.opencode","full_access").is_ok());
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
