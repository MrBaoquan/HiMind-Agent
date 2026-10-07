//! 本机推理网关的用量台账（ADR 0113 第 5 节）。
//!
//! append-only JSONL，位于 Agent 数据目录。字段与平台口径对齐但**独立存储、
//! 独立展示**：平台口径有定价与结算，本机口径只给 Token 与调用次数。
//!
//! 台账是可重建的派生数据：解析失败的行跳过并计数，不让一行坏数据毁掉整屏。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::atomic_file;
use super::paths;

const LEDGER_FILE: &str = "ai-usage-local.jsonl";
/// 台账只保留有界历史：超限时最旧的行在写入时被丢弃，避免无限增长。
const MAX_LEDGER_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct LocalUsageRecord {
    /// RFC 3339，UTC。
    pub occurred_at: String,
    /// 绑定标识：`<client>:<service>`。
    pub binding_id: String,
    pub client: String,
    pub service: String,
    pub model: String,
    pub protocol: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub status: String,
    /// 上游没返回用量：只累加调用次数，不估算 Token。
    #[serde(default)]
    pub usage_unreported: bool,
    /// 上游是平台托管服务：以平台口径为准，本机合计排除。
    #[serde(default)]
    pub platform_metered: bool,
}

impl LocalUsageRecord {
    pub(crate) fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

pub(crate) fn ledger_path() -> PathBuf {
    paths::agent_home().join("data").join(LEDGER_FILE)
}

/// 追加一条记录。台账只增不改，因此用文件锁串行化后直接 append。
pub(crate) fn append(record: &LocalUsageRecord) -> Result<(), Box<dyn Error>> {
    let path = ledger_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _guard = atomic_file::lock(&path)?;
    trim_if_oversized(&path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

/// 超过上限时整体重写为最近一半行。台账可重建，允许这种粗粒度回收。
fn trim_if_oversized(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    if metadata.len() <= MAX_LEDGER_BYTES {
        return Ok(());
    }
    let lines = read_lines(path)?;
    let keep = lines.len() / 2;
    let mut content = Vec::new();
    for line in lines.into_iter().skip(keep) {
        content.extend_from_slice(line.as_bytes());
        content.push(b'\n');
    }
    atomic_file::atomic_write(path, &content)?;
    Ok(())
}

fn read_lines(path: &std::path::Path) -> Result<Vec<String>, Box<dyn Error>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .collect())
}

/// 读取全部记录；无法解析的行跳过。返回值带 `skipped` 计数，界面可如实声明。
pub(crate) fn read_all() -> Result<(Vec<LocalUsageRecord>, u64), Box<dyn Error>> {
    let path = ledger_path();
    let mut records = Vec::new();
    let mut skipped = 0_u64;
    for line in read_lines(&path)? {
        match serde_json::from_str::<LocalUsageRecord>(&line) {
            Ok(record) => records.push(record),
            Err(_) => skipped += 1,
        }
    }
    Ok((records, skipped))
}

/// 按天的窗口聚合。`days` 为 1（当天）时不出趋势，与平台面板的今日档一致。
pub(crate) fn overview(days: i64) -> Value {
    let days = days.clamp(1, 90);
    let today = chrono::Utc::now().date_naive();
    let from = today - chrono::Duration::days(days - 1);
    let (records, skipped) = read_all().unwrap_or_else(|_| (Vec::new(), 0));

    let mut labels = Vec::new();
    let mut cursor = from;
    while cursor <= today {
        labels.push(cursor.to_string());
        cursor += chrono::Duration::days(1);
    }

    let mut totals = Totals::default();
    let mut unreported = 0_u64;
    let mut platform_metered = 0_u64;
    let mut daily: BTreeMap<String, Totals> = BTreeMap::new();
    let mut by_client: BTreeMap<String, Totals> = BTreeMap::new();
    let mut by_model: BTreeMap<String, Totals> = BTreeMap::new();
    let mut by_service: BTreeMap<String, Totals> = BTreeMap::new();

    for record in &records {
        let day = record
            .occurred_at
            .get(0..10)
            .unwrap_or_default()
            .to_string();
        if day.is_empty() || day < from.to_string() || day > today.to_string() {
            continue;
        }
        totals.add(record);
        if record.usage_unreported {
            unreported = unreported.saturating_add(1);
        }
        if record.platform_metered {
            platform_metered = platform_metered.saturating_add(1);
        }
        daily.entry(day).or_default().add(record);
        by_client
            .entry(if record.client.trim().is_empty() {
                "未标注".to_string()
            } else {
                record.client.clone()
            })
            .or_default()
            .add(record);
        by_model
            .entry(if record.model.trim().is_empty() {
                "未标注".to_string()
            } else {
                record.model.clone()
            })
            .or_default()
            .add(record);
        by_service
            .entry(if record.service.trim().is_empty() {
                "未标注".to_string()
            } else {
                record.service.clone()
            })
            .or_default()
            .add(record);
    }

    let daily_requests = labels
        .iter()
        .map(|label| daily.get(label).map(|item| item.requests).unwrap_or(0))
        .collect::<Vec<u64>>();
    let daily_tokens = labels
        .iter()
        .map(|label| daily.get(label).map(Totals::tokens).unwrap_or(0))
        .collect::<Vec<u64>>();

    json!({
        "available": true,
        "range": match days {
            1 => "today",
            7 => "7d",
            _ => "30d",
        },
        "date_from": from.to_string(),
        "date_to": today.to_string(),
        "has_trend": days > 1,
        "requests": totals.requests,
        "input_tokens": totals.input,
        "output_tokens": totals.output,
        "cached_tokens": totals.cached,
        "reasoning_tokens": totals.reasoning,
        "usage_unreported": unreported,
        "platform_metered": platform_metered,
        "skipped_records": skipped,
        "labels": labels,
        "daily_requests": daily_requests,
        "daily_tokens": daily_tokens,
        "breakdowns": {
            "client": groups(by_client),
            "model": groups(by_model),
            "service": groups(by_service),
        },
    })
}

#[derive(Default, Clone, Copy)]
struct Totals {
    requests: u64,
    input: u64,
    output: u64,
    cached: u64,
    reasoning: u64,
}

impl Totals {
    fn add(&mut self, record: &LocalUsageRecord) {
        self.requests = self.requests.saturating_add(1);
        self.input = self.input.saturating_add(record.input_tokens);
        self.output = self.output.saturating_add(record.output_tokens);
        self.cached = self.cached.saturating_add(record.cached_tokens);
        self.reasoning = self.reasoning.saturating_add(record.reasoning_tokens);
    }

