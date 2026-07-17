use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use super::types::*;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub message: String,
}

pub struct ApprovalManager {
    pending: Mutex<Vec<PendingApproval>>,
    settings: Mutex<ApprovalSettings>,
    log_entries: Mutex<Vec<LogEntry>>,
}

impl ApprovalManager {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            settings: Mutex::new(ApprovalSettings::default()),
            log_entries: Mutex::new(Vec::new()),
        }
    }

    pub fn request_approval(
        &self,
        request_type: RequestType,
        title: String,
        description: String,
    ) -> Result<bool, String> {
        let mode = self.get_mode_for_type(&request_type);

        match mode {
            ApprovalMode::AutoApprove => {
                self.add_log("info", &format!("自动批准: {title}"));
                return Ok(true);
            }
            ApprovalMode::AutoDeny => {
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
            request_type: match &request_type {
                RequestType::RemoteConnect => "remote_connect",
                RequestType::UploadCode => "upload_code",
                RequestType::UploadPlaceholder => "upload_placeholder",
            }
            .to_string(),
            title: title.clone(),
            description,
            timeout_seconds: timeout,
            remaining_seconds: timeout,
            created_at: now_string(),
        };

        let pending = PendingApproval {
            request: request.clone(),
            respond_tx: tx,
            created: Instant::now(),
            timeout_seconds: timeout,
        };

        if let Ok(mut list) = self.pending.lock() {
            list.push(pending);
        }

        self.add_log("info", &format!("等待审批: {title} (超时 {timeout}s)"));

        match rx.recv_timeout(std::time::Duration::from_secs(timeout)) {
            Ok(approved) => {
                self.add_log(
                    "info",
                    &format!(
                        "审批结果: {} → {}",
                        title,
                        if approved { "批准" } else { "拒绝" }
                    ),
                );
                Ok(approved)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.remove_pending(&id);
                self.add_log("warn", &format!("审批超时: {title} → 自动拒绝"));
                Ok(false)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.remove_pending(&id);
                self.add_log("error", &format!("审批通道断开: {title}"));
                Err("审批通道断开".to_string())
            }
        }
    }

    pub fn respond(&self, id: &str, approved: bool) -> Result<(), String> {
        let mut list = self.pending.lock().map_err(|e| e.to_string())?;
        if let Some(index) = list.iter().position(|p| p.request.id == id) {
            let approval = list.remove(index);
            let _ = approval.respond_tx.send(approved);
            Ok(())
        } else {
            Err(format!("未找到审批请求: {id}"))
        }
    }

    pub fn list_pending(&self) -> Vec<ApprovalRequest> {
        let mut result = Vec::new();
        if let Ok(mut list) = self.pending.lock() {
            list.retain(|p| p.created.elapsed().as_secs() < p.timeout_seconds);
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
        result
    }

    pub fn get_settings(&self) -> ApprovalSettings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn update_rule(&self, request_type: &str, mode: &str) -> Result<(), String> {
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        settings
            .rules
            .insert(request_type.to_string(), mode.to_string());
        Ok(())
    }

    pub fn update_timeout(&self, seconds: u64) -> Result<(), String> {
        let mut settings = self.settings.lock().map_err(|e| e.to_string())?;
        settings.timeout_seconds = seconds;
        Ok(())
    }

    pub fn get_logs(&self) -> Vec<LogEntry> {
        self.log_entries
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default()
    }

    pub fn add_log(&self, level: &str, message: &str) {
        if let Ok(mut logs) = self.log_entries.lock() {
            logs.push(LogEntry {
                time: now_string(),
                level: level.to_string(),
                message: message.to_string(),
            });
            if logs.len() > 500 {
                logs.drain(0..200);
            }
        }
    }

    fn get_mode_for_type(&self, request_type: &RequestType) -> ApprovalMode {
        let key = match request_type {
            RequestType::RemoteConnect => "remote_connect",
            RequestType::UploadCode => "upload_code",
            RequestType::UploadPlaceholder => "upload_placeholder",
        };

        if let Ok(settings) = self.settings.lock() {
            match settings.rules.get(key).map(|s| s.as_str()) {
                Some("auto_approve") => ApprovalMode::AutoApprove,
                Some("auto_deny") => ApprovalMode::AutoDeny,
                _ => request_type.default_mode(),
            }
        } else {
            request_type.default_mode()
        }
    }

    fn remove_pending(&self, id: &str) {
        if let Ok(mut list) = self.pending.lock() {
            list.retain(|p| p.request.id != id);
        }
    }
}

fn generate_id() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("apr-{:08x}", now.as_millis() as u32)
}

fn now_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}
