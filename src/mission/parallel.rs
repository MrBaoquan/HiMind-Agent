//! Pure, deterministic admission decisions for a confirmed Mission task snapshot.
//!
//! This module neither starts workers nor reserves resources. The host must repeat
//! the decision and persist reservations in one admission transaction, retaining
//! claims through verification (and until canceled workers actually stop). It must
//! also enforce confirmation, grants, executor capacity, budget and integration.
//! Resource sidecars belong to the confirmed plan; only the host can attest that
//! an isolated workspace exists and has a suitable baseline. Missing sidecars
//! conservatively use the shared source workspace. New plans may use the default
//! cap of three; the host must explicitly supply cap one for historical plans.
//!
//! Paths are lexical resource identities, not filesystem authorization: symlinks,
//! hard links and filesystem aliases must be resolved/validated by the host.

use super::contract::{MissionTaskSpec, MissionTaskView, TaskStatus};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub(crate) const DEFAULT_MAX_ACTIVE_TASKS: usize = 3;
pub(crate) const MAX_SCHEDULE_TASKS: usize = 128;

/// User-reviewed scheduling intent. Physical workspace locations are allocated
/// by the host and never accepted from a source client as proof of isolation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionScheduling {
    pub max_active_tasks: usize,
    #[serde(default)]
    pub task_resources: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub executor_limits: BTreeMap<String, usize>,
}

