//! Execute only the checks in the confirmed task, collecting host-owned exit codes.
use super::runtime_resources::CheckRuntimeResources;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HostCheck {
    pub command: String,
    pub exit_code: i32,
    pub output_sha256: String,
    #[serde(default)]
    pub test_summary: String,
    pub elapsed_ms: u64,
    pub error: String,
}

pub(crate) fn run_required_checks(
    workspace: &Path,
    checks: &[String],
    timeout: Duration,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
) -> Vec<HostCheck> {
    run_required_checks_inner(workspace, checks, timeout, is_canceled, None)
}

pub(crate) fn run_required_checks_with_resources(
    workspace: &Path,
    checks: &[String],
    timeout: Duration,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    resources: &CheckRuntimeResources,
) -> Vec<HostCheck> {
    run_required_checks_inner(workspace, checks, timeout, is_canceled, Some(resources))
}

fn run_required_checks_inner(
    workspace: &Path,
    checks: &[String],
    timeout: Duration,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    resources: Option<&CheckRuntimeResources>,
) -> Vec<HostCheck> {
    let started = Instant::now();
    checks
        .iter()
        .map(|text| {
            let check_started = Instant::now();
            let result = timeout
                .checked_sub(started.elapsed())
                .ok_or_else(|| "Mission check time budget exhausted".into())
                .and_then(|remaining| {
                    run_check(
                        workspace,
                        text,
                        remaining.min(Duration::from_secs(300)),
                        is_canceled,
                        resources,
                    )
                });
            let (exit_code, output_sha256, test_summary, error) = match result {
                Ok((code, digest, summary)) => (code, digest, summary, String::new()),
                Err(error) => (-1, String::new(), String::new(), error.to_string()),
            };
            HostCheck {
                command: text.clone(),
                exit_code,
                output_sha256,
                test_summary,
                elapsed_ms: check_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                error,
            }
        })
        .collect()
}

pub(crate) fn validate_required_check(text: &str) -> Result<(), Box<dyn Error>> {
    let words = command_words(text)?;
    let program = words.first().ok_or("required check command is empty")?;
    if !matches!(
        program.as_str(),
        "cargo" | "npm" | "node" | "python" | "python3" | "pytest" | "dotnet" | "ctest" | "git"
    ) {
        return Err(format!("unsupported required check executable: {program}").into());
    }
    if program == "git"
        && !(words.get(1).map(String::as_str) == Some("diff")
            && words.iter().any(|word| word == "--check"))
    {
        return Err("only git diff --check is supported as a Git required check".into());
    }
    Ok(())
}

/// Read only aggregate usage from a DSH run-scoped session projection.
/// Session content is never returned or persisted by this function.
pub(crate) fn read_projection_usage(
    run_home: &Path,
) -> Result<Option<crate::agent_core_contracts::LocalRunUsage>, Box<dyn Error>> {
    let sessions = run_home
        .join("storages")
        .join("session_projcache")
        .join("sessions");
    if !sessions.is_dir() {
        return Ok(None);
    }
    let mut files = std::fs::read_dir(&sessions)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    files.sort();
    let mut uncached_input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut found = false;
    for path in files.into_iter().take(32) {
        if std::fs::metadata(&path)?.len() > 8 * 1024 * 1024 {
            return Err("DSH session projection exceeds the usage-read limit".into());
        }
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let Some(totals) = value.pointer("/record/rows/tokenUsage/val/totals") else {
            continue;
        };
        let Some(input) = totals.get("uncachedInputTokens").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let Some(output) = totals.get("outputTokens").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let cache_read = totals.get("cacheReadTokens").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let cache_write = totals.get("cacheWriteTokens").and_then(serde_json::Value::as_u64).unwrap_or(0);
        // The hard token budget must use the same conservative accounting as the
        // Runtime total. Cached reads are still consumed input and must not be
        // silently excluded from a limit the user believes is complete.
        uncached_input_tokens = uncached_input_tokens.saturating_add(input);
        cache_read_tokens = cache_read_tokens.saturating_add(cache_read);
        cache_write_tokens = cache_write_tokens.saturating_add(cache_write);
        output_tokens = output_tokens.saturating_add(output);
        found = true;
    }
    Ok(found.then(|| crate::agent_core_contracts::LocalRunUsage {
        input_tokens: uncached_input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_write_tokens),
        output_tokens,
        estimated_cost: 0.0,
        currency: String::new(),
        billing_owner: "local".to_string(),
    }))
}

