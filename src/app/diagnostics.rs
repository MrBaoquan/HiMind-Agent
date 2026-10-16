use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use windows_sys::Win32::Security::ACL;
#[cfg(windows)]
use zip::write::FileOptions;

use crate::api::client::load_agent_state;
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

/// Run checks that do not require the WebView or a running Agent window.
///
/// The command is intentionally machine-readable so support can ask users to
/// attach one small JSON file instead of transcribing a message box. Exit code
/// 0 means no failed check, 2 means warnings, and 1 means an installation
/// check failed.
pub(crate) fn check_installation(options: &Options) -> Result<(Value, i32), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let root = crate::install_layout::installation_root_from_executable(&executable);
    let mut checks = Vec::new();
    let mut failures = 0_u32;
    let mut warnings = 0_u32;

    let installed = crate::install_layout::executable_is_installed(&executable);
    push_check(
        &mut checks,
        &mut failures,
        "installed_layout",
        installed,
        if installed {
            "安装目录结构完整"
        } else {
            "当前可执行文件不在完整安装布局中"
        },
        "error",
    );

    for (name, path) in [
        ("launcher", root.join(crate::install_layout::LAUNCHER_FILE)),
        ("updater", root.join(crate::install_layout::UPDATER_FILE)),
        ("icon", root.join("himind-agent.ico")),
        (
            "active_version",
            root.join(crate::install_layout::ACTIVE_VERSION_FILE),
        ),
    ] {
        let present = path.is_file();
        push_check(
            &mut checks,
            &mut failures,
            name,
            present,
            if present {
                "文件存在"
            } else {
                "文件缺失"
            },
            "error",
        );
    }

    let icon_path = root.join("himind-agent.ico");
    match inspect_ico(&icon_path) {
        Ok(report) => {
            if !report.valid {
                failures += 1;
            }
            checks.push(json!({
                "name": "icon_format",
                "ok": report.valid,
                "severity": if report.valid { "info" } else { "error" },
                "message": report.message,
                "sizes": report.sizes,
            }));
        }
        Err(error) => {
            failures += 1;
            checks.push(json!({ "name": "icon_format", "ok": false, "severity": "error", "message": error.to_string() }));
        }
    }

    let webview_dir = crate::store::paths::webview_user_data_dir(options.local_port);
    match probe_writable_directory(&webview_dir) {
        Ok(()) => checks.push(json!({ "name": "webview_data_dir", "ok": true, "severity": "info", "message": "WebView2 用户数据目录可写", "path": webview_dir })),
        Err(error) => {
            failures += 1;
            checks.push(json!({ "name": "webview_data_dir", "ok": false, "severity": "error", "message": error.to_string(), "path": webview_dir }));
        }
    }

    let storage_integrity = storage_integrity_snapshot(options.local_port);
    let low_integrity_paths = storage_integrity
        .get("low_integrity_paths")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    if low_integrity_paths {
        failures += 1;
        checks.push(json!({
            "name": "storage_integrity",
            "ok": false,
            "severity": "error",
            "message": "Agent 数据或 WebView2 目录被标记为 Low integrity；普通启动会出现只读数据库、协议注册拒绝访问或窗口无法创建。请先退出 Agent，再运行安装目录中的 repair-storage.ps1（或重新安装新版）。",
            "storage": storage_integrity,
        }));
    } else {
        checks.push(json!({
            "name": "storage_integrity",
            "ok": true,
            "severity": "info",
            "message": "Agent 数据、SQLite 文件和 WebView2 目录没有发现 Low integrity 标记",
            "storage": storage_integrity,
        }));
    }

    let protocol = protocol_registration_snapshot(&root, options);
    if protocol
        .get("registered")
        .and_then(Value::as_bool)
        .is_some_and(|registered| !registered)
    {
        warnings += 1;
        checks.push(json!({
            "name": "protocol_registration",
            "ok": false,
            "severity": "warning",
            "message": "himind-agent 协议注册缺失或指向旧安装；若日志同时出现 Access denied，通常是 HKCU\\Software\\Classes 权限或启动令牌完整性级别异常",
            "registration": protocol,
        }));
    } else {
        checks.push(json!({
            "name": "protocol_registration",
            "ok": true,
            "severity": "info",
            "message": "himind-agent 协议注册存在且指向当前安装",
            "registration": protocol,
        }));
    }
    let webview_storage = webview_storage_snapshot(&webview_dir);
    if webview_storage
        .get("lock_files")
        .and_then(Value::as_array)
        .is_some_and(|locks| !locks.is_empty())
    {
        warnings += 1;
        checks.push(json!({
            "name": "webview_data_lock",
            "ok": false,
            "severity": "warning",
            "message": "WebView2 数据目录存在锁文件；请先退出 Agent 后再删除陈旧目录，或让启动自愈使用隔离目录",
            "path": webview_dir,
            "locks": webview_storage.get("lock_files"),
        }));
    } else {
        checks.push(json!({
            "name": "webview_data_lock",
            "ok": true,
            "severity": "info",
            "message": "未发现 WebView2 数据目录锁文件",
            "path": webview_dir,
        }));
    }

    let startup = startup_evidence();
    let page_event_seen = startup
        .get("page_event_seen")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !page_event_seen {
        warnings += 1;
        checks.push(json!({
            "name": "startup_page_event",
            "ok": false,
            "severity": "warning",
            "message": "最近启动没有记录 WebView2 页面 Started/Finished 事件；如果进程和本地端口存在，通常是窗口创建或 WebView2 环境阶段卡住",
            "evidence": startup,
        }));
    } else {
        checks.push(json!({
            "name": "startup_page_event",
            "ok": true,
            "severity": "info",
            "message": "日志中存在 WebView2 页面加载事件",
            "evidence": startup,
        }));
    }

    let runtime = runtime_snapshot();
    let runtime_compatible = runtime
        .get("compatible_with_agent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !runtime_compatible {
        warnings += 1;
    }
    checks.push(json!({
        "name": "runtime",
        "ok": runtime_compatible,
        "severity": if runtime_compatible { "info" } else { "warning" },
        "message": if runtime_compatible { "内置运行时可用" } else { "内置运行时不可用或尚未完成首次自愈" },
        "status": runtime,
    }));

    let shortcut = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|home| home.join("Desktop").join("HiMind Agent.lnk"));
    if let Some(path) = shortcut {
        let exists = path.is_file();
        if !exists {
            warnings += 1;
        }
        checks.push(json!({ "name": "desktop_shortcut", "ok": exists, "severity": if exists { "info" } else { "warning" }, "message": if exists { "桌面快捷方式存在" } else { "未找到桌面快捷方式（安装时可能未勾选）" }, "path": path }));
    }

    let extension_trust = extension_trust_snapshot();
    let untrusted_keys = extension_trust
        .get("untrusted_key_ids")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !untrusted_keys.is_empty() {
        failures += 1;
    }
    checks.push(json!({
        "name": "extension_source_trust",
        "ok": untrusted_keys.is_empty(),
        "severity": if untrusted_keys.is_empty() { "info" } else { "error" },
        "message": if untrusted_keys.is_empty() {
            "已启用扩展源的缓存目录全部使用本机受信公钥".to_string()
        } else {
            format!(
                "扩展源不可用：目录里有条目使用本机不受信的签名公钥 {}（把对应的 <key-id>.pem 放进受信公钥目录即可修复）",
                untrusted_keys.join("、")
            )
        },
        "detail": extension_trust,
    }));

    let source_health = extension_source_health_snapshot();
    let failing_sources = source_health
        .get("failing")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !failing_sources.is_empty() {
        warnings += 1;
    }
    checks.push(json!({
        "name": "extension_source_health",
        "ok": failing_sources.is_empty(),
        "severity": if failing_sources.is_empty() { "info" } else { "warning" },
        "message": if failing_sources.is_empty() {
            "已启用扩展源没有记录到同步失败".to_string()
        } else {
            format!(
                "扩展源最近一次同步失败：{}。缺签名公钥时把 <key-id>.pem 放进受信公钥目录即可；自建来源也可把验证策略改成 optional",
                failing_sources
                    .iter()
                    .map(|item| format!(
                        "{}（{}）",
                        item.get("source_name").and_then(Value::as_str).unwrap_or("来源"),
                        item.get("error").and_then(Value::as_str).unwrap_or("未知原因")
                    ))
                    .collect::<Vec<_>>()
                    .join("；")
            )
        },
        "detail": source_health,
    }));

    let rail_links = dsh_rail_link_snapshot();
    let broken_links = rail_links
        .get("broken")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !broken_links.is_empty() {
        failures += 1;
    }
    checks.push(json!({
        "name": "himind_ai_rail_links",
        "ok": broken_links.is_empty(),
        "severity": if broken_links.is_empty() { "info" } else { "error" },
        "message": if broken_links.is_empty() {
            "HiMind AI 工作区的会话目录联接可遍历".to_string()
        } else {
            format!(
                "有 {} 处 HiMind AI 会话目录联接不可遍历（不受信挂载点），会话会以「页面返回 502」失败；\
                 重建这些联接即可修复，重跑安装器或执行 scripts/repair-storage.ps1 均可",
                broken_links.len()
            )
        },
        "detail": rail_links,
    }));

    let loopback_proxy = loopback_proxy_snapshot();
    let loopback_proxied = loopback_proxy
        .get("proxied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if loopback_proxied {
        warnings += 1;
    }
    checks.push(json!({
        "name": "loopback_proxy_bypass",
        "ok": !loopback_proxied,
        "severity": if loopback_proxied { "warning" } else { "info" },
        "message": if loopback_proxied {
            format!(
                "系统代理 {} 会接管发往 127.0.0.1 的请求，HiMind AI 页面可能以 502 失败；\
                 把 127.0.0.1 与 localhost 加进代理绕过列表（WinINET 的 127.* 写法不算），\
                 或设置 NO_PROXY=127.0.0.1,localhost 后重启 Agent",
                loopback_proxy
                    .get("proxy")
                    .and_then(Value::as_str)
                    .unwrap_or("未知")
            )
        } else {
            "发往环回地址的请求不会被系统代理接管".to_string()
        },
        "detail": loopback_proxy,
    }));

    let session_health = builtin_ai_session_snapshot(options);
    let session_failure = session_health
        .get("last_failure")
        .cloned()
        .unwrap_or(Value::Null);
    let session_ok = session_failure.is_null();
    if !session_ok {
        warnings += 1;
    }
    checks.push(json!({
        "name": "himind_ai_session",
        "ok": session_ok,
        "severity": if session_ok { "info" } else { "warning" },
        "message": if session_ok {
            "事件日志里没有尚未消除的 HiMind AI 会话启动失败".to_string()
        } else {
            format!(
                "最近一次 HiMind AI 会话启动失败：{}",
                session_failure
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("原因未记录")
            )
        },
        "detail": session_health,
    }));

    let result = json!({
        "schema_version": 1,
        "generated_at": unix_now(),
        "agent_version": VERSION,
        "executable": executable,
        "installation_root": root,
        "profile": crate::store::paths::profile_name(),
        "local_port": options.local_port,
        "webview_storage": webview_storage,
        "storage_integrity": storage_integrity_snapshot(options.local_port),
        "protocol_registration": protocol_registration_snapshot(&root, options),
        "startup_evidence": startup_evidence(),
        "extension_trust": extension_trust,
        "extension_source_health": source_health,
        "himind_ai_rail_links": rail_links,
        "builtin_ai_session": session_health,
        "checks": checks,
        "summary": { "failures": failures, "warnings": warnings },
    });
    let exit_code = if failures > 0 {
        1
    } else if warnings > 0 {
        2
    } else {
        0
    };
    Ok((result, exit_code))
}

