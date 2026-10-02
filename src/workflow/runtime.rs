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
    let provider = resolve_provider(&runtime.provider)?;
    enforce_runtime_network_policy(&provider, runtime.allow_network)?;
    let disable_tools = enforce_runtime_tool_policy(&provider, &runtime.tool_policy)?;
    let workspace = resolve_workspace(&runtime.workspace_path, input)?;
    // 声明了 input_artifacts 的步骤不从提示词里带上游输出：数据已经以文件路径
    // 注入 input_artifacts，提示词长度因此与 Artifact 体量无关。
    let prompt_input = if runtime.input_artifacts.is_empty() {
        input.clone()
    } else {
        without_step_outputs(input)
    };
    let prompt = build_prompt(&runtime.prompt, &prompt_input);
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
        "personal.codex" => execute_codex(&workspace, &prompt, timeout_seconds, is_canceled)?,
        "personal.github-copilot" => {
            execute_copilot(&workspace, &prompt, timeout_seconds, is_canceled)?
        }
        "himind.fixture" => fixture_runtime_output(&step.id, &workspace, input)?,
        "himind.builtin" => {
            let outcome = crate::runtime::deepseek_harness::execute_workflow(
                options
                    .ok_or("himind.builtin runtime step is unavailable without Agent options")?,
                &workspace,
                &prompt,
                timeout_seconds,
                disable_tools,
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
            let execution = crate::runtime::acp::execute_workflow(
                provider,
                &workspace,
                &prompt,
                timeout_seconds,
                run_id,
                is_canceled,
            )?;
            // ACP 步骤把真实会话事实留痕：工作台需要能回答“这一步由哪个 ACP 会话
            // 执行、真实 Agent 发起了多少次工具调用与权限请求”，否则链路不可审计。
            acp_facts = Some(json!({
                "provider": provider,
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

fn resolve_provider(provider: &str) -> Result<String, Box<dyn Error>> {
    let provider = provider.trim();
    if provider == "auto" {
        if std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1") {
            return Ok("himind.fixture".to_string());
        }
        if codex::probe().status == "ready" {
            return Ok("personal.codex".to_string());
        }
        if copilot::probe().status == "ready" {
            return Ok("personal.github-copilot".to_string());
        }
        if crate::runtime::deepseek_harness::probe().status == "ready" {
            return Ok("himind.builtin".to_string());
        }
        return Err("no local AI Runtime Provider is available for runtime step".into());
    }
    Ok(provider.to_string())
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
        "none" => match provider {
            "himind.builtin" | "himind.fixture" => Ok(true),
            other => Err(format!(
                "workflow runtime provider {other} cannot enforce tool_policy=none; use a Runtime that can mount the step without model-facing tools"
            )
            .into()),
        },
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

fn build_prompt(base: &str, input: &Value) -> String {
    let mut prompt = base.trim().to_string();
    prompt.push_str("\n\nWorkflow context (JSON):\n");
    // 紧凑 JSON：提示词最终要通过命令行传给 Runtime（Windows 整条命令行 ~32KB），
    // 缩进展开会让大 Artifact 白白多花三成体积。
    prompt.push_str(&serde_json::to_string(&redact_runtime_context(input)).unwrap_or_default());
    prompt
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
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let (executable, _) = codex::resolve_codex_executable()?;
    let result_path = process::safe_temp_path(
        &format!("workflow-codex-{}", rand::random::<u64>()),
        "result.txt",
    )?;
    process::remove_file_if_present(&result_path);
    let mut command = Command::new(executable);
    command
        .args([
            "-C",
            workspace,
            "-s",
            "workspace-write",
            "-a",
            "never",
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
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let (executable, _) = copilot::resolve_copilot_executable()?;
    let mut command = Command::new(executable);
    command
        .args(["-p", prompt, "-s", "--no-ask-user", "--allow-all-tools"])
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

    #[test]
    fn rejects_network_disabled_policy_for_unisolated_provider() {
        assert!(enforce_runtime_network_policy("personal.codex", Some(false)).is_err());
        assert!(enforce_runtime_network_policy("personal.github-copilot", Some(false)).is_err());
        assert!(enforce_runtime_network_policy("himind.fixture", Some(false)).is_ok());
        assert!(enforce_runtime_network_policy("personal.codex", Some(true)).is_ok());
        // 未声明网络约束时不得把「无法证明隔离」当成拒绝服务的理由。
        assert!(enforce_runtime_network_policy("personal.codex", None).is_ok());
    }
}