impl Default for MissionScheduling {
    fn default() -> Self {
        Self { max_active_tasks: DEFAULT_MAX_ACTIVE_TASKS, task_resources: BTreeMap::new(), executor_limits: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParallelPolicy {
    pub max_active_tasks: usize,
}

impl Default for ParallelPolicy {
    fn default() -> Self {
        Self {
            max_active_tasks: DEFAULT_MAX_ACTIVE_TASKS,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TaskWorkspace {
    #[default]
    Shared,
    Isolated {
        root: String,
    },
}

/// Additive resource declarations keyed by task id, outside MissionTaskSpec.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskResourceClaims {
    #[serde(default)]
    pub workspace: TaskWorkspace,
    /// Exact, case-sensitive names, e.g. "database:main" or "port:8080".
    #[serde(default)]
    pub exclusive_resources: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub(crate) enum PathConflictKind {
    Logical,
    Physical,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag="code",rename_all="snake_case")]
pub(crate) enum ResourceConflict {
    ExclusiveResource {
        name: String,
    },
    /// At least one scope is a write. Scopes include their descendants.
    Path {
        kind: PathConflictKind,
        left: String,
        right: String,
    },
    /// Even disjoint writes cannot share a mutable workspace directory.
    WorkspaceWriters {
        left: String,
        right: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag="code",rename_all="snake_case")]
pub(crate) enum WaitReason {
    DependenciesNotVerified {
        task_ids: Vec<String>,
    },
    MaxActiveTasks {
        limit: usize,
    },
    ExecutorCapacity { executor: String, limit: usize },
    ResourceBusy {
        holder_task_id: String,
        conflict: ResourceConflict,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TaskWait {
    pub task_id: String,
    pub reason: WaitReason,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ReadySet {
    /// Stable greedy selection in input/plan order; not a maximum packing.
    pub admitted_task_ids: Vec<String>,
    /// Only Ready/Blocked tasks are candidates. Proposed/terminal tasks stay out.
    pub waiting: Vec<TaskWait>,
    /// Running and AwaitingVerification both consume a slot and hold claims.
    pub active_task_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingError(pub String);

impl fmt::Display for SchedulingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SchedulingError {}

/// Computes one compatible admission batch against *all* current occupants.
/// A dependency must be Completed AND explicitly verified; a newly admitted
/// predecessor cannot release its dependants in the same batch.
pub(crate) fn ready_set(
    tasks: &[MissionTaskView],
    source_workspace_root: &str,
    claims: &BTreeMap<String, TaskResourceClaims>,
    policy: ParallelPolicy,
) -> Result<ReadySet, SchedulingError> {
    if !(1..=MAX_SCHEDULE_TASKS).contains(&policy.max_active_tasks) {
        return Err(SchedulingError(format!(
            "max_active_tasks must be in 1..={MAX_SCHEDULE_TASKS}"
        )));
    }
    let by_id = validate_graph(tasks)?;
    for id in claims.keys() {
        if !by_id.contains_key(id.as_str()) {
            return Err(SchedulingError(format!(
                "resource claims for unknown task {id}"
            )));
        }
    }
    let root = ResourcePath::absolute(source_workspace_root)?;
    let default_claims = TaskResourceClaims::default();
    let prepared = tasks
        .iter()
        .map(|task| {
            PreparedResources::new(
                &root,
                &task.spec,
                claims.get(&task.spec.id).unwrap_or(&default_claims),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut holders: Vec<usize> = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| holds_resources(task.status))
        .map(|(index, _)| index)
        .collect();
    let mut result = ReadySet {
        admitted_task_ids: Vec::new(),
        waiting: Vec::new(),
        active_task_count: holders.len(),
    };
    for (index, task) in tasks.iter().enumerate() {
        if !matches!(task.status, TaskStatus::Ready | TaskStatus::Blocked) {
            continue;
        }
        let dependencies: Vec<String> = task
            .spec
            .depends_on
            .iter()
            .filter(|id| {
                let dependency = &tasks[by_id[id.as_str()]];
                dependency.status != TaskStatus::Completed
                    || dependency.verification_passed != Some(true)
            })
            .cloned()
            .collect();
        let reason = if !dependencies.is_empty() {
            Some(WaitReason::DependenciesNotVerified {
                task_ids: dependencies,
            })
        } else if holders.len() >= policy.max_active_tasks {
            Some(WaitReason::MaxActiveTasks {
                limit: policy.max_active_tasks,
            })
        } else {
            holders.iter().find_map(|&holder| {
                prepared[index].conflict(&prepared[holder]).map(|conflict| {
                    WaitReason::ResourceBusy {
                        holder_task_id: tasks[holder].spec.id.clone(),
                        conflict,
                    }
                })
            })
        };
        if let Some(reason) = reason {
            result.waiting.push(TaskWait {
                task_id: task.spec.id.clone(),
                reason,
            });
        } else {
            holders.push(index);
            result.admitted_task_ids.push(task.spec.id.clone());
        }
    }
    Ok(result)
}

pub(crate) fn holds_resources(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Running | TaskStatus::AwaitingVerification
    )
}

/// Pairwise decision using the same rules as ready_set, independent of status.
pub(crate) fn resource_conflict(
    source_workspace_root: &str,
    left: &MissionTaskSpec,
    left_claims: &TaskResourceClaims,
    right: &MissionTaskSpec,
    right_claims: &TaskResourceClaims,
) -> Result<Option<ResourceConflict>, SchedulingError> {
    let root = ResourcePath::absolute(source_workspace_root)?;
    Ok(
        PreparedResources::new(&root, left, left_claims)?.conflict(&PreparedResources::new(
            &root,
            right,
            right_claims,
        )?),
    )
}

/// Resolves relative scopes against one logical root, on every host OS.
/// Windows drive/UNC paths fold case; POSIX paths preserve case. Separator,
/// redundant slash and dot normalization never turns sibling prefixes into
/// ancestor matches. Parent traversal and ambiguous drive-relative paths fail.
pub(crate) fn paths_overlap(
    source_workspace_root: &str,
    left: &str,
    right: &str,
) -> Result<bool, SchedulingError> {
    let root = ResourcePath::absolute(source_workspace_root)?;
    Ok(root.resolve(left)?.overlaps(&root.resolve(right)?))
}

fn validate_graph(tasks: &[MissionTaskView]) -> Result<BTreeMap<&str, usize>, SchedulingError> {
    if tasks.len() > MAX_SCHEDULE_TASKS {
        return Err(SchedulingError(format!(
            "task graph exceeds {MAX_SCHEDULE_TASKS} tasks"
        )));
    }
    let mut by_id = BTreeMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let id = task.spec.id.as_str();
        if id.trim().is_empty() || by_id.insert(id, index).is_some() {
            return Err(SchedulingError(format!("empty or duplicate task id: {id}")));
        }
    }
    let mut remaining = Vec::with_capacity(tasks.len());
    let mut successors = vec![Vec::new(); tasks.len()];
    for (index, task) in tasks.iter().enumerate() {
        if task.spec.depends_on.len() > tasks.len() {
            return Err(SchedulingError(format!(
                "too many dependencies: {}",
                task.spec.id
            )));
        }
        let mut seen = BTreeSet::new();
        for id in &task.spec.depends_on {
            let Some(&dependency) = by_id.get(id.as_str()) else {
                return Err(SchedulingError(format!(
                    "unknown dependency {id} on {}",
                    task.spec.id
                )));
            };
            if !seen.insert(dependency) {
                return Err(SchedulingError(format!(
                    "duplicate dependency {id} on {}",
                    task.spec.id
                )));
            }
            successors[dependency].push(index);
        }
        remaining.push(seen.len());
    }
    // Kahn's algorithm avoids recursion even for malformed/deep input graphs.
    let mut frontier: Vec<usize> = remaining
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 0)
        .map(|(index, _)| index)
        .collect();
    let mut visited = 0;
    while let Some(index) = frontier.pop() {
        visited += 1;
        for &successor in &successors[index] {
            remaining[successor] -= 1;
            if remaining[successor] == 0 {
                frontier.push(successor);
            }
        }
    }
    if visited != tasks.len() {
        return Err(SchedulingError("task dependency cycle".into()));
    }
    Ok(by_id)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PathRoot {
    Posix,
    Drive(char),
    Unc(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResourcePath {
    root: PathRoot,
    parts: Vec<String>,
}

impl ResourcePath {
    fn absolute(raw: &str) -> Result<Self, SchedulingError> {
        let normalized = path_text(raw)?;
        let (root, remainder) = if let Some(unc) = normalized.strip_prefix("//") {
            let mut parts = unc.split('/').filter(|part| !part.is_empty());
            let server = parts.next().unwrap_or("");
            let share = parts.next().unwrap_or("");
            if [server, share].iter().any(|part| {
                part.is_empty() || matches!(*part, "." | ".." | "?") || part.contains(':')
            }) {
                return Err(SchedulingError(format!("invalid UNC resource path: {raw}")));
            }
            let root = PathRoot::Unc(server.to_lowercase(), share.to_lowercase());
            return Self::from_parts(root, parts, raw);
        } else if normalized.len() >= 3
            && normalized.as_bytes()[0].is_ascii_alphabetic()
            && &normalized.as_bytes()[1..3] == b":/"
        {
            (
                PathRoot::Drive((normalized.as_bytes()[0] as char).to_ascii_lowercase()),
                &normalized[3..],
            )
        } else if let Some(remainder) = normalized.strip_prefix('/') {
            (PathRoot::Posix, remainder)
        } else {
            return Err(SchedulingError(format!(
                "absolute workspace/path required: {raw}"
            )));
        };
        Self::from_parts(root, remainder.split('/'), raw)
    }

    fn from_parts<'a>(
        root: PathRoot,
        parts: impl Iterator<Item = &'a str>,
        raw: &str,
    ) -> Result<Self, SchedulingError> {
        let windows = !matches!(root, PathRoot::Posix);
        let mut normalized = Vec::new();
        for part in parts {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." || part.contains(':') || (windows && part.ends_with(['.', ' '])) {
                return Err(SchedulingError(format!("ambiguous resource path: {raw}")));
            }
            normalized.push(if windows {
                part.to_lowercase()
            } else {
                part.to_string()
            });
        }
        Ok(Self {
            root,
            parts: normalized,
        })
    }

    fn resolve(&self, raw: &str) -> Result<Self, SchedulingError> {
        let normalized = path_text(raw)?;
        if normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':') {
            return Self::absolute(raw);
        }
        let mut resolved = Self::from_parts(self.root.clone(), normalized.split('/'), raw)?;
        resolved.parts.splice(0..0, self.parts.iter().cloned());
        Ok(resolved)
    }

    fn contains(&self, other: &Self) -> bool {
        self.root == other.root && other.parts.starts_with(&self.parts)
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.contains(other) || other.contains(self)
    }

    fn key(&self) -> String {
        let root = match &self.root {
            PathRoot::Posix => "/".into(),
            PathRoot::Drive(drive) => format!("{drive}:/"),
            PathRoot::Unc(server, share) => format!("//{server}/{share}/"),
        };
        format!("{root}{}", self.parts.join("/"))
    }
}

fn path_text(raw: &str) -> Result<String, SchedulingError> {
    if raw.trim().is_empty() || raw.chars().count() > 1_024 || raw.chars().any(char::is_control) {
        return Err(SchedulingError(
            "resource paths require 1..1024 non-control characters".into(),
        ));
    }
    Ok(raw.trim().replace('\\', "/"))
}

struct PreparedResources {
    isolated: bool,
    workspace: ResourcePath,
    names: BTreeSet<String>,
    logical_reads: BTreeSet<ResourcePath>,
    logical_writes: BTreeSet<ResourcePath>,
    physical_reads: BTreeSet<ResourcePath>,
    physical_writes: BTreeSet<ResourcePath>,
}

impl PreparedResources {
    fn new(
        source: &ResourcePath,
        task: &MissionTaskSpec,
        claims: &TaskResourceClaims,
    ) -> Result<Self, SchedulingError> {
        let workspace = match &claims.workspace {
            TaskWorkspace::Shared => source.clone(),
            TaskWorkspace::Isolated { root } => ResourcePath::absolute(root)?,
        };
        for name in &claims.exclusive_resources {
            if name.trim().is_empty() || name.trim() != name || name.chars().any(char::is_control) {
                return Err(SchedulingError(format!(
                    "invalid exclusive resource name: {name:?}"
                )));
            }
        }
        let resolve = |paths: &[String]| {
            paths
                .iter()
                .map(|path| source.resolve(path))
                .collect::<Result<BTreeSet<_>, _>>()
        };
        let logical_reads = resolve(&task.read_paths)?;
        let logical_writes = resolve(&task.write_paths)?;
        let physical = |paths: &BTreeSet<ResourcePath>| {
            paths
                .iter()
                .map(|path| {
                    if source.contains(path) {
                        let mut mapped = workspace.clone();
                        mapped
                            .parts
                            .extend_from_slice(&path.parts[source.parts.len()..]);
                        mapped
                    } else {
                        // Absolute resources outside the source are never isolated.
                        path.clone()
                    }
                })
                .collect()
        };
        Ok(Self {
            isolated: matches!(claims.workspace, TaskWorkspace::Isolated { .. }),
            physical_reads: physical(&logical_reads),
            physical_writes: physical(&logical_writes),
            workspace,
            names: claims.exclusive_resources.clone(),
            logical_reads,
            logical_writes,
        })
    }

    fn conflict(&self, other: &Self) -> Option<ResourceConflict> {
        if let Some(name) = self.names.intersection(&other.names).next() {
            return Some(ResourceConflict::ExclusiveResource { name: name.clone() });
        }
        for (kind, reads, writes, other_reads, other_writes) in [
            (
                PathConflictKind::Logical,
                &self.logical_reads,
                &self.logical_writes,
                &other.logical_reads,
                &other.logical_writes,
            ),
            (
                PathConflictKind::Physical,
                &self.physical_reads,
                &self.physical_writes,
                &other.physical_reads,
                &other.physical_writes,
            ),
        ] {
            for (pair_index, (left, right)) in [
                (writes, other_writes),
                (writes, other_reads),
                (reads, other_writes),
            ].into_iter().enumerate() {
                // Independent immutable snapshots can read the common baseline
                // while another snapshot changes unrelated output. Writes to
                // the same logical file still serialize; external absolute
                // resources still conflict in the physical pass below.
                if kind == PathConflictKind::Logical && pair_index > 0 && self.isolated && other.isolated
                    && !self.workspace.overlaps(&other.workspace) { continue; }
                for a in left {
                    for b in right {
                        if a.overlaps(b) {
                            return Some(ResourceConflict::Path {
                                kind,
                                left: a.key(),
                                right: b.key(),
                            });
                        }
                    }
                }
            }
        }
        if !self.logical_writes.is_empty()
            && !other.logical_writes.is_empty()
            && self.workspace.overlaps(&other.workspace)
        {
            return Some(ResourceConflict::WorkspaceWriters {
                left: self.workspace.key(),
                right: other.workspace.key(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::ExecutionBinding;
    use super::*;

    const ROOT: &str = "C:/Project";

    #[test]
    fn separate_snapshots_read_common_baseline_while_writing_disjoint_outputs() {
        let tasks = vec![task("backend", &["."], &["src/backend.rs"]), task("frontend", &["."], &["frontend/view.tsx"])];
        let claims = BTreeMap::from([("backend".into(), isolated("C:/attempts/backend")), ("frontend".into(), isolated("C:/attempts/frontend"))]);
        assert_eq!(schedule(&tasks, &claims).admitted_task_ids, vec!["backend", "frontend"]);
        let mut overlap = tasks.clone(); overlap[1].spec.write_paths = vec!["src/backend.rs".into()];
        assert_eq!(schedule(&overlap, &claims).admitted_task_ids, vec!["backend"]);
    }

    fn task(id: &str, reads: &[&str], writes: &[&str]) -> MissionTaskView {
        MissionTaskView {
            spec: MissionTaskSpec {
                id: id.into(),
                title: id.into(),
                objective: String::new(),
                depends_on: Vec::new(),
                read_paths: reads.iter().map(|s| (*s).into()).collect(),
                write_paths: writes.iter().map(|s| (*s).into()).collect(),
                acceptance_criteria: Vec::new(),
                required_checks: vec!["cargo test".into()],
                execution: ExecutionBinding {
                    expert: None,
                    executor: "himind.builtin".into(),
                    model: String::new(),
                    role_id: String::new(),
                    service_id: String::new(),
                },
                repair_of: String::new(),
            },
            status: TaskStatus::Ready,
            attempt_run_id: String::new(),
            verification_run_id: String::new(),
            verification_passed: None,
            evidence_refs: Vec::new(),
        }
    }

    fn isolated(root: &str) -> TaskResourceClaims {
        TaskResourceClaims {
            workspace: TaskWorkspace::Isolated { root: root.into() },
            ..TaskResourceClaims::default()
        }
    }

    fn schedule(
        tasks: &[MissionTaskView],
        claims: &BTreeMap<String, TaskResourceClaims>,
    ) -> ReadySet {
        ready_set(tasks, ROOT, claims, ParallelPolicy::default()).unwrap()
    }

    #[test]
    fn read_read_admits_together_and_input_is_unchanged() {
        let tasks = vec![task("a", &["src"], &[]), task("b", &["SRC/main.rs"], &[])];
        let before = tasks.clone();
        assert_eq!(
            schedule(&tasks, &BTreeMap::new()).admitted_task_ids,
            ["a", "b"]
        );
        assert_eq!(tasks, before);
    }

    #[test]
    fn overlapping_writes_and_read_write_conflict_in_both_directions() {
        for (left, right) in [
            (task("a", &[], &["src"]), task("b", &[], &["src/main.rs"])),
            (task("a", &["src"], &[]), task("b", &[], &["src/main.rs"])),
            (task("a", &[], &["src"]), task("b", &["src/main.rs"], &[])),
        ] {
            let result = schedule(&[left, right], &BTreeMap::new());
            assert_eq!(result.admitted_task_ids, ["a"]);
            assert!(
                matches!(&result.waiting[0].reason, WaitReason::ResourceBusy {
                holder_task_id, conflict: ResourceConflict::Path { kind: PathConflictKind::Logical, .. }
            } if holder_task_id == "a")
            );
        }
    }

    #[test]
    fn verified_dependencies_are_required_even_for_stale_ready_status() {
        let mut producer = task("producer", &[], &[]);
        let mut consumer = task("consumer", &[], &[]);
        consumer.spec.depends_on = vec!["producer".into()];
        for (status, verification) in [
            (TaskStatus::Completed, None),
            (TaskStatus::Completed, Some(false)),
            (TaskStatus::AwaitingVerification, Some(true)),
            (TaskStatus::Failed, Some(true)),
            (TaskStatus::Canceled, Some(true)),
            (TaskStatus::Running, Some(true)),
        ] {
            producer.status = status;
            producer.verification_passed = verification;
            let result = schedule(&[producer.clone(), consumer.clone()], &BTreeMap::new());
            assert!(result.admitted_task_ids.is_empty());
            assert_eq!(
                result.waiting[0].reason,
                WaitReason::DependenciesNotVerified {
                    task_ids: vec!["producer".into()],
                }
            );
        }
        producer.status = TaskStatus::Completed;
        producer.verification_passed = Some(true);
        consumer.status = TaskStatus::Blocked;
        assert_eq!(
            schedule(&[producer, consumer], &BTreeMap::new()).admitted_task_ids,
            ["consumer"]
        );
    }

    #[test]
    fn newly_admitted_predecessor_does_not_release_dependency_in_same_batch() {
        let a = task("a", &[], &[]);
        let mut b = task("b", &[], &[]);
        b.spec.depends_on = vec!["a".into()];
        assert_eq!(schedule(&[a, b], &BTreeMap::new()).admitted_task_ids, ["a"]);
    }

    #[test]
    fn different_isolated_workspaces_keep_logical_file_and_named_conflicts() {
        let tasks = [
            task("a", &[], &["src/main.rs"]),
            task("b", &[], &["SRC\\MAIN.rs"]),
        ];
        let mut claims = BTreeMap::from([
            ("a".into(), isolated("C:/workers/a")),
            ("b".into(), isolated("C:/workers/b")),
        ]);
        let result = schedule(&tasks, &claims);
        assert_eq!(result.admitted_task_ids, ["a"]);
        assert!(matches!(
            result.waiting[0].reason,
            WaitReason::ResourceBusy {
                conflict: ResourceConflict::Path {
                    kind: PathConflictKind::Logical,
                    ..
                },
                ..
            }
        ));
        for value in claims.values_mut() {
            value.exclusive_resources.insert("database:main".into());
        }
        let disjoint = [task("a", &[], &["src/a.rs"]), task("b", &[], &["src/b.rs"])];
        assert!(matches!(&schedule(&disjoint, &claims).waiting[0].reason,
            WaitReason::ResourceBusy { conflict: ResourceConflict::ExclusiveResource { name }, .. }
                if name == "database:main"));
        claims.get_mut("b").unwrap().exclusive_resources.clear();
        assert_eq!(schedule(&disjoint, &claims).admitted_task_ids, ["a", "b"]);
    }

    #[test]
    fn shared_or_reused_isolated_workspace_has_only_one_writer() {
        let tasks = [task("a", &[], &["src/a"]), task("b", &[], &["src/b"])];
        for claims in [
            BTreeMap::new(),
            BTreeMap::from([
                ("a".into(), isolated("C:/workers/same")),
                ("b".into(), isolated("c:\\WORKERS\\same\\")),
            ]),
        ] {
            let result = schedule(&tasks, &claims);
            assert_eq!(result.admitted_task_ids, ["a"]);
            assert!(matches!(
                result.waiting[0].reason,
                WaitReason::ResourceBusy {
                    conflict: ResourceConflict::WorkspaceWriters { .. },
                    ..
                }
            ));
        }
    }

    #[test]
    fn running_and_awaiting_verification_reserve_resources_regardless_of_order() {
        for status in [TaskStatus::Running, TaskStatus::AwaitingVerification] {
            let blocked = task("blocked", &[], &["src/a"]);
            let mut holder = task("holder", &[], &["src"]);
            holder.status = status;
            let unrelated = task("unrelated", &["docs"], &[]);
            let result = schedule(&[blocked, unrelated, holder], &BTreeMap::new());
            assert_eq!(result.active_task_count, 1);
            assert_eq!(result.admitted_task_ids, ["unrelated"]);
            assert!(matches!(&result.waiting[0].reason,
                WaitReason::ResourceBusy { holder_task_id, .. } if holder_task_id == "holder"));
        }
    }

    #[test]
    fn cap_counts_active_and_review_and_reserves_new_batch_members() {
        let mut tasks: Vec<_> = (0..5)
            .map(|n| task(&n.to_string(), &["src"], &[]))
            .collect();
        assert_eq!(
            schedule(&tasks, &BTreeMap::new()).admitted_task_ids,
            ["0", "1", "2"]
        );
        tasks[4].status = TaskStatus::Running;
        tasks[3].status = TaskStatus::AwaitingVerification;
        let result = schedule(&tasks, &BTreeMap::new());
        assert_eq!(result.active_task_count, 2);
        assert_eq!(result.admitted_task_ids, ["0"]);
        assert!(result
            .waiting
            .iter()
            .all(|wait| wait.reason == WaitReason::MaxActiveTasks { limit: 3 }));
        for cap in [1, 2] {
            let result = ready_set(
                &tasks,
                ROOT,
                &BTreeMap::new(),
                ParallelPolicy {
                    max_active_tasks: cap,
                },
            )
            .unwrap();
            assert!(result.admitted_task_ids.is_empty());
            assert_eq!(result.active_task_count, 2);
        }
        assert!(ready_set(
            &tasks,
            ROOT,
            &BTreeMap::new(),
            ParallelPolicy {
                max_active_tasks: 0
            }
        )
        .is_err());
    }

    #[test]
    fn terminal_and_proposed_tasks_are_not_restarted_or_counted_as_occupants() {
        let tasks: Vec<_> = [
            TaskStatus::Proposed,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Canceled,
        ]
        .into_iter()
        .enumerate()
        .map(|(n, status)| {
            let mut view = task(&n.to_string(), &[], &["src"]);
            view.status = status;
            view
        })
        .collect();
        let result = schedule(&tasks, &BTreeMap::new());
        assert!(result.admitted_task_ids.is_empty());
        assert!(result.waiting.is_empty());
        assert_eq!(result.active_task_count, 0);
    }

    #[test]
    fn path_rules_are_host_independent_and_match_only_component_ancestors() {
        for (root, left, right, overlaps) in [
            (ROOT, "SRC\\Main.rs", "c:/project/src/main.RS/", true),
            (ROOT, "./src//", "src/main.rs", true),
            (ROOT, ".", "src/main.rs", true),
            (ROOT, "src", "src-other/main.rs", false),
            (ROOT, "src/a", "src/ab", false),
            (ROOT, "C:/project", "C:/project-old", false),
            (ROOT, "C:/project/src", "D:/project/src", false),
            ("C:/", ".", "C:/anything", true),
            ("/repo", "src", "/repo/src/main.rs", true),
            ("/repo", "Src", "/repo/src", false),
            ("/repo", "src", "/repo/src2", false),
            ("/", ".", "/repo", true),
            ("/repo", "C:\\Project\\SRC", "c:/project/src/file", true),
            (
                "//server/share/repo",
                "Src",
                "\\\\SERVER\\SHARE\\repo\\src\\file",
                true,
            ),
            ("//server/share", ".", "//server/share-other/file", false),
        ] {
            assert_eq!(
                paths_overlap(root, left, right).unwrap(),
                overlaps,
                "{root}: {left}, {right}"
            );
        }
        for path in [
            "",
            "../src",
            "src/../other",
            "C:src",
            "src\0file",
            "src/file.",
            "//?/C:/repo",
        ] {
            assert!(paths_overlap(ROOT, path, "src").is_err(), "{path:?}");
        }
    }

    #[test]
    fn absolute_source_scopes_map_into_workspace_and_physical_aliases_conflict() {
        let left = task("a", &[], &["C:/Project/src/a"]);
        let right = task("b", &["C:/workers/a/src/a"], &[]);
        assert!(matches!(
            resource_conflict(
                ROOT,
                &left.spec,
                &isolated("C:/workers/a"),
                &right.spec,
                &isolated("C:/workers/b")
            )
            .unwrap(),
            Some(ResourceConflict::Path {
                kind: PathConflictKind::Physical,
                ..
            })
        ));
        let mut external = left;
        external.spec.write_paths = vec!["D:/database".into()];
        let reader = task("reader", &["d:\\DATABASE\\main.db"], &[]);
        assert!(resource_conflict(
            ROOT,
            &external.spec,
            &isolated("C:/workers/a"),
            &reader.spec,
            &isolated("C:/workers/b")
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn invalid_graphs_claims_and_limits_fail_closed() {
        let mut a = task("a", &[], &[]);
        let mut b = task("b", &[], &[]);
        a.spec.depends_on = vec!["b".into()];
        b.spec.depends_on = vec!["a".into()];
        assert!(ready_set(
            &[a.clone(), b],
            ROOT,
            &BTreeMap::new(),
            ParallelPolicy::default()
        )
        .is_err());
        a.spec.depends_on = vec!["missing".into()];
        assert!(ready_set(&[a], ROOT, &BTreeMap::new(), ParallelPolicy::default()).is_err());
        let duplicate = [task("a", &[], &[]), task("a", &[], &[])];
        assert!(ready_set(
            &duplicate,
            ROOT,
            &BTreeMap::new(),
            ParallelPolicy::default()
        )
        .is_err());
        let excessive: Vec<_> = (0..=MAX_SCHEDULE_TASKS)
            .map(|n| task(&n.to_string(), &[], &[]))
            .collect();
        assert!(ready_set(
            &excessive,
            ROOT,
            &BTreeMap::new(),
            ParallelPolicy::default()
        )
        .is_err());
        let claims = BTreeMap::from([("unknown".into(), TaskResourceClaims::default())]);
        assert!(ready_set(&[], ROOT, &claims, ParallelPolicy::default()).is_err());
    }
}