fn push_check(
    checks: &mut Vec<Value>,
    failures: &mut u32,
    name: &str,
    ok: bool,
    message: &str,
    severity: &str,
) {
    if !ok && severity == "error" {
        *failures += 1;
    }
    checks.push(json!({ "name": name, "ok": ok, "severity": severity, "message": message }));
}

/// 系统代理会不会接管发往 127.0.0.1 的请求。
///
/// reqwest 默认启用 system-proxy：Windows 上它读 WinINET 的 ProxyEnable /
/// ProxyServer / ProxyOverride，绕过列表按 NO_PROXY 语义解析。`127.*`、
/// `<local>` 这类 WinINET 写法既不是 CIDR 也不是域名，匹配不到 127.0.0.1，
/// 于是本机环回请求会被交给代理；代理不在本机时它访问不到我们的 127.0.0.1，
/// 只能回 502 Bad Gateway——「HiMind AI 页面认证失败：HTTP 502 Bad Gateway」
/// 的一种来源就是这条，页面本身其实健康。
fn loopback_proxy_snapshot() -> Value {
    let mut env_proxy = None;
    for key in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                env_proxy = Some((key.to_string(), value));
                break;
            }
        }
    }
    let env_bypass = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    let registry = registry_proxy_settings();
    let registry_proxy = registry
        .get("server")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let registry_bypass = registry
        .get("bypass")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 环境变量优先于注册表，读取顺序与 reqwest 一致。
    let (source, proxy, bypass) = match env_proxy {
        Some((_, value)) => ("env", Some(value), env_bypass.clone()),
        None => ("registry", registry_proxy, registry_bypass.clone()),
    };
    let proxied = proxy.is_some() && !bypasses_loopback(&bypass);
    json!({
        "proxied": proxied,
        "source": source,
        "proxy": proxy,
        "bypass": bypass,
        "env_bypass": env_bypass,
        "registry": registry,
    })
}

