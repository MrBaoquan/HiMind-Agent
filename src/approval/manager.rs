use fs4::fs_std::FileExt;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{fs, io::Write, path::PathBuf};

use super::types::*;

const MAX_PENDING_APPROVALS: usize = 256;
const APPROVAL_FACT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_APPROVAL_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub time: String,
    #[serde(default)]
    pub timestamp: u64,
    pub level: String,
    pub message: String,
}

pub struct ApprovalManager {
    pending: Mutex<Vec<PendingApproval>>,
    facts: Mutex<Vec<ApprovalFact>>,
    settings: Mutex<ApprovalSettings>,
    log_entries: Mutex<Vec<LogEntry>>,
    log_path: PathBuf,
    settings_path: PathBuf,
    facts_path: PathBuf,
    identity_path: PathBuf,
    instance_id: String,
    _instance_lock: Option<fs::File>,
}

impl ApprovalManager {
    /// All invocation adapters share one local queue so the desktop popup can
    /// approve requests raised by MCP, Tauri, local HTTP, or a worker.
    pub fn global() -> Arc<Self> {
        static INSTANCE: OnceLock<Arc<ApprovalManager>> = OnceLock::new();
        Arc::clone(INSTANCE.get_or_init(|| Arc::new(Self::new())))
    }

    pub fn new() -> Self {
        Self::new_in(crate::store::paths::agent_home())
    }

    fn new_in(agent_home: PathBuf) -> Self {
        let log_path = agent_home.join("logs").join("agent-events.jsonl");
        let settings_path = agent_home.join("approval-settings.json");
        let facts_path = agent_home.join("approval-requests.json");
        let identity_path = agent_home.join("agent-user-authorization.json");
        let instance_id = generate_instance_id();
        let instance_lock = acquire_instance_lock(&agent_home, &instance_id);
        let log_entries = load_persisted_logs(&log_path);
        let settings = load_persisted_settings(&settings_path);
        let (facts, interrupted) = recover_abandoned_facts(&agent_home, &facts_path)
            .unwrap_or_else(|_| (load_persisted_facts(&facts_path), 0));
        let manager = Self {
            pending: Mutex::new(Vec::new()),
            facts: Mutex::new(facts),
            settings: Mutex::new(settings),
            log_entries: Mutex::new(log_entries),
            log_path,
            settings_path,
            facts_path,
            identity_path,
            instance_id,
            _instance_lock: instance_lock,
        };
        if interrupted > 0 {
            manager.add_log(
                "warn",
                &format!("Agent 重启时终结了 {interrupted} 条未完成审批；相关操作保持拒绝状态"),
            );
        }
        manager
    }

    pub fn request_approval(
        &self,
        request_type: RequestType,
        title: String,
        description: String,
    ) -> Result<bool, String> {
        self.request_with_policy(
            request_type.key(),
            request_type.default_mode(),
            request_type.is_destructive(),
            Some(if request_type.is_destructive() {
                "R3"
            } else {
                "R2"
            }),
            title,
            description,
        )
    }

    pub fn request_capability_approval(
        &self,
        capability_id: &str,
        risk_level: &str,
        title: String,
        description: String,
    ) -> Result<bool, String> {
        let capability_id = capability_id.trim();
        if capability_id.is_empty() || capability_id.len() > 240 {
            return Err("Capability ID 不能为空且长度不能超过 240 个字符".to_string());
        }
        let risk_level = risk_level.trim().to_ascii_uppercase();
        let manual_only = super::policy::risk_rank(&risk_level) >= super::policy::risk_rank("R3");
        self.request_with_policy(
            capability_id,
            ApprovalMode::Manual,
            manual_only,
            Some(&risk_level),
            title,
            description,
        )
    }

    fn request_with_policy(
        &self,
        request_key: &str,
        default_mode: ApprovalMode,
        manual_only: bool,
        risk_level: Option<&str>,
        title: String,
        description: String,
    ) -> Result<bool, String> {
        if title.len() > 512 {
            return Err("审批标题长度不能超过 512 字节".to_string());
        }
        if description.len() > MAX_APPROVAL_TEXT_BYTES {
            return Err(format!(
                "审批说明长度不能超过 {MAX_APPROVAL_TEXT_BYTES} 字节"
            ));
        }
        let mode = self.get_mode_for_key(request_key, default_mode, manual_only, risk_level);

        match mode {
            ApprovalMode::AutoApprove => {
                self.record_immediate_fact(
                    request_key,
                    &title,
                    &description,
                    ApprovalFactStatus::Approved,
                    "local_rule_auto_approved",
                )?;
                self.add_log("info", &format!("自动批准: {title}"));
                return Ok(true);
            }
            ApprovalMode::AutoDeny => {
                self.record_immediate_fact(
                    request_key,
                    &title,
                    &description,
                    ApprovalFactStatus::Rejected,
                    "local_rule_auto_denied",
                )?;
                self.add_log("warn", &format!("自动拒绝: {title}"));
                return Ok(false);
            }
            ApprovalMode::Manual => {}
        }

        let timeout = self
            .settings
            .lock()
            .map(|s| s.timeout_seconds)
            .unwrap_or(30);

        let id = generate_id();
        let (tx, rx) = mpsc::channel();

        let request = ApprovalRequest {
            id: id.clone(),
            request_type: request_key.to_string(),
            title: title.clone(),
            description: description.clone(),
            timeout_seconds: timeout,
            remaining_seconds: timeout,
            created_at: now_string(),
            created_at_unix: unix_now(),
        };

        self.append_pending_fact(&request)?;

        let pending = PendingApproval {
            request: request.clone(),
            respond_tx: tx,
            created: Instant::now(),
            timeout_seconds: timeout,
        };

        let mut list = self
            .pending
            .lock()
            .map_err(|_| "审批队列锁已损坏".to_string())?;
        list.retain(|item| item.created.elapsed().as_secs() < item.timeout_seconds);
        if list.len() >= MAX_PENDING_APPROVALS {
            drop(list);
            let _ = self.resolve_fact(
                &request.id,
                ApprovalFactStatus::Rejected,
                "approval_queue_full",
            );
            self.add_log("warn", "审批队列已满，新的请求已拒绝");
            return Err("审批队列已满，请稍后重试".to_string());
        }
        list.push(pending);
        drop(list);

        self.add_log("info", &format!("等待审批: {title} (超时 {timeout}s)"));

        self.wait_for_decision(&id, &title, timeout, rx)
    }