fn run_check(
    workspace: &Path,
    text: &str,
    timeout: Duration,
    is_canceled: &dyn Fn() -> Result<bool, Box<dyn Error>>,
    resources: Option<&CheckRuntimeResources>,
) -> Result<(i32, String, String), Box<dyn Error>> {
    use crate::runtime::process;
    if is_canceled()? || timeout.is_zero() {
        return Err("Mission check canceled or timed out".into());
    }
    validate_required_check(text)?;
    let words = command_words(text)?;
    let program = &words[0];
    let executable = process::resolve_executable(program)
        .ok_or_else(|| format!("check executable unavailable: {program}"))?;
    let mut command = if cfg!(windows) && program == "npm" {
        let node = process::resolve_executable("node").ok_or("node is required for npm checks")?;
        let npm_cli = executable
            .parent()
            .ok_or("npm executable has no parent")?
            .join("node_modules/npm/bin/npm-cli.js");
        if !npm_cli.is_file() {
            return Err("npm-cli.js is unavailable; a shell fallback is not permitted".into());
        }
        let mut cmd = process::hidden_command(node);
        cmd.arg(npm_cli);
        cmd
    } else {
        process::hidden_command(executable)
    };
    command
        .args(&words[1..])
        .current_dir(process::canonical_workspace(workspace.to_str().ok_or("check workspace must be Unicode")?)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::remove_himind_secret_environment(&mut command);
    if let Some(resources) = resources {
        resources.apply_for_workspace(workspace, &mut command)?;
    }
    let (mut child,tree) = crate::runtime::owned_tree::OwnedTree::spawn(&mut command)?;
    let stdout = child.stdout.take().map(process::capture_output);
    let stderr = child.stderr.take().map(process::capture_output);
    let status = process::wait_for_child_until_with_cancel(
        &mut child,
        timeout,
        "Mission required check",
        &mut || is_canceled(),
    );
    if status.is_err() {
        process::terminate_process_tree(&mut child);
    }
    drop(tree);
    let out = process::join_output(stdout);
    let err = process::join_output(stderr);
    let status = status?;
    let digest = format!("{:x}", Sha256::digest(format!("{out}\n{err}").as_bytes()));
    // Numeric TAP and fixed, nonsecret remediation diagnostics only. Never
    // expose arbitrary stdout/stderr, exception messages or absolute paths.
    let summary = out
        .lines()
        .chain(err.lines())
        .filter(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() == 3
                && fields[0] == "#"
                && matches!(
                    fields[1],
                    "tests" | "suites" | "pass" | "fail" | "cancelled" | "skipped" | "todo"
                )
                && fields[2].parse::<u64>().is_ok()) || safe_remediation_diagnostic(line)
        })
        .take(16)
        .collect::<Vec<_>>()
        .join("\n");
    Ok((status.code().unwrap_or(-1), digest, summary))
}