/// 只回答一个问题：这份绕过列表覆盖环回地址吗。
///
/// 判据与 reqwest 的匹配规则一致：条目按逗号或分号切开，只认 `*`、精确主机名
/// 和 CIDR。`127.*`、`<local>` 这些 WinINET 写法在这里匹配不到 127.0.0.1，
/// 所以它们**不算**绕过——这正是需要点出来的坑。
fn bypasses_loopback(list: &str) -> bool {
    list.split([',', ';'])
        .map(|entry| entry.trim().trim_start_matches('.').to_ascii_lowercase())
        .any(|entry| {
            matches!(
                entry.as_str(),
                "*" | "127.0.0.1" | "127.0.0.0/8" | "127.0.0.1/32" | "localhost" | "::1" | "[::1]"
            )
        })
}

#[cfg(windows)]
fn registry_proxy_settings() -> Value {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    else {
        return json!({});
    };
    let enabled = key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    let server = key
        .get_value::<String, _>("ProxyServer")
        .unwrap_or_default();
    let bypass = key
        .get_value::<String, _>("ProxyOverride")
        .unwrap_or_default();
    json!({
        "enabled": enabled,
        "server": if enabled { server } else { String::new() },
        "bypass": bypass,
    })
}

#[cfg(not(windows))]
fn registry_proxy_settings() -> Value {
    json!({})
}
/// 扩展源信任快照：受信 key ID、以及每个「已启用」来源的缓存目录实际用到的 key ID。
///
/// 一条用未受信公钥签名的条目会让整个 GitHub 来源显示为不可用，而界面只给出
/// 一句话。这里在离线状态下（只读缓存，不访问网络）把「哪条目录、缺哪个公钥」
/// 变成可附在工单里的结构化证据。只统计配置里 `enabled` 的来源，避免把已经停用
/// 的历史缓存算成故障。
/// 已启用来源的「最后一次同步结论」，来自扩展源自己落盘的台账。
///
/// 缓存目录只在同步成功过之后才有内容，所以只查缓存会把「从没成功过的来源」
/// 报成一切正常——现场这台机器就是这样：目录为空、界面只报一句缺公钥。
fn extension_source_health_snapshot() -> Value {
    let path = crate::store::paths::agent_home()
        .join("data")
        .join("extension-source-health.json");
    let document = fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let mut failing = Vec::new();
    if let Some(object) = document.as_object() {
        let mut entries = object.values().collect::<Vec<_>>();
        entries.sort_by_key(|value| {
            value
                .get("source_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        for entry in entries {
            let error = entry
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if !error.is_empty() {
                failing.push(entry.clone());
            }
        }
    }
    json!({
        "path": path.to_string_lossy(),
        "sources": document,
        "failing": failing,
    })
}

/// HiMind AI 每个工作区的会话目录联接健康度。
///
/// DSH 把工作区的 `sessions`/`storages` 指向共享目录，枚举不到就说明联接被判成
/// 不受信挂载点，会话持久化插件随之起不来、Web 入口回 502。这里只读枚举，把
/// 「哪个工作区的哪个联接坏了」变成能直接附在工单里的证据。
fn dsh_rail_link_snapshot() -> Value {
    let root = crate::store::paths::agent_home()
        .join("runtimes")
        .join("deepseek-harness")
        .join("homes")
        .join("interactive")
        .join("workspaces");
    let mut links = Vec::new();
    let mut broken = Vec::new();
    if let Ok(workspaces) = fs::read_dir(&root) {
        for workspace in workspaces.flatten() {
            for entry in ["sessions", "storages"] {
                let path = workspace.path().join(entry);
                if fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                let traversable = fs::read_dir(&path).is_ok();
                let value = json!({
                    "workspace": workspace.file_name().to_string_lossy(),
                    "entry": entry,
                    "path": path.to_string_lossy(),
                    "traversable": traversable,
                });
                if !traversable {
                    broken.push(value.clone());
                }
                links.push(value);
            }
        }
    }
    json!({
        "workspaces_root": root.to_string_lossy(),
        "links": links,
        "broken": broken,
    })
}

fn extension_trust_snapshot() -> Value {
    let home = crate::store::paths::agent_home();
    let trusted = crate::app::system::trusted_signing_key_ids();
    let embedded = crate::app::system::trusted_embedded_key_ids();
    let mut enabled = Vec::<(String, String)>::new();
    if let Ok(raw) = fs::read(home.join("data").join("extension-sources.json")) {
        if let Ok(settings) = serde_json::from_slice::<Value>(&raw) {
            if let Some(sources) = settings.get("sources").and_then(Value::as_array) {
                for source in sources.iter() {
                    if source.get("enabled").and_then(Value::as_bool) != Some(true) {
                        continue;
                    }
                    let id = source
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = source
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    enabled.push((id, name));
                }
            }
        }
    }

    let mut sources = Vec::new();
    let mut untrusted = Vec::<String>::new();
    let cache = home.join("data").join("extension-source-cache");
    for (id, name) in &enabled {
        let path = cache.join(format!("{id}.json"));
        let Ok(raw) = fs::read(&path) else {
            sources.push(json!({
                "source_id": id,
                "source_name": name,
                "cached_catalog": false,
                "signature_key_ids": Vec::<String>::new(),
                "untrusted_key_ids": Vec::<String>::new(),
            }));
            continue;
        };
        let Ok(catalog) = serde_json::from_slice::<Value>(&raw) else {
            sources.push(json!({
                "source_id": id,
                "source_name": name,
                "cached_catalog": false,
                "signature_key_ids": Vec::<String>::new(),
                "untrusted_key_ids": Vec::<String>::new(),
            }));
            continue;
        };
        let mut key_ids = Vec::<String>::new();
        let mut source_untrusted = Vec::<String>::new();
        for collection in ["plugins", "skills", "workflows", "experts"] {
            let Some(items) = catalog.get(collection).and_then(Value::as_array) else {
                continue;
            };
            for item in items.iter() {
                let id = item
                    .get("signature_key_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                if !key_ids.contains(&id) {
                    key_ids.push(id.clone());
                }
                if !trusted.contains(&id)
                    && !embedded.contains(&id)
                    && !source_untrusted.contains(&id)
                {
                    source_untrusted.push(id.clone());
                }
                if (!trusted.contains(&id) && !embedded.contains(&id)) && !untrusted.contains(&id) {
                    untrusted.push(id.clone());
                }
            }
        }
        sources.push(json!({
            "source_id": id,
            "source_name": name,
            "cached_catalog": true,
            "signature_key_ids": key_ids,
            "untrusted_key_ids": source_untrusted,
        }));
    }

    json!({
        "trusted_keys_directory": crate::app::system::trusted_signing_keys_directory_hint(),
        "trusted_key_ids": trusted,
        "embedded_key_ids": embedded,
        "sources": sources,
        "untrusted_key_ids": untrusted,
    })
}

struct IcoReport {
    valid: bool,
    message: String,
    sizes: Vec<u16>,
}

fn inspect_ico(path: &Path) -> Result<IcoReport, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 6 || bytes[0..4] != [0, 0, 1, 0] {
        return Ok(IcoReport {
            valid: false,
            message: "ICO 头无效".into(),
            sizes: Vec::new(),
        });
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut sizes = Vec::new();
    for index in 0..count {
        let offset = 6 + index * 16;
        if offset + 16 > bytes.len() {
            return Ok(IcoReport {
                valid: false,
                message: "ICO 目录被截断".into(),
                sizes,
            });
        }
        let width = if bytes[offset] == 0 {
            256
        } else {
            bytes[offset] as u16
        };
        let height = if bytes[offset + 1] == 0 {
            256
        } else {
            bytes[offset + 1] as u16
        };
        let size = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into()?) as usize;
        let data_offset = u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into()?) as usize;
        let out_of_bounds = data_offset
            .checked_add(size)
            .map(|end| end > bytes.len())
            .unwrap_or(true);
        if width != height || out_of_bounds {
            return Ok(IcoReport {
                valid: false,
                message: "ICO 图像条目无效".into(),
                sizes,
            });
        }
        sizes.push(width);
    }
    let required = [16, 24, 32, 48, 64, 256];
    let valid = required.iter().all(|size| sizes.contains(size));
    Ok(IcoReport {
        valid,
        message: if valid {
            "ICO 包含完整的桌面/快捷方式尺寸".into()
        } else {
            "ICO 缺少 Windows 快捷方式所需的多尺寸图像".into()
        },
        sizes,
    })
}

