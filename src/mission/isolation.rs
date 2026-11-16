//! Host-owned Git workspaces and conservative, receipt-based integration.
//!
//! The caller owns the source admission/integration lock and must stop the task
//! before integrating. Keep this storage outside task grants and retain it until
//! evidence/recovery is durable. There is deliberately no cleanup API. This is
//! filesystem isolation, not an OS sandbox or an execution authorization grant.
//! New allocations require a clean, complete Git root without submodules or any
//! untracked input (including ignored files). Existing allocations may reopen
//! after other branches have dirtied unrelated source paths. Deletions, links,
//! reparse points, nested repositories and changed HEADs fail closed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

const VERSION: u32 = 1;
const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
type Result<T> = std::result::Result<T, IsolationError>;
type Files = BTreeMap<String, FileStamp>;
type Changes = BTreeMap<String, Option<FileStamp>>;

#[derive(Debug)]
pub(crate) enum IsolationError {
    /// The host may explain this reason and use its serial execution policy.
    Unsupported(String),
    Rejected(String),
    Conflict(String),
    Failure(String),
}

impl fmt::Display for IsolationError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, reason) = match self {
            Self::Unsupported(reason) => ("unsupported isolation", reason),
            Self::Rejected(reason) => ("isolation rejected", reason),
            Self::Conflict(reason) => ("integration conflict", reason),
            Self::Failure(reason) => ("isolation failure", reason),
        };
        write!(out, "{kind}: {reason}")
    }
}

impl std::error::Error for IsolationError {}

impl From<std::io::Error> for IsolationError {
    fn from(error: std::io::Error) -> Self {
        Self::Failure(error.to_string())
    }
}

impl From<serde_json::Error> for IsolationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Failure(error.to_string())
    }
}

