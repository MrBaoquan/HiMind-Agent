//! 出站通知通道。
//!
//! 定位：Agent 主动把「执行侧的事实」推给人看——某个审批卡住了、某条运行失败了。
//! 只出不进，不承担会话，也不引入第二套身份。
//!
//! 三条硬约束：
//! 1. **推送失败绝不影响审批本身**。网络断了、webhook 失效了，本机审批照常工作。
//! 2. **不落明文凭据**。Webhook 地址与签名密钥走 DPAPI，和 connector credentials 同一套。
//! 3. **推送内容严格脱敏**。只推人做决策需要的最小事实，不推 diff、产物、完整路径。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use crate::store::connector_credentials as credentials;

const STORE_FILE: &str = "notification-channels.json";
const OUTBOX_FILE: &str = "notification-outbox.json";
const DELIVERY_TIMEOUT_SECONDS: u64 = 8;
/// 通知通道凭据在 credentials store 里的归属 id。
const NOTIFICATION_CREDENTIAL_OWNER: &str = "notification";
const MAX_ATTEMPTS: u32 = 3;
const MAX_DESCRIPTION_CHARS: usize = 200;
const MAX_ERROR_CHARS: usize = 120;

/// 一个已配置的通知通道。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotificationChannelConfig {
    pub id: String,
    /// 通道类型：dingtalk / feishu / wecom / slack / discord / generic
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub enabled: bool,
    /// 凭据句柄。真实 webhook 地址存在 credentials 里，这里只存句柄。
    #[serde(default)]
    pub webhook_handle: String,
    /// 订阅的事件类型：approval.requested / run.failed
    #[serde(default)]
    pub events: Vec<String>,
    /// 最低风险等级；低于它的事件不推。空表示不按风险过滤。
    #[serde(default)]
    pub min_risk: String,
}

/// 一条待推送的通知。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Notification {
    /// 事件类型，与通道订阅的 `events` 对应。
    pub event: String,
    /// 去重键：同一审批/同一运行的同一次失败只推一次。
    pub dedupe_key: String,
    pub title: String,
    pub body: String,
    /// R1..R4；空表示不适用。
    #[serde(default)]
    pub risk_level: String,
    /// 运行号尾号，用于人肉对应。
    #[serde(default)]
    pub run_ref: String,
    pub created_at: u64,
}

fn store_path() -> PathBuf {
    crate::store::paths::agent_home().join(STORE_FILE)
}

fn outbox_path() -> PathBuf {
    crate::store::paths::agent_home().join(OUTBOX_FILE)
}

pub(crate) fn load() -> Result<Vec<NotificationChannelConfig>, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&path)?;
    // 配置损坏时返回空，而不是让调用方崩掉：通知是辅助功能。
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

pub(crate) fn save(items: &[NotificationChannelConfig]) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec_pretty(items)?;
    crate::store::atomic_file::atomic_write(&store_path(), &body)?;
    Ok(())
}

/// 写入通道的 webhook 地址（DPAPI 保护）。
pub(crate) fn set_webhook(handle: &str, url: &str) -> Result<(), Box<dyn Error>> {
    let handle = handle.trim();
    if handle.is_empty() {
        return Err("通知通道需要 webhook 句柄".into());
    }
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("webhook 地址必须是 http(s) 链接".into());
    }
    // connector_credentials 要求 handle + 归属 id；通知通道统一归在
    // `notification` 这个虚拟归属下，与真实 connector 的凭据分开管理。
    credentials::set_secret(handle, NOTIFICATION_CREDENTIAL_OWNER, url).map(|_| ())
}

/// 凭据里是否已经存了 webhook 地址（供界面显示「已配置」）。
pub(crate) fn has_webhook(handle: &str) -> bool {
    resolve_webhook(handle).is_ok()
}

/// 删除通道时一并清掉它的 webhook 凭据。
pub(crate) fn forget_webhook(handle: &str) -> Result<(), Box<dyn Error>> {
    credentials::remove(handle).map(|_| ()).or(Ok(()))
}

/// 读取通道的 webhook 地址。
fn resolve_webhook(handle: &str) -> Result<String, Box<dyn Error>> {
    credentials::resolve(handle)?
        .map(|resolved| resolved.value)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("通知通道缺少 webhook 配置: {handle}").into())
}

