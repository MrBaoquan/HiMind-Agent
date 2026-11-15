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
    AwaitingAssignment,
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
            Self::AwaitingAssignment => "awaiting_assignment",
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
            "awaiting_assignment" => Ok(Self::AwaitingAssignment),
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

/// A Mission resolves an exact expert asset; changing the library cannot change
/// an already reviewed plan. The digest is part of each task's plan hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionExpertPin {
    pub expert_id: String,
    pub expert_version: String,
    pub expert_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionBinding {
    /// Absent only in historical plans created before expert assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expert: Option<MissionExpertPin>,
    pub executor: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub role_id: String,
    /// Optional service profile identity. Built-in model aliases already encode
    /// the service, but missions need to explain and audit the choice without
    /// parsing the alias. External runtimes may leave this empty.
    #[serde(default)]
    pub service_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionBudget {
    pub max_rounds: u32,
    pub max_minutes: u32,
    pub max_repair_attempts: u32,
    #[serde(default)]
    pub max_total_tokens: Option<u64>,
    #[serde(default)]
    pub max_cost: Option<f64>,
    /// Explicit comparison unit. Legacy undeclared cost limits remain fail-closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionBudgetIncrease {
    pub request_id: String,
    pub expected_plan_sha256: String,
    #[serde(default)]
    pub additional_rounds: u32,
    #[serde(default)]
    pub additional_minutes: u32,
    #[serde(default)]
    pub additional_repair_attempts: u32,
    #[serde(default)]
    pub additional_tokens: Option<u64>,
    #[serde(default)]
    pub unlimited_tokens: bool,
    #[serde(default)]
    pub additional_cost: Option<f64>,
}

/// Observations only. Evidence references are opaque strings, never path grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionFeedbackInput {
    pub request_id: String,
    pub expected_plan_sha256: String,
    pub target_task_ids: Vec<String>,
    pub message: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionFeedbackSubmit {
    pub mission_id: String,
    pub input: MissionFeedbackInput,
}

impl MissionFeedbackInput {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        require_text("request_id", &self.request_id, 200)?;
        require_sha256(&self.expected_plan_sha256)?;
        if !(1..=8).contains(&self.target_task_ids.len()) {
            return Err("target_task_ids must contain 1..8 unique tasks".into());
        }
        let mut seen = HashSet::new();
        for id in &self.target_task_ids {
            require_id("target_task_ids", id)?;
            if !seen.insert(id) { return Err("target_task_ids must be unique".into()); }
        }
        // Review notes may contain line breaks; preserve the exact payload.
        if self.message.trim().is_empty() || self.message.chars().count() > 8_000
            || self.message.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
            return Err("message must contain 1..8000 characters of review text".into());
        }
        if self.evidence_refs.len() > 16 { return Err("evidence_refs exceeds 16 items".into()); }
        for reference in &self.evidence_refs { require_text("evidence_refs", reference, 1_024)?; }
        Ok(())
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;

    fn input() -> MissionFeedbackInput {
        MissionFeedbackInput { request_id: "review".into(), expected_plan_sha256: "a".repeat(64),
            target_task_ids: vec!["task-1".into()], message: "待核实\nsecond line".into(), evidence_refs: vec![] }
    }

    #[test]
    fn mission_feedback_contract_enforces_character_and_collection_bounds() {
        let mut maximum = input();
        maximum.request_id = "界".repeat(200);
        maximum.message = "界".repeat(8_000);
        maximum.target_task_ids = (0..8).map(|n| format!("task-{n}")).collect();
        maximum.evidence_refs = vec!["界".repeat(1_024); 16];
        maximum.validate().unwrap();
        for field in ["request", "message", "targets", "refs", "reference", "duplicate", "empty_targets", "empty_message", "blank_message", "blank_request", "hash", "control"] {
            let mut invalid = maximum.clone();
            match field {
                "request" => invalid.request_id.push('界'),
                "message" => invalid.message.push('界'),
                "targets" => invalid.target_task_ids.push("task-9".into()),
                "refs" => invalid.evidence_refs.push("extra".into()),
                "reference" => invalid.evidence_refs[0].push('界'),
                "duplicate" => invalid.target_task_ids[1] = invalid.target_task_ids[0].clone(),
                "empty_targets" => invalid.target_task_ids.clear(),
                "empty_message" => invalid.message.clear(),
                "blank_message" => invalid.message = " \n\t".into(),
                "blank_request" => invalid.request_id = " ".into(),
                "hash" => invalid.expected_plan_sha256 = "z".repeat(64),
                _ => invalid.message = "note\0injection".into(),
            }
            assert!(invalid.validate().is_err(), "{field}");
        }
        input().validate().unwrap();
    }

    #[test]
    fn mission_feedback_contract_rejects_unknown_envelope_and_input_fields() {
        let valid = serde_json::json!({"mission_id":"mission-test","input":input()});
        serde_json::from_value::<MissionFeedbackSubmit>(valid.clone()).unwrap();
        let mut invalid = valid.clone(); invalid["approve_paths"] = serde_json::json!(["C:/private"]);
        assert!(serde_json::from_value::<MissionFeedbackSubmit>(invalid).is_err());
        let mut invalid = valid; invalid["input"]["passed"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MissionFeedbackSubmit>(invalid).is_err());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionSourceBaseline {
    pub source_reference: String,
    pub captured_at: String,
    pub artifact_sha256: String,
    pub files: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionVerificationRetry {
    pub request_id: String,
    pub expected_plan_sha256: String,
    pub expected_verification_run_id: String,
    #[serde(default)]
    pub source_baseline: Option<MissionSourceBaseline>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionPlanProposal {
    pub summary: String,
    pub tasks: Vec<MissionTaskSpec>,
    pub required_acceptance: Vec<String>,
    pub budget: MissionBudget,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MissionPlanMode {
    Imported,
    Generated,
}

impl MissionPlanMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Imported => "imported", Self::Generated => "generated" }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "imported" => Ok(Self::Imported),
            "generated" => Ok(Self::Generated),
            other => Err(format!("unknown mission plan mode: {other}").into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionWorkflowPin {
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    #[serde(default)]
    pub entrypoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionRoleAssignment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expert: Option<MissionExpertPin>,
    pub role_id: String,
    #[serde(default)]
    pub label: String,
    pub executor: String,
    #[serde(default)]
    pub service_id: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionTaskRoleMapping {
    pub task_id: String,
    pub role_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionAssignment {
    pub roles: Vec<MissionRoleAssignment>,
    #[serde(default)]
    pub task_roles: Vec<MissionTaskRoleMapping>,
    pub applied_by: String,
    pub applied_at: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionHandoff {
    pub idempotency_key: String,
    pub source_client: String,
    pub source_reference: String,
    #[serde(default)]
    pub source_message_refs: Vec<String>,
    #[serde(default)]
    pub context_summary: String,
    pub request_sha256: String,
    pub received_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionHandoffInput {
    pub idempotency_key: String,
    pub source_client: String,
    pub source_reference: String,
    #[serde(default)]
    pub source_message_refs: Vec<String>,
    #[serde(default)]
    pub context_summary: String,
    pub title: String,
    #[serde(default)]
    pub objective: String,
    pub workspace: MissionWorkspace,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub workflow: MissionWorkflowPin,
    pub plan_mode: MissionPlanMode,
    #[serde(default)]
    pub imported_plan: Option<MissionPlanProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionAssignmentInput {
    pub roles: Vec<MissionRoleAssignment>,
    #[serde(default)]
    pub task_roles: Vec<MissionTaskRoleMapping>,
    pub applied_by: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionWorkspace {
    pub root: String,
    /// Empty historical values retain workspace confinement. Full access is a
    /// user choice confirmed with the plan, never inferred from a tool error.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permission_mode: String,
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
    #[serde(default)]
    pub approved_absolute_read_paths: Vec<String>,
    #[serde(default)]
    pub approved_absolute_write_paths: Vec<String>,
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
    /// Verification run admitted by the host before execution. Results from any
    /// other run id are rejected, preventing caller self-certification.
    pub verification_run_id: String,
    pub verification_passed: Option<bool>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct MissionCostUsage {
    pub buckets: std::collections::BTreeMap<String, f64>,
    pub unknown_runs: u64,
    pub comparable: bool,
}

/// Additive v1 DTO shared by list, inbox and detail; never grants permission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MissionEffectiveWait {
    pub contract_version: String,
    pub plan_sha256: String,
    pub cursor: u64,
    pub reason: String,
    pub actor: String,
    pub budget_reason: Option<String>,
    pub environment_blocked: bool,
    pub can_increase_budget: bool,
    pub can_resume: bool,
}

fn default_plan_mode() -> MissionPlanMode { MissionPlanMode::Generated }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    #[serde(default)]
    pub cost_usage: MissionCostUsage,
    #[serde(default)]
    pub effective_wait: Option<MissionEffectiveWait>,
    /// Accumulated usage from dispatched LocalRuns. `None` keeps the distinction
    /// between "not declared" and "not reported"; an empty struct is not used to
    /// imply that a Runtime consumed zero tokens.
    #[serde(default)]
    pub usage: Option<crate::agent_core_contracts::LocalRunUsage>,
    #[serde(default)]
    pub handoff: Option<MissionHandoff>,
    #[serde(default)]
    pub workflow: Option<MissionWorkflowPin>,
    #[serde(default)]
    pub assignment: Option<MissionAssignment>,
    #[serde(default = "default_plan_mode")]
    pub plan_mode: MissionPlanMode,
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

pub(crate) fn validate_workflow_pin(pin: &MissionWorkflowPin) -> Result<(), Box<dyn Error>> {
    require_id("workflow.workflow_id", &pin.workflow_id)?;
    if pin.workflow_version.trim().is_empty() || pin.workflow_version.chars().count() > 80 {
        return Err("workflow.workflow_version is required and must be at most 80 characters".into());
    }
    if !pin.workflow_version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')) {
        return Err("workflow.workflow_version contains unsupported characters".into());
    }
    require_sha256(&pin.workflow_sha256)?;
    if pin.entrypoint.chars().count() > MAX_ID_BYTES {
        return Err("workflow.entrypoint exceeds 160 characters".into());
    }
    Ok(())
}

/// A role the selected workflow makes available for assignment. The resolver is
/// host-owned: client UI never invents role IDs or claims a service is usable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionRoleOption {
    pub role_id: String,
    pub label: String,
    pub brief: String,
    /// Empty means the workflow leaves executor choice to the assignment UI.
    pub recommended_executor: String,
    /// Empty means the executor uses its own model configuration.
    pub recommended_service_id: String,
    pub recommended_model: String,
    pub model_override_supported: bool,
    pub availability: String,
    pub unavailable_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionAssignmentOptions {
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub roles: Vec<MissionRoleOption>,
    pub experts: Vec<crate::expert::ExpertSummary>,
    pub executors: Vec<crate::app::mission_commands::MissionExecutionOption>,
    pub source: String,
}

pub(crate) fn validate_assignment(assignment: &MissionAssignmentInput) -> Result<(), Box<dyn Error>> {
    require_text("assignment.applied_by", &assignment.applied_by, 160)?;
    if assignment.roles.is_empty() {
        return Err("assignment must contain at least one role".into());
    }
    if assignment.roles.len() > 32 {
        return Err("assignment may contain at most 32 roles".into());
    }
    let mut ids = HashSet::new();
    for role in &assignment.roles {
        require_id("assignment.role_id", &role.role_id)?;
        if !ids.insert(role.role_id.as_str()) {
            return Err(format!("duplicate assigned role: {}", role.role_id).into());
        }
        let executor = role.executor.trim();
        require_text("assignment.executor", executor, MAX_ID_BYTES)?;
        let fixture_enabled = executor == "himind.fixture"
            && std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1");
        let supported = fixture_enabled
            || matches!(executor, "himind.builtin" | "personal.codex" | "personal.github-copilot")
            || crate::runtime::acp::is_provider(executor);
        if !supported {
            return Err(format!("assigned role {} uses unsupported executor: {executor}", role.role_id).into());
        }
        if !role.service_id.trim().is_empty() {
            require_id("assignment.service_id", &role.service_id)?;
        }
        if !role.model.trim().is_empty() {
            crate::workflow::validate_model_alias(&role.model)
                .map_err(|error| format!("assigned role {} model: {error}", role.role_id))?;
            crate::workflow::model_alias_matches_provider(executor, &role.model)
                .map_err(|error| format!("assigned role {} model/provider: {error}", role.role_id))?;
            let alias_service = crate::workflow::model_alias_parts(&role.model)
                .map(|(_, service, _)| service.to_string())
                .unwrap_or_default();
            if !role.service_id.trim().is_empty() && !alias_service.is_empty() && alias_service != role.service_id.trim() {
                return Err(format!("assigned role {} service_id does not match model alias", role.role_id).into());
            }
        }
    }
    let mut task_ids = HashSet::new();
    for mapping in &assignment.task_roles {
        require_id("assignment.task_roles.task_id", &mapping.task_id)?;
        require_id("assignment.task_roles.role_id", &mapping.role_id)?;
        if !ids.contains(mapping.role_id.as_str()) {
            return Err(format!("task role mapping references unassigned role: {}", mapping.role_id).into());
        }
        if !task_ids.insert(mapping.task_id.as_str()) {
            return Err(format!("duplicate task role mapping: {}", mapping.task_id).into());
        }
    }
    Ok(())
}

pub(crate) fn validate_handoff_input(input: &MissionHandoffInput) -> Result<(), Box<dyn Error>> {
    require_text("handoff.idempotency_key", &input.idempotency_key, 200)?;
    require_id("handoff.source_client", &input.source_client)?;
    require_text("handoff.source_reference", &input.source_reference, 1_000)?;
    if input.source_message_refs.len() > 200 {
        return Err("handoff may reference at most 200 source messages".into());
    }
    if input.context_summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(format!("handoff.context_summary exceeds {MAX_SUMMARY_CHARS} characters").into());
    }
    validate_create(&MissionCreateInput {
        title: input.title.clone(), objective: input.objective.clone(),
        workspace: input.workspace.clone(), acceptance_criteria: input.acceptance_criteria.clone(),
    })?;
    validate_workflow_pin(&input.workflow)?;
    match (input.plan_mode, input.imported_plan.as_ref()) {
        (MissionPlanMode::Imported, None) => return Err("imported plan mode requires imported_plan".into()),
        (MissionPlanMode::Generated, Some(_)) => return Err("generated plan mode must not include imported_plan".into()),
        (MissionPlanMode::Imported, Some(proposal)) => validate_proposal(proposal)?,
        (MissionPlanMode::Generated, None) => {}
    }
    Ok(())
}

pub(crate) fn apply_assignment_to_plan(
    proposal: &MissionPlanProposal,
    assignment: &MissionAssignmentInput,
) -> Result<MissionPlanProposal, Box<dyn Error>> {
    validate_assignment(assignment)?;
    let by_role = assignment.roles.iter().map(|role| (role.role_id.as_str(), role)).collect::<HashMap<_, _>>();
    let mapped_roles = assignment.task_roles.iter()
        .map(|mapping| (mapping.task_id.as_str(), mapping.role_id.as_str()))
        .collect::<HashMap<_, _>>();
    let mut next = proposal.clone();
    for task in &mut next.tasks {
        let role_id = mapped_roles.get(task.id.as_str()).copied().unwrap_or(task.execution.role_id.as_str());
        let role = by_role.get(role_id).ok_or_else(|| {
            format!("task {} uses unassigned role {}", task.id, role_id)
        })?;
        task.execution.role_id = role.role_id.trim().to_string();
        task.execution.executor = role.executor.trim().to_string();
        task.execution.service_id = role.service_id.trim().to_string();
        task.execution.model = role.model.trim().to_string();
        task.execution.expert = role.expert.clone();
    }
    validate_proposal(&next)?;
    Ok(next)
}

pub(crate) fn validate_create(input: &MissionCreateInput) -> Result<(), Box<dyn Error>> {
    if !matches!(input.workspace.permission_mode.as_str(), "" | "workspace_write" | "full_access") {
        return Err("permission_mode must be workspace_write or full_access".into());
    }
    require_text("title", &input.title, MAX_TITLE_CHARS)?;
    if input.objective.chars().count() > MAX_SUMMARY_CHARS {
        return Err(format!("objective exceeds {MAX_SUMMARY_CHARS} characters").into());
    }
    require_text("workspace.root", &input.workspace.root, MAX_PATH_CHARS)?;
    validate_workspace_root(&input.workspace.root)?;
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
        || proposal.budget.max_repair_attempts > 2
    {
        return Err("plan budget must be positive and repair attempts must be <= 2".into());
    }
    if proposal.budget.max_total_tokens == Some(0) {
        return Err("max_total_tokens must be positive when declared".into());
    }
    if proposal.budget.max_cost.is_some_and(|cost| !cost.is_finite() || cost <= 0.0) {
        return Err("max_cost must be a positive finite number when declared".into());
    }
    validate_task_graph(&proposal.tasks)
}

pub(crate) fn validate_task_graph(tasks: &[MissionTaskSpec]) -> Result<(), Box<dyn Error>> {
    let mut ids = HashSet::new();
    for task in tasks {
        require_id("task.id", &task.id)?;
        require_text("task.title", &task.title, MAX_TITLE_CHARS)?;
        validate_execution_binding(task)?;
        if !ids.insert(task.id.as_str()) {
            return Err(format!("duplicate task id: {}", task.id).into());
        }
        validate_paths("task.read_paths", &task.read_paths)?;
        validate_paths("task.write_paths", &task.write_paths)?;
        validate_criteria("task.acceptance_criteria", &task.acceptance_criteria)?;
        validate_criteria("task.required_checks", &task.required_checks)?;
        if task.required_checks.is_empty() || task.required_checks.len() > 32 {
            return Err("each task must declare 1 to 32 executable required checks".into());
        }
        for check in &task.required_checks { super::checks::validate_required_check(check)?; }
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

pub(crate) fn validate_execution_binding(task: &MissionTaskSpec) -> Result<(), Box<dyn Error>> {
    let executor = task.execution.executor.trim();
    require_text("task.execution.executor", executor, MAX_ID_BYTES)?;
    require_id("task.execution.role_id", &task.execution.role_id)?;
    // `himind.fixture` is an execution-test provider, enabled only by the
    // explicit workflow fixture environment switch. It must still pass the
    // normal Workflow provider allowlist at dispatch time.
    let fixture_enabled = executor == "himind.fixture"
        && std::env::var("HIMIND_WORKFLOW_RUNTIME_FIXTURE").as_deref() == Ok("1");
    let supported = fixture_enabled
        || matches!(executor, "himind.builtin" | "personal.codex" | "personal.github-copilot")
        || crate::runtime::acp::is_provider(executor);
    if !supported {
        return Err(format!("task {} uses unsupported executor: {executor}", task.id).into());
    }
    if !task.execution.model.trim().is_empty() {
        crate::workflow::validate_model_alias(&task.execution.model)
            .map_err(|error| format!("task {} model binding: {error}", task.id))?;
        crate::workflow::model_alias_matches_provider(executor, &task.execution.model)
            .map_err(|error| format!("task {} model/provider binding: {error}", task.id))?;
    }
    Ok(())
}

/// Task paths may be relative selectors or absolute selectors. Absolute
/// selectors remain visible in the proposal and
/// require explicit read/write approval before the task can be dispatched.
pub(crate) fn validate_task_paths(
    tasks: &[MissionTaskSpec],
    _workspace_root: &str,
) -> Result<(), Box<dyn Error>> {
    for task in tasks {
        for (label, paths) in [("read_paths", &task.read_paths), ("write_paths", &task.write_paths)] {
            for raw in paths {
                require_text(label, raw, MAX_PATH_CHARS)?;
                let path = Path::new(raw.trim());
                if is_dangerous_path(raw) || has_parent_segment(path) {
                    return Err(format!(
                        "task {} {label} contains a path escape or dangerous path: {raw}",
                        task.id
                    ).into());
                }
                if is_absolute_path(raw) {
                    if !has_normal_component(path) {
                        return Err(format!("task {} {label} cannot target a filesystem root: {raw}", task.id).into());
                    }
                } else if path.components().any(|component| {
                    matches!(component, Component::RootDir | Component::Prefix(_))
                }) {
                    return Err(format!(
                        "task {} {label} must be workspace-relative or a fully qualified absolute path: {raw}",
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

pub(crate) fn validate_selection_with_scopes(
    proposal: &MissionPlanProposal,
    selected: &std::collections::BTreeSet<String>,
    approved_write_paths: &[String],
    approved_absolute_read_paths: &[String],
    approved_absolute_write_paths: &[String],
) -> Result<(), Box<dyn Error>> {
    let all_ids = proposal
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if let Some(unknown) = selected.iter().find(|id| !all_ids.contains(id.as_str())) {
        return Err(format!("selected task is not in plan: {unknown}").into());
    }
    validate_approved_paths("approved_paths", approved_write_paths)?;
    if approved_write_paths.iter().any(|path| is_absolute_path(path)) {
        return Err("approved_paths only accepts workspace-relative write scopes; use approved_absolute_write_paths for absolute paths".into());
    }
    validate_approved_paths("approved_absolute_read_paths", approved_absolute_read_paths)?;
    validate_approved_paths("approved_absolute_write_paths", approved_absolute_write_paths)?;
    if approved_absolute_read_paths.iter().any(|path| !is_absolute_path(path))
        || approved_absolute_write_paths.iter().any(|path| !is_absolute_path(path))
    {
        return Err("absolute path approvals must contain only absolute paths".into());
    }
    validate_absolute_approvals(
        "approved_absolute_read_paths",
        proposal.tasks.iter().filter(|task| selected.contains(&task.id)).flat_map(|task| task.read_paths.iter()),
        approved_absolute_read_paths,
    )?;
    validate_absolute_approvals(
        "approved_absolute_write_paths",
        proposal.tasks.iter().filter(|task| selected.contains(&task.id)).flat_map(|task| task.write_paths.iter()),
        approved_absolute_write_paths,
    )?;
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
        for path in &task.read_paths {
            if is_absolute_path(path) && !absolute_path_approved(path, approved_absolute_read_paths) {
                return Err(format!(
                    "selected task {} reads outside approved paths: {path}",
                    task.id
                )
                .into());
            }
        }
        for path in &task.write_paths {
            let allowed = if is_absolute_path(path) {
                absolute_path_approved(path, approved_absolute_write_paths)
            } else {
                path_allowed(path, approved_write_paths)
            };
            if !allowed {
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

fn validate_absolute_approvals<'a>(
    name: &str,
    planned_paths: impl Iterator<Item = &'a String>,
    approved_paths: &[String],
) -> Result<(), Box<dyn Error>> {
    let planned = planned_paths
        .filter(|path| is_absolute_path(path))
        .map(|path| path_key(path))
        .collect::<HashSet<_>>();
    for path in approved_paths.iter().filter(|path| is_absolute_path(path)) {
        if !planned.contains(&path_key(path)) {
            return Err(format!("{name} cannot authorize an absolute path absent from selected tasks: {path}").into());
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

pub(crate) fn plan_hash_for_workspace(
    mission_id: &str, workspace: &MissionWorkspace, mission_acceptance: &[String], proposal: &MissionPlanProposal,
) -> Result<String, Box<dyn Error>> {
    let original = plan_hash(mission_id, &workspace.root, mission_acceptance, proposal)?;
    if workspace.permission_mode != "full_access" { return Ok(original); }
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::json!({
        "schema_version":"mission_plan_hash.v2", "plan_sha256":original, "permission_mode":"full_access"
    }))?)))
}

pub(crate) fn normalized_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect()
}

pub(crate) fn path_allowed(path: &str, approved: &[String]) -> bool {
    let path = path_key(path);
    approved.iter().any(|root| {
        let root = path_key(root);
        if root == "." && !is_absolute_path(&path) {
            return true;
        }
        path == root || path.starts_with(&format!("{root}/"))
    })
}

pub(crate) fn absolute_path_approved(path: &str, approved: &[String]) -> bool {
    is_absolute_path(path)
        && approved.iter().any(|root| {
            is_absolute_path(root) && path_key(path) == path_key(root)
        })
}

pub(crate) fn validate_approved_paths(name: &str, paths: &[String]) -> Result<(), Box<dyn Error>> {
    validate_paths(name, paths)?;
    for raw in paths {
        let path = Path::new(raw.trim());
        if is_dangerous_path(raw) || has_parent_segment(path) {
            return Err(format!("{name} contains a path escape or dangerous path: {raw}").into());
        }
        if is_absolute_path(raw) {
            if !has_normal_component(path) {
                return Err(format!("{name} cannot authorize a filesystem root: {raw}").into());
            }
        } else if path.components().any(|component| {
            matches!(component, Component::RootDir | Component::Prefix(_))
        }) {
            return Err(format!("{name} must contain workspace-relative paths or workspace absolute paths: {raw}").into());
        }
    }
    Ok(())
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

fn validate_workspace_root(root: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(root.trim());
    if is_dangerous_path(root) || has_parent_segment(path) || !is_absolute_path(root) {
        return Err("workspace.root must be a safe absolute local path".into());
    }
    Ok(())
}

fn has_parent_segment(path: &Path) -> bool {
    path.to_string_lossy()
        .split(|character| character == '/' || character == '\\')
        .any(|component| component == "..")
}

pub(crate) fn is_absolute_path(raw: &str) -> bool {
    let normalized = normalize_path(raw);
    if cfg!(windows) {
        normalized.len() >= 3
            && normalized.as_bytes()[0].is_ascii_alphabetic()
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/'
    } else {
        normalized.starts_with('/')
    }
}

fn is_dangerous_path(raw: &str) -> bool {
    let normalized = normalize_path(raw);
    normalized.starts_with("//")
        || normalized.starts_with("/dev/")
        || normalized == "/dev"
        || normalized.starts_with("/proc/")
        || normalized == "/proc"
        || normalized.starts_with("/sys/")
        || normalized == "/sys"
}

fn has_normal_component(path: &Path) -> bool {
    path.components().any(|component| matches!(component, Component::Normal(_)))
}


fn normalize_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn path_key(path: &str) -> String {
    let normalized = normalize_path(path);
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn validate_criteria(name: &str, values: &[String]) -> Result<(), Box<dyn Error>> {
    for value in values {
        require_text(name, value, MAX_SUMMARY_CHARS)?;
    }
    Ok(())
}
