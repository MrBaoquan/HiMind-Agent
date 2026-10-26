//! 插件凭据：插件在清单里声明需求，值由 Agent 统一保管。
//!
//! 为什么不让插件自己存：凭据的**轮换、审计、卸载清理**都需要一个统一持有者。
//! 散落在各插件目录里的配置文件做不到其中任何一条，而且插件一升级就可能把用户
//! 填过的 key 覆盖掉。本模块负责声明后的保管、状态、声明指纹和取用审计：
//!
//! - 值以 DPAPI 按当前 Windows 用户加密后落盘（与 AI 服务密钥、GitHub 令牌同一套保护）；
//! - 按插件 ID 隔离：A 插件授权不等于 B 插件可用；
//! - 界面与插件都只能看到「配没配 / 配了哪些字段 / 什么时候更新的」，没有明文；
//! - 明文只在进程内存里短暂存在，不写日志、不进错误信息。
//!
//! 外部插件的运行时取值由本模块之上的 per-invocation broker 完成；Agent 只在一个
//! 短生命周期进程内按声明和授权下发，不让插件直接持有长期密钥。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::io::Write;
use std::path::PathBuf;

use crate::store::atomic_file;
use crate::store::credentials::{
    protect_secret_for_current_user, unprotect_secret_for_current_user,
};

const STORE_FILE: &str = "plugin-credentials.json";
const SCHEMA_VERSION: u32 = 1;
/// 一条凭据最多几个字段、每个字段多长：防呆，不防恶意插件（插件本来就跑在用户机器上）。
const MAX_FIELDS: usize = 8;
const MAX_VALUE_BYTES: usize = 16 * 1024;
const MAX_ID_BYTES: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default = "schema_version")]
    version: u32,
    #[serde(default)]
    plugins: BTreeMap<String, PluginCredentials>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PluginCredentials {
    #[serde(default)]
    entries: BTreeMap<String, CredentialEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CredentialEntry {
    /// Public account metadata such as username, endpoint or tenant.
    #[serde(default)]
    public_fields: BTreeMap<String, String>,
    /// 字段名 → DPAPI 保护后的值。字段名本身不是秘密，值才是。
    #[serde(default)]
    protected_fields: BTreeMap<String, String>,
    /// Fingerprint of the declaration used when this profile was saved.
    #[serde(default)]
    descriptor_fingerprint: String,
    #[serde(default)]
    updated_at: String,
}

/// 给界面和插件状态查询看的凭据状态：只有公开元数据与配置状态，没有秘密值。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PluginCredentialStatus {
    pub id: String,
    pub configured: bool,
    pub configured_fields: Vec<String>,
    pub public_fields: BTreeMap<String, String>,
    /// False when the plugin changed the declaration after this profile was saved.
    pub descriptor_current: bool,
    pub updated_at: String,
}

fn store_path() -> PathBuf {
    crate::store::paths::agent_home().join(STORE_FILE)
}

fn read_store() -> Result<StoreFile, Box<dyn Error>> {
    let path = store_path();
    if !path.is_file() {
        return Ok(StoreFile::default());
    }
    let content = std::fs::read_to_string(&path)?;
    serde_json::from_str(&content)
        .map_err(|error| format!("插件凭据存储损坏，已拒绝覆盖：{error}").into())
}

fn write_store(store: &StoreFile) -> Result<(), Box<dyn Error>> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = atomic_file::lock(&path)?;
    atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(store)?)?;
    Ok(())
}

fn validate_segment(label: &str, value: &str) -> Result<String, Box<dyn Error>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_ID_BYTES {
        return Err(format!("{label} 不能为空且不超过 {MAX_ID_BYTES} 字节").into());
    }
    if trimmed
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'/' || byte == b'\\')
    {
        return Err(format!("{label} 含有非法字符").into());
    }
    Ok(trimmed.to_string())
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// 某个插件某条凭据的状态（只回答「配没配」）。
pub(crate) fn status_of(
    store: &BTreeMap<String, PluginCredentials>,
    plugin_id: &str,
    credential_id: &str,
) -> PluginCredentialStatus {
    status_of_with_fingerprint(store, plugin_id, credential_id, "")
}