fn probe_writable_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(path)?;
    let probe = path.join(format!(".doctor-{}.tmp", std::process::id()));
    fs::write(&probe, b"himind")?;
    fs::remove_file(probe)?;
    Ok(())
}

/// Repair only the files that can make a normal launch fail. Older Agent
/// builds accidentally ran ACL warmup on their own data root, leaving SQLite
/// WAL/SHM sidecars at Low integrity. Repairing the whole installation tree on
/// every launch would traverse gigabytes of runtime files, so this function is
/// deliberately narrow and best-effort.
///
/// The WebView launch path and the background-service path both call this
/// during startup. The report is built once per process and replayed: it
/// answers "what did this launch have to fix", so running the scan twice only
/// doubled the work behind the same answer.
pub(crate) fn repair_startup_storage(local_port: u16) -> Vec<String> {
    static REPORT: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    REPORT
        .get_or_init(|| repair_startup_storage_once(local_port))
        .clone()
}

/// Every storage path a normal launch depends on, in scan order.
fn startup_storage_targets(local_port: u16) -> Vec<PathBuf> {
    let root = crate::store::paths::agent_home();
    let database = root.join(crate::store::local_runs::LOCAL_RUN_DB_FILE);
    vec![
        root.clone(),
        root.join("data"),
        root.join("logs"),
        database.clone(),
        PathBuf::from(format!("{}-wal", database.display())),
        PathBuf::from(format!("{}-shm", database.display())),
        crate::store::paths::webview_user_data_dir(local_port),
    ]
}

fn repair_startup_storage_once(local_port: u16) -> Vec<String> {
    #[cfg(windows)]
    {
        let mut messages = Vec::new();
        for path in startup_storage_targets(local_port) {
            let (level, _) = read_integrity_level(&path);
            if !level.needs_repair() {
                continue;
            }
            match repair_integrity(&path) {
                Ok(detail) => messages.push(format!(
                    "已修复 Agent 存储完整性级别：{}（{}）",
                    path.display(),
                    detail
                )),
                Err(error) => messages.push(format!(
                    "Agent 存储完整性级别修复失败：{}：{}",
                    path.display(),
                    error
                )),
            }
        }
        return messages;
    }
    #[cfg(not(windows))]
    {
        let _ = local_port;
        Vec::new()
    }
}

fn storage_integrity_snapshot(local_port: u16) -> Value {
    let items = startup_storage_targets(local_port)
        .iter()
        .map(|path| integrity_snapshot(path))
        .collect::<Vec<_>>();
    let low_integrity_paths = items
        .iter()
        .filter(|item| {
            item.get("level")
                .and_then(Value::as_str)
                .is_some_and(|level| level == "low" || level == "untrusted")
        })
        .filter_map(|item| item.get("path").cloned())
        .collect::<Vec<_>>();
    json!({
        "supported": cfg!(windows),
        "items": items,
        "low_integrity_paths": low_integrity_paths,
    })
}

/// Mandatory integrity level of one storage path.
///
/// Deliberately not `icacls`-based, for two reasons that both showed up in the
/// field:
///
/// 1. The Agent is a GUI-subsystem process, so every console child it starts
///    without `CREATE_NO_WINDOW` gets a console window of its own. Scanning
///    seven paths at startup spawned seven `icacls.exe` processes, each with a
///    visible console window — a couple of seconds of black windows flashing
///    over the desktop before the main window could appear.
/// 2. `icacls` prints the translated name of the mandatory label
///    ("Low Mandatory Level" / "低强制级别"). Matching the English text
///    reported `unknown` on a non-English system, so the repair never ran.
///
/// The level is the last sub-authority of the mandatory-label SID, which is
/// exactly what the label name encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityLevel {
    Missing,
    /// No explicit label ACE: the object inherits its parent's label.
    Inherited,
    Untrusted,
    Low,
    Medium,
    High,
    System,
    Unknown,
}

impl IntegrityLevel {
    fn label(self) -> &'static str {
        match self {
            IntegrityLevel::Missing => "missing",
            IntegrityLevel::Inherited => "inherited",
            IntegrityLevel::Untrusted => "untrusted",
            IntegrityLevel::Low => "low",
            IntegrityLevel::Medium => "medium",
            IntegrityLevel::High => "high",
            IntegrityLevel::System => "system",
            IntegrityLevel::Unknown => "unknown",
        }
    }

    /// The levels that make SQLite sidecars, protocol registration and
    /// WebView2 unusable for a normal (Medium) launch.
    fn needs_repair(self) -> bool {
        matches!(self, IntegrityLevel::Untrusted | IntegrityLevel::Low)
    }
}

