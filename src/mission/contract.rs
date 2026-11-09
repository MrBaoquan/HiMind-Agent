use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Component, Path};

const MAX_ID_BYTES: usize = 160;
const MAX_TITLE_CHARS: usize = 240;
const MAX_SUMMARY_CHARS: usize = 2_000;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_TASKS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MissionStatus {
    Draft,
    Planned,
    AwaitingConfirmation,
    Active,
    Waiting,
    Completed,
    Failed,
    Canceled,
}

impl MissionStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Planned => "planned",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "draft" => Ok(Self::Draft),
            "planned" => Ok(Self::Planned),
            "awaiting_confirmation" => Ok(Self::AwaitingConfirmation),
            "active" => Ok(Self::Active),
            "waiting" => Ok(Self::Waiting),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unknown mission status: {other}").into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStatus {
    Proposed,
    Blocked,
    Ready,
    Running,
    AwaitingVerification,
    Failed,
    Completed,
    Canceled,
}

impl TaskStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Blocked => "blocked",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::AwaitingVerification => "awaiting_verification",
            Self::Failed => "failed",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "blocked" => Ok(Self::Blocked),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "awaiting_verification" => Ok(Self::AwaitingVerification),
            "failed" => Ok(Self::Failed),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unknown task status: {other}").into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionBinding {
    pub executor: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub role_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionTaskSpec {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub required_checks: Vec<String>,
    pub execution: ExecutionBinding,
    #[serde(default)]
    pub repair_of: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionBudget {
    pub max_rounds: u32,
    pub max_minutes: u32,
    pub max_repair_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionPlanProposal {
    pub summary: String,
    pub tasks: Vec<MissionTaskSpec>,
    pub required_acceptance: Vec<String>,
    pub budget: MissionBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionWorkspace {
    pub root: String,
    #[serde(default)]
    pub read_scope: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionCreateInput {
    pub title: String,
    #[serde(default)]
    pub objective: String,
    pub workspace: MissionWorkspace,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionConfirmation {
    pub plan_sha256: String,
    pub selected_task_ids: Vec<String>,
    pub approved_paths: Vec<String>,
    pub confirmed_by: String,
    pub confirmed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionTaskView {
    pub spec: MissionTaskSpec,
    pub status: TaskStatus,
    pub attempt_run_id: String,
    pub verification_passed: Option<bool>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionView {
    pub schema_version: String,
    pub mission_id: String,
    pub title: String,
    pub objective: String,
    pub status: MissionStatus,
    pub workspace: MissionWorkspace,
    pub acceptance_criteria: Vec<String>,
    pub plan_revision: u64,
    pub plan_sha256: String,
    pub plan_summary: String,
    pub required_acceptance: Vec<String>,
    /// Tasks in the current plan revision before confirmation. These are a
    /// review projection only; they never make a task dispatchable.
    #[serde(default)]
    pub proposed_tasks: Vec<MissionTaskSpec>,
    pub budget: Option<MissionBudget>,
    pub confirmation: Option<MissionConfirmation>,
    pub tasks: Vec<MissionTaskView>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionEvent {
    pub event_id: String,
    pub mission_id: String,
    pub sequence: u64,
    pub event_type: String,
    #[serde(default)]
    pub task_id: String,
    pub summary: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub occurred_at: String,
}

pub(crate) fn validate_create(input: &MissionCreateInput) -> Result<(), Box<dyn Error>> {
    require_text("title", &input.title, MAX_TITLE_CHARS)?;
    if input.objective.chars().count() > MAX_SUMMARY_CHARS {
        return Err(format!("objective exceeds {MAX_SUMMARY_CHARS} characters").into());
    }
    require_text("workspace.root", &input.workspace.root, MAX_PATH_CHARS)?;
    validate_paths("workspace.read_scope", &input.workspace.read_scope)?;
    validate_paths("workspace.write_scope", &input.workspace.write_scope)?;
    validate_criteria("acceptance_criteria", &input.acceptance_criteria)?;
    Ok(())
}

pub(crate) fn validate_proposal(proposal: &MissionPlanProposal) -> Result<(), Box<dyn Error>> {
    require_text("plan.summary", &proposal.summary, MAX_SUMMARY_CHARS)?;
    if proposal.tasks.is_empty() {
        return Err("plan must contain at least one task".into());
    }
    if proposal.tasks.len() > MAX_TASKS {
        return Err(format!("plan may contain at most {MAX_TASKS} tasks").into());
    }
    validate_criteria("plan.required_acceptance", &proposal.required_acceptance)?;
    if proposal.budget.max_rounds == 0
        || proposal.budget.max_minutes == 0
        || proposal.budget.max_repair_attempts > 10
    {
        return Err("plan budget must be positive and repair attempts must be <= 10".into());
    }
    validate_task_graph(&proposal.tasks)
}

pub(crate) fn validate_task_graph(tasks: &[MissionTaskSpec]) -> Result<(), Box<dyn Error>> {
    let mut ids = HashSet::new();
    for task in tasks {
        require_id("task.id", &task.id)?;
        require_text("task.title", &task.title, MAX_TITLE_CHARS)?;
        require_text("task.execution.executor", &task.execution.executor, MAX_ID_BYTES)?;
        if !ids.insert(task.id.as_str()) {
            return Err(format!("duplicate task id: {}", task.id).into());
        }
        validate_paths("task.read_paths", &task.read_paths)?;
        validate_paths("task.write_paths", &task.write_paths)?;
        validate_criteria("task.acceptance_criteria", &task.acceptance_criteria)?;
        validate_criteria("task.required_checks", &task.required_checks)?;
    }
    for task in tasks {
        for dependency in &task.depends_on {
            if !ids.contains(dependency.as_str()) {
                return Err(format!(
                    "task {} depends on unknown task {}",
                    task.id, dependency
                )
                .into());
            }
        }
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let by_id = tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    for task in tasks {
        visit_task(task.id.as_str(), &by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

/// Task paths are workspace-relative selectors. Absolute paths and `..` are not
/// merely normalized away: they are rejected so a model cannot widen its read
/// or write scope by inventing a path outside the approved workspace.
pub(crate) fn validate_relative_task_paths(tasks: &[MissionTaskSpec]) -> Result<(), Box<dyn Error>> {
    for task in tasks {
        for (label, paths) in [("read_paths", &task.read_paths), ("write_paths", &task.write_paths)] {
            for raw in paths {
                require_text(label, raw, MAX_PATH_CHARS)?;
                let path = Path::new(raw.trim());
                if path.is_absolute()
                    || path.components().any(|component| {
                        matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
                    })
                {
                    return Err(format!(
                        "task {} {label} must be workspace-relative and cannot escape the workspace: {raw}",
                        task.id
                    ).into());
                }
            }
        }
    }
    Ok(())
}

fn visit_task<'a>(
    id: &'a str,
    by_id: &HashMap<&'a str, &'a MissionTaskSpec>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> Result<(), Box<dyn Error>> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(format!("task dependency cycle includes {id}").into());
    }
    if let Some(task) = by_id.get(id) {
        for dependency in &task.depends_on {
            visit_task(dependency.as_str(), by_id, visiting, visited)?;
        }
    }
    visiting.remove(id);
    visited.insert(id);
    Ok(())
}

pub(crate) fn validate_selection(
    proposal: &MissionPlanProposal,
    selected: &std::collections::BTreeSet<String>,
    approved_paths: &[String],
) -> Result<(), Box<dyn Error>> {
    let all_ids = proposal
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if let Some(unknown) = selected.iter().find(|id| !all_ids.contains(id.as_str())) {
        return Err(format!("selected task is not in plan: {unknown}").into());
    }
    validate_paths("approved_paths", approved_paths)?;
    for task in proposal.tasks.iter().filter(|task| selected.contains(&task.id)) {
        for dependency in &task.depends_on {
            if !selected.contains(dependency) {
                return Err(format!(
                    "selected task {} requires unselected dependency {}",
                    task.id, dependency
                )
                .into());
            }
        }
        for path in &task.write_paths {
            if !path_allowed(path, approved_paths) {
                return Err(format!(
                    "selected task {} writes outside approved paths: {path}",
                    task.id
                )
                .into());
            }
        }
    }
    Ok(())
}

pub(crate) fn plan_hash(
    mission_id: &str,
    workspace_root: &str,
    mission_acceptance: &[String],
    proposal: &MissionPlanProposal,
) -> Result<String, Box<dyn Error>> {
    let canonical = serde_json::json!({
        "schema_version": "mission_plan_hash.v1",
        "mission_id": mission_id,
        "workspace_root": workspace_root.replace('\\', "/"),
        "mission_acceptance": mission_acceptance,
        "summary": proposal.summary,
        "tasks": proposal.tasks,
        "required_acceptance": proposal.required_acceptance,
        "budget": proposal.budget,
    });
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical)?)
    ))
}

pub(crate) fn normalized_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.trim().replace('\\', "/"))
        .filter(|path| !path.is_empty())
        .collect()
}

pub(crate) fn path_allowed(path: &str, approved: &[String]) -> bool {
    let path = path.trim().replace('\\', "/");
    approved.iter().any(|root| {
        let root = root.trim().replace('\\', "/");
        path == root || path.starts_with(&format!("{root}/"))
    })
}

pub(crate) fn require_sha256(value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("plan_sha256 must be 64 hexadecimal characters".into());
    }
    Ok(())
}

pub(crate) fn require_id(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    require_text(name, value, MAX_ID_BYTES)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(format!("{name} contains unsupported characters: {value}").into());
    }
    Ok(())
}

pub(crate) fn require_text(name: &str, value: &str, max_chars: usize) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() {
        return Err(format!("{name} is required").into());
    }
    if value.chars().count() > max_chars {
        return Err(format!("{name} exceeds {max_chars} characters").into());
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{name} contains control characters").into());
    }
    Ok(())
}

fn validate_paths(name: &str, paths: &[String]) -> Result<(), Box<dyn Error>> {
    for path in paths {
        require_text(name, path, MAX_PATH_CHARS)?;
        if path.contains('\0') {
            return Err(format!("{name} contains an invalid path").into());
        }
    }
    Ok(())
}

fn validate_criteria(name: &str, values: &[String]) -> Result<(), Box<dyn Error>> {
    for value in values {
        require_text(name, value, MAX_SUMMARY_CHARS)?;
    }
    Ok(())
}