    fn tokens(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// 构成表按 Token 倒序，与服务端的 Top-N 语义一致。
fn groups(items: BTreeMap<String, Totals>) -> Vec<Value> {
    let mut rows = items
        .into_iter()
        .map(|(label, totals)| {
            json!({
                "key": label,
                "label": label,
                "requests": totals.requests,
                "input_tokens": totals.input,
                "output_tokens": totals.output,
                "tokens": totals.tokens(),
            })
        })
        .collect::<Vec<Value>>();
    rows.sort_by(|left, right| {
        let left = left.get("tokens").and_then(Value::as_u64).unwrap_or(0);
        let right = right.get("tokens").and_then(Value::as_u64).unwrap_or(0);
        right.cmp(&left)
    });
    rows.truncate(20);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(day: &str, client: &str, input: u64, output: u64) -> LocalUsageRecord {
        LocalUsageRecord {
            occurred_at: format!("{day}T08:00:00Z"),
            binding_id: format!("{client}:svc"),
            client: client.to_string(),
            service: "svc".to_string(),
            model: "m".to_string(),
            protocol: "openai-chat".to_string(),
            stream: false,
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            reasoning_tokens: 0,
            status: "success".to_string(),
            usage_unreported: false,
            platform_metered: false,
        }
    }

    #[test]
    fn totals_add_up_per_record() {
        let mut totals = Totals::default();
        totals.add(&record("2026-10-05", "codex", 10, 5));
        totals.add(&record("2026-10-05", "codex", 7, 3));
        assert_eq!(totals.requests, 2);
        assert_eq!(totals.tokens(), 25);
    }

    #[test]
    fn groups_sort_by_tokens_descending() {
        let mut items = BTreeMap::new();
        let mut small = Totals::default();
        small.add(&record("2026-10-05", "a", 1, 1));
        let mut large = Totals::default();
        large.add(&record("2026-10-05", "b", 100, 100));
        items.insert("a".to_string(), small);
        items.insert("b".to_string(), large);
        let rows = groups(items);
        assert_eq!(rows[0]["label"], "b");
        assert_eq!(rows[0]["tokens"], 200);
    }

    #[test]
    fn record_tolerates_missing_optional_fields() {
        let minimal = r#"{"occurred_at":"2026-10-05T08:00:00Z","binding_id":"codex:svc","client":"codex","service":"svc","model":"m","protocol":"openai-chat","input_tokens":3}"#;
        let parsed = serde_json::from_str::<LocalUsageRecord>(minimal).unwrap();
        assert_eq!(parsed.input_tokens, 3);
        assert!(!parsed.usage_unreported);
        assert_eq!(parsed.total_tokens(), 3);
    }

    /// `HIMIND_AGENT_HOME` 是进程级变量，切换它要和其它用例共用同一把全局锁。
    fn with_isolated_home(run: impl FnOnce()) {
        let _guard = crate::store::paths::test_env_lock();
        let previous = std::env::var("HIMIND_AGENT_HOME").ok();
        let root = std::env::temp_dir().join(format!(
            "himind-local-usage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        run();
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
    }

    #[test]
    fn append_and_read_round_trip_skips_broken_lines() {
        with_isolated_home(|| {
            append(&record("2026-10-05", "codex", 5, 6)).unwrap();
            let (records, skipped) = read_all().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(skipped, 0);
            assert_eq!(records[0].total_tokens(), 11);

            let mut file = OpenOptions::new().append(true).open(ledger_path()).unwrap();
            file.write_all(b"{not json}\n").unwrap();
            let (records, skipped) = read_all().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(skipped, 1);
        });
    }
}