#[cfg(windows)]
fn integrity_snapshot(path: &Path) -> Value {
    let (level, rid) = read_integrity_level(path);
    json!({
        "path": path,
        "exists": level != IntegrityLevel::Missing,
        "level": level.label(),
        "mandatory_label_rid": rid,
        "query_ok": level != IntegrityLevel::Unknown,
    })
}

#[cfg(not(windows))]
fn integrity_snapshot(path: &Path) -> Value {
    json!({ "path": path, "exists": path.exists(), "level": "unsupported" })
}

#[cfg(windows)]
fn read_integrity_level(path: &Path) -> (IntegrityLevel, Option<u32>) {
    if !path.exists() {
        return (IntegrityLevel::Missing, None);
    }
    match mandatory_label_rid(path) {
        Ok(Some(rid)) => (integrity_level_for_rid(rid), Some(rid)),
        // No label ACE means the object sits at its parent's level, which a
        // normal launch can already write to.
        Ok(None) => (IntegrityLevel::Inherited, None),
        Err(_) => (IntegrityLevel::Unknown, None),
    }
}

#[cfg(windows)]
fn integrity_level_for_rid(rid: u32) -> IntegrityLevel {
    // SECURITY_MANDATORY_*_RID. Each level owns a 0x1000-wide range: the base
    // value is the level, `+0x100` is the write-up variant.
    const LOW: u32 = 0x1000;
    const MEDIUM: u32 = 0x2000;
    const HIGH: u32 = 0x3000;
    const SYSTEM: u32 = 0x4000;
    const PROTECTED: u32 = 0x5000;
    match rid {
        value if value < LOW => IntegrityLevel::Untrusted,
        value if value < MEDIUM => IntegrityLevel::Low,
        value if value < HIGH => IntegrityLevel::Medium,
        value if value < SYSTEM => IntegrityLevel::High,
        value if value < PROTECTED => IntegrityLevel::System,
        _ => IntegrityLevel::Unknown,
    }
}

/// Last sub-authority of the object's mandatory-label SID, or `None` when the
/// object carries no explicit label ACE.
#[cfg(windows)]
fn mandatory_label_rid(path: &Path) -> Result<Option<u32>, u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let name = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sacl,
            &mut descriptor,
        )
    };
    if status != 0 {
        if !descriptor.is_null() {
            unsafe { LocalFree(descriptor) };
        }
        return Err(status);
    }
    let rid = unsafe { label_rid_in_sacl(sacl) };
    unsafe { LocalFree(descriptor) };
    Ok(rid)
}

#[cfg(windows)]
unsafe fn label_rid_in_sacl(sacl: *const ACL) -> Option<u32> {
    use windows_sys::Win32::Security::{
        GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, ACE_HEADER, SYSTEM_MANDATORY_LABEL_ACE,
    };
    use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_ACE_TYPE;

    if sacl.is_null() {
        return None;
    }
    for index in 0..u32::from((*sacl).AceCount) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        if GetAce(sacl, index, &mut ace) == 0 || ace.is_null() {
            continue;
        }
        if u32::from((*(ace as *const ACE_HEADER)).AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
            continue;
        }
        // `SidStart` is the first byte of an inline SID, not a pointer.
        let label = &*(ace as *const SYSTEM_MANDATORY_LABEL_ACE);
        let sid = std::ptr::addr_of!(label.SidStart) as *mut core::ffi::c_void;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            continue;
        }
        return Some(*GetSidSubAuthority(sid, u32::from(count - 1)));
    }
    None
}

#[cfg(windows)]
fn repair_integrity(path: &Path) -> Result<String, Box<dyn Error>> {
    let level = if path.is_dir() { "(OI)(CI)M" } else { "M" };
    // `icacls` is a console program and the Agent has no console: without
    // CREATE_NO_WINDOW Windows hands this child a console window of its own.
    let output = crate::runtime::process::hidden_command("icacls")
        .arg(path)
        .arg("/setintegritylevel")
        .arg(level)
        .arg("/C")
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(windows))]
fn repair_integrity(_path: &Path) -> Result<String, Box<dyn Error>> {
    Ok("当前平台不需要修复".to_string())
}

fn protocol_registration_snapshot(root: &Path, options: &Options) -> Value {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey(r"Software\Classes\himind-agent");
        let Ok(protocol) = key else {
            return json!({ "supported": true, "registered": false, "reason": "registry key missing" });
        };
        let command = protocol
            .open_subkey(r"shell\open\command")
            .ok()
            .and_then(|key| key.get_value::<String, _>("").ok())
            .unwrap_or_default();
        let icon = protocol
            .open_subkey("DefaultIcon")
            .ok()
            .and_then(|key| key.get_value::<String, _>("").ok())
            .unwrap_or_default();
        let launcher = root.join(crate::install_layout::LAUNCHER_FILE);
        let expected_prefix = format!("\"{}\"", launcher.display());
        let registered = !command.is_empty()
            && command.starts_with(&expected_prefix)
            && command.contains("--protocol-url \"%1\"")
            && icon.eq_ignore_ascii_case(&root.join("himind-agent.ico").display().to_string());
        return json!({
            "supported": true,
            "registered": registered,
            "command_present": !command.is_empty(),
            "command_matches_current_launcher": command.starts_with(&expected_prefix),
            "protocol_url_placeholder": command.contains("--protocol-url \"%1\""),
            "icon_matches_current_install": icon.eq_ignore_ascii_case(&root.join("himind-agent.ico").display().to_string()),
            "api_base": sanitized_api_base(&options.api_base()),
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (root, options);
        json!({ "supported": false, "registered": true })
    }
}

fn webview_storage_snapshot(path: &Path) -> Value {
    let lock_names = ["SingletonLock", "SingletonCookie", "SingletonSocket"];
    let lock_files = lock_names
        .iter()
        .filter(|name| path.join(name).exists())
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let entries = fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .take(80)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "path": path,
        "exists": path.is_dir(),
        "lock_files": lock_files,
        "entries": entries,
    })
}

/// Read the event log without requiring the UI. This is deliberately bounded
/// so a long-lived Agent cannot make `diagnostics check` produce an unbounded
/// report. The latest launch/page events make a blank-window case actionable
/// without exposing credentials or request payloads.
fn startup_evidence() -> Value {
    let path = crate::store::paths::agent_home()
        .join("logs")
        .join("agent-events.jsonl");
    startup_evidence_from_path(&path)
}

