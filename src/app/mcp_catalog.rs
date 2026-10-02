//! MCP 目录：能浏览、能安装的 `server.json`。
//!
//! 四条既定决策（docs/adr/0003-0006）在这里落地：
//!
//! - **0003**：目录条目就是 `server.json` 本身，不另立字段。自己的信息只进
//!   `_meta`；解析按 `$schema` 版本分支；条目坏了只让这一条不可安装，不清空列表。
//! - **0004**：客户端只认「目录源」一个概念。默认源是官方 registry（内网部署把
//!   URL 换成自建实例即可，见 `data/mcp-catalog-sources.json`）。
//! - **0005**：增量同步 + 本地快照，复用拓展源同一套 atomic_write + `.bak` 写法；
//!   目录只服务于「搜索」「安装」，已装的服务启动时完全不查目录。
//! - **0006**：信任属于来源；安装只 append 到 `himind-ai-mcp.json`；密钥走既有的
//!   `env`/`headers` DPAPI 通道，未验证来源安装后默认不启用。
//!
//! 内置的精选条目（`mcp_catalog_seed.json`）永远在列表里，所以断网、未同步过目录
//! 的机器打开界面仍然有可用的第一步。

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::mcp_registry::{self, McpServerConfig};
use crate::store::{atomic_file, paths};

/// 快照结构版本。版本落后就丢掉重建，不做原地迁移（ADR 0005）。
const SNAPSHOT_FORMAT_VERSION: u32 = 1;
const SOURCE_FILE_FORMAT_VERSION: u32 = 1;
/// 官方 MCP registry。内网部署用 `data/mcp-catalog-sources.json` 指向自建实例。
const DEFAULT_REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io/v0/servers";
const PAGE_SIZE: usize = 100;
/// 单次同步的翻页上限。目录是海量公开数据，宁可这一轮少拿一点，也不让刷新卡住界面。
const MAX_PAGES: usize = 20;
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const BUILTIN_SOURCE_ID: &str = "builtin";
const SEED: &str = include_str!("mcp_catalog_seed.json");

/// 同进程内串行化刷新，避免多窗口同时点刷新时对同一快照反复写盘。
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 来源
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogTrust {
    /// 随 Agent 一起发布的精选条目。
    Curated,
    /// 需要过签名验证的内网源。
    Verified,
    /// 公共源：标明来源，安装前确认一次，装完默认不启用。
    Unverified,
}