    pub fn respond(&self, id: &str, approved: bool) -> Result<(), String> {
        if !self.resolve_fact(
            id,
            if approved {
                ApprovalFactStatus::Approved
            } else {
                ApprovalFactStatus::Rejected
            },
            if approved {
                "user_approved"
            } else {
                "user_rejected"
            },
        )? {
            return Err(format!("审批请求已处理或不存在: {id}"));
        }
        let mut list = self.pending.lock().map_err(|e| e.to_string())?;
        if let Some(index) = list.iter().position(|p| p.request.id == id) {
            let approval = list.remove(index);
            if approval.respond_tx.send(approved).is_err() {
                drop(list);
                let _ = self.interrupt_resolved_fact(id, "approval_channel_disconnected");
                return Err("审批调用已经中断，操作未执行".to_string());
            }
            return Ok(());
        }
        // A request owned by another process has no in-memory sender here. Its
        // caller observes the durable decision while polling the fact store.
        Ok(())
    }

    pub fn list_pending(&self) -> Vec<ApprovalRequest> {
        let mut result = Vec::new();
        let mut expired_ids = Vec::new();
        if let Ok(mut list) = self.pending.lock() {
            list.retain(|pending| {
                let active = pending.created.elapsed().as_secs() < pending.timeout_seconds;
                if !active {
                    expired_ids.push(pending.request.id.clone());
                }
                active
            });
            result = list
                .iter()
                .map(|pending| {
                    let mut request = pending.request.clone();
                    request.remaining_seconds = pending
                        .timeout_seconds
                        .saturating_sub(pending.created.elapsed().as_secs());
                    request
                })
                .collect();
        }
        for id in expired_ids {
            if let Err(error) =
                self.resolve_fact(&id, ApprovalFactStatus::Expired, "approval_timeout")
            {
                self.add_log("error", &format!("审批过期事实保存失败: {error}"));
            }
        }
        let known = result
            .iter()
            .map(|request| request.id.clone())
            .collect::<HashSet<_>>();
        let now = unix_now();
        for fact in self.load_latest_facts() {
            if fact.status != ApprovalFactStatus::Pending || known.contains(&fact.id) {
                continue;
            }
            let agent_home = self
                .facts_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            if !approval_owner_is_alive(agent_home, &fact.owner_instance_id) {
                let _ = self.resolve_fact(
                    &fact.id,
                    ApprovalFactStatus::Interrupted,
                    "owner_process_exited",
                );
                continue;
            }
            if fact.expires_at_unix <= now {
                let _ =
                    self.resolve_fact(&fact.id, ApprovalFactStatus::Expired, "approval_timeout");
                continue;
            }
            result.push(ApprovalRequest {
                id: fact.id,
                request_type: fact.request_type,
                title: fact.title,
                description: fact.description,
                timeout_seconds: fact.expires_at_unix.saturating_sub(fact.created_at_unix),
                remaining_seconds: fact.expires_at_unix.saturating_sub(now),
                created_at: format_unix_time(fact.created_at_unix),
                created_at_unix: fact.created_at_unix,
            });
        }
        result.sort_by_key(|request| request.created_at_unix);
        result
    }

    pub fn list_recent_facts(&self) -> Vec<ApprovalFact> {
        self.load_latest_facts()
            .into_iter()
            .rev()
            .take(500)
            .collect()
    }

    pub fn get_settings(&self) -> ApprovalSettings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn effective_mode_for_risk(&self, risk_level: &str) -> ApprovalMode {
        let risk_level = risk_level.trim().to_ascii_uppercase();
        let manual_only = super::policy::risk_rank(&risk_level) >= super::policy::risk_rank("R3");
        self.get_mode_for_key(
            "approval.settings.summary",
            ApprovalMode::Manual,
            manual_only,
            Some(&risk_level),
        )
    }