fn startup_evidence_from_path(path: &Path) -> Value {
    let lines = fs::read_to_string(&path)
        .map(|text| {
            text.lines()
                .rev()
                .take(80)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut launch_count = 0_u32;
    let mut page_event_count = 0_u32;
    let mut last_launch = None;
    let mut last_page_event = None;
    for line in &lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = value.get("message").and_then(Value::as_str).unwrap_or("");
        if message.contains("界面启动") {
            launch_count += 1;
            if last_launch.is_none() {
                last_launch = Some(value.clone());
            }
        }
        if message.contains("界面页面加载") {
            page_event_count += 1;
            if last_page_event.is_none() {
                last_page_event = Some(value);
            }
        }
    }
    json!({
        "path": path,
        "launch_count_in_tail": launch_count,
        "page_event_count_in_tail": page_event_count,
        "page_event_seen": page_event_count > 0,
        "last_launch": last_launch,
        "last_page_event": last_page_event,
        "tail": lines,
    })
}

const BUILTIN_AI_SESSION_FAILURE_MARKER: &str = "HiMind AI 会话启动失败";
const BUILTIN_AI_SESSION_SUCCESS_MARKERS: [&str; 2] =
    ["HiMind AI 会话已启动", "HiMind AI 已进入扩展"];

/// HiMind AI 会话的最近结论，外加启动前置事实。
///
/// 界面上的「会话没有启动 / 服务暂时不可用」不带原因，真正的原因只有
/// `start_builtin_ai_session` 写进事件日志的那一条。这里把它捞出来，用户跑一次
/// `diagnostics check` 就能看到原因，不必翻日志或找人上机。
fn builtin_ai_session_snapshot(options: &Options) -> Value {
    let path = crate::store::paths::agent_home()
        .join("logs")
        .join("agent-events.jsonl");
    let mut snapshot = builtin_ai_session_snapshot_from_path(&path);
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("launch".to_string(), builtin_ai_launch_snapshot(options));
    }
    snapshot
}

/// 只认「最近一次会话结论」：逆序先撞上的那一条才算数，后面成功过一次就说明
/// 已经翻篇。扫描上限刻意设成和 startup_evidence 同一量级，避免长跑机器的
/// 日志把报告撑大。
fn builtin_ai_session_snapshot_from_path(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut last_failure: Option<Value> = None;
    let mut last_success: Option<Value> = None;
    for line in text.lines().rev().take(4_000) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = value.get("message").and_then(Value::as_str).unwrap_or("");
        if let Some((_, reason)) = message.split_once(BUILTIN_AI_SESSION_FAILURE_MARKER) {
            last_failure = Some(json!({
                "at": value.get("time").cloned().unwrap_or(Value::Null),
                "timestamp": value.get("timestamp").cloned().unwrap_or(Value::Null),
                "reason": crate::runtime::process::summarize_output(
                    reason.trim_start_matches([':', '：', ' ']),
                    600,
                ),
            }));
            break;
        }
        if BUILTIN_AI_SESSION_SUCCESS_MARKERS
            .iter()
            .any(|marker| message.contains(marker))
        {
            last_success = Some(json!({
                "at": value.get("time").cloned().unwrap_or(Value::Null),
                "message": message,
            }));
            break;
        }
    }
    json!({
        "path": path,
        "last_failure": last_failure,
        "last_success": last_success,
    })
}

/// 会话启动的前置事实：运行时能否解析、home 与工作区落在哪里、有没有可用的模型
/// 凭据。这里只走会话启动本身的第一步（解析与自愈，拉起进程之前），所以能在
/// 诊断里直接执行；不含密钥内容，只报凭据是否存在。
fn builtin_ai_launch_snapshot(options: &Options) -> Value {
    let workspace = crate::runtime::builtin::recent_interactive_workspace();
    match crate::runtime::builtin::prepare_interactive_launch_allow_degraded(
        options,
        workspace.as_deref(),
    ) {
        Ok(launch) => json!({
            "ok": true,
            "executable": launch.executable,
            "executable_present": launch.executable.is_file(),
            "home": launch.home,
            "workspace": launch.workspace,
            "permission_mode": launch.permission_mode,
            "service_source": launch.service_source,
            "default_model": launch.default_model,
            "api_key_present": !launch.api_key.trim().is_empty(),
            "api_key_env": launch.api_key_env,
            "base_url": sanitized_api_base(&launch.base_url),
            "control_plane_notice": launch.control_plane_notice,
        }),
        Err(error) => json!({
            "ok": false,
            "error": error,
            "workspace": workspace,
        }),
    }
}

pub(crate) fn export_bundle(
    destination: &Path,
    options: &Options,
    worker: &LocalWorkerStatus,
) -> Result<PathBuf, Box<dyn Error>> {
    export_bundle_from(destination, options, worker, "ui")
}

/// `origin` records how the bundle was collected. A bundle pulled from the
/// command line while the window is blank must be distinguishable from one the
/// user exported from a working UI, because the worker fields below are only
/// meaningful in the first case.
pub(crate) fn export_bundle_from(
    destination: &Path,
    options: &Options,
    worker: &LocalWorkerStatus,
    origin: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(destination)?;
    let mut archive = zip::ZipWriter::new(file);
    let zip_options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    archive.start_file("diagnostics.json", zip_options)?;
    archive.write_all(serde_json::to_vec_pretty(&snapshot(options, worker, origin))?.as_slice())?;
    add_logs(
        &mut archive,
        &crate::store::paths::agent_home().join("logs"),
        &crate::app::crash::crash_dir(),
        zip_options,
    )?;

    archive.start_file("feedback.txt", zip_options)?;
    archive.write_all(
        "请补充以下信息后提交诊断包：\r\n\
         1. 问题发生的大致时间\r\n\
         2. 当时执行的操作和展项名称\r\n\
         3. 是否安装加密或终端安全软件，以及产品名称\r\n\
         4. 问题是否可稳定复现\r\n\
         诊断包不会收集密码、Agent credential 或 OAuth token。\r\n"
            .as_bytes(),
    )?;
    archive.finish()?;
    Ok(destination.to_path_buf())
}

