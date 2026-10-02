//! Agent 配置层备份：导出、检视、恢复。
//!
//! 为什么备份的是"配置层"而不是整个数据目录，见
//! `docs/adr/0002-agent-config-backup-and-restore.md`。一句话版本：Agent home
//! 里九成以上的体积（`versions/`、`runtimes/`、`staging/`、`profiles/`）都能
//! 重新下载，而换机真正会丢的东西——账号、凭据、已安装拓展台账、工作区绑定
//! ——只有 1MB 出头；并且这些凭据是用 DPAPI 封在本机的，字节拷到另一台机器
//! 照样解不开，必须换成用户自己能带走的口令。
//!
//! 两条不能妥协的规则：
//!
//! - **分类失败就不打包。** 没命中任何规则的条目按"默认不进入包"处理，并把
//!   原因写进 manifest，而不是猜一个结果。
//! - **恢复只覆盖包里有的文件。** 磁盘上多出来的东西保持原样，所以"设备身份
//!   不在包里"就等于"这台机器的身份不被覆盖"。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use zip::write::FileOptions;

use crate::store::backup_crypto;
use crate::store::credentials::DPAPI_PREFIX;
use crate::store::paths::agent_home;

pub(crate) const MANIFEST_FILE: &str = "himind-agent-backup.json";
pub(crate) const CHECKSUM_FILE: &str = "checksums.sha256";
pub(crate) const CREDENTIAL_FILE: &str = "credentials.enc";
pub(crate) const FORMAT_ID: &str = "himind-agent-backup";
pub(crate) const FORMAT_VERSION: u32 = 1;

const PAYLOAD_PREFIX: &str = "payload/";
const AUTOMATIC_BACKUP_DIR: &str = "backups";
/// 凭据在包里的占位前缀。写成整段可读标记而不是 `backup:1` 这种短串，
/// 是为了让"配置文件里恰好有同名字符串"变成一个不现实的巧合。
const CREDENTIAL_MARKER: &str = "himind-backup-credential:";
const MIN_PASSPHRASE_CHARS: usize = backup_crypto::MIN_PASSPHRASE_CHARS;

// ---------------------------------------------------------------------------
// 分类
// ---------------------------------------------------------------------------

/// 一个条目的归属：进包，或者明确不进包并给出原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Include(&'static str),
    Skip(&'static str),
}

/// 进包的顶层目录。条目下面的内容按 `classify_name` 逐级过滤。
const INCLUDED_DIRS: &[(&str, &str)] = &[
    ("data", "配置与状态"),
    ("acp", "ACP 运行时档案与会话"),
    ("connectors", "连接器状态与凭据"),
    ("github", "GitHub 分发凭据"),
    ("trusted-keys", "分发信任根"),
    ("extension-data", "拓展运行数据"),
];

/// 明确不进包的顶层目录。写在这里的每一条都会出现在导出报告的跳过列表里，
/// 所以"包里有什么"不需要靠猜。
const EXCLUDED_DIRS: &[(&str, &str)] = &[
    ("plugins", "拓展内容，重装后重新安装"),
    ("skills", "技能内容，重装后重新安装"),
    ("workflows", "工作流内容，重装后重新安装"),
    ("plugin-drafts", "拓展开发草稿"),
    ("skill-drafts", "技能开发草稿"),
    ("workflow-drafts", "工作流开发草稿"),
    ("plugin-data", "插件运行数据，随插件重装重建"),
    ("plugin-development-health", "拓展开发体检缓存"),
    (
        "software-distribution-inspections",
        "分发体检凭据，临时文件",
    ),
    ("approval-owners", "审批锁文件"),
    ("outbox", "待上报队列，重启后可重建"),
    ("logs", "运行日志"),
    ("acp-logs", "ACP 会话留痕"),
    ("skill-runs", "技能运行记录"),
    ("versions", "可重新下载的版本目录"),
    ("runtimes", "可重新下载的运行时"),
    ("staging", "更新暂存目录"),
    ("previous", "上一版本程序文件"),
    ("resources", "随安装包分发的资源"),
    ("manual-backups", "用户手动备份"),
    ("backups", "自动快照"),
    ("profiles", "开发 profile 目录"),
    ("ebwebview", "WebView2 用户数据目录"),
    ("protocol-test-backup", "协议测试遗留文件"),
    ("qa-settings-layout-webview", "UI 排查用的 WebView2 数据"),
];

/// 顶层单文件规则之外的已知文件。
const NAMED_FILES: &[(&str, &str)] = &[("agent-state.device-id", "设备身份，默认不进包")];

/// `data/` 下的已知子目录。三种情况：发布暂存、事务日志、本地缓存。
/// 它们都能在本机重建，进包只会让"换台机器接着用"带上过期状态。
const EXCLUDED_DATA_DIRS: &[(&str, &str)] = &[
    ("distribution-manifests", "发布清单暂存，重新发布即可生成"),
    ("extension-transactions", "拓展安装事务日志，启动时自行恢复"),
    ("extension-provenance", "拓展来源记录，联网后从市场重新获取"),
    ("extension-source-cache", "扩展源目录缓存，联网后自动刷新"),
    ("mcp-catalog-cache", "MCP 目录快照缓存，联网后自动刷新"),
];

/// `data/` 下的已知单文件。和目录一样是「能在本机重建、进包只会带走过期状态」，
/// 但这里只有一条：MCP 目录来源配置里存着「哪些未验证来源已确认」。
/// 换台机器应当重新确认一次，而不是继承别人的信任决定。
const EXCLUDED_DATA_FILES: &[(&str, &str)] = &[(
    "mcp-catalog-sources.json",
    "MCP 目录来源与确认记录，换机器后重新确认",
)];

/// 默认不进包的设备身份。它们是本机身份，落到第二台机器上会变成"两台设备
/// 用同一个身份"，只有原地重装的用户才需要显式打开高级开关。
const DEVICE_IDENTITY_PATHS: &[&str] = &["data/agent-state.json", "data/agent-state.device-id"];