fn safe_remediation_diagnostic(line:&str)->bool {
    let fields=line.split_whitespace().collect::<Vec<_>>();
    fields.len()==4 && fields[0]=="#" && fields[1]=="remediation"
        && matches!(fields[2],"missing_preparation"|"candidate_mismatch"|"product_regression_failed"|
            "live_evidence_missing"|"check_deadline"|"prepare_failed"|"unexpected_failure")
        && fields[3].len()<=180 && fields[3].starts_with("executor-lifecycle-repair/")
        && fields[3].split('/').all(|p|!p.is_empty() && p!="." && p!="..")
        && fields[3].bytes().all(|b|b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}

fn command_words(text: &str) -> Result<Vec<String>, Box<dyn Error>> {
    if text.len() > 4096 || text.chars().any(char::is_control) {
        return Err("required checks must be direct commands without shell operators".into());
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut in_word = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if q == c => quote = None,
            (Some(_), c) => word.push(c),
            // We execute argv directly, without a shell. Operators inside a
            // quoted argument are literal program input (e.g. python -c).
            (None, '|' | '&' | ';' | '<' | '>' | '`' | '$') => {
                return Err("required checks must be direct commands without shell operators".into());
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("required check contains an unclosed quote".into());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::runtime_resources::test_support::Fixture;

    #[test]
    fn remediation_diagnostics_are_bounded_allowlisted_and_relative() {
        assert!(safe_remediation_diagnostic("# remediation missing_preparation executor-lifecycle-repair/checks-123.json"));
        for line in ["# remediation secret executor-lifecycle-repair/checks.json",
            "# remediation candidate_mismatch C:/private/file", "# remediation candidate_mismatch executor-lifecycle-repair/../secret",
            "# remediation candidate_mismatch executor-lifecycle-repair/a?token=secret",
            "# remediation candidate_mismatch executor-lifecycle-repair/a extra"] {assert!(!safe_remediation_diagnostic(line));}
        assert!(!safe_remediation_diagnostic(&format!("# remediation candidate_mismatch executor-lifecycle-repair/{}","a".repeat(180))));
    }

    #[test]
    fn resources_reach_real_node_before_script_and_preserve_candidate() {
        assert!(crate::runtime::process::resolve_executable("node").is_some(), "Node is required for this test");
        let fixture = Fixture::new();
        let keys = [
            "HIMIND_MISSION_CHECK_ROOT", "SERVICE_UI_RUNTIME_ROOT", "TEMP", "TMP", "TMPDIR",
            "npm_config_cache", "npm_config_update_notifier", "PYTHONDONTWRITEBYTECODE",
        ];
        let parent_env = keys.map(std::env::var_os);
        let resources = fixture.prepare();
        let candidate = super::super::scoped_candidate_identity(&fixture.workspace, ["."]).unwrap();
        let script = r#"
            const assert=require('node:assert/strict'),fs=require('node:fs'),p=require('node:path');
            const root=process.env.HIMIND_MISSION_CHECK_ROOT;
            assert.ok(root);
            assert.ok(p.isAbsolute(root));
            assert.ok(!root.startsWith(String.fromCharCode(92,92,63,92)));
            assert.equal(process.env.SERVICE_UI_RUNTIME_ROOT,root);
            assert.equal(process.env.TEMP,p.join(root,'tmp'));
            assert.equal(process.env.TMP,process.env.TEMP);
            assert.equal(process.env.TMPDIR,process.env.TEMP);
            assert.equal(process.env.npm_config_cache,p.join(root,'npm-cache'));
            assert.equal(process.env.npm_config_update_notifier,'false');
            assert.equal(process.env.PYTHONDONTWRITEBYTECODE,'1');
            assert.ok(fs.statSync(process.env.TEMP).isDirectory());
            assert.ok(fs.statSync(process.env.npm_config_cache).isDirectory());
            assert.ok(p.relative(process.cwd(),root).startsWith('..'));
            for(const [dir,name] of [[root,'screenshot.png'],[root,'build.js'],[process.env.TEMP,'temp.txt'],[process.env.npm_config_cache,'cache.txt']]){
                fs.writeFileSync(p.join(dir,name),'generated');
            }
            fs.writeFileSync(p.join(root,'env-evidence.json'),JSON.stringify({root,cwd:process.cwd()}));
            console.log('# tests 1');console.log('# pass 1');
        "#;
        let command = format!("node -e \"{}\"", script.lines().map(str::trim).collect::<Vec<_>>().join(" "));
        let results = run_required_checks_with_resources(
            &fixture.workspace, &[command.clone()], Duration::from_secs(10), &|| Ok(false), &resources,
        );
        assert_eq!(results[0].exit_code, 0, "{:?}", results[0]);
        assert_eq!(results[0].command, command);
        assert_eq!(results[0].test_summary, "# tests 1\n# pass 1");
        assert_eq!(results[0].output_sha256.len(), 64);
        assert_eq!(candidate, super::super::scoped_candidate_identity(&fixture.workspace, ["."]).unwrap());
        assert_eq!(parent_env, keys.map(std::env::var_os));
        let input = resources.input();
        let root = Path::new(input["root"].as_str().unwrap());
        for relative in ["screenshot.png", "build.js", "tmp/temp.txt", "npm-cache/cache.txt", "env-evidence.json"] {
            assert!(root.join(relative).is_file(), "{relative}");
        }
        let evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("env-evidence.json")).unwrap()).unwrap();
        assert_eq!(evidence["root"], input["root"]);
        assert_eq!(std::fs::canonicalize(evidence["cwd"].as_str().unwrap()).unwrap(), std::fs::canonicalize(&fixture.workspace).unwrap());
    }

    #[test]
    fn resources_preserve_real_node_failure_exit_code() {
        assert!(crate::runtime::process::resolve_executable("node").is_some(), "Node is required for this test");
        let fixture = Fixture::new();
        let resources = fixture.prepare();
        let results = run_required_checks_with_resources(
            &fixture.workspace,
            &[r#"node -e "require('node:fs').writeFileSync(require('node:path').join(process.env.HIMIND_MISSION_CHECK_ROOT,'failure.txt'),'evidence');process.exit(7)""#.to_owned()],
            Duration::from_secs(10), &|| Ok(false), &resources,
        );
        assert_eq!(results[0].exit_code, 7, "{:?}", results[0]);
        assert!(results[0].error.is_empty());
        assert_eq!(results[0].output_sha256.len(), 64);
        assert!(Path::new(resources.input()["root"].as_str().unwrap()).join("failure.txt").is_file());
    }

    #[test]
    fn legacy_checks_keep_inherited_environment_without_enabling_resources() {
        assert!(crate::runtime::process::resolve_executable("node").is_some(), "Node is required for this test");
        let fixture = Fixture::new();
        let keys = ["HIMIND_MISSION_CHECK_ROOT", "SERVICE_UI_RUNTIME_ROOT", "TEMP", "TMP", "TMPDIR",
            "npm_config_cache", "npm_config_update_notifier", "PYTHONDONTWRITEBYTECODE"];
        let expected = keys.iter().filter_map(|key| std::env::var(key).ok().map(|value| (*key, value)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let expected = serde_json::to_string(&expected).unwrap();
        let script = format!(
            "const assert=require('node:assert/strict');const expected=JSON.parse(String.fromCharCode({}));for(const key of String.fromCharCode({}).split(',')){{assert.equal(process.env[key],expected[key]);}}",
            expected.bytes().map(|b| b.to_string()).collect::<Vec<_>>().join(","),
            keys.join(",").bytes().map(|b| b.to_string()).collect::<Vec<_>>().join(","),
        );
        // Long task resource paths can make encoded env fixtures exceed the
        // real 4096-byte command boundary. Keep the boundary and run a script
        // from this generated fixture workspace instead.
        std::fs::write(fixture.workspace.join("legacy-env.js"),script).unwrap();
        let results = run_required_checks(
            &fixture.workspace, &["node legacy-env.js".into()], Duration::from_secs(10), &|| Ok(false),
        );
        assert_eq!(results[0].exit_code, 0, "{:?}", results[0]);
        assert!(!fixture.cache.exists());
    }

    #[test]
    fn resources_for_another_workspace_fail_before_node_spawn() {
        assert!(crate::runtime::process::resolve_executable("node").is_some(), "Node is required for this test");
        let fixture = Fixture::new();
        let resources = fixture.prepare();
        let other = fixture.root.join("other-workspace");
        std::fs::create_dir(&other).unwrap();
        let results = run_required_checks_with_resources(
            &other, &[r#"node -e "require('node:fs').writeFileSync('must-not-run','bad')""#.to_owned()],
            Duration::from_secs(10), &|| Ok(false), &resources,
        );
        assert_eq!(results[0].exit_code, -1);
        assert!(results[0].error.contains("different workspace"));
        assert!(!other.join("must-not-run").exists());
    }

    #[test]
    fn command_parser_preserves_arguments_and_rejects_shells() {
        assert_eq!(
            command_words(r#"cargo test "two words""#).unwrap(),
            vec!["cargo", "test", "two words"]
        );
        assert_eq!(command_words(r#"python -c "import json;json.load(open('reports/visual.json',encoding='utf-8'))""#).unwrap(),
            vec!["python", "-c", "import json;json.load(open('reports/visual.json',encoding='utf-8'))"]);
        for text in [
            "cargo test && echo ok",
            "cargo test | cat",
            "cargo test;exit",
            "cargo 'test",
        ] {
            assert!(command_words(text).is_err(), "{text}");
        }
    }
    #[test]
    fn host_records_real_failure_instead_of_accepting_agent_pass() {
        if crate::runtime::process::resolve_executable("node").is_none() {
            return;
        }
        let results = run_required_checks(
            Path::new("."),
            &[r#"node -e "process.exit(7)""#.to_string()],
            Duration::from_secs(10),
            &|| Ok(false),
        );
        assert_eq!(results[0].exit_code, 7);
        assert_eq!(results[0].output_sha256.len(), 64);
        let checks = run_required_checks(Path::new("."),
            &[r#"node -e "const a=1;process.exit(a > 0 ? 0 : 9)""#.to_string()],
            Duration::from_secs(10), &|| Ok(false));
        assert_eq!(checks[0].exit_code, 0, "{:?}", checks[0]);
    }
    #[test]
    fn projection_usage_reads_only_aggregate_token_totals() {
        let root = std::env::temp_dir().join(format!(
            "himind-mission-usage-{}-{}",
            std::process::id(), rand::random::<u64>()
        ));
        let sessions = root.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("session-a.json"), serde_json::json!({
            "record": {"rows": {"tokenUsage": {"val": {"totals": {
                "uncachedInputTokens": 120, "outputTokens": 30,
                "cacheReadTokens": 500, "cacheWriteTokens": 4
            }}}}, "private": "must not be returned"}
        }).to_string()).unwrap();
        let usage = read_projection_usage(&root).unwrap().unwrap();
        assert_eq!(usage.input_tokens, 624);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.estimated_cost, 0.0);
        let _ = std::fs::remove_dir_all(root);
    }

}
