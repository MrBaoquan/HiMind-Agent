//! Unified credential catalog for Settings → 账号与凭据.
//!
//! This is a logical layer over the existing stores. It intentionally does not
//! move secrets between those stores yet: workbench, AI, built-in integrations,
//! plugins and workflow connectors keep their proven adapters while the UI and
//! broker receive one stable, secret-free view.

use serde::Serialize;
use std::collections::BTreeMap;

use crate::store::plugin_credentials::{CredentialAuditEvent, PluginCredentialStatus};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialFieldView {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub secret: bool,
    pub required: bool,
    pub configured: bool,
    pub value: String,
    pub placeholder: String,
    pub help: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialCatalogItem {
    pub id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub owner_name: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
    pub configured: bool,
    pub status: String,
    pub description: String,
    pub docs_url: String,
    pub manage_action: String,
    pub deep_link: String,
    pub editable: bool,
    pub fields: Vec<CredentialFieldView>,
    pub configured_fields: Vec<String>,
    pub public_fields: BTreeMap<String, String>,
    pub descriptor_current: bool,
    pub verify_capability: String,
    pub updated_at: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialCatalogSnapshot {
    pub items: Vec<CredentialCatalogItem>,
    pub warnings: Vec<String>,
    pub generated_at: String,
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn empty_item(
    id: impl Into<String>,
    owner_kind: &str,
    owner_id: impl Into<String>,
    owner_name: impl Into<String>,
    label: impl Into<String>,
    kind: &str,
    manage_action: &str,
) -> CredentialCatalogItem {
    CredentialCatalogItem {
        id: id.into(),
        owner_kind: owner_kind.to_string(),
        owner_id: owner_id.into(),
        owner_name: owner_name.into(),
        label: label.into(),
        kind: kind.to_string(),
        required: true,
        configured: false,
        status: "missing".to_string(),
        description: String::new(),
        docs_url: String::new(),
        manage_action: manage_action.to_string(),
        deep_link: String::new(),
        editable: false,
        fields: Vec::new(),
        configured_fields: Vec::new(),
        public_fields: BTreeMap::new(),
        descriptor_current: true,
        verify_capability: String::new(),
        updated_at: String::new(),
        last_used_at: String::new(),
    }
}

fn last_used_index() -> BTreeMap<(String, String, String), String> {
    let mut index = BTreeMap::new();
    if let Ok(events) = crate::store::plugin_credentials::audit_events(500) {
        for event in events {
            index.insert(
                (
                    event.owner_kind.clone(),
                    event.owner_id.clone(),
                    event.credential_id.clone(),
                ),
                event.at,
            );
        }
    }
    index
}

fn field_views(
    fields: &[crate::capability::plugin::PluginCredentialField],
    status: Option<&PluginCredentialStatus>,
) -> Vec<CredentialFieldView> {
    fields
        .iter()
        .map(|field| {
            let configured = status.is_some_and(|status| {
                if field.secret {
                    status.configured_fields.iter().any(|key| key == &field.key)
                } else {
                    status.public_fields.contains_key(&field.key)
                }
            });
            CredentialFieldView {
                key: field.key.clone(),
                label: field.display_label().to_string(),
                field_type: field.field_type.clone(),
                secret: field.secret,
                required: field.required,
                configured,
                value: if field.secret {
                    String::new()
                } else {
                    status
                        .and_then(|status| status.public_fields.get(&field.key))
                        .cloned()
                        .unwrap_or_default()
                },
                placeholder: field.placeholder.clone(),
                help: field.help.clone(),
            }
        })
        .collect()
}

fn core_items(
    warnings: &mut Vec<String>,
    last_used: &BTreeMap<(String, String, String), String>,
) -> Vec<CredentialCatalogItem> {
    let mut items = Vec::new();

    let workbench_store = crate::store::workbenches::load(
        &crate::store::paths::agent_home()
            .join("data")
            .join("agent-state.json"),
    );
    match workbench_store {
        Ok(Some(store)) => {
            if let Some(active) = store.active_connection() {
                let view = store.views().into_iter().find(|view| view.id == active.id);
                let mut item = empty_item(
                    "core.workbench",
                    "core",
                    "himind.workbench",
                    "HiMind 工作台",
                    "工作台账号",
                    "oauth",
                    "workbench",
                );
                item.configured = view.as_ref().is_some_and(|view| view.authorized);
                item.status = if item.configured {
                    "valid".to_string()
                } else if view.as_ref().is_some_and(|view| view.registered) {
                    "unverified".to_string()
                } else {
                    "missing".to_string()
                };
                item.description =
                    "用于接收工作台任务、同步运行记录和读取获准的组织能力。".to_string();
                item.public_fields.insert(
                    "account".to_string(),
                    view.as_ref()
                        .map(|view| {
                            if view.user_name.trim().is_empty() {
                                view.display_name.clone()
                            } else {
                                view.user_name.clone()
                            }
                        })
                        .unwrap_or_default(),
                );
                item.public_fields.insert(
                    "endpoint".to_string(),
                    view.as_ref()
                        .map(|view| view.api_base.clone())
                        .unwrap_or_default(),
                );
                item.fields.push(CredentialFieldView {
                    key: "account".to_string(),
                    label: "当前账号".to_string(),
                    field_type: "text".to_string(),
                    secret: false,
                    required: false,
                    configured: item.configured,
                    value: item
                        .public_fields
                        .get("account")
                        .cloned()
                        .unwrap_or_default(),
                    placeholder: String::new(),
                    help: String::new(),
                });
                item.last_used_at = last_used
                    .get(&(
                        "core".to_string(),
                        "himind.workbench".to_string(),
                        "workbench".to_string(),
                    ))
                    .cloned()
                    .unwrap_or_default();
                items.push(item);
            }
        }
        Ok(None) => {}
        Err(error) => warnings.push(format!("工作台账号状态读取失败：{error}")),
    }

    match crate::store::ai_services::list() {
        Ok(services) => {
            let mut item = empty_item(
                "core.ai-services",
                "core",
                "himind.ai-services",
                "AI 服务",
                "模型服务凭据",
                "api_key",
                "ai-services",
            );
            item.configured = !services.is_empty();
            item.status = if item.configured {
                "unverified"
            } else {
                "missing"
            }
            .to_string();
            item.required = false;
            item.description = if services.is_empty() {
                "尚未配置本机 AI 服务。".to_string()
            } else {
                format!("{} 个本机服务已配置模型凭据。", services.len())
            };
            item.public_fields
                .insert("service_count".to_string(), services.len().to_string());
            item.fields.push(CredentialFieldView {
                key: "service_count".to_string(),
                label: "已配置服务".to_string(),
                field_type: "text".to_string(),
                secret: false,
                required: false,
                configured: item.configured,
                value: services.len().to_string(),
                placeholder: String::new(),
                help: "模型、协议和能力路由在 AI 连接页面维护。".to_string(),
            });
            items.push(item);
        }
        Err(error) => warnings.push(format!("AI 服务摘要读取失败：{error}")),
    }

    match crate::store::github_credentials::status() {
        Ok(account) => {
            let mut item = empty_item(
                "core.github",
                "core",
                "himind.github",
                "GitHub",
                "发布账号",
                if account.auth_kind == "app" {
                    "oauth"
                } else {
                    "api_key"
                },
                "github-account",
            );
            item.configured = account.authorized;
            item.status = if account.authorized {
                "valid"
            } else {
                "missing"
            }
            .to_string();
            item.required = false;
            item.description = "用于创建 tag、Release 和发布扩展制品。".to_string();
            item.public_fields
                .insert("account".to_string(), account.login.clone());
            item.public_fields
                .insert("token_kind".to_string(), account.token_kind.clone());
            item.fields.push(CredentialFieldView {
                key: "account".to_string(),
                label: "账号".to_string(),
                field_type: "text".to_string(),
                secret: false,
                required: false,
                configured: account.authorized,
                value: account.login,
                placeholder: String::new(),
                help: String::new(),
            });
            item.updated_at = account.updated_at;
            items.push(item);
        }
        Err(error) => warnings.push(format!("GitHub 账号状态读取失败：{error}")),
    }

    items
}

fn builtin_items(warnings: &mut Vec<String>) -> Vec<CredentialCatalogItem> {
    let mut items = Vec::new();

    match crate::store::credentials::list_local_svn_connections() {
        Ok(connections) => {
            let mut item = empty_item(
                "builtin.svn.account",
                "builtin",
                "com.himind.builtin.svn",
                "SVN",
                "SVN 账号",
                "userpass",
                "svn-account",
            );
            if let Some(connection) = connections.first() {
                item.configured = !connection.username.trim().is_empty()
                    && !connection.encrypted_password.trim().is_empty();
                item.status = match connection.status.as_str() {
                    "invalid" => "invalid",
                    "unreachable" => "unavailable",
                    "ready" => "valid",
                    _ => "unverified",
                }
                .to_string();
                item.public_fields
                    .insert("username".to_string(), connection.username.clone());
                item.public_fields
                    .insert("base_url".to_string(), connection.base_url.clone());
                item.description = connection.last_error.clone();
                item.fields = vec![
                    CredentialFieldView {
                        key: "username".to_string(),
                        label: "账号".to_string(),
                        field_type: "text".to_string(),
                        secret: false,
                        required: true,
                        configured: true,
                        value: connection.username.clone(),
                        placeholder: String::new(),
                        help: String::new(),
                    },
                    CredentialFieldView {
                        key: "base_url".to_string(),
                        label: "仓库地址".to_string(),
                        field_type: "url".to_string(),
                        secret: false,
                        required: true,
                        configured: true,
                        value: connection.base_url.clone(),
                        placeholder: String::new(),
                        help: String::new(),
                    },
                ];
            } else {
                item.description = "用于访问当前账号有权限的 SVN 项目仓库。".to_string();
            }
            item.verify_capability = "svn.connection.test".to_string();
            items.push(item);
        }
        Err(error) => warnings.push(format!("SVN 账号状态读取失败：{error}")),
    }

    let login = crate::store::credentials::local_login_status_json();
    let mut item = empty_item(
        "builtin.inner-admin.account",
        "builtin",
        "com.himind.builtin.inner-admin",
        "内网平台",
        "内网平台账号",
        "userpass",
        "inner-account",
    );
    item.configured = login
        .get("authenticated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    item.status = if item.configured {
        "unverified"
    } else {
        "missing"
    }
    .to_string();
    item.description = "用于内网工程同步、代码上传和展项信息读取。".to_string();
    if let Some(account) = login.get("account").and_then(serde_json::Value::as_str) {
        if !account.trim().is_empty() {
            item.public_fields
                .insert("username".to_string(), account.to_string());
            item.fields.push(CredentialFieldView {
                key: "username".to_string(),
                label: "账号".to_string(),
                field_type: "text".to_string(),
                secret: false,
                required: true,
                configured: true,
                value: account.to_string(),
                placeholder: String::new(),
                help: String::new(),
            });
        }
    }
    item.verify_capability = "network.inner-admin.login.status".to_string();
    items.push(item);
    items
}

fn plugin_items(
    warnings: &mut Vec<String>,
    last_used: &BTreeMap<(String, String, String), String>,
) -> Vec<CredentialCatalogItem> {
    let plugins = match crate::capability::plugin::scan_plugins() {
        Ok(items) => items,
        Err(error) => {
            warnings.push(format!("插件清单读取失败：{error}"));
            return Vec::new();
        }
    };
    let mut items = Vec::new();
    for plugin in plugins
        .into_iter()
        .filter(|plugin| plugin.source != "builtin" && !plugin.credentials.is_empty())
    {
        let fingerprint =
            crate::capability::plugin::credential_declaration_fingerprint(&plugin.credentials);
        let broker_allowed = plugin.permissions.iter().any(|permission| {
            permission == crate::capability::credential_broker::BROKER_PERMISSION
        });
        let status_input = plugin
            .credentials
            .iter()
            .map(|credential| (credential.id.clone(), fingerprint.clone()))
            .collect::<Vec<_>>();
        let statuses = match crate::store::plugin_credentials::statuses_with_fingerprints(
            &plugin.id,
            &status_input,
        ) {
            Ok(statuses) => statuses,
            Err(error) => {
                warnings.push(format!("插件 {} 凭据状态读取失败：{error}", plugin.name));
                continue;
            }
        };
        let by_id = statuses
            .into_iter()
            .map(|status| (status.id.clone(), status))
            .collect::<BTreeMap<_, _>>();
        for credential in plugin.credentials {
            let status = by_id.get(&credential.id);
            let mut item = empty_item(
                format!("plugin.{}:{}", plugin.id, credential.id),
                "plugin",
                plugin.id.clone(),
                plugin.name.clone(),
                if credential.label.trim().is_empty() {
                    credential.id.clone()
                } else {
                    credential.label.clone()
                },
                if credential.kind.trim().is_empty() {
                    "api_key"
                } else {
                    credential.kind.as_str()
                },
                "plugin-credential",
            );
            item.required = credential.required;
            let mut description = if broker_allowed {
                credential.description.clone()
            } else if credential.description.trim().is_empty() {
                "插件缺少 secret.broker.acquire 权限，运行时无法获取该凭据。".to_string()
            } else {
                format!(
                    "{}；插件缺少 secret.broker.acquire 权限，运行时无法获取该凭据。",
                    credential.description
                )
            };
            if !plugin.enabled {
                description = if description.trim().is_empty() {
                    "插件已停用；凭据仍保留，可在此清理。".to_string()
                } else {
                    format!("插件已停用；{description}")
                };
            }
            item.description = description;
            item.docs_url = credential.docs_url.clone();
            item.editable = true;
            item.verify_capability = credential.verify_capability.clone();
            item.status = if !plugin.enabled {
                "disabled"
            } else if !broker_allowed {
                "invalid"
            } else if status.is_some_and(|status| !status.descriptor_current) {
                "needs_review"
            } else if status.is_some_and(|status| status.configured) {
                "unverified"
            } else if credential.required {
                "missing"
            } else {
                "optional"
            }
            .to_string();
            item.configured = status.is_some_and(|status| status.configured);
            item.configured_fields = status
                .map(|status| status.configured_fields.clone())
                .unwrap_or_default();
            item.public_fields = status
                .map(|status| status.public_fields.clone())
                .unwrap_or_default();
            item.descriptor_current = status
                .map(|status| status.descriptor_current)
                .unwrap_or(true);
            item.updated_at = status
                .map(|status| status.updated_at.clone())
                .unwrap_or_default();
            item.last_used_at = last_used
                .get(&(
                    "plugin".to_string(),
                    plugin.id.clone(),
                    credential.id.clone(),
                ))
                .cloned()
                .unwrap_or_default();
            item.fields = field_views(&credential.fields, status);
            items.push(item);
        }
    }
    items
}

fn workflow_items(
    warnings: &mut Vec<String>,
    last_used: &BTreeMap<(String, String, String), String>,
) -> Vec<CredentialCatalogItem> {
    let workflows =
        match crate::workflow::WorkflowStore::open_default().and_then(|store| store.list()) {
            Ok(items) => items,
            Err(error) => {
                warnings.push(format!("工作流连接器读取失败：{error}"));
                return Vec::new();
            }
        };
    let stored = match crate::store::connector_credentials::list() {
        Ok(items) => items,
        Err(error) => {
            warnings.push(format!("工作流连接器凭据状态读取失败：{error}"));
            return Vec::new();
        }
    };
    let stored_by_handle = stored
        .into_iter()
        .map(|item| (item.handle.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut items = Vec::new();
    for workflow in workflows {
        for connector in &workflow.package.connectors {
            if connector.credential_ownership != "agent" {
                continue;
            }
            for credential in &connector.credentials {
                let row = stored_by_handle.get(&credential.handle);
                let mut item = empty_item(
                    format!("workflow.{}:{}", workflow.package.id, credential.handle),
                    "workflow",
                    workflow.package.id.clone(),
                    workflow.package.name.clone(),
                    format!("{} · {}", connector.name, credential.handle),
                    credential.kind.as_str(),
                    "workflow-credential",
                );
                item.required = credential.required;
                item.configured = row.is_some();
                item.status = if item.configured {
                    "unverified"
                } else {
                    "missing"
                }
                .to_string();
                item.description = format!(
                    "{} 运行时使用；目标字段 {}。",
                    connector.name, credential.target
                );
                item.editable = true;
                item.public_fields
                    .insert("connector".to_string(), connector.id.clone());
                item.public_fields
                    .insert("target".to_string(), credential.target.clone());
                item.updated_at = row.map(|row| row.updated_at.clone()).unwrap_or_default();
                item.last_used_at = last_used
                    .get(&(
                        "workflow".to_string(),
                        workflow.package.id.clone(),
                        credential.handle.clone(),
                    ))
                    .cloned()
                    .unwrap_or_default();
                item.fields.push(CredentialFieldView {
                    key: credential.target.clone(),
                    label: credential.target.clone(),
                    field_type: if credential.kind == "file_path" {
                        "file".to_string()
                    } else {
                        "password".to_string()
                    },
                    secret: true,
                    required: credential.required,
                    configured: item.configured,
                    value: String::new(),
                    placeholder: if credential.kind == "file_path" {
                        "选择凭据文件".to_string()
                    } else {
                        "输入 Secret".to_string()
                    },
                    help: String::new(),
                });
                items.push(item);
            }
        }
    }
    items
}

pub(crate) fn snapshot() -> CredentialCatalogSnapshot {
    let mut warnings = Vec::new();
    let last_used = last_used_index();
    let mut items = core_items(&mut warnings, &last_used);
    items.extend(builtin_items(&mut warnings));
    items.extend(plugin_items(&mut warnings, &last_used));
    items.extend(workflow_items(&mut warnings, &last_used));
    items.sort_by(|left, right| {
        left.owner_kind
            .cmp(&right.owner_kind)
            .then(left.owner_name.cmp(&right.owner_name))
            .then(left.label.cmp(&right.label))
    });
    CredentialCatalogSnapshot {
        items,
        warnings,
        generated_at: now_stamp(),
    }
}

pub(crate) fn audit(limit: usize) -> Vec<CredentialAuditEvent> {
    crate::store::plugin_credentials::audit_events(limit).unwrap_or_default()
}

#[tauri::command]
pub(crate) fn get_credential_catalog() -> CredentialCatalogSnapshot {
    snapshot()
}

#[tauri::command]
pub(crate) fn get_credential_audit(limit: Option<usize>) -> Vec<CredentialAuditEvent> {
    audit(limit.unwrap_or(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_builtins_and_never_exposes_secret_values() {
        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-credential-center-{}-{}",
            std::process::id(),
            now_stamp()
        ));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("HIMIND_AGENT_HOME", &root);

        let plugin_root = root
            .join("plugins")
            .join("com.test.credential-plugin")
            .join("current");
        std::fs::create_dir_all(&plugin_root).unwrap();
        std::fs::write(
            plugin_root.join("plugin.json"),
            r#"{
                "id":"com.test.credential-plugin",
                "name":"Credential Test",
                "version":"1.0.0",
                "runtime":"process-jsonrpc-stdio",
                "permissions":["secret.broker.acquire"],
                "credentials":[{
                    "id":"demo.api",
                    "label":"Demo API",
                    "kind":"api_key",
                    "required":true,
                    "fields":[{
                        "key":"api_key","label":"API Key","type":"password",
                        "secret":true,"required":true
                    }]
                }]
            }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugins")
                .join("com.test.credential-plugin")
                .join("disabled"),
            b"",
        )
        .unwrap();
        let manifest = crate::capability::plugin::parse_plugin_manifest(
            &std::fs::read_to_string(plugin_root.join("plugin.json")).unwrap(),
        )
        .unwrap();
        let fingerprint =
            crate::capability::plugin::credential_declaration_fingerprint(&manifest.credentials);
        let mut secret_fields = BTreeMap::new();
        secret_fields.insert("api_key".to_string(), "catalog-secret".to_string());
        crate::store::plugin_credentials::save_profile(
            "com.test.credential-plugin",
            "demo.api",
            &BTreeMap::new(),
            &secret_fields,
            &fingerprint,
        )
        .unwrap();

        let snapshot = snapshot();
        let ids = snapshot
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"builtin.svn.account"));
        assert!(ids.contains(&"builtin.inner-admin.account"));
        assert!(ids.contains(&"core.ai-services"));
        assert!(ids.contains(&"core.github"));
        let plugin = snapshot
            .items
            .iter()
            .find(|item| item.id == "plugin.com.test.credential-plugin:demo.api")
            .expect("disabled plugin credential must stay visible");
        assert_eq!(plugin.status, "disabled");
        assert!(plugin.configured);
        assert!(plugin.description.contains("已停用"));
        assert!(snapshot.items.iter().all(|item| {
            item.fields
                .iter()
                .all(|field| !field.secret || field.value.is_empty())
        }));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("protected_fields"));
        assert!(!encoded.contains("catalog-secret"));
        assert!(!encoded.contains("encrypted_"));

        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tauri::command]
pub(crate) async fn open_credential_center(app: tauri::AppHandle) -> Result<(), String> {
    crate::app::commands::open_settings_window(
        app,
        Some("settings".to_string()),
        Some("accounts".to_string()),
        None,
        None,
    )
    .await
}