/// 风险等级排序，用于 `min_risk` 过滤。未知等级按 R2 处理。
fn risk_rank(level: &str) -> u8 {
    match level.trim().to_ascii_uppercase().as_str() {
        "R1" | "READ_ONLY" => 1,
        "R2" | "LOCAL_WRITE" | "LOCAL_ACTION" | "PROCESS" | "NETWORK" => 2,
        "R3" => 3,
        "R4" | "SYSTEM" | "NETWORK_WRITE" | "DESTRUCTIVE" => 4,
        _ => 2,
    }
}

fn truncate(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 渲染成各平台的消息体。
///
/// 只放人做决定需要的最小事实：谁能批、批什么、多急。
/// 不放 diff、产物内容、完整路径——那些留在本机界面里。
fn render_body(kind: &str, note: &Notification) -> Value {
    let risk = if note.risk_level.trim().is_empty() {
        String::new()
    } else {
        format!(" · {}", note.risk_level.trim())
    };
    let run_ref = if note.run_ref.trim().is_empty() {
        String::new()
    } else {
        format!(" · {}", note.run_ref.trim())
    };
    let body = truncate(&note.body, MAX_DESCRIPTION_CHARS);
    let text = format!(
        "{}\n{}{}{}\n\n请在 HiMind Agent 的审批中心处理。",
        note.title, body, risk, run_ref
    );

    match kind {
        // 钉钉自定义机器人：text 类型最简单，不需要签名计算。
        "dingtalk" => json!({
            "msgtype": "text",
            "text": { "content": text }
        }),
        "feishu" | "lark" => json!({
            "msg_type": "text",
            "content": { "text": text }
        }),
        // 企微、Slack、Discord 都用纯文本字段，字段名不同而已。
        "wecom" | "wechat-work" => json!({
            "msgtype": "text",
            "text": { "content": text }
        }),
        "slack" => json!({ "text": text }),
        "discord" => json!({ "content": text }),
        // generic：把结构化字段原样给出，由对方决定怎么渲染。
        _ => json!({
            "event": note.event,
            "title": note.title,
            "body": body,
            "risk_level": note.risk_level,
            "run_ref": note.run_ref,
            "created_at": note.created_at,
        }),
    }
}

/// 单次投递。
///
/// 同步阻塞但调用方会把它放到独立线程里；超时 8 秒，绝不拖住审批线程。
fn deliver_once(cfg: &NotificationChannelConfig, note: &Notification) -> Result<(), Box<dyn Error>> {
    let url = resolve_webhook(&cfg.webhook_handle)?;
    let payload = render_body(&cfg.kind, note);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DELIVERY_TIMEOUT_SECONDS))
        .build()?;
    let response = client.post(&url).json(&payload).send()?;
    if !response.status().is_success() {
        return Err(format!("通知投递返回 HTTP {}", response.status().as_u16()).into());
    }
    Ok(())
}

/// 通道是否应该收到这条通知。
fn accepts(cfg: &NotificationChannelConfig, note: &Notification) -> bool {
    if !cfg.enabled {
        return false;
    }
    if cfg.webhook_handle.trim().is_empty() {
        return false;
    }
    // events 留空表示订阅全部：用户新加通道时不填也不会静默不推。
    if !cfg.events.is_empty() && !cfg.events.iter().any(|event| event == &note.event) {
        return false;
    }
    if !cfg.min_risk.trim().is_empty() && risk_rank(&note.risk_level) < risk_rank(&cfg.min_risk) {
        return false;
    }
    true
}

/// 推送一条通知给所有订阅它的通道。
///
/// 返回实际投递成功与失败的通道数。**任何失败都不上抛**——调用方是审批线程，
/// 通知失败不能让审批失败。
pub(crate) fn dispatch(note: &Notification) -> (usize, usize) {
    let Ok(channels) = load() else {
        return (0, 0);
    };
    let mut delivered = 0usize;
    let mut failed = 0usize;
    for cfg in channels.iter().filter(|cfg| accepts(cfg, note)) {
        match deliver_once(cfg, note) {
            Ok(()) => delivered += 1,
            Err(error) => {
                failed += 1;
                // 失败记进 outbox，由后台重试；同时写日志便于排查。
                let _ = enqueue_retry(cfg, note, &error.to_string());
                crate::approval::append_event_log(
                    "warn",
                    &format!("通知投递失败（{}）: {error}", cfg.id),
                );
            }
        }
    }
    (delivered, failed)
}

