//! Shared state contract for activating a locally built extension.
//!
//! A candidate is not an active runtime asset.  This module owns the boundary
//! between candidate confirmation and the single active development version
//! for a given profile/target/extension identity.

use crate::extension_projects::ExtensionProjectKind;
use crate::store::atomic_file;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const EXTENSION_ACTIVATION_SCHEMA_VERSION: &str = "extension_activation.v1";
const EXTENSION_ACTIVATION_JOURNAL_SCHEMA_VERSION: &str = "extension_activation_journal.v1";
static ACTIVATION_TRANSACTION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationKind {
    Plugin,
    Skill,
    Workflow,
    Expert,
    Instruction,
}

impl ActivationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::Expert => "expert",
            Self::Instruction => "instruction",
        }
    }
}

impl From<ExtensionProjectKind> for ActivationKind {
    fn from(value: ExtensionProjectKind) -> Self {
        match value {
            ExtensionProjectKind::Plugin => Self::Plugin,
            ExtensionProjectKind::Skill => Self::Skill,
            ExtensionProjectKind::Workflow => Self::Workflow,
            ExtensionProjectKind::Expert => Self::Expert,
            ExtensionProjectKind::Instruction => Self::Instruction,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationTargetScope {
    Workspace,
    ManagedDeployment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationTarget {
    pub profile: String,
    pub workspace_root: String,
    pub scope: ActivationTargetScope,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deployment_id: String,
    #[serde(default)]
    pub clients: Vec<String>,
}

impl ActivationTarget {
    pub(crate) fn workspace(
        profile: impl Into<String>,
        workspace_root: impl Into<String>,
        clients: &[String],
    ) -> Result<Self, Box<dyn Error>> {
        let target = Self {
            profile: profile.into().trim().to_ascii_lowercase(),
            workspace_root: workspace_root.into(),
            scope: ActivationTargetScope::Workspace,
            deployment_id: String::new(),
            clients: normalize_clients(clients),
        };
        target.validate()?;
        Ok(target)
    }

    pub(crate) fn managed_deployment(
        profile: impl Into<String>,
        workspace_root: impl Into<String>,
        deployment_id: impl Into<String>,
        clients: &[String],
    ) -> Result<Self, Box<dyn Error>> {
        let target = Self {
            profile: profile.into().trim().to_ascii_lowercase(),
            workspace_root: workspace_root.into(),
            scope: ActivationTargetScope::ManagedDeployment,
            deployment_id: deployment_id.into(),
            clients: normalize_clients(clients),
        };
        target.validate()?;
        Ok(target)
    }

    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.profile.trim().is_empty() {
            return Err("activation target profile is required".into());
        }
        if self.workspace_root.trim().is_empty() {
            return Err("activation target workspace_root is required".into());
        }
        match self.scope {
            ActivationTargetScope::Workspace if !self.deployment_id.trim().is_empty() => {
                return Err("workspace activation target cannot name a managed deployment".into())
            }
            ActivationTargetScope::ManagedDeployment if self.deployment_id.trim().is_empty() => {
                return Err("managed deployment activation target requires deployment_id".into())
            }
            _ => {}
        }
        if self.clients.iter().any(|client| client.trim().is_empty()) {
            return Err("activation target clients must not contain empty ids".into());
        }
        Ok(())
    }

    pub(crate) fn activation_key(
        &self,
        kind: ActivationKind,
        extension_id: &str,
    ) -> Result<String, Box<dyn Error>> {
        self.validate()?;
        let extension_id = extension_id.trim();
        if extension_id.is_empty() {
            return Err("activation target extension_id is required".into());
        }
        let scope = match self.scope {
            // Runtime discovery is profile-global for the first release: the
            // plugin/skill registries and workflow/expert stores can expose only
            // one active development version per ID.  Keep workspace isolation
            // in candidates and receipts, but do not create multiple active
            // pointers that the discovery surfaces cannot distinguish.
            ActivationTargetScope::Workspace => "workspace".to_string(),
            ActivationTargetScope::ManagedDeployment => {
                format!(
                    "deployment:{}",
                    self.deployment_id.trim().to_ascii_lowercase()
                )
            }
        };
        Ok(format!(
            "{}|{}|{}|{}",
            self.profile.trim().to_ascii_lowercase(),
            scope,
            kind.as_str(),
            extension_id.to_ascii_lowercase()
        ))
    }
}

pub(crate) fn normalize_clients(clients: &[String]) -> Vec<String> {
    let mut normalized = clients
        .iter()
        .map(|client| client.trim().to_ascii_lowercase())
        .filter(|client| !client.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_path_key(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    // Windows canonicalize() returns an extended-length path.  Both spellings
    // identify the same workspace, so keeping the prefix would falsely reject a
    // second build from the same source tree.
    let normalized = normalized.strip_prefix("//?/UNC/").map_or_else(
        || {
            normalized
                .strip_prefix("//?/")
                .unwrap_or(&normalized)
                .to_string()
        },
        |rest| format!("//{rest}"),
    );
    normalized.trim_end_matches('/').to_ascii_lowercase()
}

/// Resolve filesystem aliases (notably Windows 8.3 names such as `ADMINI~1`)
/// before comparing workspace identities. Both spellings can name the same
/// directory, so string normalization alone is insufficient.
fn normalized_workspace_path(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalize_path_key(&resolved.to_string_lossy())
}

pub(crate) fn ensure_workspace_identity(
    existing: Option<&Path>,
    incoming: &Path,
    kind: ActivationKind,
) -> Result<(), Box<dyn Error>> {
    let Some(existing) = existing else {
        return Ok(());
    };
    let existing = normalized_workspace_path(existing);
    let incoming = normalized_workspace_path(incoming);
    let related =
        |parent: &str, child: &str| child == parent || child.starts_with(&format!("{parent}/"));
    if existing == incoming || related(&existing, &incoming) || related(&incoming, &existing) {
        return Ok(());
    }
    Err(format!(
        "{} 候选已绑定其他源码工作区：{}；当前构建来自 {}。请提升版本或先移除此工作区登记，禁止静默覆盖",
        kind.as_str(),
        existing,
        incoming
    )
    .into())
}

pub(crate) fn ensure_receipt_workspace(
    receipt: &ActivationReceipt,
    workspace_root: &Path,
) -> Result<(), Box<dyn Error>> {
    let active = normalized_workspace_path(Path::new(&receipt.target.workspace_root));
    let requested = normalized_workspace_path(workspace_root);
    if active == requested {
        return Ok(());
    }
    Err(format!(
        "当前活动开发版来自其他工作区：{}；禁止从 {} 回滚它",
        receipt.target.workspace_root,
        workspace_root.display()
    )
    .into())
}

pub(crate) fn resolve_activation_target(
    profile: &str,
    workspace_root: &Path,
    clients: &[String],
    managed_deployment_id: Option<&str>,
) -> Result<ActivationTarget, Box<dyn Error>> {
    let workspace = workspace_root.to_string_lossy();
    match managed_deployment_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(deployment_id) => {
            ActivationTarget::managed_deployment(profile, workspace, deployment_id, clients)
        }
        None => ActivationTarget::workspace(profile, workspace, clients),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationStatus {
    Staged,
    Active,
    Partial,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationSurfaceState {
    Pending,
    Visible,
    Conflict,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationSurface {
    pub id: String,
    pub state: ActivationSurfaceState,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl ActivationSurface {
    pub(crate) fn visible(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            state: ActivationSurfaceState::Visible,
            detail: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationReceipt {
    pub schema_version: String,
    pub kind: ActivationKind,
    pub extension_id: String,
    pub version: String,
    pub candidate_sha256: String,
    pub project_id: String,
    pub workspace_key: String,
    pub target: ActivationTarget,
    pub status: ActivationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_sha256: Option<String>,
    #[serde(default)]
    pub surfaces: Vec<ActivationSurface>,
    pub activated_at: String,
    pub updated_at: String,
}

impl ActivationReceipt {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != EXTENSION_ACTIVATION_SCHEMA_VERSION {
            return Err(format!("unsupported activation schema: {}", self.schema_version).into());
        }
        self.target.activation_key(self.kind, &self.extension_id)?;
        if self.version.trim().is_empty() {
            return Err("activation receipt version is required".into());
        }
        validate_sha256("candidate_sha256", &self.candidate_sha256)?;
        if let Some(previous) = self.previous_sha256.as_deref() {
            validate_sha256("previous_sha256", previous)?;
        }
        if self.project_id.trim().is_empty() || self.workspace_key.trim().is_empty() {
            return Err("activation receipt requires project_id and workspace_key".into());
        }
        if self.activated_at.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err("activation receipt timestamps are required".into());
        }
        if self.status == ActivationStatus::Active
            && self.surfaces.iter().any(|surface| {
                matches!(
                    surface.state,
                    ActivationSurfaceState::Conflict
                        | ActivationSurfaceState::Failed
                        | ActivationSurfaceState::Pending
                )
            })
        {
            return Err(
                "active receipt cannot contain failed, conflicting, or pending surfaces".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn is_fully_visible(&self) -> bool {
        self.status == ActivationStatus::Active
            && self
                .surfaces
                .iter()
                .all(|surface| surface.state == ActivationSurfaceState::Visible)
            && !self.surfaces.is_empty()
    }
}

fn validate_sha256(label: &str, value: &str) -> Result<(), Box<dyn Error>> {
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} must be a 64-character SHA-256 hex digest").into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationReport {
    pub receipt: ActivationReceipt,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ActivationRollbackResult {
    pub activation: ActivationReceipt,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ActivationTransactionPhase {
    Staged,
    Committing,
    Committed,
    RollingBack,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationJournal {
    schema_version: String,
    transaction_id: String,
    activation_key: String,
    phase: ActivationTransactionPhase,
    receipt: ActivationReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<ActivationReceipt>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ActivationTransaction {
    journal_path: PathBuf,
    journal: ActivationJournal,
}

impl ActivationTransaction {
    pub(crate) fn transaction_id(&self) -> &str {
        &self.journal.transaction_id
    }

    pub(crate) fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    pub(crate) fn candidate_sha256(&self) -> &str {
        &self.journal.receipt.candidate_sha256
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ActivationRecoveryReport {
    pub completed_commits: usize,
    pub completed_rollbacks: usize,
    pub discarded_staged: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationLedger {
    schema_version: String,
    #[serde(default)]
    activations: BTreeMap<String, ActivationReceipt>,
}

impl Default for ActivationLedger {
    fn default() -> Self {
        Self {
            schema_version: EXTENSION_ACTIVATION_SCHEMA_VERSION.to_string(),
            activations: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActivationStore {
    path: PathBuf,
}

impl ActivationStore {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load(&self) -> Result<BTreeMap<String, ActivationReceipt>, Box<dyn Error>> {
        if !self.path.is_file() {
            return Ok(BTreeMap::new());
        }
        let ledger: ActivationLedger = serde_json::from_slice(&std::fs::read(&self.path)?)?;
        if ledger.schema_version != EXTENSION_ACTIVATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported activation ledger schema: {}",
                ledger.schema_version
            )
            .into());
        }
        Ok(ledger.activations)
    }

    pub(crate) fn active_for(
        &self,
        target: &ActivationTarget,
        kind: ActivationKind,
        extension_id: &str,
    ) -> Result<Option<ActivationReceipt>, Box<dyn Error>> {
        let key = target.activation_key(kind, extension_id)?;
        Ok(self.load()?.get(&key).cloned())
    }

    pub(crate) fn record(&self, receipt: &ActivationReceipt) -> Result<String, Box<dyn Error>> {
        receipt.validate()?;
        let key = receipt
            .target
            .activation_key(receipt.kind, &receipt.extension_id)?;
        let _lock = atomic_file::lock(&self.path)?;
        let mut ledger = if self.path.is_file() {
            serde_json::from_slice::<ActivationLedger>(&std::fs::read(&self.path)?)?
        } else {
            ActivationLedger::default()
        };
        if ledger.schema_version != EXTENSION_ACTIVATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported activation ledger schema: {}",
                ledger.schema_version
            )
            .into());
        }
        ledger.activations.insert(key.clone(), receipt.clone());
        atomic_file::atomic_write(&self.path, &serde_json::to_vec_pretty(&ledger)?)?;
        Ok(key)
    }

    /// Persist a pending switch without changing the active ledger.
    ///
    /// Callers stage every projection before this point.  A failed projection
    /// leaves the current active receipt untouched and can discard the staged
    /// transaction.
    pub(crate) fn stage(
        &self,
        receipt: &ActivationReceipt,
    ) -> Result<ActivationTransaction, Box<dyn Error>> {
        receipt.validate()?;
        let key = receipt
            .target
            .activation_key(receipt.kind, &receipt.extension_id)?;
        let _lock = atomic_file::lock(&self.path)?;
        let ledger = self.load_ledger_unlocked()?;
        let previous = ledger.activations.get(&key).cloned();
        let now = now_stamp();
        let transaction_id = next_transaction_id();
        let journal = ActivationJournal {
            schema_version: EXTENSION_ACTIVATION_JOURNAL_SCHEMA_VERSION.to_string(),
            transaction_id: transaction_id.clone(),
            activation_key: key,
            phase: ActivationTransactionPhase::Staged,
            receipt: receipt.clone(),
            previous,
            created_at: now.clone(),
            updated_at: now,
        };
        let journal_path = self.journal_path(&transaction_id);
        self.write_journal(&journal_path, &journal)?;
        Ok(ActivationTransaction {
            journal_path,
            journal,
        })
    }

    /// Complete a staged switch.  Repeating commit for the same transaction is
    /// idempotent; a rolled-back transaction cannot be committed again.
    pub(crate) fn commit(
        &self,
        transaction: &mut ActivationTransaction,
    ) -> Result<(), Box<dyn Error>> {
        let _lock = atomic_file::lock(&self.path)?;
        let mut journal = self.read_journal(&transaction.journal_path)?;
        match journal.phase {
            ActivationTransactionPhase::Committed => {
                transaction.journal = journal;
                return Ok(());
            }
            ActivationTransactionPhase::RolledBack => {
                return Err("cannot commit a rolled-back activation transaction".into());
            }
            ActivationTransactionPhase::Staged | ActivationTransactionPhase::Committing => {
                journal.phase = ActivationTransactionPhase::Committing;
                journal.updated_at = now_stamp();
                self.write_journal(&transaction.journal_path, &journal)?;
                let mut ledger = self.load_ledger_unlocked()?;
                ledger
                    .activations
                    .insert(journal.activation_key.clone(), journal.receipt.clone());
                self.write_ledger_unlocked(&ledger)?;
                journal.phase = ActivationTransactionPhase::Committed;
                journal.updated_at = now_stamp();
                self.write_journal(&transaction.journal_path, &journal)?;
            }
            ActivationTransactionPhase::RollingBack => {
                return Err("cannot commit while activation rollback is in progress".into());
            }
        }
        transaction.journal = journal;
        Ok(())
    }

    /// Restore the previous active receipt.  A never-committed staged
    /// transaction simply becomes rolled back without touching the ledger.
    pub(crate) fn rollback(
        &self,
        transaction: &mut ActivationTransaction,
    ) -> Result<(), Box<dyn Error>> {
        let _lock = atomic_file::lock(&self.path)?;
        let mut journal = self.read_journal(&transaction.journal_path)?;
        if journal.phase == ActivationTransactionPhase::RolledBack {
            transaction.journal = journal;
            return Ok(());
        }
        if journal.phase == ActivationTransactionPhase::Staged {
            journal.phase = ActivationTransactionPhase::RolledBack;
            journal.updated_at = now_stamp();
            self.write_journal(&transaction.journal_path, &journal)?;
            transaction.journal = journal;
            return Ok(());
        }
        journal.phase = ActivationTransactionPhase::RollingBack;
        journal.updated_at = now_stamp();
        self.write_journal(&transaction.journal_path, &journal)?;
        let mut ledger = self.load_ledger_unlocked()?;
        if let Some(previous) = journal.previous.clone() {
            ledger
                .activations
                .insert(journal.activation_key.clone(), previous);
        } else {
            ledger.activations.remove(&journal.activation_key);
        }
        self.write_ledger_unlocked(&ledger)?;
        journal.phase = ActivationTransactionPhase::RolledBack;
        journal.updated_at = now_stamp();
        self.write_journal(&transaction.journal_path, &journal)?;
        transaction.journal = journal;
        Ok(())
    }

    /// Complete or discard transactions interrupted before their final state.
    /// Staged work is discarded; Committing and RollingBack are completed in
    /// the direction recorded in the journal.
    pub(crate) fn recover(&self) -> Result<ActivationRecoveryReport, Box<dyn Error>> {
        let root = self.journal_dir();
        if !root.is_dir() {
            return Ok(ActivationRecoveryReport::default());
        }
        let mut paths = std::fs::read_dir(&root)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        let mut report = ActivationRecoveryReport::default();
        let _lock = atomic_file::lock(&self.path)?;
        for path in paths {
            let mut journal = self.read_journal(&path)?;
            match journal.phase {
                ActivationTransactionPhase::Staged => {
                    journal.phase = ActivationTransactionPhase::RolledBack;
                    journal.updated_at = now_stamp();
                    self.write_journal(&path, &journal)?;
                    report.discarded_staged += 1;
                }
                ActivationTransactionPhase::Committing => {
                    let mut ledger = self.load_ledger_unlocked()?;
                    ledger
                        .activations
                        .insert(journal.activation_key.clone(), journal.receipt.clone());
                    self.write_ledger_unlocked(&ledger)?;
                    journal.phase = ActivationTransactionPhase::Committed;
                    journal.updated_at = now_stamp();
                    self.write_journal(&path, &journal)?;
                    report.completed_commits += 1;
                }
                ActivationTransactionPhase::RollingBack => {
                    let mut ledger = self.load_ledger_unlocked()?;
                    if let Some(previous) = journal.previous.clone() {
                        ledger
                            .activations
                            .insert(journal.activation_key.clone(), previous);
                    } else {
                        ledger.activations.remove(&journal.activation_key);
                    }
                    self.write_ledger_unlocked(&ledger)?;
                    journal.phase = ActivationTransactionPhase::RolledBack;
                    journal.updated_at = now_stamp();
                    self.write_journal(&path, &journal)?;
                    report.completed_rollbacks += 1;
                }
                ActivationTransactionPhase::Committed | ActivationTransactionPhase::RolledBack => {}
            }
        }
        Ok(report)
    }

    fn journal_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("extension-activation-transactions")
    }

    fn journal_path(&self, transaction_id: &str) -> PathBuf {
        self.journal_dir().join(format!("{transaction_id}.json"))
    }

    fn load_ledger_unlocked(&self) -> Result<ActivationLedger, Box<dyn Error>> {
        if !self.path.is_file() {
            return Ok(ActivationLedger::default());
        }
        let ledger: ActivationLedger = serde_json::from_slice(&std::fs::read(&self.path)?)?;
        if ledger.schema_version != EXTENSION_ACTIVATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported activation ledger schema: {}",
                ledger.schema_version
            )
            .into());
        }
        Ok(ledger)
    }

    fn write_ledger_unlocked(&self, ledger: &ActivationLedger) -> Result<(), Box<dyn Error>> {
        atomic_file::atomic_write(&self.path, &serde_json::to_vec_pretty(ledger)?)?;
        Ok(())
    }

    fn write_journal(
        &self,
        path: &Path,
        journal: &ActivationJournal,
    ) -> Result<(), Box<dyn Error>> {
        atomic_file::atomic_write(path, &serde_json::to_vec_pretty(journal)?)?;
        Ok(())
    }

    fn read_journal(&self, path: &Path) -> Result<ActivationJournal, Box<dyn Error>> {
        let journal: ActivationJournal = serde_json::from_slice(&std::fs::read(path)?)?;
        if journal.schema_version != EXTENSION_ACTIVATION_JOURNAL_SCHEMA_VERSION {
            return Err(format!(
                "unsupported activation journal schema: {}",
                journal.schema_version
            )
            .into());
        }
        let expected = journal
            .receipt
            .target
            .activation_key(journal.receipt.kind, &journal.receipt.extension_id)?;
        if expected != journal.activation_key {
            return Err("activation journal key does not match receipt".into());
        }
        Ok(journal)
    }
}

fn next_transaction_id() -> String {
    let sequence = ACTIVATION_TRANSACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{}-{sequence:x}", std::process::id())
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(ch: char) -> String {
        std::iter::repeat(ch).take(64).collect()
    }

    fn receipt(target: ActivationTarget, status: ActivationStatus) -> ActivationReceipt {
        ActivationReceipt {
            schema_version: EXTENSION_ACTIVATION_SCHEMA_VERSION.to_string(),
            kind: ActivationKind::Plugin,
            extension_id: "com.example.tool".to_string(),
            version: "1.0.0".to_string(),
            candidate_sha256: sha('a'),
            project_id: "plugin:com.example.tool@workspace".to_string(),
            workspace_key: "f:/work/extensions".to_string(),
            target,
            status,
            previous_sha256: Some(sha('b')),
            surfaces: vec![ActivationSurface::visible("mcp")],
            activated_at: "2026-10-30T16:20:00+08:00".to_string(),
            updated_at: "2026-10-30T16:20:00+08:00".to_string(),
        }
    }

    #[test]
    fn target_key_normalizes_profile_path_and_clients() {
        let left = ActivationTarget::workspace(
            " Development ",
            r"F:\Work\Extensions\\",
            &["Codex".into(), "codex".into(), "OpenCode".into()],
        )
        .unwrap();
        let right = ActivationTarget::workspace(
            "development",
            "f:/work/extensions",
            &["opencode".into(), "codex".into()],
        )
        .unwrap();
        assert_eq!(
            left.activation_key(ActivationKind::Plugin, "com.example.tool")
                .unwrap(),
            right
                .activation_key(ActivationKind::Plugin, "com.example.tool")
                .unwrap()
        );
        assert_eq!(left.clients, vec!["codex", "opencode"]);
    }

    #[test]
    fn managed_target_requires_deployment_id() {
        let error = ActivationTarget {
            profile: "development".into(),
            workspace_root: "f:/work/extensions".into(),
            scope: ActivationTargetScope::ManagedDeployment,
            deployment_id: String::new(),
            clients: Vec::new(),
        }
        .validate()
        .unwrap_err();
        assert!(error.to_string().contains("deployment_id"));
    }

    #[test]
    fn candidate_workspace_identity_rejects_different_project() {
        let error = ensure_workspace_identity(
            Some(Path::new(r"F:\work\a")),
            Path::new(r"F:\work\b"),
            ActivationKind::Plugin,
        )
        .unwrap_err();
        assert!(error.to_string().contains("禁止静默覆盖"));
        ensure_workspace_identity(
            Some(Path::new(r"F:\work\a")),
            Path::new("f:/work/a"),
            ActivationKind::Plugin,
        )
        .unwrap();
    }

    #[test]
    fn candidate_workspace_identity_accepts_related_canonical_paths() {
        ensure_workspace_identity(
            Some(Path::new(
                r"\\?\F:\WebProjects\demo\skill-drafts\demo\1.0.0\package",
            )),
            Path::new(r"F:\WebProjects\demo\skill-drafts\demo\1.0.0"),
            ActivationKind::Skill,
        )
        .unwrap();
        ensure_workspace_identity(
            Some(Path::new(r"F:\WebProjects\demo-a")),
            Path::new(r"F:\WebProjects\demo-a-copy"),
            ActivationKind::Skill,
        )
        .unwrap_err();
    }

    #[test]
    fn candidate_workspace_identity_resolves_filesystem_aliases() {
        let root = std::env::temp_dir().join(format!(
            "himind-workspace-alias-{}-{}",
            std::process::id(),
            now_stamp()
        ));
        let workspace = root.join("workspace");
        let nested = root.join("nested");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let aliased = nested.join("..").join("workspace");
        ensure_workspace_identity(Some(&workspace), &aliased, ActivationKind::Skill).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_workspace_must_match_the_active_receipt() {
        let target = ActivationTarget::workspace("development", r"F:\work\a", &[]).unwrap();
        let item = receipt(target, ActivationStatus::Active);
        ensure_receipt_workspace(&item, Path::new(r"\\?\F:\work\a")).unwrap();
        assert!(ensure_receipt_workspace(&item, Path::new(r"F:\work\b")).is_err());
    }

    #[test]
    fn active_receipt_rejects_conflicting_surface() {
        let target = ActivationTarget::workspace("development", "f:/work/extensions", &[]).unwrap();
        let mut item = receipt(target, ActivationStatus::Active);
        item.surfaces = vec![ActivationSurface {
            id: "codex".into(),
            state: ActivationSurfaceState::Conflict,
            detail: "existing unmanaged skill".into(),
        }];
        assert!(item.validate().is_err());
    }

    #[test]
    fn store_replaces_same_identity_and_preserves_separate_profiles() {
        let root = std::env::temp_dir().join(format!(
            "himind-activation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ActivationStore::new(root.join("activation.json"));
        let dev = ActivationTarget::workspace("development", "f:/work/a", &[]).unwrap();
        let other_workspace = ActivationTarget::workspace("development", "f:/work/b", &[]).unwrap();
        let prod = ActivationTarget::workspace("production", "f:/work/a", &[]).unwrap();
        let first = receipt(dev.clone(), ActivationStatus::Active);
        store.record(&first).unwrap();
        let mut second = first.clone();
        second.candidate_sha256 = sha('c');
        second.previous_sha256 = Some(sha('a'));
        store.record(&second).unwrap();
        let mut third = second.clone();
        third.candidate_sha256 = sha('d');
        third.previous_sha256 = Some(sha('c'));
        third.target = other_workspace.clone();
        store.record(&third).unwrap();
        store
            .record(&receipt(prod, ActivationStatus::Active))
            .unwrap();
        assert_eq!(store.load().unwrap().len(), 2);
        assert_eq!(
            store
                .active_for(&other_workspace, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('d')
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_store(label: &str) -> (PathBuf, ActivationStore, ActivationTarget) {
        let root = std::env::temp_dir().join(format!(
            "himind-activation-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ActivationStore::new(root.join("activation.json"));
        let target = ActivationTarget::workspace("development", "f:/work/extensions", &[]).unwrap();
        (root, store, target)
    }

    fn receipt_with_sha(target: &ActivationTarget, ch: char) -> ActivationReceipt {
        let mut item = receipt(target.clone(), ActivationStatus::Active);
        item.candidate_sha256 = sha(ch);
        item
    }

    #[test]
    fn stage_does_not_change_active_and_commit_is_idempotent() {
        let (root, store, target) = temp_store("transaction-commit");
        store.record(&receipt_with_sha(&target, 'a')).unwrap();
        let mut transaction = store.stage(&receipt_with_sha(&target, 'b')).unwrap();
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('a')
        );
        store.commit(&mut transaction).unwrap();
        store.commit(&mut transaction).unwrap();
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('b')
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_discards_staged_and_restores_committed_receipt() {
        let (root, store, target) = temp_store("transaction-rollback");
        store.record(&receipt_with_sha(&target, 'a')).unwrap();
        let mut staged = store.stage(&receipt_with_sha(&target, 'b')).unwrap();
        store.rollback(&mut staged).unwrap();
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('a')
        );
        let mut committed = store.stage(&receipt_with_sha(&target, 'c')).unwrap();
        store.commit(&mut committed).unwrap();
        store.rollback(&mut committed).unwrap();
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('a')
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_completes_commit_and_rollback_intents() {
        let (root, store, target) = temp_store("transaction-recovery");
        store.record(&receipt_with_sha(&target, 'a')).unwrap();
        let committing = store.stage(&receipt_with_sha(&target, 'b')).unwrap();
        let mut journal = store.read_journal(committing.journal_path()).unwrap();
        journal.phase = ActivationTransactionPhase::Committing;
        store
            .write_journal(committing.journal_path(), &journal)
            .unwrap();
        let report = store.recover().unwrap();
        assert_eq!(report.completed_commits, 1);
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('b')
        );

        let rolling_back = store.stage(&receipt_with_sha(&target, 'c')).unwrap();
        let mut journal = store.read_journal(rolling_back.journal_path()).unwrap();
        journal.phase = ActivationTransactionPhase::RollingBack;
        store
            .write_journal(rolling_back.journal_path(), &journal)
            .unwrap();
        let report = store.recover().unwrap();
        assert_eq!(report.completed_rollbacks, 1);
        assert_eq!(
            store
                .active_for(&target, ActivationKind::Plugin, "com.example.tool")
                .unwrap()
                .unwrap()
                .candidate_sha256,
            sha('b')
        );

        let staged_only = store.stage(&receipt_with_sha(&target, 'd')).unwrap();
        let report = store.recover().unwrap();
        assert_eq!(report.discarded_staged, 1);
        assert!(staged_only.journal_path().is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