struct PrepareRequest<'a> {
    pub source_root: &'a Path,
    pub storage_root: &'a Path,
    pub mission_id: &'a str,
    pub task_id: &'a str,
    pub attempt_id: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileStamp {
    pub sha256: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskIsolation {
    pub version: u32,
    pub storage_root: PathBuf,
    pub source_root: PathBuf,
    pub isolated_root: PathBuf,
    pub mission_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub baseline_head: String,
    pub git_common_dir: PathBuf,
    pub worktree_git_dir: PathBuf,
    pub baseline_files: Files,
}

#[derive(Debug)]
pub(crate) enum Preparation {
    Ready(TaskIsolation),
    Unsupported { reason: String },
}

impl TaskIsolation {
    pub(crate) fn isolated_root(&self) -> &Path {
        &self.isolated_root
    }

    pub(crate) fn integrate(&self, write_paths: &[String]) -> Result<IntegrationResult> {
        integrate(self, write_paths)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntegrationReceipt {
    pub version: u32,
    pub source_root: PathBuf,
    pub mission_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub baseline_head: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationResult {
    pub receipt: IntegrationReceipt,
    pub already_integrated: bool,
    pub patch_path: PathBuf,
    pub evidence_path: PathBuf,
    pub receipt_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MissionBaseline {
    version: u32,
    mission_id: String,
    source_root: PathBuf,
    head: String,
    files: Files,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IntegrationEvidence {
    receipt: IntegrationReceipt,
    write_paths: Vec<String>,
    before: Changes,
    after: Changes,
}

/// Call under the host admission lock. All tasks in a mission pin the same HEAD.
/// A failed/unknown allocation is preserved for inspection, never removed.
pub(crate) fn prepare(
    source_root: &Path,
    storage_root: &Path,
    mission_id: &str,
    task_id: &str,
    attempt_id: &str,
) -> Result<Preparation> {
    match prepare_request(PrepareRequest {
        source_root,
        storage_root,
        mission_id,
        task_id,
        attempt_id,
    }) {
        Ok(isolation) => Ok(Preparation::Ready(isolation)),
        Err(IsolationError::Unsupported(reason)) => Ok(Preparation::Unsupported { reason }),
        Err(error) => Err(error),
    }
}

fn prepare_request(request: PrepareRequest<'_>) -> Result<TaskIsolation> {
    for id in [request.mission_id, request.task_id, request.attempt_id] {
        if id.is_empty() || id.len() > 160 || id.chars().any(char::is_control) {
            return Err(IsolationError::Rejected(
                "invalid mission/task/attempt identity".into(),
            ));
        }
    }
    let source = canonical_directory(request.source_root)
        .map_err(|error| IsolationError::Unsupported(error.to_string()))?;
    let storage = future_directory(request.storage_root)?;
    if path_contains(&source, &storage) || path_contains(&storage, &source) {
        return Err(IsolationError::Rejected(
            "host storage and source must be separate directories".into(),
        ));
    }
    checked_create_dir(&storage)?;
    let storage = canonical_directory(&storage)?;
    let mission_dir = storage
        .join("mission-isolation-v1")
        .join(id_key(request.mission_id));
    let attempt_dir = mission_dir
        .join(id_key(request.task_id))
        .join(id_key(request.attempt_id));
    let isolated = attempt_dir.join("worktree");
    guard_path(&attempt_dir)?;
    let metadata_path = attempt_dir.join("attempt.json");
    if metadata_path.exists() {
        let record: TaskIsolation = read_json(&metadata_path)?;
        if record.version != VERSION
            || record.source_root != source
            || record.storage_root != storage
            || record.isolated_root != isolated
            || record.mission_id != request.mission_id
            || record.task_id != request.task_id
            || record.attempt_id != request.attempt_id
        {
            return Err(IsolationError::Rejected(
                "existing allocation identity mismatch".into(),
            ));
        }
        verify_attempt(&record)?;
        return Ok(record);
    }
    if isolated.exists() {
        return Err(IsolationError::Rejected(
            "unrecorded workspace exists; preserved without cleanup".into(),
        ));
    }
    let head = preflight_source(&source)?;
    let modes = tree_modes(&source, &head)?;
    let files = snapshot(&source, &modes)
        .map_err(|error| IsolationError::Unsupported(error.to_string()))?;
    if files.keys().ne(modes.keys()) {
        return Err(IsolationError::Unsupported(
            "untracked/ignored source inputs cannot be silently omitted".into(),
        ));
    }
    let baseline = MissionBaseline {
        version: VERSION,
        mission_id: request.mission_id.into(),
        source_root: source.clone(),
        head: head.clone(),
        files: files.clone(),
    };
    checked_create_dir(&attempt_dir)?;
    let pin_path = mission_dir.join("baseline.json");
    if pin_path.exists() {
        if read_json::<MissionBaseline>(&pin_path)? != baseline {
            return Err(IsolationError::Unsupported(
                "mission source/baseline changed; do not rebind existing tasks".into(),
            ));
        }
    } else {
        write_json_new(&pin_path, &baseline)?;
    }
    let common = git_directory(&source, "--git-common-dir")?;
    git(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            "--",
            path_str(&isolated)?,
            &head,
        ],
        None,
    )?;
    let record = TaskIsolation {
        version: VERSION,
        storage_root: storage,
        source_root: source,
        isolated_root: isolated,
        mission_id: request.mission_id.into(),
        task_id: request.task_id.into(),
        attempt_id: request.attempt_id.into(),
        baseline_head: head,
        git_common_dir: common,
        worktree_git_dir: git_directory(&attempt_dir.join("worktree"), "--git-dir")?,
        baseline_files: files,
    };
    if snapshot(&record.isolated_root, &modes)? != record.baseline_files {
        return Err(IsolationError::Unsupported(
            "Git checkout transformed baseline bytes; workspace preserved".into(),
        ));
    }
    write_json_new(&metadata_path, &record)?;
    verify_attempt(&record)?;
    Ok(record)
}

/// Call with the task stopped and the source integration lock held. This never
/// commits, stages the source index, deletes user files or releases task claims.
/// An intent is durable before apply; replay recognizes its exact after-state
/// following a crash between apply and receipt. Mixed states require recovery.
pub(crate) fn integrate(
    attempt: &TaskIsolation,
    write_paths: &[String],
) -> Result<IntegrationResult> {
    verify_attempt(attempt)?;
    let directory = attempt_directory(attempt);
    let mut scopes: Vec<_> = write_paths
        .iter()
        .map(|path| relative_path(path))
        .collect::<Result<_>>()?;
    scopes.sort();
    scopes.dedup();
    for scope in &scopes {
        guard_path(&attempt.source_root.join(scope))?;
        guard_path(&attempt.isolated_root.join(scope))?;
    }
    let candidate = snapshot(&attempt.isolated_root, &baseline_modes(attempt))?;
    let paths = changed_paths(&attempt.baseline_files, &candidate);
    for path in &paths {
        if !scopes.iter().any(|scope| scope_contains(scope, path)) {
            return Err(IsolationError::Rejected(format!(
                "changed path is outside declared write_paths: {path}"
            )));
        }
        guard_path(&attempt.source_root.join(path))?;
        if !candidate.contains_key(path) {
            return Err(IsolationError::Unsupported(format!(
                "deletion is not supported; source file is retained: {path}"
            )));
        }
    }
    let before = changes_for(&paths, &attempt.baseline_files);
    let after = changes_for(&paths, &candidate);
    let evidence_path = directory.join("integration.json");
    let patch_path = directory.join("integration.patch");
    let receipt_path = directory.join("receipt.json");
    let existing = evidence_path.exists();
    let evidence: IntegrationEvidence = if existing {
        let evidence: IntegrationEvidence = read_json(&evidence_path)?;
        let expected = receipt_for(
            attempt,
            paths.clone(),
            evidence.receipt.patch_sha256.clone(),
        );
        if evidence.receipt != expected
            || evidence.write_paths != scopes
            || evidence.before != before
            || evidence.after != after
            || sha256(&read_regular(&patch_path)?) != evidence.receipt.patch_sha256
        {
            return Err(IsolationError::Rejected(
                "candidate, authorization or saved patch changed after integration intent".into(),
            ));
        }
        evidence
    } else {
        if receipt_path.exists() || patch_path.exists() {
            return Err(IsolationError::Rejected(
                "incomplete host evidence; preserved for explicit recovery".into(),
            ));
        }
        require_state(
            attempt,
            &before,
            "affected source files no longer match the baseline",
        )?;
        let patch = build_patch(attempt, &paths, &after)?;
        let evidence = IntegrationEvidence {
            receipt: receipt_for(attempt, paths.clone(), sha256(&patch)),
            write_paths: scopes,
            before,
            after,
        };
        write_new(&patch_path, &patch)?;
        write_json_new(&evidence_path, &evidence)?;
        evidence
    };
    let result = |already_integrated| IntegrationResult {
        receipt: evidence.receipt.clone(),
        already_integrated,
        patch_path: patch_path.clone(),
        evidence_path: evidence_path.clone(),
        receipt_path: receipt_path.clone(),
    };
    if receipt_path.exists() {
        if read_json::<IntegrationReceipt>(&receipt_path)? != evidence.receipt {
            return Err(IsolationError::Rejected(
                "integration receipt identity mismatch".into(),
            ));
        }
        require_state(
            attempt,
            &evidence.after,
            "source changed after its recorded integration",
        )?;
        return Ok(result(true));
    }
    if existing && state_matches(attempt, &evidence.after)? {
        write_json_new(&receipt_path, &evidence.receipt)?;
        return Ok(result(true));
    }
    require_state(
        attempt,
        &evidence.before,
        "source is changed or partially integrated; refusing to reapply",
    )?;
    if !paths.is_empty() {
        git(
            &attempt.source_root,
            &[
                "apply",
                "--binary",
                "--whitespace=nowarn",
                "--check",
                path_str(&patch_path)?,
            ],
            None,
        )
        .map_err(|error| IsolationError::Conflict(error.to_string()))?;
        // Recheck after --check: no grant to overwrite intervening source edits.
        require_state(
            attempt,
            &evidence.before,
            "source changed during apply preflight",
        )?;
        git(
            &attempt.source_root,
            &[
                "apply",
                "--binary",
                "--whitespace=nowarn",
                path_str(&patch_path)?,
            ],
            None,
        )?;
    }
    require_state(
        attempt,
        &evidence.after,
        "apply did not produce the recorded candidate; evidence retained",
    )?;
    write_json_new(&receipt_path, &evidence.receipt)?;
    Ok(result(false))
}

fn receipt_for(
    attempt: &TaskIsolation,
    changed_paths: Vec<String>,
    patch_sha256: String,
) -> IntegrationReceipt {
    IntegrationReceipt {
        version: VERSION,
        source_root: attempt.source_root.clone(),
        mission_id: attempt.mission_id.clone(),
        task_id: attempt.task_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        baseline_head: attempt.baseline_head.clone(),
        patch_sha256,
        changed_paths,
    }
}

fn preflight_source(root: &Path) -> Result<String> {
    guard_path(&root.join(".git"))
        .map_err(|error| IsolationError::Unsupported(error.to_string()))?;
    let top = git(root, &["rev-parse", "--show-toplevel"], None).map_err(|_| {
        IsolationError::Unsupported("source must be a Git repository root with a commit".into())
    })?;
    if canonical_directory(Path::new(text(&top)?.trim()))? != root {
        return Err(IsolationError::Unsupported(
            "source must be the exact Git root, not a repository subdirectory".into(),
        ));
    }
    let status = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored",
        ],
        None,
    )?;
    if !status.is_empty() {
        return Err(IsolationError::Unsupported(
            "source must be clean, with no staged, changed, untracked or ignored inputs".into(),
        ));
    }
    let entries = git(root, &["ls-files", "-v", "-z"], None)?;
    if entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry[0] == b'S' || entry[0].is_ascii_lowercase())
    {
        return Err(IsolationError::Unsupported(
            "sparse/skip-worktree/assume-unchanged inputs are unsupported".into(),
        ));
    }
    head(root).map_err(|error| {
        IsolationError::Unsupported(format!(
            "source requires an existing baseline commit: {error}"
        ))
    })
}

fn tree_modes(root: &Path, baseline: &str) -> Result<BTreeMap<String, String>> {
    let mut modes = BTreeMap::new();
    let tree = git(root, &["ls-tree", "-r", "-z", baseline], None)?;
    for entry in tree
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = text(entry)?;
        let (header, raw) = entry
            .split_once('\t')
            .ok_or_else(|| IsolationError::Failure("invalid Git tree entry".into()))?;
        let mode = header.split_whitespace().next().unwrap_or("");
        if !matches!(mode, "100644" | "100755") || raw.eq_ignore_ascii_case(".gitmodules") {
            return Err(IsolationError::Unsupported(
                "submodules, symbolic links and special Git entries are unsupported".into(),
            ));
        }
        modes.insert(relative_path(raw)?, mode.into());
    }
    Ok(modes)
}

fn baseline_modes(attempt: &TaskIsolation) -> BTreeMap<String, String> {
    attempt
        .baseline_files
        .iter()
        .map(|(path, stamp)| (path.clone(), stamp.mode.clone()))
        .collect()
}

fn verify_attempt(attempt: &TaskIsolation) -> Result<()> {
    let expected = attempt
        .storage_root
        .join("mission-isolation-v1")
        .join(id_key(&attempt.mission_id))
        .join(id_key(&attempt.task_id))
        .join(id_key(&attempt.attempt_id))
        .join("worktree");
    if attempt.version != VERSION
        || attempt.isolated_root != expected
        || path_contains(&attempt.source_root, &attempt.storage_root)
        || path_contains(&attempt.storage_root, &attempt.source_root)
    {
        return Err(IsolationError::Rejected(
            "invalid host allocation path/version".into(),
        ));
    }
    guard_path(&attempt.source_root)?;
    guard_path(&attempt.isolated_root)?;
    guard_path(&attempt.source_root.join(".git"))?;
    guard_path(&attempt.isolated_root.join(".git"))?;
    let saved: TaskIsolation = read_json(&attempt_directory(attempt).join("attempt.json"))?;
    let pin: MissionBaseline = read_json(
        &attempt_directory(attempt)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("baseline.json"),
    )?;
    if saved != *attempt
        || pin.version != VERSION
        || pin.source_root != attempt.source_root
        || pin.mission_id != attempt.mission_id
        || pin.head != attempt.baseline_head
        || pin.files != attempt.baseline_files
    {
        return Err(IsolationError::Rejected(
            "host metadata/source/baseline identity mismatch".into(),
        ));
    }
    if head(&attempt.source_root)? != attempt.baseline_head
        || head(&attempt.isolated_root)? != attempt.baseline_head
    {
        return Err(IsolationError::Conflict(
            "source or isolated HEAD changed from the pinned baseline".into(),
        ));
    }
    if git_directory(&attempt.source_root, "--git-common-dir")? != attempt.git_common_dir
        || git_directory(&attempt.isolated_root, "--git-common-dir")? != attempt.git_common_dir
        || git_directory(&attempt.isolated_root, "--git-dir")? != attempt.worktree_git_dir
        || attempt.worktree_git_dir == attempt.git_common_dir
    {
        return Err(IsolationError::Rejected(
            "workspace no longer belongs to its recorded Git worktree".into(),
        ));
    }
    let symbolic = git_command(&attempt.isolated_root, None)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()?;
    if symbolic.status.code() != Some(1) {
        return Err(IsolationError::Rejected(
            "task workspace must retain detached HEAD".into(),
        ));
    }
    Ok(())
}

fn attempt_directory(attempt: &TaskIsolation) -> PathBuf {
    // verify_attempt binds this exact depth and layout before reading metadata.
    attempt.isolated_root.parent().unwrap().to_path_buf()
}

fn head(root: &Path) -> Result<String> {
    let value = git(root, &["rev-parse", "--verify", "HEAD^{commit}"], None)?;
    let value = text(&value)?.trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IsolationError::Unsupported(
            "Git HEAD is not a supported immutable commit identity".into(),
        ));
    }
    Ok(value.into())
}