/// Everything a support case needs has to be in here, because the user should
/// not have to be walked through copying files by hand. `runtime-install.log`
/// carries the reason the Runtime import failed (the installer already reads its
/// last line for its failure dialog) and the crash directory is where a panic or
/// an abort leaves its report, so both belong in the bundle.
fn add_logs<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    log_dir: &Path,
    crash_dir: &Path,
    options: FileOptions,
) -> Result<(), Box<dyn Error>> {
    for name in [
        "agent-events.jsonl",
        "agent-events.jsonl.1",
        "agent-events.jsonl.2",
        "runtime-install.log",
        "agent-updater.log",
        "svn-events.jsonl",
        "svn-events.jsonl.1",
        "svn-events.jsonl.2",
    ] {
        add_file(
            archive,
            &log_dir.join(name),
            &format!("logs/{name}"),
            options,
        )?;
    }
    if let Ok(entries) = fs::read_dir(crash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            add_file(archive, &path, &format!("logs/crashes/{name}"), options)?;
        }
    }
    Ok(())
}

fn add_file<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    source: &Path,
    entry: &str,
    options: FileOptions,
) -> Result<(), Box<dyn Error>> {
    if !source.is_file() {
        return Ok(());
    }
    archive.start_file(entry, options)?;
    let mut file = fs::File::open(source)?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)?;
    archive.write_all(&content)?;
    Ok(())
}