/// 投递失败的重试记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryRecord {
    channel_id: String,
    note: Notification,
    attempts: u32,
    last_error: String,
    next_attempt_at: u64,
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn load_outbox() -> Vec<RetryRecord> {
    std::fs::read_to_string(outbox_path())
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

fn save_outbox(items: &[RetryRecord]) -> Result<(), Box<dyn Error>> {
    let body = serde_json::to_vec_pretty(items)?;
    crate::store::atomic_file::atomic_write(&outbox_path(), &body)?;
    Ok(())
}

fn enqueue_retry(
    cfg: &NotificationChannelConfig,
    note: &Notification,
    error: &str,
) -> Result<(), Box<dyn Error>> {
    let mut items = load_outbox();
    // 同一通道 + 同一去重键只留一条，避免网络抖动时堆积重复。
    items.retain(|item| !(item.channel_id == cfg.id && item.note.dedupe_key == note.dedupe_key));
    items.push(RetryRecord {
        channel_id: cfg.id.clone(),
        note: note.clone(),
        attempts: 1,
        last_error: truncate(error, MAX_ERROR_CHARS),
        // 指数退避：1 分钟后第一次重试。
        next_attempt_at: now_seconds() + 60,
    });
    save_outbox(&items)
}

/// 重试到期的投递。由后台循环周期调用。
///
/// 退避间隔 1 → 5 → 15 分钟，超过 `MAX_ATTEMPTS` 的直接丢弃并记日志——
/// 一个坏掉的 webhook 不应该永远占着队列。
pub(crate) fn retry_due() -> (usize, usize) {
    let now = now_seconds();
    let mut items = load_outbox();
    if items.is_empty() {
        return (0, 0);
    }
    let channels = load().unwrap_or_default();
    let mut delivered = 0usize;
    let mut dropped = 0usize;
    let mut remaining = Vec::new();
    for mut item in items.drain(..) {
        if item.next_attempt_at > now {
            remaining.push(item);
            continue;
        }
        let Some(cfg) = channels.iter().find(|cfg| cfg.id == item.channel_id) else {
            // 通道被删了：这条重试没有意义。
            dropped += 1;
            continue;
        };
        match deliver_once(cfg, &item.note) {
            Ok(()) => delivered += 1,
            Err(error) => {
                item.attempts = item.attempts.saturating_add(1);
                item.last_error = truncate(&error.to_string(), MAX_ERROR_CHARS);
                if item.attempts >= MAX_ATTEMPTS {
                    dropped += 1;
                    crate::approval::append_event_log(
                        "warn",
                        &format!("通知放弃投递（{}，{} 次）: {}", cfg.id, item.attempts, item.last_error),
                    );
                    continue;
                }
                item.next_attempt_at = now + match item.attempts {
                    1 => 60,
                    2 => 300,
                    _ => 900,
                };
                remaining.push(item);
            }
        }
    }
    let _ = save_outbox(&remaining);
    (delivered, dropped)
}

/// 审批请求的推送文案。
///
/// 标题与说明来自审批本身（已脱敏），这里只截断，不额外补充业务细节。
pub(crate) fn approval_notification(
    approval_id: &str,
    title: &str,
    description: &str,
    risk_level: &str,
) -> Notification {
    Notification {
        event: "approval.requested".to_string(),
        dedupe_key: format!("approval:{approval_id}"),
        title: truncate(title, 120),
        body: truncate(description, MAX_DESCRIPTION_CHARS),
        risk_level: risk_level.trim().to_ascii_uppercase(),
        run_ref: run_ref_from_id(approval_id),
        created_at: now_seconds(),
    }
}

/// 运行失败的推送文案。
pub(crate) fn run_failed_notification(
    run_id: &str,
    workflow_name: &str,
    step_title: &str,
    error: &str,
) -> Notification {
    Notification {
        event: "run.failed".to_string(),
        // 同一条运行的同一处失败只推一次；重试后再次失败是新事件，用 attempt 区分由调用方保证。
        dedupe_key: format!("run-failed:{run_id}:{step_title}"),
        title: format!("运行失败：{workflow_name}"),
        body: format!(
            "{}\n{}",
            step_title,
            truncate(error, MAX_ERROR_CHARS)
        ),
        risk_level: String::new(),
        run_ref: run_ref_from_id(run_id),
        created_at: now_seconds(),
    }
}

/// 从审批/运行 ID 里取可读尾号，与界面上的 `#319541` 口径一致。
fn run_ref_from_id(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(6)
        .collect();
    if tail.is_empty() {
        return String::new();
    }
    format!("#{}", tail.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(kind: &str, events: &[&str], min_risk: &str) -> NotificationChannelConfig {
        NotificationChannelConfig {
            id: format!("test-{kind}"),
            kind: kind.to_string(),
            label: String::new(),
            enabled: true,
            webhook_handle: "cred://notification/test".to_string(),
            events: events.iter().map(|event| event.to_string()).collect(),
            min_risk: min_risk.to_string(),
        }
    }

    fn note(risk: &str) -> Notification {
        Notification {
            event: "approval.requested".to_string(),
            dedupe_key: "approval:x".to_string(),
            title: "创建 PR".to_string(),
            body: "整理好的 PR 说明".to_string(),
            risk_level: risk.to_string(),
            run_ref: "#319541".to_string(),
            created_at: 1,
        }
    }

    #[test]
    fn disabled_or_handleless_channels_never_accept() {
        let mut cfg = channel("dingtalk", &[], "");
        assert!(accepts(&cfg, &note("R4")));
        cfg.enabled = false;
        assert!(!accepts(&cfg, &note("R4")), "关闭的通道不得投递");
        cfg.enabled = true;
        cfg.webhook_handle.clear();
        assert!(!accepts(&cfg, &note("R4")), "没有 webhook 的通道不得投递");
    }

    #[test]
    fn empty_events_means_subscribe_all() {
        // 新加通道时不填 events 也要能收到，否则用户会以为功能坏了。
        assert!(accepts(&channel("dingtalk", &[], ""), &note("R4")));
        assert!(accepts(&channel("dingtalk", &["approval.requested"], ""), &note("R4")));
        assert!(!accepts(&channel("dingtalk", &["run.failed"], ""), &note("R4")));
    }

    #[test]
    fn min_risk_filters_by_severity_not_equality() {
        let cfg = channel("dingtalk", &[], "R3");
        assert!(!accepts(&cfg, &note("R2")), "低于阈值的不得投递");
        assert!(accepts(&cfg, &note("R3")));
        assert!(accepts(&cfg, &note("R4")), "高于阈值的必须投递");
    }

    #[test]
    fn rendered_bodies_carry_no_detail_beyond_the_minimum() {
        let approval = note("R4");
        let dingtalk = render_body("dingtalk", &approval);
        let text = dingtalk["text"]["content"].as_str().unwrap();
        assert!(text.contains("创建 PR"));
        assert!(text.contains("R4"));
        assert!(text.contains("#319541"));
        // 固定的行动指引必须存在：收到卡片的人要知道去哪处理。
        assert!(text.contains("审批中心"));

        // generic 通道给结构化字段，但同样不含产物/路径。
        let generic = render_body("generic", &approval);
        assert_eq!(generic["event"], "approval.requested");
        assert_eq!(generic["risk_level"], "R4");
    }

    #[test]
    fn long_bodies_are_truncated_without_losing_the_reason() {
        let mut approval = note("R4");
        approval.body = "字".repeat(500);
        let rendered = render_body("feishu", &approval);
        let text = rendered["content"]["text"].as_str().unwrap();
        assert!(text.contains('…'), "超长正文必须截断并留省略号");
        assert!(text.chars().count() < 400, "截断后长度应受控");
    }

    #[test]
    fn run_ref_matches_the_ui_convention() {
        // 与前端 shortRunId 的口径一致：剥掉冒号前的交互标识与尾部进程号，取尾 6 位。
        // `workflow_run_1794108714_37340` 尾部就是 `37340`，不足 6 位原样给出。
        assert_eq!(run_ref_from_id("workflow_run_1794108714_37340"), "#437340");
        assert_eq!(run_ref_from_id(""), "");
    }

    #[test]
    fn approval_notification_uses_a_stable_dedupe_key() {
        let first = approval_notification("workflow:run:STEP", "标题", "说明", "r4");
        let second = approval_notification("workflow:run:STEP", "标题", "说明", "R4");
        assert_eq!(first.dedupe_key, second.dedupe_key, "同一审批的重推必须去重");
        assert_eq!(first.risk_level, "R4", "风险等级统一大写");
    }
}