    /// Bind the local approval posture to the Dashboard user and Agent that
    /// confirmed it. A changed identity invalidates risk acknowledgement and
    /// returns the local profile to balanced until the new user confirms it.
    pub fn bind_identity(&self, user_id: &str, agent_id: &str) -> Result<bool, String> {
        let user_id = user_id.trim();
        let agent_id = agent_id.trim();
        if user_id.is_empty() || agent_id.is_empty() {
            return Err("审批身份绑定需要 Dashboard 用户和 Agent 标识".to_string());
        }
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        if settings.owner_user_id == user_id && settings.agent_id == agent_id {
            return Ok(false);
        }
        let previous = settings.clone();
        let previous_profile = settings.profile.clone();
        settings.owner_user_id = user_id.to_string();
        settings.agent_id = agent_id.to_string();
        settings.binding_updated_at = unix_now();
        settings.risk_acknowledged_at = 0;
        reset_identity_sensitive_rules(&mut settings);
        if matches!(previous_profile.as_str(), "relaxed" | "trusted" | "focus") {
            settings.profile = "balanced".to_string();
            if previous_profile == "focus" {
                settings.notification_mode = "popup".to_string();
            }
        }
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        drop(settings);
        self.add_log(
            "warn",
            &format!(
                "审批身份已切换为 Dashboard 用户 {user_id} / Agent {agent_id}；宽松档位已回到平衡，请重新确认风险"
            ),
        );
        Ok(true)
    }

    /// Remove the Dashboard binding when the user logs out. Requests then use
    /// the local profile only after an explicit local confirmation.
    pub fn clear_identity(&self) -> Result<bool, String> {
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        if settings.owner_user_id.is_empty() && settings.agent_id.is_empty() {
            return Ok(false);
        }
        let previous = settings.clone();
        settings.owner_user_id.clear();
        settings.agent_id.clear();
        settings.binding_updated_at = unix_now();
        settings.risk_acknowledged_at = 0;
        reset_identity_sensitive_rules(&mut settings);
        if matches!(settings.profile.as_str(), "relaxed" | "trusted" | "focus") {
            settings.profile = "balanced".to_string();
        }
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        drop(settings);
        self.add_log("warn", "Dashboard 账号已退出，审批档位已回到平衡");
        Ok(true)
    }

    pub fn should_show_popup(&self) -> bool {
        self.settings
            .lock()
            .map(|settings| settings.notification_mode == "popup")
            .unwrap_or(true)
    }