/// 顶层条目（home 根下的目录或文件）的分类。返回 `None` 表示没有规则覆盖。
fn classify_top_level(name: &str, is_dir: bool) -> Option<Decision> {
    if let Some(rule) = INCLUDED_DIRS.iter().find(|(known, _)| *known == name) {
        return Some(if is_dir {
            Decision::Include(rule.1)
        } else {
            Decision::Skip("同名文件不是目录")
        });
    }
    if let Some(rule) = EXCLUDED_DIRS.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if let Some(rule) = NAMED_FILES.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if is_transient_name(name) {
        return Some(Decision::Skip("临时文件或历史副本"));
    }
    if !is_dir {
        return Some(if name.to_ascii_lowercase().ends_with(".json") {
            Decision::Include("本机配置")
        } else {
            Decision::Skip("程序文件或版本指针")
        });
    }
    None
}

/// 被包含目录里单个条目的分类。返回 `None` 表示没有规则覆盖 —— 调用方按
/// "默认不打包"处理，而不是顺手带上。
fn classify_name(name: &str) -> Option<Decision> {
    if let Some(rule) = EXCLUDED_DIRS.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if let Some(rule) = EXCLUDED_DATA_DIRS.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if let Some(rule) = EXCLUDED_DATA_FILES.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if let Some(rule) = INCLUDED_DIRS.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Include(rule.1));
    }
    if let Some(rule) = NAMED_FILES.iter().find(|(known, _)| *known == name) {
        return Some(Decision::Skip(rule.1));
    }
    if is_transient_name(name) {
        return Some(Decision::Skip("临时文件或历史副本"));
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".json") || lower.ends_with(".pem") || lower.ends_with(".jsonl.lock") {
        return Some(Decision::Include("配置文件"));
    }
    None
}

/// 文件名本身是否就该被排除：锁、临时、日志、历史副本、可执行文件。
fn is_transient_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let suffix = [
        ".lock",
        ".tmp",
        ".cache",
        ".log",
        ".jsonl",
        ".journal",
        ".exe",
        ".ico",
        ".sqlite3",
        ".sqlite3-journal",
        "-journal",
    ];
    if suffix.iter().any(|item| lower.ends_with(item)) {
        return true;
    }
    if lower.ends_with("-outbox") {
        return true;
    }
    [
        ".bak",
        ".before-",
        ".stale-",
        ".pre-",
        ".previous",
        ".claimed-",
        ".replacing",
        ".repairing",
    ]
    .iter()
    .any(|item| lower.contains(item))
}

/// 完整相对路径的分类。导出、检视、恢复都用这一个入口，避免三处规则漂移。
fn classify_path(rel: &Path) -> Option<Decision> {
    let mut segments = rel.components().filter_map(|part| match part {
        Component::Normal(value) => Some(value.to_string_lossy().to_string()),
        _ => None,
    });
    let first = segments.next()?;
    let top = classify_top_level(&first, true)?;
    for segment in segments {
        if segment.eq_ignore_ascii_case(&first) {
            continue;
        }
        match classify_name(&segment)? {
            Decision::Skip(reason) => return Some(Decision::Skip(reason)),
            Decision::Include(_) => {}
        }
    }
    Some(top)
}

/// 给 UI 用的范围说明。前端的"备份包含什么"直接渲染这份数据，避免界面文案
/// 和真实分类器各说各话。
#[derive(Serialize, Clone, Debug)]
pub(crate) struct ScopeEntry {
    pub name: String,
    pub category: String,
    pub reason: String,
    pub included: bool,
}

pub(crate) fn scope_entries() -> Vec<ScopeEntry> {
    let mut entries = Vec::new();
    for (name, category) in INCLUDED_DIRS {
        entries.push(ScopeEntry {
            name: (*name).to_string(),
            category: (*category).to_string(),
            reason: String::new(),
            included: true,
        });
    }
    entries.push(ScopeEntry {
        name: "home 根目录的 *.json".to_string(),
        category: "本机配置".to_string(),
        reason: String::new(),
        included: true,
    });
    for (name, reason) in EXCLUDED_DIRS {
        entries.push(ScopeEntry {
            name: (*name).to_string(),
            category: String::new(),
            reason: (*reason).to_string(),
            included: false,
        });
    }
    for (name, reason) in EXCLUDED_DATA_DIRS {
        entries.push(ScopeEntry {
            name: format!("data/{name}"),
            category: String::new(),
            reason: (*reason).to_string(),
            included: false,
        });
    }
    for (name, reason) in EXCLUDED_DATA_FILES {
        entries.push(ScopeEntry {
            name: format!("data/{name}"),
            category: String::new(),
            reason: (*reason).to_string(),
            included: false,
        });
    }
    entries
}