fn status_of_with_fingerprint(
    store: &BTreeMap<String, PluginCredentials>,
    plugin_id: &str,
    credential_id: &str,
    expected_fingerprint: &str,
) -> PluginCredentialStatus {
    let entry = store
        .get(plugin_id)
        .and_then(|plugin| plugin.entries.get(credential_id));
    PluginCredentialStatus {
        id: credential_id.to_string(),
        configured: entry.is_some_and(|entry| !entry.protected_fields.is_empty()),
        configured_fields: entry
            .map(|entry| entry.protected_fields.keys().cloned().collect())
            .unwrap_or_default(),
        public_fields: entry
            .map(|entry| entry.public_fields.clone())
            .unwrap_or_default(),
        descriptor_current: entry.is_some_and(|entry| {
            expected_fingerprint.trim().is_empty()
                || entry.descriptor_fingerprint.trim().is_empty()
                || entry.descriptor_fingerprint == expected_fingerprint
        }),
        updated_at: entry
            .map(|entry| entry.updated_at.clone())
            .unwrap_or_default(),
    }
}

/// 一次读出某插件全部凭据状态，供界面拼装「声明 + 状态」。
pub(crate) fn statuses(
    plugin_id: &str,
    credential_ids: &[String],
) -> Result<Vec<PluginCredentialStatus>, Box<dyn Error>> {
    statuses_with_fingerprints(
        plugin_id,
        &credential_ids
            .iter()
            .map(|id| (id.clone(), String::new()))
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn statuses_with_fingerprints(
    plugin_id: &str,
    credentials: &[(String, String)],
) -> Result<Vec<PluginCredentialStatus>, Box<dyn Error>> {
    let store = read_store()?;
    Ok(credentials
        .iter()
        .map(|(id, fingerprint)| {
            status_of_with_fingerprint(&store.plugins, plugin_id, id, fingerprint)
        })
        .collect())
}

/// 写入（或覆盖）一条凭据。值在这里就加密，明文不落盘。
pub(crate) fn save(
    plugin_id: &str,
    credential_id: &str,
    fields: &BTreeMap<String, String>,
) -> Result<PluginCredentialStatus, Box<dyn Error>> {
    save_profile(plugin_id, credential_id, &BTreeMap::new(), fields, "")
}

pub(crate) fn save_profile(
    plugin_id: &str,
    credential_id: &str,
    public_fields: &BTreeMap<String, String>,
    secret_fields: &BTreeMap<String, String>,
    descriptor_fingerprint: &str,
) -> Result<PluginCredentialStatus, Box<dyn Error>> {
    let plugin_id = validate_segment("插件 ID", plugin_id)?;
    let credential_id = validate_segment("凭据 ID", credential_id)?;
    if public_fields.is_empty() && secret_fields.is_empty() {
        return Err("凭据至少要有一个字段".into());
    }
    if public_fields.len() + secret_fields.len() > MAX_FIELDS {
        return Err(format!("凭据字段最多 {MAX_FIELDS} 个").into());
    }
    let mut normalized_public = BTreeMap::new();
    for (name, value) in public_fields {
        let name = validate_segment("字段名", name)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("字段 {name} 的值超过 {MAX_VALUE_BYTES} 字节").into());
        }
        normalized_public.insert(name, value.trim().to_string());
    }
    let mut protected_fields = BTreeMap::new();
    for (name, value) in secret_fields {
        let name = validate_segment("字段名", name)?;
        if value.trim().is_empty() {
            return Err(format!("字段 {name} 的值不能为空").into());
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("字段 {name} 的值超过 {MAX_VALUE_BYTES} 字节").into());
        }
        protected_fields.insert(name, protect_secret_for_current_user(value)?);
    }
    let mut store = read_store()?;
    let existing = store
        .plugins
        .get(&plugin_id)
        .and_then(|plugin| plugin.entries.get(&credential_id))
        .cloned()
        .unwrap_or_default();
    let mut merged_public = existing.public_fields;
    merged_public.extend(normalized_public);
    let mut merged_secret = existing.protected_fields;
    merged_secret.extend(protected_fields);
    store
        .plugins
        .entry(plugin_id.clone())
        .or_default()
        .entries
        .insert(
            credential_id.clone(),
            CredentialEntry {
                public_fields: merged_public,
                protected_fields: merged_secret,
                descriptor_fingerprint: descriptor_fingerprint.trim().to_string(),
                updated_at: now_stamp(),
            },
        );
    write_store(&store)?;
    Ok(status_of_with_fingerprint(
        &store.plugins,
        &plugin_id,
        &credential_id,
        descriptor_fingerprint,
    ))
}

/// 删除一条凭据；返回是否真的删掉了东西（用于界面提示）。
pub(crate) fn remove(plugin_id: &str, credential_id: &str) -> Result<bool, Box<dyn Error>> {
    let plugin_id = validate_segment("插件 ID", plugin_id)?;
    let credential_id = validate_segment("凭据 ID", credential_id)?;
    let mut store = read_store()?;
    let removed = store
        .plugins
        .get_mut(&plugin_id)
        .and_then(|plugin| plugin.entries.remove(&credential_id))
        .is_some();
    if removed {
        if store
            .plugins
            .get(&plugin_id)
            .is_some_and(|plugin| plugin.entries.is_empty())
        {
            store.plugins.remove(&plugin_id);
        }
        write_store(&store)?;
    }
    Ok(removed)
}