fn snapshot(options: &Options, worker: &LocalWorkerStatus, origin: &str) -> Value {
    let state = match load_agent_state(&options.state_path) {
        Ok(state) => json!({
            "readable": true,
            "agent_id": state.agent_id,
            "device_id": state.device_id,
            "credential_present": !state.credential.is_empty(),
            "credential_pending": !state.credential_pending.is_empty(),
            "credential_updated_at": state.credential_updated_at,
        }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let authorization = match crate::api::oauth::authorization_snapshot(&options.state_path) {
        Ok(Some(value)) => json!({
            "readable": true,
            "present": true,
            "agent_id": value.agent_id,
            "user_id": value.user_id,
            "scope": value.scope,
            "refresh_expires_at": value.refresh_expires_at,
            "updated_at": value.updated_at,
            "last_verified_at": value.last_verified_at,
        }),
        Ok(None) => json!({ "readable": true, "present": false }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let trust = match crate::app::trust::status() {
        Ok(report) => json!({
            "readable": true,
            "healthy": report.healthy,
            "issues": report.issues,
            "local_key_ids": report.local_key_ids,
            "active_key_id": report.state.active_key_id,
            "key_ids": report.state.key_ids,
            "revoked_key_ids": report.state.revoked_key_ids,
            "bundle_digest": report.state.bundle_digest,
            "generated_at": report.state.generated_at,
            "synced_at": report.state.synced_at,
        }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let state_file = &options.state_path;
    json!({
        "generated_at": unix_now(),
        "collected_by": origin,
        "agent": {
            "version": VERSION,
            "profile": crate::store::paths::profile_name(),
            "api_base": sanitized_api_base(&options.api_base()),
            "local_port": options.local_port,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "native_dpapi": cfg!(windows),
        },
        // The Runtime is the other half of the install and the usual suspect
        // when a session will not start, so report what the Agent currently
        // believes about it rather than only the Agent's own identity.
        "runtime": runtime_snapshot(),
        "webview": {
            "user_data_dir": webview_dir(options.local_port),
        },
        "worker": {
            "online": worker.dashboard_worker_online,
            "agent_id": worker.dashboard_agent_id,
            "error": crate::approval::manager::redact_message(&worker.dashboard_worker_error),
            "local_service_online": worker.local_service_online,
            "local_service_error": crate::approval::manager::redact_message(&worker.local_service_error),
        },
        "storage": {
            "state_file_exists": state_file.is_file(),
            "state_backup_exists": crate::store::atomic_file::backup_path(state_file).is_file(),
            "authorization_file_exists":
                crate::api::oauth::authorization_path(state_file).is_file(),
        },
        "device_identity": state,
        "user_authorization": authorization,
        "distribution_trust": trust,
    })
}

/// What the Agent currently believes about the Runtime.
///
/// `status()` shells out to the Runtime CLI, which is exactly the thing that
/// usually fails on a broken machine, so it is captured best-effort: a failure
/// is recorded as the reason rather than aborting the whole bundle.
fn runtime_snapshot() -> Value {
    match serde_json::to_value(crate::runtime::builtin::status()) {
        Ok(value) => value,
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    }
}

/// The WebView2 user data directory this process would use.
///
/// A second Agent instance pointing at the same directory makes WebView2
/// creation fail with no error anywhere, which shows up as a tray icon with no
/// main window. Recording the path is what distinguishes that from a rendering
/// failure.
fn webview_dir(port: u16) -> String {
    crate::store::paths::webview_user_data_dir(port)
        .to_string_lossy()
        .to_string()
}

fn sanitized_api_base(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_ai_session_snapshot_from_path, export_bundle_from, startup_evidence_from_path,
    };
    use crate::api::client::save_agent_state;
    use crate::api::types::AgentState;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use std::fs;
    use std::io::Read;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, RwLock};

    #[test]
    fn startup_evidence_reports_the_newest_launch_and_page_event() {
        let root = std::env::temp_dir().join(format!(
            "himind-startup-evidence-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let log = root.join("agent-events.jsonl");
        fs::write(
            &log,
            concat!(
                "{\"message\":\"界面启动：version=0.3.67\"}\n",
                "{\"message\":\"界面页面加载：window=main event=Finished\"}\n",
                "{\"message\":\"界面启动：version=0.3.69\"}\n",
                "{\"message\":\"界面页面加载：window=main event=Started\"}\n"
            ),
        )
        .unwrap();

        let evidence = startup_evidence_from_path(&log);
        assert_eq!(evidence["launch_count_in_tail"], 2);
        assert_eq!(evidence["page_event_count_in_tail"], 2);
        assert!(evidence["last_launch"]["message"]
            .as_str()
            .unwrap()
            .contains("0.3.69"));
        assert!(evidence["last_page_event"]["message"]
            .as_str()
            .unwrap()
            .contains("Started"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn builtin_ai_session_snapshot_only_reports_an_uncleared_failure() {
        let root = std::env::temp_dir().join(format!(
            "himind-session-evidence-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let log = root.join("agent-events.jsonl");
        fs::write(
            &log,
            concat!(
                "{\"time\":\"10:00:00\",\"timestamp\":1,\"message\":\"HiMind AI 会话启动失败: HiMind AI 启动超时（等待 20 秒），请检查运行时状态\"}\n",
                "{\"time\":\"10:01:00\",\"timestamp\":2,\"message\":\"HiMind AI 会话已启动\"}\n",
                "{\"time\":\"10:02:00\",\"timestamp\":3,\"message\":\"HiMind AI 会话启动失败: 无法启动 HiMind AI：拒绝访问。 (os error 5)\"}\n"
            ),
        )
        .unwrap();

        let snapshot = builtin_ai_session_snapshot_from_path(&log);
        assert!(snapshot["last_failure"]["reason"]
            .as_str()
            .unwrap()
            .contains("os error 5"));
        assert!(snapshot["last_success"].is_null());

        fs::write(
            &log,
            concat!(
                "{\"timestamp\":3,\"message\":\"HiMind AI 会话启动失败: 无法启动 HiMind AI：拒绝访问。 (os error 5)\"}\n",
                "{\"timestamp\":4,\"message\":\"HiMind AI 已进入扩展工作区: F:\\\\work\"}\n"
            ),
        )
        .unwrap();
        let cleared = builtin_ai_session_snapshot_from_path(&log);
        assert!(cleared["last_failure"].is_null());
        assert!(cleared["last_success"]["message"]
            .as_str()
            .unwrap()
            .contains("已进入扩展工作区"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bundle_contains_health_data_without_credentials() {
        let root = std::env::temp_dir().join(format!(
            "himind-diagnostics-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let credential = "device-credential-that-must-not-leak";
        save_agent_state(
            &state_path,
            &AgentState {
                agent_id: "agt-test".to_string(),
                credential: credential.to_string(),
                credential_protected: String::new(),
                credential_pending: String::new(),
                credential_pending_protected: String::new(),
                credential_updated_at: super::unix_now(),
                device_id: "device-test".to_string(),
                access_token: String::new(),
                access_token_expires_in: 0,
                access_scope: String::new(),
                user_id: String::new(),
            },
        )
        .unwrap();
        let options = Options {
            api_base: crate::api_base_cell("http://user:password@127.0.0.1:18083?token=secret"),
            state_path,
            workbench_mode: crate::app::runtime_mode::mode_cell(
                crate::app::runtime_mode::AgentMode::Connected,
            ),
            once: false,
            interval_seconds: 10,
            local_app: true,
            hidden: false,
            local_port: 18082,
            reenroll: false,
            enrollment_token: String::new(),
            agent_credential: Arc::new(RwLock::new(String::new())),
            identity_generation: Arc::new(AtomicU64::new(0)),
            platform_access: Arc::new(RwLock::new(None)),
            task_execution: Arc::new(RwLock::new(None)),
        };
        let destination = root.join("diagnostics.zip");
        export_bundle_from(&destination, &options, &LocalWorkerStatus::default(), "cli").unwrap();
        let file = fs::File::open(&destination).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut diagnostics = String::new();
        archive
            .by_name("diagnostics.json")
            .unwrap()
            .read_to_string(&mut diagnostics)
            .unwrap();
        assert!(diagnostics.contains("agt-test"));
        assert!(diagnostics.contains("distribution_trust"));
        assert!(!diagnostics.contains(credential));
        assert!(!diagnostics.contains("password"));
        assert!(!diagnostics.contains("token=secret"));
        // A bundle pulled from the command line must say so: it was collected
        // because the window would not come up, so its worker fields are
        // placeholders rather than observations, and the runtime and WebView2
        // facts are what the reader should go on.
        assert!(diagnostics.contains("\"collected_by\": \"cli\""));
        assert!(diagnostics.contains("\"runtime\""));
        assert!(diagnostics.contains("user_data_dir"));
        let _ = fs::remove_dir_all(root);
    }

    /// The bundle is what a support case is opened with, so it has to carry the
    /// logs that explain a failed install — a user should never be walked
    /// through copying files by hand.
    #[test]
    fn bundle_carries_install_logs_and_crash_reports() {
        let root = std::env::temp_dir().join(format!(
            "himind-diagnostics-logs-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        let logs = root.join("logs");
        let crashes = root.join("crashes");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&crashes).unwrap();
        fs::write(logs.join("runtime-install.log"), "error: 运行时安装失败").unwrap();
        fs::write(logs.join("agent-updater.log"), "updater line").unwrap();
        fs::write(logs.join("agent-events.jsonl"), "{\"level\":\"info\"}\n").unwrap();
        fs::write(logs.join("svn-events.jsonl"), "{}\n").unwrap();
        fs::write(crashes.join("crash-1.txt"), "panic report").unwrap();

        let destination = root.join("bundle.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&destination).unwrap());
        super::add_logs(&mut writer, &logs, &crashes, super::FileOptions::default()).unwrap();
        writer.finish().unwrap();

        let mut archive = zip::ZipArchive::new(fs::File::open(&destination).unwrap()).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect::<Vec<_>>();
        for expected in [
            "logs/runtime-install.log",
            "logs/agent-updater.log",
            "logs/agent-events.jsonl",
            "logs/svn-events.jsonl",
            "logs/crashes/crash-1.txt",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "bundle is missing {expected}: {names:?}"
            );
        }

        // A log that does not exist must not break the bundle: a fresh install
        // has no crash reports and no updater log yet.
        let mut sparse = zip::ZipWriter::new(fs::File::create(root.join("sparse.zip")).unwrap());
        assert!(super::add_logs(
            &mut sparse,
            &root.join("nowhere"),
            &root.join("nowhere-crashes"),
            super::FileOptions::default(),
        )
        .is_ok());

        let _ = fs::remove_dir_all(root);
    }
    /// The startup scan has to read the integrity level without starting a
    /// console program. A console child of this GUI process is handed a console
    /// window of its own, so the `icacls`-per-path query this replaced flashed a
    /// black window for every path it checked. Guard both halves: a normal file
    /// is not flagged, and a Low label is still recognised.
    #[cfg(windows)]
    #[test]
    fn integrity_reader_sees_labels_without_a_console_query() {
        let root =
            std::env::temp_dir().join(format!("himind-integrity-reader-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("probe.txt");
        fs::write(&file, b"probe").unwrap();

        let (level, rid) = super::read_integrity_level(&file);
        assert!(
            !level.needs_repair(),
            "a freshly created file must not be scheduled for repair: {level:?} rid={rid:?}"
        );

        let labelled = std::process::Command::new("icacls")
            .arg(&file)
            .arg("/setintegritylevel")
            .arg("L")
            .arg("/C")
            .output()
            .expect("icacls ships with Windows");
        assert!(
            labelled.status.success(),
            "could not set a Low label for the test: {}",
            String::from_utf8_lossy(&labelled.stderr)
        );

        let (level, rid) = super::read_integrity_level(&file);
        assert_eq!(rid, Some(0x1000), "a Low label reads as SID RID 0x1000");
        assert!(level.needs_repair(), "a Low label must trigger a repair");

        assert_eq!(
            super::read_integrity_level(&root.join("missing.txt")).0,
            super::IntegrityLevel::Missing
        );

        let _ = fs::remove_dir_all(&root);
    }
}