// ---------------------------------------------------------------------------
// 包结构
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct ManifestEntry {
    pub path: String,
    pub category: String,
    pub size: u64,
    pub sha256: String,
    /// 这个文件里被封装的凭据条数。0 表示没有敏感值。
    pub credentials: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct SkippedEntry {
    pub path: String,
    pub reason: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct BackupManifest {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub agent_version: String,
    pub profile: String,
    pub platform: String,
    pub machine: String,
    pub includes_device_identity: bool,
    pub credentials_sealed: bool,
    pub entries: Vec<ManifestEntry>,
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct CategorySummary {
    pub category: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct ExportReport {
    pub path: String,
    pub created_at: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub credentials: u32,
    pub includes_device_identity: bool,
    pub categories: Vec<CategorySummary>,
    pub skipped: Vec<SkippedEntry>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct InspectReport {
    pub path: String,
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub machine: String,
    pub agent_version: String,
    pub profile: String,
    pub includes_device_identity: bool,
    pub needs_passphrase: bool,
    pub file_count: usize,
    pub total_bytes: u64,
    pub credentials: u32,
    pub credential_files: Vec<String>,
    pub categories: Vec<CategorySummary>,
    pub skipped: Vec<SkippedEntry>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct MissingPath {
    pub path: String,
    pub source: String,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct RestoreReport {
    pub path: String,
    pub snapshot: String,
    pub restored: Vec<String>,
    pub credentials: u32,
    pub credential_failures: Vec<String>,
    pub missing_paths: Vec<MissingPath>,
    /// 包不覆盖、需要 Agent 恢复后重新推送的外部落点。
    pub pending_push: Vec<String>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

pub(crate) struct ExportRequest {
    pub destination: PathBuf,
    pub passphrase: Option<String>,
    pub include_device_identity: bool,
}

struct Collected {
    included: Vec<(PathBuf, &'static str)>,
    skipped: Vec<SkippedEntry>,
}

pub(crate) fn export(request: &ExportRequest) -> Result<ExportReport, Box<dyn Error>> {
    let home = agent_home();
    if !home.is_dir() {
        return Err(format!("Agent 数据目录不存在: {}", home.display()).into());
    }

    let mut collected = collect(&home, request.include_device_identity)?;
    collected
        .included
        .sort_by(|left, right| left.0.cmp(&right.0));

    let mut payload: Vec<(String, Vec<u8>)> = Vec::new();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut secrets: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut categories: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();

    for (rel, category) in &collected.included {
        let display = display_path(rel);
        let bytes = match fs::read(home.join(rel)) {
            Ok(bytes) => bytes,
            Err(error) => {
                // 单个文件读不到不该让整份备份失败，但必须留下记录。
                collected.skipped.push(SkippedEntry {
                    path: display.clone(),
                    reason: format!("读取失败，已跳过：{error}"),
                    size: 0,
                });
                continue;
            }
        };
        let (stored, credentials) = if display.to_ascii_lowercase().ends_with(".json") {
            rewrite_secrets_out(&bytes, &mut secrets, &mut warnings, &display)?
        } else {
            (bytes, 0)
        };

        let digest = hex_digest(&stored);
        let size = stored.len() as u64;
        let bucket = categories.entry(category).or_insert((0, 0));
        bucket.0 += 1;
        bucket.1 += size;
        entries.push(ManifestEntry {
            path: display.clone(),
            category: (*category).to_string(),
            size,
            sha256: digest,
            credentials,
        });
        payload.push((format!("{PAYLOAD_PREFIX}{display}"), stored));
    }

    let credentials = secrets.len() as u32;
    let sealed = if credentials > 0 {
        let passphrase = request.passphrase.as_deref().unwrap_or_default();
        if passphrase.trim().is_empty() {
            return Err(format!(
                "包里有 {credentials} 段本机凭据，要带走它们必须设置一个口令；也可以先清空凭据再导出"
            )
            .into());
        }
        let plaintext = serde_json::to_vec(&secrets)?;
        Some(backup_crypto::seal(&plaintext, passphrase)?)
    } else {
        None
    };

    let created_at = now_rfc3339();
    let manifest = BackupManifest {
        format: FORMAT_ID.to_string(),
        version: FORMAT_VERSION,
        created_at: created_at.clone(),
        agent_version: crate::VERSION.to_string(),
        profile: crate::store::paths::profile_name(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        machine: machine_label(),
        includes_device_identity: request.include_device_identity,
        credentials_sealed: sealed.is_some(),
        entries: entries.clone(),
        skipped: collected.skipped.clone(),
    };

    if let Some(parent) = request.destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let checksums: String = entries
        .iter()
        .map(|entry| format!("{}  {}\n", entry.sha256, entry.path))
        .collect();

    let write_result = (|| -> Result<(), Box<dyn Error>> {
        let file = fs::File::create(&request.destination)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        archive.start_file(MANIFEST_FILE, options)?;
        archive.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        for (name, bytes) in &payload {
            archive.start_file(name.as_str(), options)?;
            archive.write_all(bytes)?;
        }
        if let Some(sealed) = &sealed {
            archive.start_file(CREDENTIAL_FILE, options)?;
            archive.write_all(sealed)?;
        }
        archive.start_file(CHECKSUM_FILE, options)?;
        archive.write_all(checksums.as_bytes())?;
        archive.finish()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        // 半截的备份包比没有备份更危险：看起来存在，恢复时才发现是坏的。
        let _ = fs::remove_file(&request.destination);
        return Err(error);
    }

    let total_bytes = entries.iter().map(|entry| entry.size).sum();
    Ok(ExportReport {
        path: request.destination.to_string_lossy().to_string(),
        created_at,
        file_count: entries.len(),
        total_bytes,
        credentials,
        includes_device_identity: request.include_device_identity,
        categories: categories
            .into_iter()
            .map(|(category, (files, bytes))| CategorySummary {
                category: category.to_string(),
                files,
                bytes,
            })
            .collect(),
        skipped: collected.skipped,
        warnings,
    })
}

fn collect(home: &Path, include_device_identity: bool) -> Result<Collected, Box<dyn Error>> {
    let mut collected = Collected {
        included: Vec::new(),
        skipped: Vec::new(),
    };
    let mut top: Vec<fs::DirEntry> = fs::read_dir(home)?.collect::<Result<Vec<_>, _>>()?;
    top.sort_by_key(|entry| entry.file_name());

    for entry in top {
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = PathBuf::from(&name);
        let file_type = entry.file_type()?;
        let decision = classify_top_level(&name, file_type.is_dir());
        match decision {
            Some(Decision::Include(category)) => {
                if file_type.is_dir() {
                    walk_dir(&entry.path(), &rel, category, &mut collected)?;
                } else {
                    collected.included.push((rel, category));
                }
            }
            Some(Decision::Skip(reason)) => collected.skipped.push(SkippedEntry {
                path: name,
                reason: reason.to_string(),
                size: entry_size(&entry),
            }),
            None => collected.skipped.push(SkippedEntry {
                path: name,
                reason: "未分类，默认不打包".to_string(),
                size: entry_size(&entry),
            }),
        }
    }

    if !include_device_identity {
        let mut kept = Vec::new();
        for (rel, category) in collected.included {
            if is_device_identity(&rel) {
                collected.skipped.push(SkippedEntry {
                    path: display_path(&rel),
                    reason: "设备身份默认不进包，仅原地重装时可勾选".to_string(),
                    size: fs::metadata(home.join(&rel))
                        .map(|meta| meta.len())
                        .unwrap_or(0),
                });
            } else {
                kept.push((rel, category));
            }
        }
        collected.included = kept;
    }
    collected
        .skipped
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(collected)
}

fn walk_dir(
    dir: &Path,
    rel: &Path,
    category: &'static str,
    collected: &mut Collected,
) -> Result<(), Box<dyn Error>> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let child = rel.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            // 链接指向哪里不在包的保证范围内，跟着它会把包外的内容拖进来。
            collected.skipped.push(SkippedEntry {
                path: display_path(&child),
                reason: "链接条目，不跟随".to_string(),
                size: 0,
            });
            continue;
        }
        match classify_name(&name) {
            Some(Decision::Include(_)) => {
                if file_type.is_dir() {
                    walk_dir(&entry.path(), &child, category, collected)?;
                } else {
                    collected.included.push((child, category));
                }
            }
            Some(Decision::Skip(reason)) => collected.skipped.push(SkippedEntry {
                path: display_path(&child),
                reason: reason.to_string(),
                size: entry_size(&entry),
            }),
            None => collected.skipped.push(SkippedEntry {
                path: display_path(&child),
                reason: "未分类，默认不打包".to_string(),
                size: entry_size(&entry),
            }),
        }
    }
    Ok(())
}

fn entry_size(entry: &fs::DirEntry) -> u64 {
    entry.metadata().map(|meta| meta.len()).unwrap_or(0)
}

fn is_device_identity(rel: &Path) -> bool {
    let display = display_path(rel).to_ascii_lowercase();
    DEVICE_IDENTITY_PATHS.iter().any(|known| *known == display)
}

/// 把 JSON 里能用本机 DPAPI 解开的凭据抽出来，原地换成占位标记。
///
/// 解不开的值保持原样：它在这台机器上都读不出来，说明早就失效了，改写它反而
/// 会掩盖问题。调用方会把这些文件写进报告的警告里。
fn rewrite_secrets_out(
    bytes: &[u8],
    secrets: &mut BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    display: &str,
) -> Result<(Vec<u8>, u32), Box<dyn Error>> {
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        // 不是合法 JSON 就原样收进包：分类已经决定了它属于配置层，这里没有
        // 理由因为解析失败把它丢掉。
        return Ok((bytes.to_vec(), 0));
    };
    let mut unreadable = 0_u32;
    let count = extract_secrets(&mut value, secrets, &mut unreadable);
    if unreadable > 0 {
        warnings.push(format!(
            "{display}: {unreadable} 段凭据在本机无法解密，已原样保留（恢复到别的机器后仍然不可用）"
        ));
    }
    if count == 0 {
        return Ok((bytes.to_vec(), 0));
    }
    Ok((serde_json::to_vec_pretty(&value)?, count))
}

fn extract_secrets(
    value: &mut Value,
    secrets: &mut BTreeMap<String, String>,
    unreadable: &mut u32,
) -> u32 {
    match value {
        Value::String(text) => {
            if !text.starts_with(DPAPI_PREFIX) {
                return 0;
            }
            match crate::store::credentials::unprotect_secret_for_current_user(text) {
                Ok(plaintext) => {
                    let key = secrets.len().to_string();
                    secrets.insert(key.clone(), plaintext);
                    *text = format!("{CREDENTIAL_MARKER}{key}");
                    1
                }
                Err(_) => {
                    *unreadable += 1;
                    0
                }
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| extract_secrets(item, secrets, unreadable))
            .sum(),
        Value::Object(map) => map
            .iter_mut()
            .map(|(_, item)| extract_secrets(item, secrets, unreadable))
            .sum(),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 检视
// ---------------------------------------------------------------------------

struct Package {
    manifest: BackupManifest,
    payload: BTreeMap<String, Vec<u8>>,
    sealed: Option<Vec<u8>>,
}

fn open_package(path: &Path) -> Result<Package, Box<dyn Error>> {
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut manifest_raw = Vec::new();
    archive
        .by_name(MANIFEST_FILE)
        .map_err(|_| format!("这不是 HiMind Agent 备份包：缺少 {MANIFEST_FILE}"))?
        .read_to_end(&mut manifest_raw)?;
    let manifest: BackupManifest = serde_json::from_slice(&manifest_raw)
        .map_err(|error| format!("备份清单无法读取：{error}"))?;
    if manifest.format != FORMAT_ID {
        return Err(format!("不认识的备份格式：{}", manifest.format).into());
    }
    if manifest.version > FORMAT_VERSION {
        return Err(format!(
            "备份包版本 {} 比当前 Agent 支持的 {FORMAT_VERSION} 更新，请先升级 Agent",
            manifest.version
        )
        .into());
    }

    let mut payload: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut sealed: Option<Vec<u8>> = None;
    let names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|entry| entry.name().to_string())
        })
        .collect();
    for name in names {
        if name.ends_with('/') {
            continue;
        }
        if name == CREDENTIAL_FILE {
            let mut bytes = Vec::new();
            archive.by_name(&name)?.read_to_end(&mut bytes)?;
            sealed = Some(bytes);
            continue;
        }
        if let Some(rel) = name.strip_prefix(PAYLOAD_PREFIX) {
            let mut bytes = Vec::new();
            archive.by_name(&name)?.read_to_end(&mut bytes)?;
            payload.insert(rel.to_string(), bytes);
        }
    }
    Ok(Package {
        manifest,
        payload,
        sealed,
    })
}

pub(crate) fn inspect(path: &Path) -> Result<InspectReport, Box<dyn Error>> {
    let package = open_package(path)?;
    let mut warnings = Vec::new();
    let mut codes: BTreeMap<String, u32> = BTreeMap::new();
    let mut credential_files = Vec::new();
    let mut categories: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut total_bytes = 0_u64;

    for entry in &package.manifest.entries {
        total_bytes += entry.size;
        let bucket = categories.entry(entry.category.clone()).or_insert((0, 0));
        bucket.0 += 1;
        bucket.1 += entry.size;
        if entry.credentials > 0 {
            credential_files.push(entry.path.clone());
            codes.insert(entry.path.clone(), entry.credentials);
        }
    }

    // 清单说有多少凭据，就要求包里的占位标记数量一致。不一致说明包被动过，
    // 这时候宁可在检视阶段就说清楚。
    for (name, bytes) in &package.payload {
        let expected = codes.get(name).copied().unwrap_or(0);
        if expected == 0 {
            continue;
        }
        let found = count_markers(bytes);
        if found != expected {
            warnings.push(format!(
                "{name}: 清单记录 {expected} 段凭据，包里实际有 {found} 段"
            ));
        }
    }
    for name in &package.manifest.entries {
        if name.credentials > 0 && !package.payload.contains_key(&name.path) {
            warnings.push(format!("{}: 清单里有记录，包里没有对应文件", name.path));
        }
    }

    let credentials: u32 = package
        .manifest
        .entries
        .iter()
        .map(|entry| entry.credentials)
        .sum();
    if package.sealed.is_some() && credentials == 0 {
        warnings.push("包里带有凭据文件，但没有任何文件引用它".to_string());
    }
    if package.sealed.is_none() && credentials > 0 {
        warnings.push("清单声明含凭据，但缺少凭据文件，恢复后这些凭据将不可用".to_string());
    }

    Ok(InspectReport {
        path: path.to_string_lossy().to_string(),
        format: package.manifest.format.clone(),
        version: package.manifest.version,
        created_at: package.manifest.created_at.clone(),
        machine: package.manifest.machine.clone(),
        agent_version: package.manifest.agent_version.clone(),
        profile: package.manifest.profile.clone(),
        includes_device_identity: package.manifest.includes_device_identity,
        needs_passphrase: package.sealed.is_some(),
        file_count: package.manifest.entries.len(),
        total_bytes,
        credentials,
        credential_files,
        categories: categories
            .into_iter()
            .map(|(category, (files, bytes))| CategorySummary {
                category,
                files,
                bytes,
            })
            .collect(),
        skipped: package.manifest.skipped.clone(),
        warnings,
    })
}

fn count_markers(bytes: &[u8]) -> u32 {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => count_markers_in(&value),
        Err(_) => 0,
    }
}

fn count_markers_in(value: &Value) -> u32 {
    match value {
        Value::String(text) => u32::from(text.starts_with(CREDENTIAL_MARKER)),
        Value::Array(items) => items.iter().map(count_markers_in).sum(),
        Value::Object(map) => map.values().map(count_markers_in).sum(),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 恢复
// ---------------------------------------------------------------------------

pub(crate) fn restore(
    path: &Path,
    passphrase: Option<&str>,
) -> Result<RestoreReport, Box<dyn Error>> {
    let home = agent_home();
    fs::create_dir_all(&home)?;
    let package = open_package(path)?;

    // 1. 先验完整性：包坏了就不要动磁盘上的任何东西。
    for entry in &package.manifest.entries {
        let Some(bytes) = package.payload.get(&entry.path) else {
            return Err(format!("备份包缺少文件：{}", entry.path).into());
        };
        if bytes.len() as u64 != entry.size || hex_digest(bytes) != entry.sha256 {
            return Err(format!("备份包已损坏：校验失败于 {}", entry.path).into());
        }
    }

    // 2. 解凭据。有凭据文件就必须有口令，否则恢复出来的账号是空的。
    let mut secrets: BTreeMap<String, String> = BTreeMap::new();
    if let Some(sealed) = &package.sealed {
        let Some(passphrase) = passphrase.filter(|value| !value.trim().is_empty()) else {
            return Err("这个备份包用口令保护了凭据，请先输入导出时设置的口令".into());
        };
        let plaintext = backup_crypto::open(sealed, passphrase)?;
        secrets = serde_json::from_slice(&plaintext)
            .map_err(|error| format!("凭据内容无法解析：{error}"))?;
    }

    // 3. 快照。目录名带时间戳，恢复失败也能靠它回到恢复前的状态。
    let snapshot = home.join(AUTOMATIC_BACKUP_DIR).join(format!(
        "auto-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let mut restored = Vec::new();
    let mut warnings = Vec::new();
    let mut credential_failures = Vec::new();
    let mut credential_count = 0_u32;
    let mut snapshot_manifest = Vec::new();

    for entry in &package.manifest.entries {
        let Some(rel) = safe_relative(&entry.path) else {
            warnings.push(format!("{}: 路径不合法，已跳过", entry.path));
            continue;
        };
        let target = home.join(&rel);
        let mut bytes = match package.payload.get(&entry.path) {
            Some(bytes) => bytes.clone(),
            None => continue,
        };

        if entry.credentials > 0 {
            let (replaced, failures) = apply_secrets(&mut bytes, &secrets, &entry.path);
            credential_count += replaced;
            credential_failures.extend(failures);
        }

        if target.is_file() {
            let snapshot_path = snapshot.join(PAYLOAD_PREFIX).join(&rel);
            if let Some(parent) = snapshot_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Err(error) = fs::copy(&target, &snapshot_path) {
                warnings.push(format!("{}: 恢复前快照失败（{error}）", entry.path));
            } else {
                snapshot_manifest.push(entry.path.clone());
            }
        }

        crate::store::atomic_file::atomic_write(&target, &bytes)?;
        restored.push(entry.path.clone());
    }

    if !restored.is_empty() {
        // 换机恢复时本机没有旧文件可快照，快照目录还没被创建过。这一步失败会
        // 让已经写好的配置看起来像"恢复失败"，所以目录要显式建出来。
        fs::create_dir_all(&snapshot)?;
        let record = serde_json::json!({
            "created_at": now_rfc3339(),
            "source_backup": path.to_string_lossy(),
            "source_machine": package.manifest.machine,
            "restored": snapshot_manifest,
        });
        fs::write(
            snapshot.join("restore.json"),
            serde_json::to_vec_pretty(&record)?,
        )?;
    }

    let missing_paths = missing_absolute_paths(&package);

    Ok(RestoreReport {
        path: path.to_string_lossy().to_string(),
        snapshot: snapshot.to_string_lossy().to_string(),
        restored,
        credentials: credential_count,
        credential_failures,
        missing_paths,
        pending_push: pending_push(&package),
        warnings,
    })
}

/// 恢复时只接受"干净的相对路径"。带盘符、`..`、UNC 前缀的条目一律不要 ——
/// 备份包是用户从外面拿进来的文件，不能让它决定往哪里写。
fn safe_relative(value: &str) -> Option<PathBuf> {
    let normalized = value.replace('\\', "/");
    let mut path = PathBuf::new();
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(':') {
            return None;
        }
        path.push(segment);
    }
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path)
}

fn apply_secrets(
    bytes: &mut Vec<u8>,
    secrets: &BTreeMap<String, String>,
    display: &str,
) -> (u32, Vec<String>) {
    let mut failures = Vec::new();
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return (0, failures);
    };
    let replaced = replace_markers(&mut value, secrets, &mut failures, display);
    if replaced == 0 {
        return (0, failures);
    }
    match serde_json::to_vec_pretty(&value) {
        Ok(encoded) => {
            *bytes = encoded;
            (replaced, failures)
        }
        Err(error) => {
            failures.push(format!("{display}: 凭据写回失败（{error}）"));
            (0, failures)
        }
    }
}

fn replace_markers(
    value: &mut Value,
    secrets: &BTreeMap<String, String>,
    failures: &mut Vec<String>,
    display: &str,
) -> u32 {
    match value {
        Value::String(text) => {
            let Some(key) = text.strip_prefix(CREDENTIAL_MARKER) else {
                return 0;
            };
            let Some(plaintext) = secrets.get(key) else {
                failures.push(format!("{display}: 包里的凭据 {key} 已在导出后被移除"));
                return 0;
            };
            match crate::store::credentials::protect_secret_for_current_user(plaintext) {
                Ok(protected) => {
                    *text = protected;
                    1
                }
                Err(error) => {
                    failures.push(format!("{display}: 凭据无法重新加密（{error}）"));
                    0
                }
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| replace_markers(item, secrets, failures, display))
            .sum(),
        Value::Object(map) => map
            .iter_mut()
            .map(|(_, item)| replace_markers(item, secrets, failures, display))
            .sum(),
        _ => 0,
    }
}

/// 恢复后指向旧机器的绝对路径。这里只报出来，不改写：路径该指向哪里只有
/// 用户知道，猜一个等于替他做决定。
const PATH_KEYS: &[&str] = &[
    "rendered_root",
    "workspace_root",
    "project_root",
    "catalog_path",
    "unity_editor_path",
    "editor_path",
    "roots",
];

fn missing_absolute_paths(package: &Package) -> Vec<MissingPath> {
    let mut missing = BTreeMap::new();
    for (name, bytes) in &package.payload {
        if !name.to_ascii_lowercase().ends_with(".json") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            continue;
        };
        collect_missing(&value, name, &mut missing);
    }
    missing.into_values().collect()
}

fn collect_missing(value: &Value, source: &str, missing: &mut BTreeMap<String, MissingPath>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if PATH_KEYS.contains(&key.as_str()) {
                    collect_path_values(item, source, missing);
                }
                collect_missing(item, source, missing);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_missing(item, source, missing);
            }
        }
        _ => {}
    }
}

fn collect_path_values(value: &Value, source: &str, missing: &mut BTreeMap<String, MissingPath>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if looks_absolute(text) && !Path::new(text).exists() && !text.contains("..") {
                missing.remove(text);
                missing.insert(
                    text.to_string(),
                    MissingPath {
                        path: text.to_string(),
                        source: source.to_string(),
                    },
                );
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_path_values(item, source, missing);
            }
        }
        _ => {}
    }
}

fn looks_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || value.starts_with("\\\\")
}

/// 包不负责覆盖的外部落点。它们由 Agent 在恢复之后按恢复出来的配置重新推送，
/// 所以这里只做告知，不做写入。
fn pending_push(package: &Package) -> Vec<String> {
    let mut items = Vec::new();
    if let Some(bytes) = package.payload.get("skill-deployments.json") {
        let count = serde_json::from_slice::<Value>(bytes)
            .ok()
            .map(|value| deployment_count(&value))
            .unwrap_or(0);
        if count > 0 {
            items.push(format!("{count} 个已部署技能的目标目录"));
        }
    }
    if package.payload.contains_key("data/himind-ai-mcp.json") {
        items.push("AI 客户端的 MCP 注册".to_string());
    }
    items
}

fn deployment_count(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.len(),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("deployments") {
                return items.len();
            }
            map.len()
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn display_path(rel: &Path) -> String {
    rel.components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn machine_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    match std::env::var("USERNAME") {
        Ok(user) if !user.trim().is_empty() => format!("{host}\\{user}"),
        _ => host,
    }
}

pub(crate) fn min_passphrase_chars() -> usize {
    MIN_PASSPHRASE_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;
    use std::sync::MutexGuard;

    /// 独立的临时 home。返回的 guard 必须活到测试结束：`HIMIND_AGENT_HOME`
    /// 是进程级变量，放开会让并行测试互相搬走对方的目录。
    fn temp_home(label: &str) -> (PathBuf, MutexGuard<'static, ()>) {
        let guard = test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-backup-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        (root, guard)
    }

    fn write_json(path: &Path, value: Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn sample_home(root: &Path) -> String {
        let secret = crate::store::credentials::protect_secret_for_current_user("svn-password-42")
            .expect("DPAPI 只在本机可用");
        write_json(
            &root.join("svn-connections.json"),
            serde_json::json!([
                { "id": "corp", "url": "svn://corp/trunk", "username": "zhang", "password": secret }
            ]),
        );
        write_json(
            &root.join("data").join("agent-preferences.json"),
            serde_json::json!({ "auto_start": true }),
        );
        write_json(
            &root.join("data").join("agent-state.json"),
            serde_json::json!({ "agent_id": "agt-1", "credential": secret }),
        );
        write_json(
            &root.join("data").join("extension-sources.json"),
            serde_json::json!({ "sources": [] }),
        );
        fs::write(root.join("data").join("agent-state.json.lock"), b"").unwrap();
        fs::write(root.join("local-runs.sqlite3"), b"sqlite").unwrap();
        fs::create_dir_all(root.join("plugins").join("demo")).unwrap();
        fs::write(root.join("plugins").join("demo").join("plugin.json"), b"{}").unwrap();
        secret
    }

    #[test]
    fn exports_configuration_and_refuses_to_carry_device_identity_by_default() {
        let (root, _guard) = temp_home("export");
        sample_home(&root);
        let destination = root
            .parent()
            .unwrap()
            .join(format!("himind-backup-export-{}.zip", std::process::id()));
        let report = export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("passphrase-for-tests".to_string()),
            include_device_identity: false,
        })
        .unwrap();

        let paths: Vec<&str> = report
            .skipped
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        assert!(paths.contains(&"data/agent-state.json"), "{paths:?}");
        assert!(paths.contains(&"local-runs.sqlite3"), "{paths:?}");
        assert!(paths.contains(&"plugins"), "{paths:?}");
        assert_eq!(report.credentials, 1, "只有 SVN 密码会被封装");

        let package = open_package(&destination).unwrap();
        assert!(package.payload.contains_key("svn-connections.json"));
        assert!(package.payload.contains_key("data/extension-sources.json"));
        assert!(!package.payload.contains_key("data/agent-state.json"));
        let raw = String::from_utf8(package.payload["svn-connections.json"].clone()).unwrap();
        assert!(raw.contains(CREDENTIAL_MARKER));
        assert!(!raw.contains("dpapi:v1:"));

        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&root);
    }

    /// MCP 目录的缓存和「已确认的未验证来源」不进包：换台机器要重新确认一次，
    /// 不能把上一台机器的信任决定一起带过去。UI 的备份范围照着这份分类渲染。
    #[test]
    fn catalog_cache_and_source_acknowledgements_stay_out_of_the_package() {
        let (root, _guard) = temp_home("catalog-scope");
        sample_home(&root);
        write_json(
            &root.join("data").join("mcp-catalog-sources.json"),
            serde_json::json!({
                "schema_version": 1,
                "sources": [],
                "acknowledged_sources": ["official"]
            }),
        );
        let cache = root.join("data").join("mcp-catalog-cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("official.json"), b"{}").unwrap();

        let destination = root
            .parent()
            .unwrap()
            .join(format!("himind-backup-catalog-{}.zip", std::process::id()));
        let report = export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("passphrase-for-tests".to_string()),
            include_device_identity: false,
        })
        .unwrap();
        let paths: Vec<&str> = report
            .skipped
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        assert!(
            paths.contains(&"data/mcp-catalog-sources.json"),
            "{paths:?}"
        );
        assert!(paths.contains(&"data/mcp-catalog-cache"), "{paths:?}");

        let package = open_package(&destination).unwrap();
        assert!(!package
            .payload
            .contains_key("data/mcp-catalog-sources.json"));
        assert!(package
            .payload
            .keys()
            .all(|key| !key.starts_with("data/mcp-catalog-cache/")));

        // 界面上「不进备份包」那栏直接渲染这份数据，所以确认记录必须带原因出现。
        let scope = scope_entries();
        let entry = scope
            .iter()
            .find(|item| item.name == "data/mcp-catalog-sources.json")
            .expect("备份范围要列出目录来源文件");
        assert!(!entry.included);
        assert!(entry.reason.contains("重新确认"), "{}", entry.reason);

        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_reencrypts_credentials_for_the_current_user_and_snapshots_first() {
        let (root, _guard) = temp_home("restore");
        let secret = sample_home(&root);
        let destination = root
            .parent()
            .unwrap()
            .join(format!("himind-backup-restore-{}.zip", std::process::id()));
        export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("passphrase-for-tests".to_string()),
            include_device_identity: true,
        })
        .unwrap();

        // 恢复前把配置改坏，确认恢复真的覆盖了它。
        write_json(&root.join("svn-connections.json"), serde_json::json!([]));
        let report = restore(&destination, Some("passphrase-for-tests")).unwrap();
        assert!(report
            .restored
            .contains(&"svn-connections.json".to_string()));
        assert_eq!(report.credentials, 2, "SVN 密码与设备凭据都要重新封装");
        assert!(report.credential_failures.is_empty());

        let restored: Value =
            serde_json::from_slice(&fs::read(root.join("svn-connections.json")).unwrap()).unwrap();
        let stored = restored[0]["password"].as_str().unwrap();
        assert_ne!(stored, secret, "恢复后必须重新封装，不是原样写回");
        assert_eq!(
            crate::store::credentials::unprotect_secret_for_current_user(stored).unwrap(),
            "svn-password-42"
        );

        let snapshot = PathBuf::from(&report.snapshot);
        assert!(snapshot.is_dir());
        assert!(
            snapshot.join("restore.json").is_file(),
            "快照要留下这次恢复的来源"
        );

        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(snapshot.parent().unwrap());
    }

    #[test]
    fn a_tampered_payload_is_rejected_before_anything_is_written() {
        let (root, _guard) = temp_home("tamper");
        sample_home(&root);
        let destination = root
            .parent()
            .unwrap()
            .join(format!("himind-backup-tamper-{}.zip", std::process::id()));
        export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("passphrase-for-tests".to_string()),
            include_device_identity: false,
        })
        .unwrap();
        rewrite_zip_entry(
            &destination,
            "payload/data/extension-sources.json",
            br#"{"sources":["tampered"]}"#,
        );

        write_json(
            &root.join("data").join("extension-sources.json"),
            serde_json::json!({"sources": ["original"]}),
        );
        let error = restore(&destination, Some("passphrase-for-tests")).unwrap_err();
        assert!(error.to_string().contains("损坏"), "{error}");
        let untouched: Value = serde_json::from_slice(
            &fs::read(root.join("data").join("extension-sources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(untouched["sources"][0], "original");
        assert!(!root.join("backups").exists(), "校验失败时不应该产生快照");

        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_wrong_passphrase_stops_the_restore() {
        let (root, _guard) = temp_home("passphrase");
        sample_home(&root);
        let destination = root.parent().unwrap().join(format!(
            "himind-backup-passphrase-{}.zip",
            std::process::id()
        ));
        export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("passphrase-for-tests".to_string()),
            include_device_identity: false,
        })
        .unwrap();
        let error = restore(&destination, Some("passphrase-for-testz")).unwrap_err();
        assert!(error.to_string().contains("口令"), "{error}");
        let error = restore(&destination, None).unwrap_err();
        assert!(error.to_string().contains("口令"), "{error}");
        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn credentials_without_a_passphrase_are_refused() {
        let (root, _guard) = temp_home("no-passphrase");
        sample_home(&root);
        let destination = root
            .parent()
            .unwrap()
            .join(format!("himind-backup-nopass-{}.zip", std::process::id()));
        let error = export(&ExportRequest {
            destination,
            passphrase: None,
            include_device_identity: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("口令"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 导出 → 检视 → 换机恢复。单点用例覆盖不到"包里的清单和恢复出来的东西
    /// 对不上"这类偏差，所以整条链路走一遍，答案以磁盘为准。
    #[test]
    fn export_inspect_and_restore_form_a_complete_round_trip() {
        let (source, _guard) = temp_home("roundtrip-source");
        sample_home(&source);
        let destination = source.parent().unwrap().join(format!(
            "himind-backup-roundtrip-{}.zip",
            std::process::id()
        ));
        let exported = export(&ExportRequest {
            destination: destination.clone(),
            passphrase: Some("round-trip-passphrase".to_string()),
            include_device_identity: false,
        })
        .unwrap();

        let inspected = inspect(&destination).unwrap();
        assert_eq!(inspected.format, FORMAT_ID);
        assert_eq!(inspected.version, FORMAT_VERSION);
        assert_eq!(inspected.file_count, exported.file_count);
        assert_eq!(inspected.credentials, exported.credentials);
        assert_eq!(inspected.total_bytes, exported.total_bytes);
        assert!(inspected.needs_passphrase, "带凭据的包必须声明需要口令");
        assert!(!inspected.includes_device_identity);
        assert!(inspected.warnings.is_empty(), "{:?}", inspected.warnings);
        assert!(!inspected.machine.trim().is_empty());
        assert!(!inspected.created_at.trim().is_empty());
        assert_eq!(
            inspected.credential_files,
            vec!["svn-connections.json".to_string()]
        );
        let categories: Vec<&str> = inspected
            .categories
            .iter()
            .map(|item| item.category.as_str())
            .collect();
        assert!(categories.contains(&"本机配置"), "{categories:?}");
        assert!(categories.contains(&"配置与状态"), "{categories:?}");

        // 换一台机器：空的 home，只有这个包。恢复后落点必须和清单一致。
        let target = source.parent().unwrap().join(format!(
            "himind-backup-roundtrip-target-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(&target).unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &target);

        let restored = restore(&destination, Some("round-trip-passphrase")).unwrap();
        assert_eq!(restored.restored.len(), inspected.file_count);
        assert_eq!(restored.credentials, inspected.credentials);
        assert!(
            restored.credential_failures.is_empty(),
            "{:?}",
            restored.credential_failures
        );
        assert!(
            !target.join("data").join("agent-state.json").exists(),
            "设备身份默认不落地"
        );
        assert!(target.join("data").join("extension-sources.json").is_file());
        assert!(
            !target.join("plugins").exists(),
            "拓展内容由重装负责，不进包也不恢复"
        );

        let stored_svn: Value =
            serde_json::from_slice(&fs::read(target.join("svn-connections.json")).unwrap())
                .unwrap();
        let stored = stored_svn[0]["password"].as_str().unwrap();
        assert!(
            stored.starts_with(crate::store::credentials::DPAPI_PREFIX),
            "{stored}"
        );
        assert_eq!(
            crate::store::credentials::unprotect_secret_for_current_user(stored).unwrap(),
            "svn-password-42"
        );

        let snapshot = PathBuf::from(&restored.snapshot);
        let record: Value =
            serde_json::from_slice(&fs::read(snapshot.join("restore.json")).unwrap()).unwrap();
        assert_eq!(
            record["source_backup"],
            destination.to_string_lossy().as_ref()
        );

        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&target);
    }

    /// 每个写进 Agent home 的落点都必须有明确归属。新增一处存储却忘了同步分类器
    /// 时，这条用例会失败，而不是等到用户导出备份才发现少了东西。
    #[test]
    fn every_agent_home_storage_path_is_classified() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut unclassified = Vec::new();
        for path in source_files(&root) {
            let text = fs::read_to_string(&path).unwrap_or_default();
            for name in join_literals(&text) {
                if classify_storage_name(&name).is_none() {
                    unclassified.push(format!("{}: {name}", path.display()));
                }
            }
        }
        unclassified.sort();
        unclassified.dedup();
        assert!(
            unclassified.is_empty(),
            "这些存储落点没有登记到备份分类器：\n{}",
            unclassified.join("\n")
        );
    }

    fn source_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map(|value| value == "rs").unwrap_or(false) {
                    files.push(path);
                }
            }
        }
        files
    }

    /// 从 `agent_home().join("...")` 这类写法里取出落点名字。只认紧跟着
    /// `agent_home()` 的 `join`，其它 `join`（拼 URL、拼日志行）一律不看。
    fn join_literals(text: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut cursor = text;
        while let Some(index) = cursor.find("agent_home()") {
            cursor = &cursor[index + "agent_home()".len()..];
            let mut rest = cursor.trim_start();
            while let Some(stripped) = rest.strip_prefix(".join(") {
                rest = stripped.trim_start();
                let Some(stripped) = rest.strip_prefix('"') else {
                    break;
                };
                match stripped.find('"') {
                    Some(end) => {
                        names.push(stripped[..end].to_string());
                        rest = &stripped[end + 1..];
                    }
                    None => break,
                }
                rest = rest.trim_start();
            }
        }
        names
    }

    /// 落点名字 → 归属。带 `/` 的按第一段判断，文件按文件名规则判断。
    fn classify_storage_name(name: &str) -> Option<Decision> {
        let (head, tail) = match name.split_once('/') {
            Some((head, tail)) => (head, Some(tail)),
            None => (name, None),
        };
        if let Some(tail) = tail {
            let decision = classify_path(Path::new(name))?;
            let _ = tail;
            return Some(decision);
        }
        if matches!(name, "resource_dir" | "app_dir") {
            // 程序自身目录，不属于 Agent home 的分类范围。
            return Some(Decision::Skip("程序目录"));
        }
        classify_top_level(head, false).or_else(|| classify_name(head))
    }

    fn rewrite_zip_entry(path: &Path, target: &str, replacement: &[u8]) {
        let file = fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            entries.push((name, bytes));
        }
        drop(archive);

        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options =
                FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, bytes) in &entries {
                writer.start_file(name.as_str(), options).unwrap();
                if name == target {
                    writer.write_all(replacement).unwrap();
                } else {
                    writer.write_all(bytes).unwrap();
                }
            }
            writer.finish().unwrap();
        }
        fs::write(path, buffer.into_inner()).unwrap();
    }
}