impl CatalogTrust {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "curated" | "builtin" => Self::Curated,
            "verified" => Self::Verified,
            _ => Self::Unverified,
        }
    }

    fn requires_acknowledgement(self) -> bool {
        matches!(self, Self::Unverified)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogSourceKind {
    /// 随 Agent 发布的条目，不联网。
    Builtin,
    /// 官方 registry 及其自建实例：`{servers:[{server}], metadata:{nextCursor}}` + `updated_since`。
    Registry,
    /// 普通 HTTP 目录：一个数组，或 `{servers:[...]}` / `{entries:[...]}` 包裹。
    Http,
}

impl CatalogSourceKind {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "builtin" => Self::Builtin,
            "http" => Self::Http,
            _ => Self::Registry,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CatalogSource {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_registry_kind")]
    pub kind: CatalogSourceKind,
    #[serde(default = "default_unverified")]
    pub trust: CatalogTrust,
}

fn default_true() -> bool {
    true
}

fn default_registry_kind() -> CatalogSourceKind {
    CatalogSourceKind::Registry
}

fn default_unverified() -> CatalogTrust {
    CatalogTrust::Unverified
}

impl CatalogSource {
    fn builtin() -> Self {
        Self {
            id: BUILTIN_SOURCE_ID.to_string(),
            label: "内置精选".to_string(),
            url: String::new(),
            enabled: true,
            kind: CatalogSourceKind::Builtin,
            trust: CatalogTrust::Curated,
        }
    }

    /// `HIMIND_MCP_CATALOG_URL` 让自建实例不必改配置文件；没有就用官方 registry。
    fn default_registry() -> Self {
        let url = std::env::var("HIMIND_MCP_CATALOG_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());
        Self {
            id: "official".to_string(),
            label: "MCP 目录".to_string(),
            url,
            enabled: true,
            // 自建实例按官方 registry 协议部署，所以换 URL 不改类型。
            kind: CatalogSourceKind::Registry,
            trust: CatalogTrust::Unverified,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CatalogSourceFile {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    sources: Vec<CatalogSource>,
    /// 确认过的来源（ADR 0006：按来源记一次，不按条目）。
    #[serde(default)]
    acknowledged_sources: Vec<String>,
}

fn source_file_path() -> PathBuf {
    paths::agent_home().join("data/mcp-catalog-sources.json")
}

fn load_source_file() -> CatalogSourceFile {
    let path = source_file_path();
    if path.is_file() {
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(document) = serde_json::from_slice::<CatalogSourceFile>(&bytes) {
                if document.schema_version == SOURCE_FILE_FORMAT_VERSION {
                    return document;
                }
            }
        }
        crate::app::crash::record_event("warn", "MCP 目录来源配置无法解析，已回退到默认来源");
    }
    CatalogSourceFile {
        schema_version: SOURCE_FILE_FORMAT_VERSION,
        sources: vec![CatalogSource::default_registry()],
        acknowledged_sources: Vec::new(),
    }
}

fn save_source_file(document: &CatalogSourceFile) -> Result<(), Box<dyn Error>> {
    let path = source_file_path();
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(document)?)?;
    Ok(())
}

/// 来源列表：内置精选永远在，其余来自配置（默认一条官方 registry）。
fn sources() -> Vec<CatalogSource> {
    let mut list = vec![CatalogSource::builtin()];
    list.extend(
        load_source_file()
            .sources
            .into_iter()
            .filter(|source| !source.id.trim().is_empty() && source.id != BUILTIN_SOURCE_ID),
    );
    list
}

fn acknowledged(source_id: &str) -> bool {
    load_source_file()
        .acknowledged_sources
        .iter()
        .any(|item| item == source_id)
}

fn acknowledge_source(source_id: &str) {
    let mut document = load_source_file();
    if document
        .acknowledged_sources
        .iter()
        .any(|item| item == source_id)
    {
        return;
    }
    document.acknowledged_sources.push(source_id.to_string());
    if let Err(error) = save_source_file(&document) {
        crate::app::crash::record_event(
            "warn",
            &format!("MCP 目录来源确认记录写入失败: {source_id} ({error})"),
        );
    }
}

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CatalogSnapshot {
    format_version: u32,
    source_id: String,
    /// 分页游标：翻页写盘成功后才推进（ADR 0005）。
    #[serde(default)]
    cursor: String,
    /// 增量游标：本轮见过的最新条目更新时间，下一轮作为 `updated_since` 发出去。
    #[serde(default)]
    updated_since: String,
    #[serde(default)]
    fetched_at: u64,
    /// 原样的 `server.json` 文档（ADR 0003：条目就是 server.json）。
    #[serde(default)]
    entries: Vec<Value>,
}

impl CatalogSnapshot {
    fn empty(source_id: &str) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            source_id: source_id.to_string(),
            cursor: String::new(),
            updated_since: String::new(),
            fetched_at: 0,
            entries: Vec::new(),
        }
    }
}

fn cache_dir() -> PathBuf {
    paths::agent_home().join("data/mcp-catalog-cache")
}

fn cache_path(source_id: &str) -> PathBuf {
    cache_dir().join(format!("{source_id}.json"))
}

/// 读快照。格式版本落后等同于没有快照，下一次刷新走全量（ADR 0005）。
fn load_snapshot(source_id: &str) -> Option<CatalogSnapshot> {
    let path = cache_path(source_id);
    if !path.is_file() {
        return None;
    }
    match serde_json::from_slice::<CatalogSnapshot>(&fs::read(&path).ok()?) {
        Ok(snapshot) if snapshot.format_version == SNAPSHOT_FORMAT_VERSION => Some(snapshot),
        _ => None,
    }
}

fn save_snapshot(snapshot: &CatalogSnapshot) -> Result<(), Box<dyn Error>> {
    let path = cache_path(&snapshot.source_id);
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(snapshot)?)?;
    Ok(())
}

/// 用新取到的一页更新快照：同名条目替换，其余追加，然后把这一页落盘。
fn merge_page(snapshot: &mut CatalogSnapshot, entries: Vec<Value>) {
    for entry in entries {
        let name = entry_name(&entry);
        if name.is_empty() {
            snapshot.entries.push(entry);
            continue;
        }
        match snapshot
            .entries
            .iter_mut()
            .find(|existing| entry_name(existing) == name)
        {
            Some(existing) => *existing = entry,
            None => snapshot.entries.push(entry),
        }
    }
}

// ---------------------------------------------------------------------------
// server.json 解析（宽容，按版本分支）
// ---------------------------------------------------------------------------

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn entry_name(document: &Value) -> String {
    text(document, "name")
}

fn schema_version(document: &Value) -> String {
    let raw = text(document, "$schema");
    raw.rsplit('/')
        .nth(1)
        .filter(|segment| segment.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(&raw)
        .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgumentKind {
    Positional,
    Named,
}

#[derive(Clone, Debug)]
struct ParsedArgument {
    key: String,
    kind: ArgumentKind,
    /// 写死在条目里的值；空表示要用户或模板提供。
    value: String,
    required: bool,
    placeholder: String,
    description: String,
    /// 模板变量：`{key}` 占位，值由用户填。
    variables: Vec<ParsedInput>,
}

#[derive(Clone, Debug)]
struct ParsedInput {
    /// 用户填值时的键：`env:API_KEY` / `header:Authorization` / `arg:directory` / `var:token`。
    key: String,
    label: String,
    kind: &'static str,
    required: bool,
    secret: bool,
    description: String,
    placeholder: String,
    value: String,
}

#[derive(Clone, Debug)]
struct ParsedPackage {
    registry_type: String,
    identifier: String,
    version: String,
    runtime_hint: String,
    transport: String,
    runtime_arguments: Vec<ParsedArgument>,
    package_arguments: Vec<ParsedArgument>,
    environment: Vec<ParsedInput>,
}

#[derive(Clone, Debug)]
struct ParsedRemote {
    transport: String,
    url: String,
    headers: Vec<ParsedInput>,
    variables: Vec<ParsedInput>,
}

#[derive(Clone, Debug, Default)]
struct ParsedEntry {
    id: String,
    title: String,
    description: String,
    version: String,
    schema: String,
    repository: String,
    website_url: String,
    runtime_label: String,
    packages: Vec<ParsedPackage>,
    remotes: Vec<ParsedRemote>,
}

fn parse_inputs(values: &[Value], kind: &'static str) -> Vec<ParsedInput> {
    values
        .iter()
        .filter_map(|item| {
            let name = text(item, "name");
            if name.is_empty() {
                return None;
            }
            let secret = flag(item, "isSecret");
            Some(ParsedInput {
                key: format!("{kind}:{name}"),
                label: name.clone(),
                kind,
                // 声明了 `isRequired` 或 `isSecret` 就必须由用户提供；密钥没有默认值一说。
                required: flag(item, "isRequired") || secret,
                secret,
                description: text(item, "description"),
                placeholder: text(item, "placeholder"),
                value: if secret {
                    String::new()
                } else {
                    text(item, "value")
                },
            })
        })
        .collect()
}

fn parse_variables(value: &Value, kind: &'static str) -> Vec<ParsedInput> {
    let Some(map) = value.get("variables").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .map(|(name, item)| ParsedInput {
            key: format!("{kind}:{name}"),
            label: name.clone(),
            kind,
            required: flag(item, "isRequired"),
            secret: false,
            description: text(item, "description"),
            placeholder: text(item, "placeholder"),
            value: text(item, "default"),
        })
        .collect()
}

fn parse_argument(value: &Value, index: usize) -> ParsedArgument {
    let named = text(value, "type").eq_ignore_ascii_case("named");
    let name = text(value, "name");
    let hint = text(value, "valueHint");
    let key = if named {
        name.trim_start_matches('-').to_string()
    } else if !hint.is_empty() {
        hint
    } else {
        format!("arg{index}")
    };
    let required = flag(value, "isRequired");
    ParsedArgument {
        key,
        kind: if named {
            ArgumentKind::Named
        } else {
            ArgumentKind::Positional
        },
        value: text(value, "value"),
        required,
        placeholder: text(value, "placeholder"),
        description: text(value, "description"),
        variables: parse_variables(value, "var"),
    }
}

fn parse_arguments(values: &[Value]) -> Vec<ParsedArgument> {
    values
        .iter()
        .enumerate()
        .map(|(index, item)| parse_argument(item, index))
        .collect()
}

/// 按 `$schema` 版本分支。未知版本不报错，按最新已知结构解析，坏字段降级成空值。
fn parse_entry(document: &Value) -> Result<ParsedEntry, String> {
    if !document.is_object() {
        return Err("目录条目不是 JSON 对象".to_string());
    }
    let schema = schema_version(document);
    if schema.is_empty() {
        return Err("目录条目缺少 $schema，无法判断格式版本".to_string());
    }
    let id = entry_name(document);
    if id.is_empty() {
        return Err("目录条目缺少 name".to_string());
    }
    let description = text(document, "description");
    let version = text(document, "version");
    let meta = document
        .get("_meta")
        .and_then(|value| value.get("io.himind.catalog"));
    let parsed = ParsedEntry {
        id: id.clone(),
        title: {
            let title = text(document, "title");
            if title.is_empty() {
                id.clone()
            } else {
                title
            }
        },
        description: if description.is_empty() {
            "目录没有提供说明。".to_string()
        } else {
            description
        },
        version: if version.is_empty() {
            "未标注版本".to_string()
        } else {
            version
        },
        schema,
        repository: document
            .get("repository")
            .map(|value| text(value, "url"))
            .unwrap_or_default(),
        website_url: text(document, "websiteUrl"),
        runtime_label: meta
            .map(|value| text(value, "runtimeLabel"))
            .unwrap_or_default(),
        packages: list(document, "packages")
            .iter()
            .map(|package| ParsedPackage {
                registry_type: text(package, "registryType"),
                identifier: text(package, "identifier"),
                version: text(package, "version"),
                runtime_hint: text(package, "runtimeHint"),
                transport: {
                    let transport = package.get("transport").map(|value| text(value, "type"));
                    transport
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "stdio".to_string())
                },
                runtime_arguments: parse_arguments(list(package, "runtimeArguments")),
                package_arguments: parse_arguments(list(package, "packageArguments")),
                environment: parse_inputs(list(package, "environmentVariables"), "env"),
            })
            .collect(),
        remotes: list(document, "remotes")
            .iter()
            .map(|remote| ParsedRemote {
                transport: text(remote, "type"),
                url: text(remote, "url"),
                headers: parse_inputs(list(remote, "headers"), "header"),
                variables: parse_variables(remote, "var"),
            })
            .collect(),
    };
    Ok(parsed)
}

/// 运行时命令：`runtimeHint` 优先，其次按包类型给默认值。
fn runtime_command(package: &ParsedPackage) -> Option<&'static str> {
    if !package.runtime_hint.is_empty() {
        return match package.runtime_hint.as_str() {
            "npx" => Some("npx"),
            "uvx" => Some("uvx"),
            "docker" => Some("docker"),
            _ => None,
        };
    }
    match package.registry_type.as_str() {
        "npm" => Some("npx"),
        "pypi" => Some("uvx"),
        _ => None,
    }
}

/// npm 包规格：`identifier` + `@version`。`@scope/name` 里的 `@` 是作用域不是
/// 版本号，所以只在「最后一段里已经有 `@`」时才认为版本已经写死。
fn package_spec(package: &ParsedPackage) -> String {
    let mut spec = package.identifier.clone();
    if !package.version.is_empty() && !package_version_pinned(&spec) {
        spec.push('@');
        spec.push_str(&package.version);
    }
    spec
}

fn package_version_pinned(spec: &str) -> bool {
    spec.rsplit('/').next().unwrap_or(spec).contains('@')
}

/// 把版本并进条目已经写好的包名位置。
///
/// 多数条目把包名留给我们补（`npx -y <包名>@<版本>`），但也有条目在
/// `runtimeArguments` 里自己写了包名：`--package @clize/inbox clize-mcp`、
/// `-p @circulara/plugin circulara-mcp`。这两种形态下包名只能出现在一个位置，
/// 版本必须并进那一项；再补一份就是传给服务的多余参数，钉版本静默失效
/// （真机验证过：条目写 0.35.5，npx 装的是最新的 0.36.0）。
///
/// 命中返回 true，表示包名位置由条目给出；返回 false 时由调用方补包规格。
fn pin_package_argument(args: &mut [String], package: &ParsedPackage) -> bool {
    let identifier = package.identifier.trim();
    if identifier.is_empty() {
        return false;
    }
    for value in args.iter_mut() {
        let candidate = value.trim();
        // `@scope/name` 和 `@scope/name@1.2.3` 都算同一个包；裸名字同理。
        let matches_identifier = candidate == identifier
            || (candidate.starts_with(identifier)
                && candidate[identifier.len()..].starts_with('@'));
        if matches_identifier {
            *value = package_spec(package);
            return true;
        }
    }
    false
}

/// 一条条目「能不能装、装成什么」。装不了的必须给出人话原因，而不是从列表里消失。
#[derive(Clone, Debug)]
enum InstallPlan {
    Package {
        package: ParsedPackage,
        runtime: &'static str,
    },
    Remote {
        remote: ParsedRemote,
    },
}

fn install_plan(entry: &ParsedEntry) -> Result<InstallPlan, String> {
    let unsupported: Vec<String> = entry
        .packages
        .iter()
        .filter(|package| runtime_command(package).is_none() || package.identifier.is_empty())
        .map(|package| package.registry_type.clone())
        .filter(|value| !value.is_empty())
        .collect();
    for package in &entry.packages {
        if package.identifier.is_empty() {
            continue;
        }
        if let Some(runtime) = runtime_command(package) {
            return Ok(InstallPlan::Package {
                package: package.clone(),
                runtime,
            });
        }
    }
    for remote in &entry.remotes {
        if remote.transport.eq_ignore_ascii_case("streamable-http") && !remote.url.is_empty() {
            return Ok(InstallPlan::Remote {
                remote: remote.clone(),
            });
        }
    }
    if entry
        .remotes
        .iter()
        .any(|remote| remote.transport.eq_ignore_ascii_case("sse"))
    {
        return Err("这个工具只提供 SSE 连接，当前版本还不支持".to_string());
    }
    if !unsupported.is_empty() {
        return Err(format!(
            "暂不支持 {} 类型的安装包，可以手动添加",
            unsupported.join(" / ")
        ));
    }
    Err("目录条目没有可用的安装方式".to_string())
}

// ---------------------------------------------------------------------------
// 安装映射（条目 → McpServerConfig）
// ---------------------------------------------------------------------------

/// 解析参数值：条目里写死的用写的，其余从用户填的值里取，模板变量做 `{key}` 替换。
fn resolve_argument(
    argument: &ParsedArgument,
    values: &BTreeMap<String, String>,
) -> Result<Option<String>, String> {
    let mut template = argument.value.clone();
    for variable in &argument.variables {
        let supplied = values
            .get(&variable.key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| (!variable.value.is_empty()).then(|| variable.value.clone()));
        match supplied {
            Some(value) => template = template.replace(&format!("{{{}}}", variable.label), &value),
            None if variable.required => {
                return Err(format!("缺少必填参数 {}", variable.label));
            }
            None => {}
        }
    }
    if !template.trim().is_empty() {
        return Ok(Some(template));
    }
    let key = format!("arg:{}", argument.key);
    let supplied = values
        .get(&key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match supplied {
        Some(value) => Ok(Some(value)),
        None if argument.required => Err(format!(
            "缺少必填参数「{}」",
            if argument.placeholder.is_empty() {
                argument.key.clone()
            } else {
                argument.placeholder.clone()
            }
        )),
        None => Ok(None),
    }
}

fn resolve_arguments(
    arguments: &[ParsedArgument],
    values: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let mut resolved = Vec::new();
    for argument in arguments {
        let Some(value) = resolve_argument(argument, values)? else {
            continue;
        };
        match argument.kind {
            ArgumentKind::Named => {
                resolved.push(format!("--{}", argument.key));
                // 纯开关（条目里没写 value，用户也没填）只输出名字。
                if !value.trim().is_empty() && !value.starts_with("--") {
                    resolved.push(value);
                }
            }
            ArgumentKind::Positional => resolved.push(value),
        }
    }
    Ok(resolved)
}

/// 从条目派生的服务 ID。后端只认 ASCII 字母数字与 `_`、`-`，长度上限 32。
fn slug(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            out.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn suggested_name(entry_id: &str, taken: &BTreeSet<String>) -> String {
    let tail = entry_id.rsplit('/').next().unwrap_or(entry_id);
    let mut base = slug(tail);
    if base.is_empty() {
        base = "mcp-server".to_string();
    }
    base.truncate(32);
    if matches!(base.as_str(), "himind" | "himind-agent") {
        base.push_str("-tool");
    }
    let mut candidate = base.clone();
    let mut index = 2;
    while taken.contains(&candidate.to_ascii_lowercase()) {
        let suffix = format!("-{index}");
        let mut stem = base.clone();
        stem.truncate(32 - suffix.len());
        candidate = format!("{stem}{suffix}");
        index += 1;
    }
    candidate
}

fn resolve_input_value(value: &ParsedInput, values: &BTreeMap<String, String>) -> Option<String> {
    values
        .get(&value.key)
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .or_else(|| {
            (!value.secret && !value.value.is_empty()).then(|| value.value.trim().to_string())
        })
}

fn resolve_inputs(
    values: &[ParsedInput],
    supplied: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for value in values {
        match resolve_input_value(value, supplied) {
            Some(resolved) => {
                out.insert(value.label.clone(), resolved);
            }
            None if value.required => {
                return Err(format!("缺少必填项「{}」", value.label));
            }
            None => {}
        }
    }
    Ok(out)
}

/// 条目 → 服务配置。这是唯一一条安装路径：装完仍然落在 `himind-ai-mcp.json`。
fn build_config(
    entry: &ParsedEntry,
    plan: &InstallPlan,
    values: &BTreeMap<String, String>,
    server_name: &str,
    display_name: &str,
    enabled: bool,
) -> Result<McpServerConfig, String> {
    let mut config = McpServerConfig {
        server_name: server_name.to_string(),
        display_name: display_name.to_string(),
        transport: "stdio".to_string(),
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: String::new(),
        url: String::new(),
        headers: BTreeMap::new(),
        tool_call_timeout_ms: 30_000,
        fail_on_startup_error: false,
        reconnect: true,
        enabled,
    };
    match plan {
        InstallPlan::Package { package, runtime } => {
            config.command = (*runtime).to_string();
            let mut args = resolve_arguments(&package.runtime_arguments, values)?;
            if *runtime == "docker" {
                return Err("Docker 类型的安装包需要手动添加连接".to_string());
            }
            // 条目自己写了包名就把版本并进去，没写才补一份（两种形态见
            // `pin_package_argument` 的说明）。
            if !pin_package_argument(&mut args, package) {
                args.push(package_spec(package));
            }
            args.extend(resolve_arguments(&package.package_arguments, values)?);
            config.args = args;
            config.env = resolve_inputs(&package.environment, values)?;
        }
        InstallPlan::Remote { remote } => {
            let mut url = remote.url.clone();
            for variable in &remote.variables {
                match resolve_input_value(variable, values) {
                    Some(value) => url = url.replace(&format!("{{{}}}", variable.label), &value),
                    None if variable.required => {
                        return Err(format!("缺少必填项「{}」", variable.label));
                    }
                    None => {}
                }
            }
            config.transport = "streamable-http".to_string();
            config.url = url;
            config.headers = resolve_inputs(&remote.headers, values)?;
        }
    }
    if config.display_name.trim().is_empty() {
        config.display_name = entry.title.clone();
    }
    mcp_registry::validate_config(&config)?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// 对外视图
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CatalogInputView {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
    pub secret: bool,
    pub description: String,
    pub placeholder: String,
    pub value: String,
    /// 参数类输入用「选择目录」，环境变量与请求头用文本框。
    pub picker: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CatalogEntryView {
    pub id: String,
    pub source_id: String,
    pub source_label: String,
    pub trust: CatalogTrust,
    pub title: String,
    pub description: String,
    pub version: String,
    pub schema: String,
    pub repository: String,
    pub website_url: String,
    pub transport: String,
    pub runtime: String,
    pub runtime_label: String,
    pub command_preview: String,
    pub installable: bool,
    pub reason: String,
    pub installed_as: String,
    pub suggested_name: String,
    pub inputs: Vec<CatalogInputView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CatalogSourceView {
    pub id: String,
    pub label: String,
    pub trust: CatalogTrust,
    pub url: String,
    pub count: usize,
    pub fetched_at: u64,
    pub acknowledged: bool,
    pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CatalogView {
    pub entries: Vec<CatalogEntryView>,
    pub sources: Vec<CatalogSourceView>,
    pub fetched_at: u64,
    pub stale: bool,
}

fn input_view(value: &ParsedInput) -> CatalogInputView {
    CatalogInputView {
        key: value.key.clone(),
        label: value.label.clone(),
        kind: value.kind.to_string(),
        required: value.required,
        secret: value.secret,
        description: value.description.clone(),
        placeholder: value.placeholder.clone(),
        value: value.value.clone(),
        picker: if value.kind == "arg" {
            "directory".to_string()
        } else {
            String::new()
        },
    }
}

fn entry_inputs(plan: &InstallPlan) -> Vec<CatalogInputView> {
    let mut inputs = Vec::new();
    match plan {
        InstallPlan::Package { package, .. } => {
            for argument in package
                .runtime_arguments
                .iter()
                .chain(package.package_arguments.iter())
            {
                for variable in &argument.variables {
                    inputs.push(input_view(variable));
                }
                if argument.value.trim().is_empty() && argument.required {
                    inputs.push(CatalogInputView {
                        key: format!("arg:{}", argument.key),
                        label: if argument.placeholder.is_empty() {
                            argument.key.clone()
                        } else {
                            argument.placeholder.clone()
                        },
                        kind: "arg".to_string(),
                        required: true,
                        secret: false,
                        description: argument.description.clone(),
                        placeholder: argument.placeholder.clone(),
                        value: String::new(),
                        picker: "directory".to_string(),
                    });
                }
            }
            inputs.extend(package.environment.iter().map(input_view));
        }
        InstallPlan::Remote { remote } => {
            inputs.extend(remote.variables.iter().map(input_view));
            inputs.extend(remote.headers.iter().map(input_view));
        }
    }
    inputs
}

fn command_preview(plan: &InstallPlan) -> (String, String, String) {
    match plan {
        InstallPlan::Package { package, runtime } => {
            let mut parts = vec![(*runtime).to_string()];
            // 预览要跟真装出来的命令同一个形状：条目里写死的值按参数类型还原
            // （命名参数带上名字），包名的位置也走 `pin_package_argument`，
            // 否则卡片上显示的版本位置和实际执行的不一样。
            let mut declared = declared_arguments(&package.runtime_arguments);
            if !pin_package_argument(&mut declared, package) {
                declared.push(package_spec(package));
            }
            declared.extend(declared_arguments(&package.package_arguments));
            parts.extend(declared.into_iter().filter(|part| !part.trim().is_empty()));
            (parts.join(" "), "stdio".to_string(), (*runtime).to_string())
        }
        InstallPlan::Remote { remote } => (
            remote.url.clone(),
            "streamable-http".to_string(),
            String::new(),
        ),
    }
}

/// 条目里写死的参数，按命令行的真实形状展开（命名参数带上名字）。
/// 只给预览用：用户填的部分不在这里，必填项留空也不报错。
fn declared_arguments(arguments: &[ParsedArgument]) -> Vec<String> {
    let mut resolved = Vec::new();
    for argument in arguments {
        let value = argument.value.trim();
        match argument.kind {
            ArgumentKind::Named => {
                resolved.push(format!("--{}", argument.key));
                if !value.is_empty() && !value.starts_with("--") {
                    resolved.push(value.to_string());
                }
            }
            ArgumentKind::Positional if !value.is_empty() => resolved.push(value.to_string()),
            ArgumentKind::Positional => {}
        }
    }
    resolved
}

fn entry_view(
    document: &Value,
    source: &CatalogSource,
    configs: &[McpServerConfig],
    taken: &mut BTreeSet<String>,
) -> CatalogEntryView {
    let source_label = source.label.clone();
    let fallback_id = entry_name(document);
    let mut view = match parse_entry(document) {
        Ok(entry) => {
            let plan = install_plan(&entry);
            let (preview, transport, runtime) = match &plan {
                Ok(plan) => command_preview(plan),
                Err(_) => (String::new(), String::new(), String::new()),
            };
            let inputs = plan.as_ref().map(entry_inputs).unwrap_or_default();
            let (installable, reason) = match &plan {
                Ok(_) => (true, String::new()),
                Err(reason) => (false, reason.clone()),
            };
            let installed_as = match &plan {
                Ok(plan) => installed_service(plan, configs),
                Err(_) => String::new(),
            };
            CatalogEntryView {
                id: entry.id.clone(),
                source_id: source.id.clone(),
                source_label,
                trust: source.trust,
                title: entry.title,
                description: entry.description,
                version: entry.version,
                schema: entry.schema,
                repository: entry.repository,
                website_url: entry.website_url,
                transport,
                runtime,
                runtime_label: entry.runtime_label,
                command_preview: preview,
                installable,
                reason,
                installed_as,
                suggested_name: String::new(),
                inputs,
            }
        }
        Err(reason) => CatalogEntryView {
            id: if fallback_id.is_empty() {
                "(未命名条目)".to_string()
            } else {
                fallback_id
            },
            source_id: source.id.clone(),
            source_label,
            trust: source.trust,
            title: text(document, "title"),
            description: text(document, "description"),
            version: text(document, "version"),
            schema: schema_version(document),
            repository: String::new(),
            website_url: String::new(),
            transport: String::new(),
            runtime: String::new(),
            runtime_label: String::new(),
            command_preview: String::new(),
            installable: false,
            reason,
            installed_as: String::new(),
            suggested_name: String::new(),
            inputs: Vec::new(),
        },
    };
    if view.title.trim().is_empty() {
        view.title = view.id.clone();
    }
    view.suggested_name = suggested_name(&view.id, taken);
    if view.installable {
        taken.insert(view.suggested_name.to_ascii_lowercase());
    }
    view
}

fn sorted_sources() -> Vec<CatalogSource> {
    let mut list = sources();
    // 内置精选永远排最前，其余按配置顺序。
    list.sort_by_key(|source| source.id != BUILTIN_SOURCE_ID);
    list
}

/// 只读当前状态：读内置条目 + 各来源快照，不联网。
pub(crate) fn view(state_path: &Path) -> CatalogView {
    let configs = installed_configs(state_path);
    let mut taken: BTreeSet<String> = configs
        .iter()
        .map(|config| config.server_name.to_ascii_lowercase())
        .collect();
    let mut entries = Vec::new();
    let mut source_views = Vec::new();
    let mut newest = 0u64;
    let mut network_sources = 0usize;
    let mut fresh_sources = 0usize;

    for source in sorted_sources() {
        match source.kind {
            CatalogSourceKind::Builtin => {
                let documents = seed_documents();
                for document in &documents {
                    entries.push(entry_view(document, &source, &configs, &mut taken));
                }
                source_views.push(CatalogSourceView {
                    id: source.id.clone(),
                    label: source.label.clone(),
                    trust: source.trust,
                    url: String::new(),
                    count: documents.len(),
                    fetched_at: now_secs(),
                    acknowledged: true,
                    error: String::new(),
                });
            }
            CatalogSourceKind::Registry | CatalogSourceKind::Http => {
                if !source.enabled {
                    source_views.push(CatalogSourceView {
                        id: source.id.clone(),
                        label: source.label.clone(),
                        trust: source.trust,
                        url: source.url.clone(),
                        count: 0,
                        fetched_at: 0,
                        acknowledged: acknowledged(&source.id),
                        error: "已停用".to_string(),
                    });
                    continue;
                }
                network_sources += 1;
                let snapshot = load_snapshot(&source.id);
                let count = snapshot
                    .as_ref()
                    .map(|item| item.entries.len())
                    .unwrap_or_default();
                let fetched_at = snapshot
                    .as_ref()
                    .map(|item| item.fetched_at)
                    .unwrap_or_default();
                newest = newest.max(fetched_at);
                if fetched_at > 0 && now_secs().saturating_sub(fetched_at) <= STALE_AFTER.as_secs()
                {
                    fresh_sources += 1;
                }
                if let Some(snapshot) = &snapshot {
                    for document in &snapshot.entries {
                        entries.push(entry_view(document, &source, &configs, &mut taken));
                    }
                }
                source_views.push(CatalogSourceView {
                    id: source.id.clone(),
                    label: source.label.clone(),
                    trust: source.trust,
                    url: source.url.clone(),
                    count,
                    fetched_at,
                    acknowledged: acknowledged(&source.id),
                    error: String::new(),
                });
            }
        }
    }

    // 精选排前，其余按标题排序：用户先看到「放了就能用」的那几个。
    entries.sort_by(|left, right| {
        (left.trust != CatalogTrust::Curated)
            .cmp(&(right.trust != CatalogTrust::Curated))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });

    CatalogView {
        entries,
        sources: source_views,
        fetched_at: newest,
        stale: network_sources == 0 || fresh_sources == 0,
    }
}

/// 已装的服务，用来反查「这个条目装过没有」。规则与 `build_config` 严格对称：
/// 包按运行时命令 + 包标识，远端按 URL 模板前缀。手工加的同一款工具也能被认出来，
/// 所以不需要额外的安装索引文件（ADR 0006：不设第二个 store、不设目录专属文件）。
fn installed_configs(state_path: &Path) -> Vec<McpServerConfig> {
    mcp_registry::list_configs(state_path).unwrap_or_default()
}

fn matches_config(plan: &InstallPlan, config: &McpServerConfig) -> bool {
    match plan {
        InstallPlan::Package { package, runtime } => {
            if !config.transport.eq_ignore_ascii_case("stdio") || config.command != *runtime {
                return false;
            }
            if package.identifier.is_empty() {
                return false;
            }
            let pinned = format!("{}@", package.identifier);
            config
                .args
                .iter()
                .any(|argument| argument == &package.identifier || argument.starts_with(&pinned))
        }
        InstallPlan::Remote { remote } => {
            if !config.transport.eq_ignore_ascii_case("streamable-http") || config.url.is_empty() {
                return false;
            }
            // 远端条目可以带 `{variable}` 模板，装完的 URL 是替换过的那一份，
            // 所以比对到占位符之前的静态前缀，而不是整串相等。
            match remote.url.split('{').next().unwrap_or_default() {
                "" => config.url == remote.url,
                prefix if prefix.len() >= 8 => config.url.starts_with(prefix),
                _ => config.url == remote.url,
            }
        }
    }
}

fn installed_service(plan: &InstallPlan, configs: &[McpServerConfig]) -> String {
    configs
        .iter()
        .find(|config| matches_config(plan, config))
        .map(|config| config.server_name.clone())
        .unwrap_or_default()
}

fn seed_documents() -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(SEED).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 同步
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(NETWORK_TIMEOUT)
        .user_agent("HiMind-Agent")
        .build()
        .map_err(|error| error.to_string())
}

/// 从 registry 响应里取出条目和下一页游标，同时记下见过的最新更新时间。
fn read_registry_page(body: &Value) -> (Vec<Value>, String, String) {
    let mut entries = Vec::new();
    let mut newest = String::new();
    for item in list(body, "servers").iter() {
        let document = item.get("server").cloned().unwrap_or_else(|| item.clone());
        if let Some(stamp) = item
            .get("_meta")
            .and_then(Value::as_object)
            .and_then(|meta| meta.values().next())
            .map(|meta| text(meta, "updatedAt"))
            .filter(|value| !value.is_empty())
        {
            if stamp > newest {
                newest = stamp;
            }
        }
        entries.push(document);
    }
    let cursor = body
        .get("metadata")
        .map(|meta| text(meta, "nextCursor"))
        .unwrap_or_default();
    (entries, cursor, newest)
}

fn registry_url(
    source: &CatalogSource,
    cursor: &str,
    updated_since: &str,
) -> Result<url::Url, String> {
    let mut url = url::Url::parse(&source.url).map_err(|error| error.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("limit", &PAGE_SIZE.to_string());
        if !cursor.is_empty() {
            query.append_pair("cursor", cursor);
        } else if !updated_since.is_empty() {
            query.append_pair("updated_since", updated_since);
        }
    }
    Ok(url)
}

/// 一次增量同步：翻页 → 每页落盘 → 再推进游标。中断只会重读，不会跳过。
fn sync_registry(
    source: &CatalogSource,
    client: &reqwest::blocking::Client,
) -> Result<usize, String> {
    let mut snapshot =
        load_snapshot(&source.id).unwrap_or_else(|| CatalogSnapshot::empty(&source.id));
    let incremental = !snapshot.updated_since.is_empty();
    let mut cursor = String::new();
    let mut newest = String::new();
    let mut pages = 0;
    loop {
        pages += 1;
        let url = registry_url(source, &cursor, &snapshot.updated_since)?;
        let response = client
            .get(url.clone())
            .send()
            .map_err(|error| format!("无法连接目录源: {error}"))?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST && incremental {
            // 增量参数不被接受时退回全量，而不是把刷新整体判失败。
            crate::app::crash::record_event(
                "warn",
                &format!("目录源不接受增量游标，改为全量刷新: {}", source.id),
            );
            snapshot.updated_since.clear();
            cursor.clear();
            pages = 0;
            continue;
        }
        let body = response
            .error_for_status()
            .map_err(|error| format!("目录源返回错误: {error}"))?
            .json::<Value>()
            .map_err(|error| format!("目录源响应无法解析: {error}"))?;
        let (entries, next, stamp) = read_registry_page(&body);
        if entries.is_empty() && next.is_empty() {
            break;
        }
        merge_page(&mut snapshot, entries);
        if stamp > newest {
            newest = stamp;
        }
        snapshot.fetched_at = now_secs();
        snapshot.cursor = next.clone();
        snapshot.updated_since = if newest.is_empty() {
            snapshot.updated_since.clone()
        } else {
            newest.clone()
        };
        snapshot.source_id = source.id.clone();
        snapshot.format_version = SNAPSHOT_FORMAT_VERSION;
        save_snapshot(&snapshot).map_err(|error| format!("目录快照写入失败: {error}"))?;
        if next.is_empty() || pages >= MAX_PAGES {
            break;
        }
        cursor = next;
    }
    snapshot.fetched_at = now_secs();
    if snapshot.updated_since.is_empty() {
        snapshot.updated_since = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    }
    save_snapshot(&snapshot).map_err(|error| format!("目录快照写入失败: {error}"))?;
    Ok(snapshot.entries.len())
}

fn sync_http(source: &CatalogSource, client: &reqwest::blocking::Client) -> Result<usize, String> {
    let body = client
        .get(&source.url)
        .send()
        .map_err(|error| format!("无法连接目录源: {error}"))?
        .error_for_status()
        .map_err(|error| format!("目录源返回错误: {error}"))?
        .json::<Value>()
        .map_err(|error| format!("目录源响应无法解析: {error}"))?;
    let documents = if let Some(items) = body.as_array() {
        items.clone()
    } else if !list(&body, "servers").is_empty() {
        read_registry_page(&body).0
    } else if !list(&body, "entries").is_empty() {
        list(&body, "entries")
            .iter()
            .map(|item| item.get("server").cloned().unwrap_or_else(|| item.clone()))
            .collect()
    } else {
        return Err("目录源响应里没有 servers 或 entries".to_string());
    };
    let mut snapshot = CatalogSnapshot::empty(&source.id);
    let count = documents.len();
    merge_page(&mut snapshot, documents);
    snapshot.fetched_at = now_secs();
    snapshot.updated_since = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    save_snapshot(&snapshot).map_err(|error| format!("目录快照写入失败: {error}"))?;
    Ok(count)
}

/// 刷新所有启用的来源，然后返回最新视图。单个来源失败不影响其它来源。
pub(crate) fn refresh(state_path: &Path) -> CatalogView {
    let _guard = REFRESH_LOCK.lock();
    let Ok(client) = http_client() else {
        return view(state_path);
    };
    for source in sorted_sources() {
        if !source.enabled || matches!(source.kind, CatalogSourceKind::Builtin) {
            continue;
        }
        let result = match source.kind {
            CatalogSourceKind::Registry => sync_registry(&source, &client),
            _ => sync_http(&source, &client),
        };
        if let Err(error) = result {
            crate::app::crash::record_event(
                "warn",
                &format!("MCP 目录刷新失败: {} ({error})", source.id),
            );
        }
    }
    view(state_path)
}

/// 启动时的后台刷新：快照还新就不打扰。（ADR 0005：永不阻塞、失败静默。）
pub(crate) fn refresh_if_stale(state_path: &Path) {
    let current = view(state_path);
    if !current.stale {
        return;
    }
    let _ = refresh(state_path);
}

// ---------------------------------------------------------------------------
// 安装
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct InstallRequest {
    pub source_id: String,
    pub entry_id: String,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub server_name: String,
    /// 用户已在界面上确认过来自未验证来源的工具。
    #[serde(default)]
    pub acknowledge: bool,
}

fn find_document(source_id: &str, entry_id: &str) -> Result<(CatalogSource, Value), String> {
    let source = sorted_sources()
        .into_iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| "目录来源不存在".to_string())?;
    let documents = if matches!(source.kind, CatalogSourceKind::Builtin) {
        seed_documents()
    } else {
        load_snapshot(&source.id)
            .map(|snapshot| snapshot.entries)
            .ok_or_else(|| "目录快照不可用，请先刷新目录".to_string())?
    };
    documents
        .into_iter()
        .find(|document| entry_name(document) == entry_id)
        .map(|document| (source, document))
        .ok_or_else(|| "目录里找不到这个条目，可能已经下架".to_string())
}

pub(crate) fn install(
    state_path: &Path,
    request: &InstallRequest,
) -> Result<McpServerConfig, String> {
    let (source, document) = find_document(&request.source_id, &request.entry_id)?;
    if source.trust.requires_acknowledgement() && !acknowledged(&source.id) && !request.acknowledge
    {
        return Err(format!(
            "「{}」来自未验证来源，确认后才能安装",
            source.label
        ));
    }
    let entry = parse_entry(&document)?;
    let plan = install_plan(&entry)?;
    if let InstallPlan::Package { package, .. } = &plan {
        if runtime_command(package).is_none() {
            return Err("暂不支持这种安装包类型".to_string());
        }
    }
    let configs = installed_configs(state_path);
    let mut taken: BTreeSet<String> = configs
        .iter()
        .map(|config| config.server_name.to_ascii_lowercase())
        .collect();
    let server_name = {
        let requested = request.server_name.trim();
        if requested.is_empty() {
            suggested_name(&entry.id, &taken)
        } else {
            // 用户改过名字时，把其它服务也避开这个位置。
            taken.insert(requested.to_ascii_lowercase());
            requested.to_string()
        }
    };
    let display_name = if request.display_name.trim().is_empty() {
        entry.title.clone()
    } else {
        request.display_name.trim().to_string()
    };
    // ADR 0006：未验证来源装完默认不启用，第一次启用是用户的显式动作。
    let enabled = !source.trust.requires_acknowledgement();
    let config = build_config(
        &entry,
        &plan,
        &request.values,
        &server_name,
        &display_name,
        enabled,
    )?;
    let saved =
        mcp_registry::upsert_config(state_path, config).map_err(|error| error.to_string())?;
    if source.trust.requires_acknowledgement() {
        acknowledge_source(&source.id);
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::paths::test_env_lock;
    use serde_json::json;
    use std::sync::MutexGuard;

    /// 独立的临时 home。返回的 guard 必须活到测试结束：`HIMIND_AGENT_HOME`
    /// 是进程级变量，放开会让并行测试互相搬走对方的目录。
    fn temp_home(label: &str) -> (PathBuf, MutexGuard<'static, ()>) {
        let guard = test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-mcp-catalog-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        fs::create_dir_all(root.join("data")).unwrap();
        (root, guard)
    }

    fn state_path(root: &Path) -> PathBuf {
        root.join("data").join("agent-state.json")
    }

    fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root);
    }

    fn npm_entry() -> Value {
        json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "com.pulsemcp/remote-filesystem",
            "title": "Remote Filesystem",
            "description": "把对象存储当文件系统用。",
            "version": "0.1.2",
            "repository": { "url": "https://github.com/pulsemcp/mcp-servers", "source": "github" },
            "packages": [{
                "registryType": "npm",
                "registryBaseUrl": "https://registry.npmjs.org",
                "identifier": "remote-filesystem-mcp-server",
                "version": "0.1.2",
                "runtimeHint": "npx",
                "transport": { "type": "stdio" },
                "runtimeArguments": [{ "type": "positional", "value": "-y" }],
                "environmentVariables": [
                    { "name": "GCS_BUCKET", "isRequired": true, "description": "桶名" },
                    { "name": "GCS_PRIVATE_KEY", "isSecret": true }
                ]
            }]
        })
    }

    #[test]
    fn curated_seed_entries_all_map_to_valid_configs() {
        let documents = seed_documents();
        assert_eq!(documents.len(), 4);
        let (root, _guard) = temp_home("seed");
        let installed = installed_configs(&state_path(&root));
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut names = Vec::new();
        for document in &documents {
            let source = CatalogSource::builtin();
            let view = entry_view(document, &source, &installed, &mut taken);
            assert!(view.installable, "{} 不可安装：{}", view.id, view.reason);
            names.push(view.suggested_name.clone());
        }
        // 精选条目派生的服务名必须是后端认的合法名字，而且互不重复。
        assert_eq!(
            names,
            vec!["memory", "sequential-thinking", "filesystem", "everything"]
        );
        for name in &names {
            assert!(mcp_registry::validate_config(&McpServerConfig {
                server_name: name.clone(),
                display_name: name.clone(),
                transport: "stdio".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "pkg".to_string()],
                env: BTreeMap::new(),
                cwd: String::new(),
                url: String::new(),
                headers: BTreeMap::new(),
                tool_call_timeout_ms: 30_000,
                fail_on_startup_error: false,
                reconnect: true,
                enabled: true,
            })
            .is_ok());
        }
        cleanup(&root);
    }

    #[test]
    fn curated_filesystem_entry_requires_a_directory() {
        let document = seed_documents()
            .into_iter()
            .find(|document| entry_name(document).ends_with("filesystem"))
            .unwrap();
        let entry = parse_entry(&document).unwrap();
        let plan = install_plan(&entry).unwrap();
        let inputs = entry_inputs(&plan);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].key, "arg:directory");
        assert!(inputs[0].required);
        // 目录没填时必须报缺参数，而不是装出一个残缺的连接。
        let error = build_config(
            &entry,
            &plan,
            &BTreeMap::new(),
            "filesystem",
            "文件系统",
            true,
        )
        .unwrap_err();
        assert!(error.contains("必填"), "{error}");
        let config = build_config(
            &entry,
            &plan,
            &BTreeMap::from([("arg:directory".to_string(), "D:\\work".to_string())]),
            "filesystem",
            "文件系统",
            true,
        )
        .unwrap();
        assert_eq!(config.command, "npx");
        assert_eq!(
            config.args,
            vec![
                "-y",
                "@modelcontextprotocol/server-filesystem@2026.8.31",
                "D:\\work"
            ]
        );
    }

    #[test]
    fn npm_package_maps_to_runtime_arguments_and_required_env() {
        let entry = parse_entry(&npm_entry()).unwrap();
        let plan = install_plan(&entry).unwrap();
        let config = build_config(
            &entry,
            &plan,
            &BTreeMap::from([
                ("env:GCS_BUCKET".to_string(), "bucket".to_string()),
                ("env:GCS_PRIVATE_KEY".to_string(), "key".to_string()),
            ]),
            "remote-filesystem",
            "Remote Filesystem",
            true,
        )
        .unwrap();
        assert_eq!(config.command, "npx");
        assert_eq!(
            config.args,
            vec!["-y", "remote-filesystem-mcp-server@0.1.2"]
        );
        assert_eq!(
            config.env.get("GCS_BUCKET").map(String::as_str),
            Some("bucket")
        );
        // 密钥没填时必须报错，而不是留一个连不上的连接。
        assert!(build_config(
            &entry,
            &plan,
            &BTreeMap::from([("env:GCS_BUCKET".to_string(), "bucket".to_string())]),
            "remote-filesystem",
            "Remote Filesystem",
            true,
        )
        .is_err());
    }

    /// 条目把包名写进 `runtimeArguments` 时，版本必须并进那一项。
    ///
    /// 真机验证过这条：`npx --package @clize/inbox clize-mcp @clize/inbox@0.35.5`
    /// 里 npx 只认 `--package`，末尾那个带版本的包名会变成传给服务的普通参数，
    /// 实际装到的是最新版（0.36.0），钉版本静默失效。
    #[test]
    fn version_pin_merges_into_declared_package_name() {
        let named = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "ai.clize/inbox",
            "title": "Agent Inbox",
            "version": "0.35.5",
            "packages": [{
                "registryType": "npm",
                "identifier": "@clize/inbox",
                "version": "0.35.5",
                "runtimeHint": "npx",
                "runtimeArguments": [
                    { "value": "@clize/inbox", "type": "named", "name": "--package" },
                    { "value": "clize-mcp", "type": "positional" }
                ],
                "packageArguments": [
                    { "value": "inbox", "type": "named", "name": "--profile" }
                ]
            }]
        });
        let entry = parse_entry(&named).unwrap();
        let plan = install_plan(&entry).unwrap();
        assert_eq!(
            command_preview(&plan).0,
            "npx --package @clize/inbox@0.35.5 clize-mcp --profile inbox"
        );
        let config = build_config(
            &entry,
            &plan,
            &BTreeMap::new(),
            "inbox",
            "Agent Inbox",
            false,
        )
        .unwrap();
        assert_eq!(
            config.args,
            vec![
                "--package",
                "@clize/inbox@0.35.5",
                "clize-mcp",
                "--profile",
                "inbox"
            ]
        );

        // 包名按位置参数写（`-p <包名> <命令>`）是同一件事的另一种写法。
        let positional = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "ai.circulara/plugin",
            "title": "Circulara",
            "version": "0.1.2",
            "packages": [{
                "registryType": "npm",
                "identifier": "@circulara/plugin",
                "version": "0.1.2",
                "runtimeHint": "npx",
                "runtimeArguments": [
                    { "value": "-y", "type": "positional" },
                    { "value": "-p", "type": "positional" },
                    { "value": "@circulara/plugin", "type": "positional" },
                    { "value": "circulara-mcp", "type": "positional" }
                ]
            }]
        });
        let entry = parse_entry(&positional).unwrap();
        let plan = install_plan(&entry).unwrap();
        let config = build_config(
            &entry,
            &plan,
            &BTreeMap::new(),
            "circulara",
            "Circulara",
            true,
        )
        .unwrap();
        assert_eq!(
            config.args,
            vec!["-y", "-p", "@circulara/plugin@0.1.2", "circulara-mcp"]
        );

        // 条目里已经把版本写进包名时不要出现第二份。
        let already_pinned = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "io.example/pinned",
            "title": "Pinned",
            "version": "0.1.0",
            "packages": [{
                "registryType": "npm",
                "identifier": "@example/pinned",
                "version": "0.1.0",
                "runtimeHint": "npx",
                "runtimeArguments": [
                    { "value": "@example/pinned@0.1.0", "type": "positional" }
                ]
            }]
        });
        let entry = parse_entry(&already_pinned).unwrap();
        let plan = install_plan(&entry).unwrap();
        let config =
            build_config(&entry, &plan, &BTreeMap::new(), "pinned", "Pinned", true).unwrap();
        assert_eq!(config.args, vec!["@example/pinned@0.1.0"]);
    }

    #[test]
    fn remote_entry_uses_streamable_http_and_keeps_sse_out() {
        let document = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-09-29/server.schema.json",
            "name": "ac.inference.sh/mcp",
            "description": "远端工具。",
            "version": "1.0.0",
            "remotes": [
                { "type": "streamable-http", "url": "https://api.example.com/mcp" },
                { "type": "sse", "url": "https://api.example.com/sse" }
            ]
        });
        let entry = parse_entry(&document).unwrap();
        let plan = install_plan(&entry).unwrap();
        let (preview, transport, _) = command_preview(&plan);
        assert_eq!(preview, "https://api.example.com/mcp");
        assert_eq!(transport, "streamable-http");

        let sse_only = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-09-29/server.schema.json",
            "name": "ac.inference.sh/sse-only",
            "description": "只有 SSE。",
            "version": "1.0.0",
            "remotes": [{ "type": "sse", "url": "https://api.example.com/sse" }]
        });
        let entry = parse_entry(&sse_only).unwrap();
        let reason = install_plan(&entry).unwrap_err();
        assert!(reason.contains("SSE"), "{reason}");
    }

    #[test]
    fn unknown_package_type_is_listed_but_not_installable() {
        let document = json!({
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "io.example/oci-tool",
            "description": "OCI 包。",
            "version": "2.0.0",
            "packages": [{
                "registryType": "oci",
                "identifier": "ghcr.io/example/tool",
                "transport": { "type": "stdio" }
            }]
        });
        let source = CatalogSource {
            id: "official".to_string(),
            label: "MCP 目录".to_string(),
            url: DEFAULT_REGISTRY_URL.to_string(),
            enabled: true,
            kind: CatalogSourceKind::Registry,
            trust: CatalogTrust::Unverified,
        };
        let view = entry_view(&document, &source, &[], &mut BTreeSet::new());
        // 坏条目保留在列表里，只标注不能装，不能整片清空。
        assert_eq!(view.id, "io.example/oci-tool");
        assert!(!view.installable);
        assert!(
            view.reason.contains("oci") || view.reason.contains("安装方式"),
            "{}",
            view.reason
        );
    }

    #[test]
    fn entry_without_schema_is_reported_not_dropped() {
        let document = json!({ "name": "io.example/legacy", "version": "1" });
        let source = CatalogSource::builtin();
        let view = entry_view(&document, &source, &[], &mut BTreeSet::new());
        assert_eq!(view.id, "io.example/legacy");
        assert!(!view.installable);
        assert!(view.reason.contains("$schema"), "{}", view.reason);
    }

    #[test]
    fn unverified_source_installs_disabled_and_needs_one_acknowledgement() {
        let (root, _guard) = temp_home("install");
        let path = state_path(&root);
        let request = InstallRequest {
            source_id: BUILTIN_SOURCE_ID.to_string(),
            entry_id: "io.himind.curated/memory".to_string(),
            values: BTreeMap::new(),
            display_name: String::new(),
            server_name: String::new(),
            acknowledge: false,
        };
        let config = install(&path, &request).unwrap();
        assert_eq!(config.server_name, "memory");
        // 精选条目不要求确认，装完就是启用状态。
        assert!(config.enabled);

        // 未验证来源：第一次必须确认，之后按来源记住。
        let mut snapshot = CatalogSnapshot::empty("official");
        snapshot.fetched_at = now_secs();
        merge_page(&mut snapshot, vec![npm_entry()]);
        save_snapshot(&snapshot).unwrap();
        let mut request = InstallRequest {
            source_id: "official".to_string(),
            entry_id: "com.pulsemcp/remote-filesystem".to_string(),
            values: BTreeMap::from([
                ("env:GCS_BUCKET".to_string(), "bucket".to_string()),
                ("env:GCS_PRIVATE_KEY".to_string(), "key".to_string()),
            ]),
            display_name: String::new(),
            server_name: String::new(),
            acknowledge: false,
        };
        let error = install(&path, &request).unwrap_err();
        assert!(error.contains("未验证来源"), "{error}");
        assert!(acknowledged("official") == false);
        request.acknowledge = true;
        let config = install(&path, &request).unwrap();
        assert_eq!(config.server_name, "remote-filesystem");
        assert!(!config.enabled, "未验证来源装完默认不启用");
        assert!(acknowledged("official"));
        cleanup(&root);
    }

    #[test]
    fn registry_page_shapes_are_read_and_merged_by_name() {
        let body = json!({
            "servers": [
                { "server": npm_entry(), "_meta": { "io.modelcontextprotocol.registry/official": { "updatedAt": "2026-09-29T10:00:00Z" } } }
            ],
            "metadata": { "nextCursor": "page-2", "count": 1 }
        });
        let (entries, cursor, stamp) = read_registry_page(&body);
        assert_eq!(entries.len(), 1);
        assert_eq!(cursor, "page-2");
        assert_eq!(stamp, "2026-09-29T10:00:00Z");

        let mut snapshot = CatalogSnapshot::empty("official");
        merge_page(&mut snapshot, entries.clone());
        let mut updated = npm_entry();
        updated["version"] = json!("0.1.3");
        merge_page(&mut snapshot, vec![updated]);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(text(&snapshot.entries[0], "version"), "0.1.3");
    }

    #[test]
    fn registry_url_carries_cursor_or_incremental_watermark() {
        let source = CatalogSource::default_registry();
        let full = registry_url(&source, "", "").unwrap();
        assert!(full.as_str().contains("limit=100"));
        assert!(!full.as_str().contains("updated_since"));
        let incremental = registry_url(&source, "", "2026-09-29T10:00:00Z").unwrap();
        assert!(incremental
            .as_str()
            .contains("updated_since=2026-09-29T10%3A00%3A00Z"));
        let paged = registry_url(&source, "abc", "2026-09-29T10:00:00Z").unwrap();
        assert!(paged.as_str().contains("cursor=abc"));
        assert!(!paged.as_str().contains("updated_since"));
    }

    #[test]
    fn snapshot_format_version_bump_discards_old_file() {
        let (root, _guard) = temp_home("format");
        fs::create_dir_all(cache_dir()).unwrap();
        fs::write(
            cache_path("official"),
            br#"{"format_version":0,"source_id":"official","entries":[]}"#,
        )
        .unwrap();
        assert!(load_snapshot("official").is_none());
        cleanup(&root);
    }

    #[test]
    fn view_always_contains_builtin_entries_and_reports_staleness() {
        let (root, _guard) = temp_home("view");
        let path = state_path(&root);
        let view = view(&path);
        assert!(view.entries.len() >= 4);
        assert!(view.stale, "没有快照时必须标明列表可能不完整");
        assert!(view
            .entries
            .iter()
            .all(|entry| !entry.suggested_name.is_empty()));
        cleanup(&root);
    }

    #[test]
    fn installed_entries_are_marked_with_their_service_name() {
        let (root, _guard) = temp_home("mark");
        let path = state_path(&root);
        install(
            &path,
            &InstallRequest {
                source_id: BUILTIN_SOURCE_ID.to_string(),
                entry_id: "io.himind.curated/memory".to_string(),
                values: BTreeMap::new(),
                display_name: String::new(),
                server_name: String::new(),
                acknowledge: false,
            },
        )
        .unwrap();
        let view = view(&path);
        let entry = view
            .entries
            .iter()
            .find(|entry| entry.id == "io.himind.curated/memory")
            .unwrap();
        assert_eq!(entry.installed_as, "memory");
        // 已经装过的条目，建议名不再抢占服务名。
        let other = view
            .entries
            .iter()
            .find(|entry| entry.id == "io.himind.curated/filesystem")
            .unwrap();
        assert_eq!(other.suggested_name, "filesystem");
        cleanup(&root);
    }
}
