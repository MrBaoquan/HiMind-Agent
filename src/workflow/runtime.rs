use serde_json::{json, Value};
use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};

use super::{WorkflowPackage, WorkflowStep, WorkflowStepExecution};
use crate::runtime::{codex, copilot, process};
use crate::Options;

const RUNTIME_OUTPUT_LIMIT: usize = 64 * 1024;

pub(crate) fn execute_runtime_step(
    package: &WorkflowPackage,
    step: &WorkflowStep,
    input: &Value,
    options: Option<&Options>,
) -> Result<WorkflowStepExecution, Box<dyn Error>> {
    let runtime = step
        .runtime
        .as_ref()
        .ok_or_else(|| format!("workflow runtime step {} is missing runtime", step.id))?;
    let provider = resolve_provider(&runtime.provider)?;
    let workspace = resolve_workspace(&runtime.workspace_path, input)?;
    let prompt = build_prompt(&runtime.prompt, input);
    let output = match provider.as_str() {
        "personal.codex" => execute_codex(&workspace, &prompt)?,
        "personal.github-copilot" => execute_copilot(&workspace, &prompt)?,
        "himind.fixture" => fixture_runtime_output(&step.id),
        "himind.builtin" => crate::runtime::deepseek_harness::execute_workflow(
            options.ok_or("himind.builtin runtime step is unavailable without Agent options")?,
            &workspace,
            &prompt,
        )?,
        value => return Err(format!("unsupported workflow runtime provider: {value}").into()),
    };
    let structured = parse_runtime_payload(&output);
    let mut result = json!({
        "schema_version": "workflow_runtime_result.v1",
        "provider": provider,
        "workspace": workspace,
        "summary": output,
    });
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
    let candidates = std::iter::once(trimmed).chain(
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
        return Err("no local AI Runtime Provider is available for runtime step".into());
    }
    Ok(provider.to_string())
}

fn fixture_runtime_output(step_id: &str) -> String {
    if step_id.contains("REVIEW") {
        serde_json::json!({
            "feedback": {
                "decision": "accepted",
                "reason": "fixture review accepted the current change"
            }
        })
        .to_string()
    } else {
        serde_json::json!({
            "summary": "fixture runtime applied the requested development change",
            "changes": ["fixture change"],
            "verification": [{"name": "fixture", "status": "passed"}]
        })
        .to_string()
    }
}

fn resolve_workspace(template: &str, input: &Value) -> Result<String, Box<dyn Error>> {
    let template = template.trim();
    let value = if template.is_empty() {
        input
            .get("project_root")
            .or_else(|| input.get("workspace_root"))
            .and_then(Value::as_str)
    } else if let Some(key) = template.strip_prefix("input.") {
        input.get(key).and_then(Value::as_str)
    } else if let Some(key) = template.strip_prefix("workflow_context.") {
        input
            .get("workflow_context")
            .and_then(|context| context.get(key))
            .and_then(Value::as_str)
    } else {
        Some(template)
    }
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .ok_or("workflow runtime step workspace is unavailable")?;
    let workspace = process::canonical_workspace(value)?;
    Ok(workspace.to_string_lossy().to_string())
}

fn build_prompt(base: &str, input: &Value) -> String {
    let mut prompt = base.trim().to_string();
    prompt.push_str("\n\nWorkflow context (JSON):\n");
    prompt.push_str(
        &serde_json::to_string_pretty(&redact_runtime_context(input)).unwrap_or_default(),
    );
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

fn execute_codex(workspace: &str, prompt: &str) -> Result<String, Box<dyn Error>> {
    let (executable, _) = codex::resolve_codex_executable()?;
    let result_path = process::safe_temp_path(
        &format!("workflow-codex-{}", std::process::id()),
        "result.txt",
    )?;
    let _ = std::fs::remove_file(&result_path);
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
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(prompt.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "Codex runtime step failed: {}",
            process::summarize_output(
                String::from_utf8_lossy(&output.stderr).trim(),
                RUNTIME_OUTPUT_LIMIT
            )
        )
        .into());
    }
    if result_path.is_file() {
        let result = std::fs::read_to_string(&result_path)?;
        let _ = std::fs::remove_file(&result_path);
        return Ok(process::summarize_output(
            result.trim(),
            RUNTIME_OUTPUT_LIMIT,
        ));
    }
    let _ = std::fs::remove_file(&result_path);
    Ok(process::summarize_output(
        String::from_utf8_lossy(&output.stdout).trim(),
        RUNTIME_OUTPUT_LIMIT,
    ))
}

fn execute_copilot(workspace: &str, prompt: &str) -> Result<String, Box<dyn Error>> {
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
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "GitHub Copilot runtime step failed: {}",
            process::summarize_output(
                String::from_utf8_lossy(&output.stderr).trim(),
                RUNTIME_OUTPUT_LIMIT
            )
        )
        .into());
    }
    Ok(process::summarize_output(
        String::from_utf8_lossy(&output.stdout).trim(),
        RUNTIME_OUTPUT_LIMIT,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