/// 插件卸载/移除时清掉它名下的凭据，别留孤儿秘密。
pub(crate) fn remove_all_for_plugin(plugin_id: &str) -> Result<bool, Box<dyn Error>> {
    let plugin_id = validate_segment("插件 ID", plugin_id)?;
    let mut store = read_store()?;
    let removed = store.plugins.remove(&plugin_id).is_some();
    if removed {
        write_store(&store)?;
    }
    Ok(removed)
}

/// 读取秘密字段。仅供内核和 broker 使用；插件进程必须通过 broker 获取。
pub(crate) fn secret_fields(
    plugin_id: &str,
    credential_id: &str,
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let plugin_id = validate_segment("插件 ID", plugin_id)?;
    let credential_id = validate_segment("凭据 ID", credential_id)?;
    let store = read_store()?;
    let entry = store
        .plugins
        .get(&plugin_id)
        .and_then(|plugin| plugin.entries.get(&credential_id))
        .ok_or_else(|| format!("凭据未配置: {plugin_id}/{credential_id}"))?;
    let mut fields = BTreeMap::new();
    for (name, value) in &entry.protected_fields {
        fields.insert(name.clone(), unprotect_secret_for_current_user(value)?);
    }
    Ok(fields)
}

/// Resolve the public and secret values of one profile for a brokered consumer.
pub(crate) fn broker_fields(
    plugin_id: &str,
    credential_id: &str,
    expected_fingerprint: &str,
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let plugin_id = validate_segment("插件 ID", plugin_id)?;
    let credential_id = validate_segment("凭据 ID", credential_id)?;
    let store = read_store()?;
    let status = status_of_with_fingerprint(
        &store.plugins,
        &plugin_id,
        &credential_id,
        expected_fingerprint,
    );
    if !status.configured {
        return Err(format!("凭据未配置: {plugin_id}/{credential_id}").into());
    }
    if !status.descriptor_current {
        return Err("插件更新了凭据声明，请先在账号与凭据中心确认".into());
    }
    let entry = store
        .plugins
        .get(&plugin_id)
        .and_then(|plugin| plugin.entries.get(&credential_id))
        .ok_or_else(|| format!("凭据未配置: {plugin_id}/{credential_id}"))?;
    let mut fields = entry.public_fields.clone();
    for (name, value) in &entry.protected_fields {
        fields.insert(name.clone(), unprotect_secret_for_current_user(value)?);
    }
    Ok(fields)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CredentialAuditEvent {
    pub at: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub credential_id: String,
    pub capability: String,
    pub result: String,
    pub reason: String,
}

fn audit_path() -> PathBuf {
    crate::store::paths::agent_home().join("credential-audit.jsonl")
}

pub(crate) fn record_audit(
    owner_kind: &str,
    owner_id: &str,
    credential_id: &str,
    capability: &str,
    result: &str,
    reason: &str,
) -> Result<(), Box<dyn Error>> {
    let path = audit_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = atomic_file::lock(&path)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let event = CredentialAuditEvent {
        at: now_stamp(),
        owner_kind: owner_kind.trim().to_string(),
        owner_id: owner_id.trim().to_string(),
        credential_id: credential_id.trim().to_string(),
        capability: capability.trim().to_string(),
        result: result.trim().to_string(),
        reason: reason.trim().to_string(),
    };
    writeln!(file, "{}", serde_json::to_string(&event)?)?;
    Ok(())
}

pub(crate) fn audit_events(limit: usize) -> Result<Vec<CredentialAuditEvent>, Box<dyn Error>> {
    let path = audit_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)?;
    let mut events = content
        .lines()
        .rev()
        .take(limit.clamp(1, 500))
        .filter_map(|line| serde_json::from_str::<CredentialAuditEvent>(line).ok())
        .collect::<Vec<_>>();
    events.reverse();
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HIMIND_AGENT_HOME 是进程级环境变量：并行的用例会互相顶掉，必须与全仓
    /// 其它改动该变量的测试共用同一把锁，否则会互相看到对方的临时 home。
    /// 测试用：把 agent_home 指到临时目录，跑完还原。
    struct PinnedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
        root: PathBuf,
    }

    impl PinnedHome {
        fn new(label: &str) -> Self {
            let lock = crate::store::paths::test_env_lock();
            let root = std::env::temp_dir().join(format!(
                "himind-plugin-credentials-{label}-{}-{}",
                std::process::id(),
                now_stamp()
            ));
            let _ = std::fs::create_dir_all(&root);
            let previous = std::env::var_os("HIMIND_AGENT_HOME");
            std::env::set_var("HIMIND_AGENT_HOME", &root);
            Self {
                _lock: lock,
                previous,
                root,
            }
        }
    }

    impl Drop for PinnedHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
                None => std::env::remove_var("HIMIND_AGENT_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn saves_lists_and_removes_a_credential_without_leaking_plaintext() {
        let _home = PinnedHome::new("roundtrip");
        let mut fields = BTreeMap::new();
        fields.insert("api_key".to_string(), "sk-secret-value".to_string());

        let status = save("com.test.plugin", "demo.api", &fields).unwrap();
        assert!(status.configured);
        assert_eq!(status.configured_fields, vec!["api_key".to_string()]);

        // 落盘内容里不能出现明文。
        let raw = std::fs::read_to_string(store_path()).unwrap();
        assert!(!raw.contains("sk-secret-value"), "凭据明文不得落盘: {raw}");
        assert!(raw.contains("com.test.plugin"));

        let listed = statuses("com.test.plugin", &["demo.api".to_string()]).unwrap();
        assert!(listed[0].configured);

        // 内核侧取值仍然拿得到原值（broker 通过这条路径解析）。
        let secrets = secret_fields("com.test.plugin", "demo.api").unwrap();
        assert_eq!(
            secrets.get("api_key").map(String::as_str),
            Some("sk-secret-value")
        );

        assert!(remove("com.test.plugin", "demo.api").unwrap());
        assert!(!statuses("com.test.plugin", &["demo.api".to_string()]).unwrap()[0].configured);
    }

    #[test]
    fn isolates_plugins_and_rejects_unsafe_ids() {
        let _home = PinnedHome::new("isolation");
        let mut fields = BTreeMap::new();
        fields.insert("token".to_string(), "abc".to_string());
        save("com.test.a", "shared", &fields).unwrap();

        // 另一个插件查同一个凭据 ID：必须是「未配置」。
        assert!(
            !statuses("com.test.b", &["shared".to_string()]).unwrap()[0].configured,
            "凭据必须按插件隔离"
        );
        assert!(secret_fields("com.test.b", "shared").is_err());
        assert!(save("../escape", "shared", &fields).is_err());
        assert!(save("com.test.a", "shared/../x", &fields).is_err());
    }

    #[test]
    fn uninstall_cleanup_drops_every_credential_of_a_plugin() {
        let _home = PinnedHome::new("cleanup");
        let mut fields = BTreeMap::new();
        fields.insert("token".to_string(), "abc".to_string());
        save("com.test.a", "one", &fields).unwrap();
        save("com.test.a", "two", &fields).unwrap();

        assert!(remove_all_for_plugin("com.test.a").unwrap());
        assert!(!statuses("com.test.a", &["one".to_string()]).unwrap()[0].configured);
        assert!(!statuses("com.test.a", &["two".to_string()]).unwrap()[0].configured);
    }
    #[test]
    fn public_fields_and_declaration_fingerprint_are_status_only() {
        let _home = PinnedHome::new("metadata");
        let mut public_fields = BTreeMap::new();
        public_fields.insert("username".to_string(), "alice".to_string());
        let mut secret_fields = BTreeMap::new();
        secret_fields.insert("password".to_string(), "super-secret".to_string());
        let status = save_profile(
            "com.test.plugin",
            "demo.account",
            &public_fields,
            &secret_fields,
            "fingerprint-a",
        )
        .unwrap();
        assert!(status.configured);
        assert_eq!(
            status.public_fields.get("username").map(String::as_str),
            Some("alice")
        );
        assert!(status.descriptor_current);

        let changed = statuses_with_fingerprints(
            "com.test.plugin",
            &[("demo.account".to_string(), "fingerprint-b".to_string())],
        )
        .unwrap();
        assert!(!changed[0].descriptor_current);
        let raw = std::fs::read_to_string(store_path()).unwrap();
        assert!(!raw.contains("super-secret"));
    }

    #[test]
    fn audit_events_never_leak_values() {
        let _home = PinnedHome::new("audit");
        record_audit(
            "plugin",
            "com.test.plugin",
            "demo.api",
            "demo.run",
            "allowed",
            "",
        )
        .unwrap();
        let events = audit_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].result, "allowed");
        let raw = std::fs::read_to_string(audit_path()).unwrap();
        assert!(!raw.contains("super-secret"));
    }
}
