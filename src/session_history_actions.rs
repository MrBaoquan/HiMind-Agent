//! Native history actions. All launch inputs come from a freshly resolved history record.
use crate::session_history::{associate_workspace, SessionSource, SessionSummary};
use crate::workspace_registry::Registry;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ResumePlan {
    pub session_id: String,
    pub client: String,
    pub working_directory: String,
    pub command: String,
    pub available: bool,
}

pub(crate) fn valid_resume_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn client_name(source: SessionSource) -> Result<&'static str, String> {
    match source {
        SessionSource::ClaudeCode => Ok("claude"),
        SessionSource::Codex => Ok("codex"),
        _ => Err("SESSION_RESUME_UNSUPPORTED".into()),
    }
}

// Prefer a native executable or PowerShell shim. Running a .cmd shim would introduce
// another shell parser, so it is deliberately excluded from this launch boundary.
fn client_path(source: SessionSource) -> Option<PathBuf> {
    let name = client_name(source).ok()?;
    #[cfg(windows)]
    {
        [format!("{name}.exe"), format!("{name}.ps1")]
            .iter()
            .find_map(|name| crate::runtime::process::resolve_executable(name))
    }
    #[cfg(not(windows))]
    {
        crate::runtime::process::resolve_executable(name)
    }
}

pub(crate) fn available(source: SessionSource, id: &str) -> bool {
    cfg!(windows) && valid_resume_id(id) && client_path(source).is_some()
}

fn ps_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn build_plan(
    registry: &Registry,
    project_id: &str,
    session: &SessionSummary,
    executable: Option<&Path>,
) -> Result<ResumePlan, String> {
    let name = client_name(session.source.clone())?;
    if !valid_resume_id(&session.native_id) {
        return Err("SESSION_RESUME_ID_INVALID".into());
    }
    if session.project_id.as_deref() != Some(project_id) {
        return Err("SESSION_NOT_FOUND".into());
    }
    let directory = std::fs::canonicalize(&session.working_directory)
        .map_err(|_| "SESSION_DIRECTORY_UNAVAILABLE")?;
    if !directory.is_dir() {
        return Err("SESSION_DIRECTORY_UNAVAILABLE".into());
    }
    let cwd = directory.to_string_lossy().to_string();
    if associate_workspace(registry, &cwd)?.is_none_or(|hit| hit.project_id != project_id) {
        return Err("SESSION_DIRECTORY_CHANGED".into());
    }
    let program = executable
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| name.into());
    let args = match session.source {
        SessionSource::ClaudeCode => format!("'--resume' {}", ps_literal(&session.native_id)),
        SessionSource::Codex => format!("'resume' {}", ps_literal(&session.native_id)),
        _ => unreachable!(),
    };
    Ok(ResumePlan {
        session_id: session.id.clone(),
        client: session.source.label().into(),
        command: format!(
            "Set-Location -LiteralPath {}; & {} {args}",
            ps_literal(&cwd),
            ps_literal(&program)
        ),
        working_directory: cwd,
        available: cfg!(windows) && executable.is_some(),
    })
}

pub(crate) fn resolve(project_id: &str, session_id: &str) -> Result<ResumePlan, String> {
    let registry = Registry::load().map_err(|e| e.to_string())?;
    if registry.project(project_id).is_none() {
        return Err("PROJECT_NOT_FOUND".into());
    }
    let (source, _) = session_id.split_once(':').ok_or("SESSION_ID_INVALID")?;
    let scan = match source {
        "claude_code" => crate::session_history_sources::scan_claude_code(&registry),
        "codex" => crate::session_history_sources::scan_codex(&registry),
        _ => return Err("SESSION_RESUME_UNSUPPORTED".into()),
    };
    let session = scan
        .sessions
        .iter()
        .find(|s| s.id == session_id && s.project_id.as_deref() == Some(project_id))
        .ok_or("SESSION_NOT_FOUND")?;
    build_plan(
        &registry,
        project_id,
        session,
        client_path(session.source.clone()).as_deref(),
    )
}