    pub fn update_profile(&self, profile: &str, confirmed: bool) -> Result<(), String> {
        let profile = profile.trim();
        if !matches!(
            profile,
            "strict" | "balanced" | "relaxed" | "trusted" | "silent_deny" | "focus"
        ) {
            return Err(format!("不支持的审批档位: {profile}"));
        }
        if profile == "trusted" && !confirmed {
            return Err("启用完全信任档位必须明确确认自担风险".to_string());
        }
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        let previous = settings.clone();
        settings.profile = profile.to_string();
        settings.risk_acknowledged_at = if profile == "trusted" && confirmed {
            unix_now()
        } else {
            0
        };
        // Focus is specifically the no-popup workflow. Keep an explicit
        // notification choice for other profiles so users can switch back.
        if profile == "focus" {
            settings.notification_mode = "inbox".to_string();
        }
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn update_notification_mode(&self, mode: &str) -> Result<(), String> {
        let mode = mode.trim();
        if !matches!(mode, "popup" | "tray" | "inbox") {
            return Err(format!("不支持的审批提醒方式: {mode}"));
        }
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        let previous = settings.clone();
        if settings.profile == "focus" {
            settings.profile = "balanced".to_string();
        }
        settings.notification_mode = mode.to_string();
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn update_rule(&self, request_type: &str, mode: &str) -> Result<(), String> {
        let request_type = request_type.trim();
        if request_type.is_empty() || request_type.len() > 240 {
            return Err("审批请求类型不能为空且长度不能超过 240 个字符".to_string());
        }
        if !matches!(mode, "inherit" | "manual" | "auto_approve" | "auto_deny") {
            return Err(format!("不支持的审批模式: {mode}"));
        }
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        let is_r4 = request_type == "risk:R4";
        let is_r3 = request_type == "risk:R3"
            || super::policy::is_destructive_capability(request_type)
            || matches!(
                request_type,
                "software.distribution.release.publish" | "extension.review.decide"
            );
        if mode == "auto_approve" && (is_r4 || (is_r3 && settings.profile != "trusted")) {
            return Err(if is_r4 {
                "R4 和系统硬拒绝目标不能配置为自动批准".to_string()
            } else {
                "R3 仅可在完全信任档位下自动批准；请先明确启用该档位".to_string()
            });
        }
        let previous = settings.clone();
        if mode == "inherit" {
            settings.rules.remove(request_type);
        } else {
            settings
                .rules
                .insert(request_type.to_string(), mode.to_string());
        }
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn update_timeout(&self, seconds: u64) -> Result<(), String> {
        if !(1..=86_400).contains(&seconds) {
            return Err("审批超时必须在 1 秒到 24 小时之间".to_string());
        }
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        let previous = settings.clone();
        settings.timeout_seconds = seconds;
        if let Err(error) = persist_settings(&self.settings_path, &settings) {
            *settings = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn get_logs(&self) -> Vec<LogEntry> {
        self.log_entries
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default()
    }

    pub fn add_log(&self, level: &str, message: &str) {
        let entry = LogEntry {
            time: now_string(),
            timestamp: unix_now(),
            level: level.to_string(),
            message: redact_message(message),
        };
        if let Ok(mut logs) = self.log_entries.lock() {
            persist_log_entry(&self.log_path, &entry);
            logs.push(entry);
            if logs.len() > 500 {
                logs.drain(0..200);
            }
        }
    }

    fn get_mode_for_key(
        &self,
        request_key: &str,
        default_mode: ApprovalMode,
        manual_only: bool,
        risk_level: Option<&str>,
    ) -> ApprovalMode {
        if let Ok(settings) = self.settings.lock() {
            // A Dashboard-bound profile must never survive an account switch
            // or logout. The persisted OAuth identity is read on every
            // invocation so background workers fail closed even before the UI
            // refreshes its identity status.
            if !settings.owner_user_id.is_empty() || !settings.agent_id.is_empty() {
                let identity_matches =
                    crate::api::oauth::persisted_authorization_identity(&self.identity_path)
                        .map(|(agent_id, user_id)| {
                            agent_id == settings.agent_id && user_id == settings.owner_user_id
                        })
                        .unwrap_or(false);
                if !identity_matches {
                    return ApprovalMode::Manual;
                }
            }
            let profile = settings.profile.as_str();
            let trusted_r3 = profile == "trusted"
                && settings.risk_acknowledged_at > 0
                && risk_level
                    .map(|risk| super::policy::risk_rank(risk) <= super::policy::risk_rank("R3"))
                    .unwrap_or(false);
            let silent_deny = profile == "silent_deny";
            let risk_key = risk_level.map(|risk| format!("risk:{risk}"));
            let keys = [
                Some(request_key),
                risk_key.as_deref(),
                Some("controlled_operation"),
                Some("*"),
            ];
            for key in keys.into_iter().flatten() {
                match settings.rules.get(key).map(String::as_str) {
                    Some("auto_approve") if !manual_only || trusted_r3 => {
                        return ApprovalMode::AutoApprove
                    }
                    Some("auto_deny") => return ApprovalMode::AutoDeny,
                    Some("manual") => return ApprovalMode::Manual,
                    _ => {}
                }
            }
            if silent_deny {
                return ApprovalMode::AutoDeny;
            }
            if profile == "strict" {
                return ApprovalMode::Manual;
            }
            if risk_level
                .map(|risk| {
                    let rank = super::policy::risk_rank(risk);
                    (matches!(profile, "balanced" | "focus") && rank <= 1)
                        || (profile == "relaxed" && rank <= 2)
                        || (profile == "trusted" && settings.risk_acknowledged_at > 0 && rank <= 3)
                })
                .unwrap_or(false)
            {
                return ApprovalMode::AutoApprove;
            }
            if manual_only && !trusted_r3 {
                return ApprovalMode::Manual;
            }
            default_mode
        } else {
            default_mode
        }
    }

    fn append_pending_fact(&self, request: &ApprovalRequest) -> Result<(), String> {
        let _file_lock = crate::store::atomic_file::lock(&self.facts_path)
            .map_err(|error| format!("锁定审批事实失败: {error}"))?;
        let mut facts = load_persisted_facts(&self.facts_path);
        facts.push(ApprovalFact {
            schema_version: 1,
            id: request.id.clone(),
            request_type: request.request_type.clone(),
            title: request.title.clone(),
            description: request.description.clone(),
            status: ApprovalFactStatus::Pending,
            created_at_unix: request.created_at_unix,
            expires_at_unix: request
                .created_at_unix
                .saturating_add(request.timeout_seconds),
            resolved_at_unix: 0,
            resolution_reason: String::new(),
            owner_instance_id: self.instance_id.clone(),
        });
        if facts.len() > 1000 {
            let remove = facts.len() - 1000;
            facts.drain(0..remove);
        }
        persist_facts_locked(&self.facts_path, &facts)?;
        if let Ok(mut cached) = self.facts.lock() {
            *cached = facts;
        }
        Ok(())
    }

    fn record_immediate_fact(
        &self,
        request_type: &str,
        title: &str,
        description: &str,
        status: ApprovalFactStatus,
        reason: &str,
    ) -> Result<(), String> {
        let _file_lock = crate::store::atomic_file::lock(&self.facts_path)
            .map_err(|error| format!("锁定审批事实失败: {error}"))?;
        let mut facts = load_persisted_facts(&self.facts_path);
        let now = unix_now();
        facts.push(ApprovalFact {
            schema_version: 1,
            id: generate_id(),
            request_type: request_type.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            status,
            created_at_unix: now,
            expires_at_unix: now,
            resolved_at_unix: now,
            resolution_reason: reason.to_string(),
            owner_instance_id: self.instance_id.clone(),
        });
        if facts.len() > 1000 {
            let remove = facts.len() - 1000;
            facts.drain(0..remove);
        }
        persist_facts_locked(&self.facts_path, &facts)?;
        if let Ok(mut cached) = self.facts.lock() {
            *cached = facts;
        }
        Ok(())
    }

    fn resolve_fact(
        &self,
        id: &str,
        status: ApprovalFactStatus,
        reason: &str,
    ) -> Result<bool, String> {
        let _file_lock = crate::store::atomic_file::lock(&self.facts_path)
            .map_err(|error| format!("锁定审批事实失败: {error}"))?;
        let mut facts = load_persisted_facts(&self.facts_path);
        let Some(index) = facts.iter().position(|fact| fact.id == id) else {
            return Ok(false);
        };
        if facts[index].status != ApprovalFactStatus::Pending {
            return Ok(false);
        }
        let previous = facts[index].clone();
        facts[index].status = status;
        facts[index].resolved_at_unix = unix_now();
        facts[index].resolution_reason = reason.to_string();
        if let Err(error) = persist_facts_locked(&self.facts_path, &facts) {
            facts[index] = previous;
            return Err(error);
        }
        if let Ok(mut cached) = self.facts.lock() {
            *cached = facts;
        }
        Ok(true)
    }

    fn interrupt_resolved_fact(&self, id: &str, reason: &str) -> Result<bool, String> {
        let _file_lock = crate::store::atomic_file::lock(&self.facts_path)
            .map_err(|error| format!("锁定审批事实失败: {error}"))?;
        let mut facts = load_persisted_facts(&self.facts_path);
        let Some(index) = facts.iter().position(|fact| fact.id == id) else {
            return Ok(false);
        };
        if facts[index].status == ApprovalFactStatus::Interrupted {
            return Ok(false);
        }
        facts[index].status = ApprovalFactStatus::Interrupted;
        facts[index].resolved_at_unix = unix_now();
        facts[index].resolution_reason = reason.to_string();
        persist_facts_locked(&self.facts_path, &facts)?;
        if let Ok(mut cached) = self.facts.lock() {
            *cached = facts;
        }
        Ok(true)
    }

    fn load_latest_facts(&self) -> Vec<ApprovalFact> {
        let facts = load_persisted_facts(&self.facts_path);
        if let Ok(mut cached) = self.facts.lock() {
            *cached = facts.clone();
        }
        facts
    }

    fn wait_for_decision(
        &self,
        id: &str,
        title: &str,
        timeout: u64,
        rx: mpsc::Receiver<bool>,
    ) -> Result<bool, String> {
        let started = Instant::now();
        let mut last_fact_poll = Instant::now();
        loop {
            let elapsed = started.elapsed().as_secs();
            if elapsed >= timeout {
                self.remove_pending(id);
                let _ = self.resolve_fact(id, ApprovalFactStatus::Expired, "approval_timeout");
                self.add_log("warn", &format!("审批超时: {title} → 自动拒绝"));
                return Ok(false);
            }
            let remaining = Duration::from_secs(timeout.saturating_sub(elapsed));
            let wait = remaining.min(Duration::from_millis(250));
            match rx.recv_timeout(wait) {
                Ok(approved) => {
                    self.remove_pending(id);
                    self.add_log(
                        "info",
                        &format!(
                            "审批结果: {} → {}",
                            title,
                            if approved { "批准" } else { "拒绝" }
                        ),
                    );
                    return Ok(approved);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.remove_pending(id);
                    let _ = self.resolve_fact(
                        id,
                        ApprovalFactStatus::Interrupted,
                        "approval_channel_disconnected",
                    );
                    return Err("审批通道断开".to_string());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if last_fact_poll.elapsed() < APPROVAL_FACT_POLL_INTERVAL {
                continue;
            }
            last_fact_poll = Instant::now();
            if let Some(fact) = self
                .load_latest_facts()
                .into_iter()
                .find(|fact| fact.id == id)
            {
                match fact.status {
                    ApprovalFactStatus::Approved => {
                        self.remove_pending(id);
                        self.add_log("info", &format!("审批结果: {title} → 批准"));
                        return Ok(true);
                    }
                    ApprovalFactStatus::Rejected
                    | ApprovalFactStatus::Expired
                    | ApprovalFactStatus::Interrupted => {
                        self.remove_pending(id);
                        self.add_log("warn", &format!("审批结果: {title} → 拒绝"));
                        return Ok(false);
                    }
                    ApprovalFactStatus::Pending => {}
                }
            }
        }
    }

    fn remove_pending(&self, id: &str) {
        if let Ok(mut list) = self.pending.lock() {
            list.retain(|p| p.request.id != id);
        }
    }
}

fn load_persisted_logs(path: &std::path::Path) -> Vec<LogEntry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries = content
        .lines()
        .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
        .collect::<Vec<_>>();
    if entries.len() > 500 {
        entries.drain(0..entries.len() - 500);
    }
    entries
}

fn load_persisted_settings(path: &std::path::Path) -> ApprovalSettings {
    let Ok(content) = fs::read(path) else {
        return ApprovalSettings::default();
    };
    serde_json::from_slice::<ApprovalSettings>(&content).unwrap_or_default()
}

fn load_persisted_facts(path: &std::path::Path) -> Vec<ApprovalFact> {
    let read = |candidate: &std::path::Path| {
        fs::read(candidate)
            .ok()
            .and_then(|content| serde_json::from_slice::<Vec<ApprovalFact>>(&content).ok())
    };
    read(path)
        .or_else(|| read(&crate::store::atomic_file::backup_path(path)))
        .unwrap_or_default()
}

fn recover_abandoned_facts(
    agent_home: &std::path::Path,
    path: &std::path::Path,
) -> Result<(Vec<ApprovalFact>, usize), String> {
    let _lock = crate::store::atomic_file::lock(path)
        .map_err(|error| format!("锁定审批事实失败: {error}"))?;
    let mut facts = load_persisted_facts(path);
    let interrupted_at = unix_now();
    let mut interrupted = 0usize;
    for fact in &mut facts {
        if fact.status == ApprovalFactStatus::Pending
            && !approval_owner_is_alive(agent_home, &fact.owner_instance_id)
        {
            fact.status = ApprovalFactStatus::Interrupted;
            fact.resolved_at_unix = interrupted_at;
            fact.resolution_reason = "agent_restarted".to_string();
            interrupted += 1;
        }
    }
    if interrupted > 0 {
        persist_facts_locked(path, &facts)?;
    }
    Ok((facts, interrupted))
}

fn persist_facts_locked(path: &std::path::Path, facts: &[ApprovalFact]) -> Result<(), String> {
    let content =
        serde_json::to_vec_pretty(facts).map_err(|error| format!("序列化审批事实失败: {error}"))?;
    crate::store::atomic_file::atomic_write(path, &content)
        .map_err(|error| format!("保存审批事实失败: {error}"))
}

fn approval_owner_lock_path(agent_home: &std::path::Path, instance_id: &str) -> PathBuf {
    agent_home
        .join("approval-owners")
        .join(format!("{instance_id}.lock"))
}

fn acquire_instance_lock(agent_home: &std::path::Path, instance_id: &str) -> Option<fs::File> {
    let path = approval_owner_lock_path(agent_home, instance_id);
    fs::create_dir_all(path.parent()?).ok()?;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    file.lock_exclusive().ok()?;
    Some(file)
}

fn approval_owner_is_alive(agent_home: &std::path::Path, instance_id: &str) -> bool {
    if instance_id.trim().is_empty() {
        return false;
    }
    let path = approval_owner_lock_path(agent_home, instance_id);
    let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(true) => {
            let _ = file.unlock();
            false
        }
        Ok(false) => true,
        // Lock inspection failure must not let another process cancel a live
        // approval. The request will still expire fail-closed at its deadline.
        Err(_) => true,
    }
}

fn persist_settings(path: &std::path::Path, settings: &ApprovalSettings) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("序列化审批设置失败: {error}"))?;
    let _lock = crate::store::atomic_file::lock(path)
        .map_err(|error| format!("锁定审批设置失败: {error}"))?;
    crate::store::atomic_file::atomic_write(path, &content)
        .map_err(|error| format!("保存审批设置失败: {error}"))
}

fn reset_identity_sensitive_rules(settings: &mut ApprovalSettings) {
    // Explicit auto-approve rules are user authority, not device authority.
    // Do not carry them across Dashboard identities or into independent mode.
    settings.rules.retain(|key, mode| {
        mode != "auto_approve"
            || matches!(
                key.as_str(),
                "remote_connect" | "upload_code" | "upload_placeholder"
            )
    });
}

fn persist_log_entry(path: &std::path::Path, entry: &LogEntry) {
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    rotate_logs(path);
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if let Ok(line) = serde_json::to_string(entry) {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

fn rotate_logs(path: &std::path::Path) {
    const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
    if path.metadata().map(|value| value.len()).unwrap_or_default() < MAX_LOG_BYTES {
        return;
    }
    let second = path.with_extension("jsonl.2");
    let first = path.with_extension("jsonl.1");
    let _ = fs::remove_file(&second);
    let _ = fs::rename(&first, &second);
    let _ = fs::rename(path, &first);
}

pub(crate) fn redact_message(message: &str) -> String {
    const MARKERS: [&str; 6] = [
        "Bearer ",
        "credential=",
        "access_token=",
        "refresh_token=",
        "password=",
        "token=",
    ];
    let mut result = message.to_string();
    for marker in MARKERS {
        let mut offset = 0;
        while let Some(relative) = result[offset..].find(marker) {
            let start = offset + relative + marker.len();
            let end = result[start..]
                .find(|value: char| value.is_whitespace() || matches!(value, '&' | ',' | ';' | '"'))
                .map(|value| start + value)
                .unwrap_or(result.len());
            if end <= start {
                break;
            }
            result.replace_range(start..end, "[REDACTED]");
            offset = start + "[REDACTED]".len();
        }
    }
    result
}

fn generate_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("apr-{}-{sequence}", now.as_millis())
}

fn generate_instance_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{sequence}", std::process::id(), unix_now())
}

fn now_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_unix_time(now.as_secs())
}

fn format_unix_time(secs: u64) -> String {
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod log_tests {
    use super::{persist_settings, redact_message};
    use crate::approval::types::ApprovalSettings;
    use std::fs;
    use std::path::PathBuf;

    fn test_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-approval-{label}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn diagnostic_logs_redact_common_secret_fields() {
        let message =
            "request token=secret123&scope=a Bearer access-secret credential=device-secret";
        let redacted = redact_message(message);
        assert!(!redacted.contains("secret123"));
        assert!(!redacted.contains("access-secret"));
        assert!(!redacted.contains("device-secret"));
        assert!(redacted.matches("[REDACTED]").count() >= 3);
    }

    #[test]
    fn approval_settings_are_written_as_valid_json() {
        let path = test_path("settings");
        let settings = ApprovalSettings::default();
        persist_settings(&path, &settings).expect("persist settings");
        let restored: ApprovalSettings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.timeout_seconds, settings.timeout_seconds);
        assert_eq!(restored.rules, settings.rules);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(crate::store::atomic_file::backup_path(&path));
        let _ = fs::remove_file(crate::store::atomic_file::lock_path(&path));
    }
}

#[cfg(test)]
mod destructive_tests {
    use super::ApprovalManager;
    use crate::approval::types::{ApprovalFactStatus, ApprovalMode, PendingApproval, RequestType};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn test_home(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "himind-approval-home-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn destructive_requests_cannot_be_auto_approved() {
        let home = test_home("destructive");
        let manager = Arc::new(ApprovalManager::new_in(home.clone()));
        assert!(manager
            .update_rule("filesystem.delete", "auto_approve")
            .is_err());
        assert!(matches!(
            RequestType::FilesystemDelete.default_mode(),
            ApprovalMode::Manual
        ));

        let pending_manager = Arc::clone(&manager);
        let worker = thread::spawn(move || {
            pending_manager.request_approval(
                RequestType::FilesystemDelete,
                "删除测试文件".to_string(),
                "test target".to_string(),
            )
        });
        let request_id = (0..50).find_map(|_| {
            let pending = manager.list_pending();
            let id = pending.first().map(|item| item.id.clone());
            if id.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            id
        });
        let request_id = request_id.expect("destructive request must enter manual queue");
        manager.respond(&request_id, false).expect("reject request");
        assert_eq!(
            worker.join().expect("approval worker panicked").unwrap(),
            false
        );
        let facts = manager.list_recent_facts();
        assert_eq!(facts[0].status, ApprovalFactStatus::Rejected);
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn wildcard_auto_approve_covers_non_destructive_only() {
        let home = test_home("wildcard");
        let manager = ApprovalManager::new_in(home.clone());
        manager.update_rule("*", "auto_approve").unwrap();
        let approved = manager
            .request_approval(
                RequestType::UploadCode,
                "通配放权测试".to_string(),
                "non-destructive".to_string(),
            )
            .unwrap();
        assert!(approved);
        assert!(manager
            .update_rule("filesystem.delete", "auto_approve")
            .is_err());
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn trusted_profile_allows_r3_but_not_r4() {
        let home = test_home("trusted-profile");
        let manager = ApprovalManager::new_in(home.clone());
        manager.update_profile("trusted", true).unwrap();
        assert!(matches!(
            manager.get_mode_for_key("filesystem.delete", ApprovalMode::Manual, true, Some("R3")),
            ApprovalMode::AutoApprove
        ));
        assert!(matches!(
            manager.get_mode_for_key(
                "unsafe.system.delete",
                ApprovalMode::Manual,
                true,
                Some("R4")
            ),
            ApprovalMode::Manual
        ));
        assert!(manager
            .request_capability_approval(
                "filesystem.delete",
                "R3",
                "删除测试文件".to_string(),
                "target=test.txt".to_string(),
            )
            .unwrap());
        assert!(manager.list_pending().is_empty());
        assert_eq!(
            manager.list_recent_facts()[0].resolution_reason,
            "local_rule_auto_approved"
        );
        drop(std::fs::remove_dir_all(home));
    }

    #[test]
    fn profiles_have_distinct_default_risk_postures() {
        let home = test_home("profile-postures");
        let manager = ApprovalManager::new_in(home.clone());
        manager.update_profile("strict", false).unwrap();
        assert!(matches!(
            manager.effective_mode_for_risk("R1"),
            ApprovalMode::Manual
        ));
        manager.update_profile("balanced", false).unwrap();
        assert!(matches!(
            manager.effective_mode_for_risk("R1"),
            ApprovalMode::AutoApprove
        ));
        assert!(matches!(
            manager.effective_mode_for_risk("R2"),
            ApprovalMode::Manual
        ));
        manager.update_profile("relaxed", false).unwrap();
        assert!(matches!(
            manager.effective_mode_for_risk("R2"),
            ApprovalMode::AutoApprove
        ));
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn focus_profile_suppresses_popup_and_routes_to_inbox() {
        let home = test_home("focus-profile");
        let manager = ApprovalManager::new_in(home.clone());
        manager.update_profile("focus", true).unwrap();
        assert!(!manager.should_show_popup());
        assert_eq!(manager.get_settings().notification_mode, "inbox");
        manager.update_notification_mode("popup").unwrap();
        assert!(manager.should_show_popup());
        assert_eq!(manager.get_settings().profile, "balanced");
        drop(std::fs::remove_dir_all(home));
    }

    #[test]
    fn inherit_removes_an_explicit_rule() {
        let home = test_home("inherit-rule");
        let manager = ApprovalManager::new_in(home.clone());
        manager
            .update_rule("ai.client.import", "auto_approve")
            .unwrap();
        assert!(manager
            .get_settings()
            .rules
            .contains_key("ai.client.import"));
        manager.update_rule("ai.client.import", "inherit").unwrap();
        assert!(!manager
            .get_settings()
            .rules
            .contains_key("ai.client.import"));
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn automatic_approval_is_recorded_as_a_fact() {
        let home = test_home("automatic-fact");
        let manager = ApprovalManager::new_in(home.clone());
        manager
            .update_rule("ai.client.import", "auto_approve")
            .unwrap();

        assert!(manager
            .request_capability_approval(
                "ai.client.import",
                "R2",
                "导入 AI 配置".to_string(),
                "target=codex".to_string(),
            )
            .unwrap());

        let facts = manager.list_recent_facts();
        assert_eq!(facts[0].request_type, "ai.client.import");
        assert_eq!(facts[0].status, ApprovalFactStatus::Approved);
        assert_eq!(facts[0].resolution_reason, "local_rule_auto_approved");
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn dashboard_binding_downgrades_on_identity_change_and_logout() {
        let home = test_home("identity-binding");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("agent-user-authorization.json"),
            serde_json::json!({
                "version": 1,
                "agent_id": "agent-a",
                "user_id": "user-a",
                "scope": "agent.profile",
                "refresh_token_protected": "test",
                "refresh_expires_at": 4_000_000_000u64,
                "updated_at": 1,
            })
            .to_string(),
        )
        .unwrap();
        let manager = ApprovalManager::new_in(home.clone());
        manager.bind_identity("user-a", "agent-a").unwrap();
        manager.update_profile("trusted", true).unwrap();
        manager
            .update_rule("ai.client.import", "auto_approve")
            .unwrap();
        assert!(matches!(
            manager.get_mode_for_key("filesystem.delete", ApprovalMode::Manual, true, Some("R3")),
            ApprovalMode::AutoApprove
        ));

        fs::write(
            home.join("agent-user-authorization.json"),
            serde_json::json!({
                "version": 1,
                "agent_id": "agent-a",
                "user_id": "user-b",
                "scope": "agent.profile",
                "refresh_token_protected": "test",
                "refresh_expires_at": 4_000_000_000u64,
                "updated_at": 1,
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            manager.get_mode_for_key("filesystem.delete", ApprovalMode::Manual, true, Some("R3")),
            ApprovalMode::Manual
        ));
        manager.bind_identity("user-b", "agent-a").unwrap();
        assert_eq!(manager.get_settings().profile, "balanced");
        assert!(!manager
            .get_settings()
            .rules
            .contains_key("ai.client.import"));
        manager.clear_identity().unwrap();
        assert!(manager.get_settings().owner_user_id.is_empty());
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn exact_manual_rule_overrides_controlled_operation_auto_approval() {
        let home = test_home("exact-rule-priority");
        let manager = Arc::new(ApprovalManager::new_in(home.clone()));
        manager
            .update_rule("controlled_operation", "auto_approve")
            .unwrap();
        manager.update_rule("ai.client.import", "manual").unwrap();

        let worker_manager = Arc::clone(&manager);
        let worker = thread::spawn(move || {
            worker_manager.request_capability_approval(
                "ai.client.import",
                "R2",
                "导入 AI 配置".to_string(),
                "target=codex".to_string(),
            )
        });
        let request_id = (0..50).find_map(|_| {
            let id = manager
                .list_pending()
                .first()
                .map(|request| request.id.clone());
            if id.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            id
        });
        let request_id = request_id.expect("exact manual rule must enter the approval queue");
        manager.respond(&request_id, false).unwrap();
        assert!(!worker.join().expect("approval worker panicked").unwrap());
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn disconnected_approval_call_is_not_left_as_approved() {
        let home = test_home("disconnected-call");
        let manager = ApprovalManager::new_in(home.clone());
        let request = crate::approval::types::ApprovalRequest {
            id: "apr-disconnected".to_string(),
            request_type: "ai.client.import".to_string(),
            title: "导入 AI 配置".to_string(),
            description: "target=codex".to_string(),
            timeout_seconds: 30,
            remaining_seconds: 30,
            created_at: "00:00:00".to_string(),
            created_at_unix: super::unix_now(),
        };
        manager.append_pending_fact(&request).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        manager.pending.lock().unwrap().push(PendingApproval {
            request,
            respond_tx: tx,
            created: Instant::now(),
            timeout_seconds: 30,
        });

        assert!(manager.respond("apr-disconnected", true).is_err());
        let facts = manager.list_recent_facts();
        assert_eq!(facts[0].status, ApprovalFactStatus::Interrupted);
        assert_eq!(facts[0].resolution_reason, "approval_channel_disconnected");
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn another_agent_process_can_decide_a_durable_request() {
        let home = test_home("cross-process");
        let requester = Arc::new(ApprovalManager::new_in(home.clone()));
        let worker_manager = Arc::clone(&requester);
        let worker = thread::spawn(move || {
            worker_manager.request_capability_approval(
                "ai.client.import",
                "R2",
                "导入 AI 配置".to_string(),
                "target=codex".to_string(),
            )
        });
        let desktop = ApprovalManager::new_in(home.clone());
        let request_id = (0..50).find_map(|_| {
            let id = desktop
                .list_pending()
                .first()
                .map(|request| request.id.clone());
            if id.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            id
        });
        let request_id = request_id.expect("desktop broker must discover durable request");
        desktop.respond(&request_id, true).expect("approve request");
        assert!(worker.join().expect("approval worker panicked").unwrap());
        assert_eq!(
            desktop.list_recent_facts()[0].status,
            ApprovalFactStatus::Approved
        );
        drop(desktop);
        drop(requester);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn abandoned_pending_request_is_interrupted_on_recovery() {
        let home = test_home("recovery");
        let manager = ApprovalManager::new_in(home.clone());
        let request = crate::approval::types::ApprovalRequest {
            id: "apr-abandoned".to_string(),
            request_type: "ai.client.import".to_string(),
            title: "待恢复审批".to_string(),
            description: "test".to_string(),
            timeout_seconds: 30,
            remaining_seconds: 30,
            created_at: "00:00:00".to_string(),
            created_at_unix: super::unix_now(),
        };
        manager.append_pending_fact(&request).unwrap();
        drop(manager);

        let recovered = ApprovalManager::new_in(home.clone());
        let facts = recovered.list_recent_facts();
        assert_eq!(facts[0].status, ApprovalFactStatus::Interrupted);
        assert_eq!(facts[0].resolution_reason, "agent_restarted");
        drop(recovered);
        let _ = fs::remove_dir_all(home);
    }
}