fn git_directory(root: &Path, argument: &str) -> Result<PathBuf> {
    let value = git(root, &["rev-parse", argument], None)?;
    let path = PathBuf::from(text(&value)?.trim());
    canonical_directory(&if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn snapshot(root: &Path, modes: &BTreeMap<String, String>) -> Result<Files> {
    guard_path(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut files = Files::new();
    let mut total = 0;
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        guard_path(&directory)?;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if directory == root && entry.file_name().eq_ignore_ascii_case(".git") {
                guard_path(&path)?;
                continue;
            }
            entries += 1;
            if entries > MAX_FILES {
                return Err(IsolationError::Unsupported(
                    "workspace exceeds 10000 entries".into(),
                ));
            }
            guard_path(&path)?;
            let relative = relative_path(path_str(path.strip_prefix(root).unwrap())?)?;
            #[cfg(unix)]
            if path_str(path.strip_prefix(root).unwrap())?.contains('\\') {
                return Err(IsolationError::Unsupported(
                    "literal backslashes in Git filenames are unsupported".into(),
                ));
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                total += metadata.len();
                if total > MAX_BYTES {
                    return Err(IsolationError::Unsupported(
                        "workspace exceeds 256 MiB".into(),
                    ));
                }
                files.insert(
                    relative.clone(),
                    stamp(&path, modes.get(&relative).map(String::as_str))?.unwrap(),
                );
            } else {
                return Err(IsolationError::Rejected(
                    "workspace contains a special file".into(),
                ));
            }
        }
    }
    Ok(files)
}

fn stamp(path: &Path, baseline_mode: Option<&str>) -> Result<Option<FileStamp>> {
    guard_path(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(IsolationError::Rejected(format!(
            "expected a bounded regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        }
    };
    #[cfg(not(unix))]
    let mode = baseline_mode.unwrap_or("100644");
    #[cfg(unix)]
    let _ = baseline_mode;
    let bytes = read_regular(path)?;
    Ok(Some(FileStamp {
        sha256: sha256(&bytes),
        mode: mode.into(),
    }))
}

fn changed_paths(before: &Files, after: &Files) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

fn changes_for(paths: &[String], files: &Files) -> Changes {
    paths
        .iter()
        .map(|path| (path.clone(), files.get(path).cloned()))
        .collect()
}

fn state_matches(attempt: &TaskIsolation, expected: &Changes) -> Result<bool> {
    if head(&attempt.source_root)? != attempt.baseline_head {
        return Err(IsolationError::Conflict("source HEAD changed".into()));
    }
    let staged = git(
        &attempt.source_root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            &attempt.baseline_head,
            "--",
        ],
        None,
    )?;
    for path in staged
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = relative_path(text(path)?)?;
        if expected
            .keys()
            .any(|affected| scope_contains(&path, affected) || scope_contains(affected, &path))
        {
            return Ok(false);
        }
    }
    for (path, expected) in expected {
        if stamp(
            &attempt.source_root.join(path),
            expected.as_ref().map(|stamp| stamp.mode.as_str()),
        )? != *expected
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn require_state(attempt: &TaskIsolation, expected: &Changes, reason: &str) -> Result<()> {
    if !state_matches(attempt, expected)? {
        return Err(IsolationError::Conflict(reason.into()));
    }
    Ok(())
}

fn build_patch(attempt: &TaskIsolation, paths: &[String], after: &Changes) -> Result<Vec<u8>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let index = attempt_directory(attempt).join("candidate.index");
    guard_path(&index)?;
    git(
        &attempt.isolated_root,
        &["read-tree", &attempt.baseline_head],
        Some(&index),
    )?;
    // Raw source blobs make the patch match actual baseline bytes, including
    // CRLF/filter checkouts, without running arbitrary clean/textconv filters.
    for path in paths {
        if let Some(stamp) = attempt.baseline_files.get(path) {
            index_file(attempt, &index, &attempt.source_root, path, &stamp.mode)?;
        }
    }
    let base = git(&attempt.isolated_root, &["write-tree"], Some(&index))?;
    for path in paths {
        index_file(
            attempt,
            &index,
            &attempt.isolated_root,
            path,
            &after[path].as_ref().unwrap().mode,
        )?;
    }
    git(
        &attempt.isolated_root,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            text(&base)?.trim(),
            "--",
        ],
        Some(&index),
    )
}

fn index_file(
    attempt: &TaskIsolation,
    index: &Path,
    root: &Path,
    path: &str,
    mode: &str,
) -> Result<()> {
    let absolute = root.join(path);
    guard_path(&absolute)?;
    let object = git(
        &attempt.isolated_root,
        &[
            "hash-object",
            "-w",
            "--no-filters",
            "--",
            path_str(&absolute)?,
        ],
        None,
    )?;
    git(
        &attempt.isolated_root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            mode,
            text(&object)?.trim(),
            path,
        ],
        Some(index),
    )?;
    Ok(())
}

fn git_command(root: &Path, index: Option<&Path>) -> Command {
    let mut command = crate::runtime::process::hidden_command("git");
    // Inherited Git routing/index/config injection must not redirect the host.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}

fn git(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let Output {
        status,
        stdout,
        stderr,
    } = git_command(root, index).args(args).output()?;
    if !status.success() {
        return Err(IsolationError::Failure(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(stdout)
}

fn relative_path(raw: &str) -> Result<String> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains(':')
        || normalized.chars().any(char::is_control)
    {
        return Err(IsolationError::Rejected(format!(
            "a workspace-relative path is required: {raw:?}"
        )));
    }
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.eq_ignore_ascii_case(".git") || part.ends_with(['.', ' ']) {
            return Err(IsolationError::Rejected(format!(
                "unsafe relative path: {raw:?}"
            )));
        }
        parts.push(part);
    }
    Ok(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

fn scope_contains(scope: &str, path: &str) -> bool {
    let (scope, path) = if cfg!(windows) {
        (scope.to_lowercase(), path.to_lowercase())
    } else {
        (scope.to_string(), path.to_string())
    };
    scope == "."
        || scope == path
        || path
            .strip_prefix(&scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn guard_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(IsolationError::Rejected(
            "host paths must be absolute without parent traversal".into(),
        ));
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                let linked = metadata.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || metadata.file_attributes() & 0x400 != 0
                };
                if linked {
                    return Err(IsolationError::Rejected(format!(
                        "symlink/reparse path is unsupported: {}",
                        ancestor.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn future_absolute(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    guard_path(&path)?;
    Ok(path)
}

fn future_directory(path: &Path) -> Result<PathBuf> {
    let path = future_absolute(path)?;
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(
            ancestor.file_name().ok_or_else(|| {
                IsolationError::Rejected("storage has no existing ancestor".into())
            })?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| IsolationError::Rejected("storage has no existing ancestor".into()))?;
    }
    let mut resolved = canonical_directory(ancestor)?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    guard_path(&resolved)?;
    Ok(resolved)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let path = future_absolute(path)?;
    let canonical = fs::canonicalize(&path)?;
    #[cfg(windows)]
    let canonical = {
        let value = path_str(&canonical)?;
        PathBuf::from(if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            value.strip_prefix(r"\\?\").unwrap_or(value).to_string()
        })
    };
    if !canonical.is_dir() {
        return Err(IsolationError::Unsupported(
            "workspace is not a directory".into(),
        ));
    }
    guard_path(&canonical)?;
    Ok(canonical)
}

fn checked_create_dir(path: &Path) -> Result<()> {
    guard_path(path)?;
    fs::create_dir_all(path)?;
    guard_path(path)
}

fn path_contains(parent: &Path, path: &Path) -> bool {
    if cfg!(windows) {
        PathBuf::from(parent.to_string_lossy().to_lowercase())
            .components()
            .zip(PathBuf::from(path.to_string_lossy().to_lowercase()).components())
            .all(|(left, right)| left == right)
            && parent.components().count() <= path.components().count()
    } else {
        path.starts_with(parent)
    }
}

fn read_regular(path: &Path) -> Result<Vec<u8>> {
    guard_path(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(IsolationError::Rejected(
            "expected a bounded regular evidence/content file".into(),
        ));
    }
    Ok(fs::read(path)?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&read_regular(path)?)?)
}

fn write_json_new(path: &Path, value: &impl Serialize) -> Result<()> {
    write_new(path, &serde_json::to_vec_pretty(value)?)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    guard_path(path)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn id_key(id: &str) -> String {
    // Full identities are checked in metadata before reuse, including on a
    // hash collision. Compact paths keep Git for Windows within its limits.
    sha256(id.as_bytes())[..16].into()
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| {
        IsolationError::Unsupported("non-UTF-8 Git paths/output are unsupported".into())
    })
}
fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        IsolationError::Unsupported("non-UTF-8 filesystem path is unsupported".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Repo {
        root: PathBuf,
        source: PathBuf,
        storage: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "himind-isolation-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let source = root.join("source");
            let storage = root.join("host");
            fs::create_dir_all(source.join("src")).unwrap();
            git(&source, &["init", "--quiet"], None).unwrap();
            git(
                &source,
                &["config", "user.email", "fixture@example.invalid"],
                None,
            )
            .unwrap();
            git(&source, &["config", "user.name", "Isolation Fixture"], None).unwrap();
            git(&source, &["config", "core.autocrlf", "false"], None).unwrap();
            fs::write(source.join("src/a.txt"), b"a baseline\n").unwrap();
            fs::write(source.join("src/b.txt"), b"b baseline\n").unwrap();
            git(&source, &["add", "."], None).unwrap();
            git(
                &source,
                &["commit", "--quiet", "-m", "fixture baseline"],
                None,
            )
            .unwrap();
            Self {
                root,
                source,
                storage,
            }
        }

        fn prepare(&self, task: &str) -> Result<TaskIsolation> {
            prepare_request(PrepareRequest {
                source_root: &self.source,
                storage_root: &self.storage,
                mission_id: "mission",
                task_id: task,
                attempt_id: "attempt-1",
            })
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            // Only the exact, uniquely created fixture root is eligible. Production
            // code has no directory removal or worktree prune operation.
            if self.root.parent() == Some(std::env::temp_dir().as_path())
                && self
                    .root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("himind-isolation-")
            {
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }

    fn writes(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).into()).collect()
    }

    #[test]
    fn two_detached_worktrees_integrate_disjoint_files_and_replay_once() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        let b = repo.prepare("b").unwrap();
        assert_ne!(a.isolated_root, b.isolated_root);
        assert_eq!(a.baseline_head, b.baseline_head);
        fs::write(a.isolated_root.join("src/a.txt"), b"a output\n").unwrap();
        fs::write(b.isolated_root.join("src/b.txt"), b"b output\n").unwrap();
        let first = integrate(&a, &writes(&["src/a.txt"])).unwrap();
        assert!(!first.already_integrated);
        let second = integrate(&b, &writes(&["src/b.txt"])).unwrap();
        assert!(!second.already_integrated);
        assert_eq!(
            fs::read(repo.source.join("src/a.txt")).unwrap(),
            b"a output\n"
        );
        assert_eq!(
            fs::read(repo.source.join("src/b.txt")).unwrap(),
            b"b output\n"
        );
        assert_eq!(head(&repo.source).unwrap(), a.baseline_head);
        assert!(
            git(&repo.source, &["diff", "--cached", "--name-only"], None)
                .unwrap()
                .is_empty()
        );
        let replay = integrate(&a, &writes(&["src/a.txt"])).unwrap();
        assert!(replay.already_integrated);
        assert_eq!(replay.receipt, first.receipt);
        assert_eq!(
            sha256(&fs::read(&replay.patch_path).unwrap()),
            replay.receipt.patch_sha256
        );
        assert_eq!(repo.prepare("a").unwrap(), a);
    }

    #[test]
    fn actual_untracked_binary_files_are_integrated_without_using_task_index() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        let bytes = [0, 255, 1, 0, 13, 10, 128];
        fs::write(a.isolated_root.join("src/new.bin"), bytes).unwrap();
        // The task's staged state is never used as an integration authority.
        git(&a.isolated_root, &["add", "src/new.bin"], None).unwrap();
        fs::write(
            a.isolated_root.join("src/new.bin"),
            [bytes.as_slice(), &[7]].concat(),
        )
        .unwrap();
        let result = integrate(&a, &writes(&["src"])).unwrap();
        assert_eq!(result.receipt.changed_paths, ["src/new.bin"]);
        assert_eq!(
            fs::read(repo.source.join("src/new.bin")).unwrap(),
            [bytes.as_slice(), &[7]].concat()
        );
        assert!(
            String::from_utf8_lossy(&fs::read(result.patch_path).unwrap())
                .contains("GIT binary patch")
        );
    }

    #[test]
    fn out_of_scope_sibling_and_same_file_conflicts_are_refused() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        let b = repo.prepare("b").unwrap();
        fs::write(a.isolated_root.join("src/a.txt"), b"a output\n").unwrap();
        assert!(matches!(
            integrate(&a, &writes(&["src/a"])),
            Err(IsolationError::Rejected(_))
        ));
        fs::write(b.isolated_root.join("src/a.txt"), b"b competing output\n").unwrap();
        integrate(&a, &writes(&["src/a.txt"])).unwrap();
        assert!(matches!(
            integrate(&b, &writes(&["src"])),
            Err(IsolationError::Conflict(_))
        ));
        assert_eq!(
            fs::read(repo.source.join("src/a.txt")).unwrap(),
            b"a output\n"
        );
    }

    #[test]
    fn original_dirty_untracked_ignored_submodule_and_nonroot_inputs_are_unsupported() {
        for kind in [
            "dirty",
            "untracked",
            "ignored",
            "submodule",
            "nonroot",
            "not-git",
            "unborn",
        ] {
            let repo = Repo::new();
            match kind {
                "dirty" => fs::write(repo.source.join("src/a.txt"), b"dirty\n").unwrap(),
                "untracked" => fs::write(repo.source.join("private-input.txt"), b"input").unwrap(),
                "ignored" => {
                    fs::write(repo.source.join(".git/info/exclude"), b"secret.txt\n").unwrap();
                    fs::write(repo.source.join("secret.txt"), b"input").unwrap();
                }
                "submodule" => {
                    fs::write(
                        repo.source.join(".gitmodules"),
                        b"[submodule \"child\"]\npath = child\n",
                    )
                    .unwrap();
                    git(&repo.source, &["add", ".gitmodules"], None).unwrap();
                    git(
                        &repo.source,
                        &["commit", "--quiet", "-m", "fixture submodule"],
                        None,
                    )
                    .unwrap();
                }
                "unborn" => {
                    let unborn = repo.root.join("unborn");
                    fs::create_dir(&unborn).unwrap();
                    git(&unborn, &["init", "--quiet"], None).unwrap();
                }
                _ => {}
            }
            let source = match kind {
                "nonroot" => repo.source.join("src"),
                "unborn" => repo.root.join("unborn"),
                "not-git" => {
                    let plain = repo.root.join("plain");
                    fs::create_dir(&plain).unwrap();
                    plain
                }
                _ => repo.source.clone(),
            };
            assert!(
                matches!(
                    prepare_request(PrepareRequest {
                        source_root: &source,
                        storage_root: &repo.storage,
                        mission_id: "mission",
                        task_id: "a",
                        attempt_id: "one"
                    }),
                    Err(IsolationError::Unsupported(_))
                ),
                "{kind}"
            );
        }
    }

    #[test]
    fn crash_after_apply_recovers_receipt_without_applying_twice() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        fs::write(a.isolated_root.join("src/a.txt"), b"once\n").unwrap();
        let result = integrate(&a, &writes(&["src/a.txt"])).unwrap();
        fs::remove_file(&result.receipt_path).unwrap();
        let replay = integrate(&a, &writes(&["src/a.txt"])).unwrap();
        assert!(replay.already_integrated);
        assert!(replay.receipt_path.is_file());
        assert_eq!(fs::read(repo.source.join("src/a.txt")).unwrap(), b"once\n");
        fs::write(
            a.isolated_root.join("src/a.txt"),
            b"changed after receipt\n",
        )
        .unwrap();
        assert!(matches!(
            integrate(&a, &writes(&["src/a.txt"])),
            Err(IsolationError::Rejected(_))
        ));
    }

    #[test]
    fn deletions_traversal_and_absolute_scopes_do_not_touch_source() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        for scope in [
            "../src",
            "C:/outside",
            "/outside",
            ".git/config",
            "src/../src",
        ] {
            assert!(matches!(
                integrate(&a, &writes(&[scope])),
                Err(IsolationError::Rejected(_))
            ));
        }
        fs::remove_file(a.isolated_root.join("src/a.txt")).unwrap();
        assert!(matches!(
            integrate(&a, &writes(&["src"])),
            Err(IsolationError::Unsupported(_))
        ));
        assert_eq!(
            fs::read(repo.source.join("src/a.txt")).unwrap(),
            b"a baseline\n"
        );
    }

    #[test]
    fn source_head_change_and_forged_metadata_are_refused() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        let mut forged = a.clone();
        forged.task_id = "different-task".into();
        assert!(matches!(
            integrate(&forged, &writes(&["src"])),
            Err(IsolationError::Rejected(_))
        ));
        git(
            &repo.source,
            &[
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "fixture changed HEAD",
            ],
            None,
        )
        .unwrap();
        assert!(matches!(
            integrate(&a, &writes(&["src"])),
            Err(IsolationError::Conflict(_))
        ));
    }

    #[test]
    fn symlink_or_junction_candidate_is_rejected() {
        let repo = Repo::new();
        let a = repo.prepare("a").unwrap();
        let outside = repo.root.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("private.txt"), b"do not read").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, a.isolated_root.join("src/link")).unwrap();
        #[cfg(windows)]
        {
            let status = crate::runtime::process::hidden_command("cmd.exe")
                .current_dir(a.isolated_root.join("src"))
                .args(["/D", "/C", "mklink", "/J", "link"])
                .arg(&outside)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
        assert!(matches!(
            integrate(&a, &writes(&["src"])),
            Err(IsolationError::Rejected(_))
        ));
        assert_eq!(
            fs::read(outside.join("private.txt")).unwrap(),
            b"do not read"
        );
    }

    #[test]
    fn public_preparation_api_preserves_identity_and_rejects_nested_storage() {
        let repo = Repo::new();
        let Preparation::Ready(isolation) =
            prepare(&repo.source, &repo.storage, "mission", "api", "one").unwrap()
        else {
            panic!("clean root must be supported");
        };
        assert_eq!(isolation.isolated_root(), isolation.isolated_root.as_path());
        let Preparation::Ready(replay) =
            prepare(&repo.source, &repo.storage, "mission", "api", "one").unwrap()
        else {
            panic!("recorded allocation must reopen");
        };
        assert_eq!(isolation, replay);
        assert!(matches!(
            prepare(
                &repo.source,
                &repo.source.join(".host"),
                "mission",
                "api",
                "two"
            ),
            Err(IsolationError::Rejected(_))
        ));
        fs::write(repo.source.join("src/a.txt"), b"dirty\n").unwrap();
        assert!(matches!(
            prepare(&repo.source, &repo.storage, "mission", "new-task", "one"),
            Ok(Preparation::Unsupported { .. })
        ));
    }
}