pub(crate) fn launch_with(
    plan: &ResumePlan,
    launch: impl FnOnce(&ResumePlan) -> Result<(), String>,
) -> Result<(), String> {
    if !plan.available {
        return Err("SESSION_CLIENT_NOT_FOUND".into());
    }
    static LAUNCHES: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    let mut launches = LAUNCHES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "SESSION_LAUNCH_BUSY")?;
    launches.retain(|_, time| time.elapsed() < Duration::from_secs(5));
    if launches.contains_key(&plan.session_id) {
        return Err("SESSION_ALREADY_LAUNCHING".into());
    }
    launch(plan)?;
    launches.insert(plan.session_id.clone(), Instant::now());
    Ok(())
}

pub(crate) fn launch(plan: &ResumePlan) -> Result<(), String> {
    launch_with(plan, |plan| {
        #[cfg(windows)]
        {
            use base64::Engine;
            use std::os::windows::process::CommandExt;
            let script = format!(
                "$ErrorActionPreference='Stop'; try {{ {} }} catch {{ Write-Error $_ }}",
                plan.command
            );
            let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let powershell = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .ok_or("SESSION_TERMINAL_UNAVAILABLE")?
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let mut command = std::process::Command::new(powershell);
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NoExit",
                    "-EncodedCommand",
                    &base64::engine::general_purpose::STANDARD.encode(bytes),
                ])
                .current_dir(&plan.working_directory)
                .creation_flags(0x00000010);
            crate::runtime::process::remove_himind_secret_environment(&mut command);
            let mut child = command.spawn().map_err(|_| "SESSION_LAUNCH_FAILED")?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = plan;
            Err("SESSION_TERMINAL_UNSUPPORTED".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session_history::SessionCapabilities,
        workspace_registry::{RegistryProject, RegistryWorkspace},
    };

    #[test]
    fn session_history_resume_rejects_flags_shell_and_ambiguous_ids() {
        for bad in [
            "", "--help", "a b", "a;echo", "a\nb", "$(echo)", "a'", "a&b", "a:other",
        ] {
            assert!(!valid_resume_id(bad), "{bad}");
        }
        assert!(valid_resume_id("0190-abCD_123"));
        assert_eq!(ps_literal("a'b&$(bad)"), "'a''b&$(bad)'");
    }

    #[test]
    fn session_history_resume_revalidates_directory_and_uses_launch_double() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("history-resume-{}-quote'&", rand::random::<u128>()));
        std::fs::create_dir_all(&root).unwrap();
        let registry = Registry {
            projects: vec![RegistryProject {
                id: "p".into(),
                name: "P".into(),
                ..Default::default()
            }],
            workspaces: vec![RegistryWorkspace {
                id: "w".into(),
                project_id: "p".into(),
                path: root.to_string_lossy().into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let summary = SessionSummary::new(
            SessionSource::Codex,
            format!("{:032x}", rand::random::<u128>()),
            root.to_string_lossy().into(),
            "test".into(),
            None,
            None,
            SessionCapabilities::default(),
            Some(crate::session_history::WorkspaceAssociation {
                project_id: "p".into(),
                project_name: "P".into(),
                binding_path: root.to_string_lossy().into(),
            }),
        )
        .unwrap();
        let plan = build_plan(
            &registry,
            "p",
            &summary,
            Some(Path::new("C:/client'&/codex.ps1")),
        )
        .unwrap();
        assert!(plan.command.contains("client''&"));
        assert!(plan.command.contains("'resume'"));
        assert!(!plan.command.contains("--continue"));
        let plan = ResumePlan {
            available: true,
            ..plan
        };
        let called = std::cell::Cell::new(false);
        launch_with(&plan, |_| {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(
            launch_with(&plan, |_| panic!("duplicate launch")),
            Err("SESSION_ALREADY_LAUNCHING".into())
        );
        assert!(build_plan(&registry, "other", &summary, None).is_err());
        std::fs::remove_dir_all(&root).unwrap();
        assert!(build_plan(&registry, "p", &summary, None).is_err());
    }
}
