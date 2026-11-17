//! Client-owned accounts and Agent service registration have different lifecycles.
//! No account tokens are read, copied, or inferred from a provider configuration.
use super::*;

static SOURCE_ACTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ClientSourceCapabilities {
    pub registration_effect: String,
    pub native_account: bool,
    pub select_default: bool,
    pub selection_hint: String,
    pub load_hint: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ClientSourceStatus {
    pub capabilities: ClientSourceCapabilities,
    pub registered: bool,
    pub managed: bool,
    pub configured_source: String,
    pub default_model: String,
    pub auth_status: String,
    pub effective_status: String,
    pub scope: String,
    pub config_path: String,
    pub can_restore: bool,
    pub warnings: Vec<String>,
}

pub(crate) fn capabilities(target: &str) -> ClientSourceCapabilities {
    let (effect, native, default, selection, load) = match target {
        "codex" => (
            "changes_default",
            true,
            false,
            "分发会设置默认来源；原生账号在 Codex 中登录。",
            "重启使用此配置目录的 Codex 后检查来源；当前会话不会自动切换。",
        ),
        "claude-code" => (
            "changes_default",
            true,
            false,
            "分发会设置用户级端点和模型；项目设置、环境和组织策略可能覆盖它。",
            "启动新的 Claude Code 会话后检查 /status，并按需 /login。",
        ),
        "opencode" => (
            "additive",
            false,
            true,
            "添加来源后在 OpenCode 用 /connect 登录、/models 选择；也可在此显式设置默认模型。",
            "启动新会话后核对 /models；项目配置和会话选择可能覆盖默认值。",
        ),
        "vscode" | "continue" | "workbuddy" => (
            "additive",
            false,
            false,
            "添加模型来源，具体模型与功能配置由客户端选择。",
            "重新加载客户端并在模型选择器核对。",
        ),
        "cc-switch" => (
            "external_manager",
            false,
            false,
            "登记到配置管理器，不代表实际客户端已经启用。",
            "在 CC Switch 中应用来源，再核对目标客户端。",
        ),
        "claude-desktop" => (
            "configuration_profile",
            false,
            false,
            "切换第三方配置档案；其账号和目录机制与 Claude Code 不同。",
            "重启 Claude Desktop 并核对当前档案；原生账号自动切换尚未支持。",
        ),
        "zcode" => (
            "additive",
            false,
            false,
            "添加供应商及模型规则，由客户端选择。",
            "重新加载客户端后核对模型来源。",
        ),
        "crush" => (
            "model_slots",
            false,
            false,
            "分发会修改 large / small 模型槽位。",
            "启动新会话并核对两个模型槽位。",
        ),
        "kimi-code" | "qwen-code" | "aider" | "qoder" | "qoder-cn" => (
            "changes_default",
            false,
            false,
            "分发会修改客户端默认模型配置；可恢复分发前配置。",
            "启动新会话后核对模型来源。",
        ),
        _ => (
            "unknown",
            false,
            false,
            "由客户端管理；尚未声明自动来源操作。",
            "请在客户端核对当前模型和账号。",
        ),
    };
    ClientSourceCapabilities {
        registration_effect: effect.into(),
        native_account: native,
        select_default: default,
        selection_hint: selection.into(),
        load_hint: load.into(),
    }
}

fn read_text(path: &Path) -> Result<String, Box<dyn Error>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn json_object(text: &str) -> Result<Value, Box<dyn Error>> {
    let root = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&crate::app::mcp_targets::strip_jsonc_comments(text))?
    };
    if !root.is_object() {
        return Err("客户端配置顶层必须为对象".into());
    }
    Ok(root)
}

fn environment_warnings(target: &str) -> Vec<String> {
    let keys: &[&str] = match target {
        "codex" => &["OPENAI_BASE_URL", "CHATGPT_BASE_URL", "OPENAI_API_KEY"],
        "claude-code" => &[
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ],
        _ => &[],
    };
    keys.iter().filter(|key| env::var_os(key).is_some_and(|v| !v.is_empty()))
        .map(|key| format!("Agent 进程环境存在 {key}；客户端是否继承及最终优先级需在客户端核对，未修改该变量。"))
        .collect()
}

fn restore_available(target: &str, binding: Option<&AIProviderImportBinding>) -> bool {
    let Some(restore) = binding.and_then(|b| b.restore.as_ref()) else {
        return false;
    };
    match target {
        "codex" => restore
            .get("config")
            .and_then(Value::as_str)
            .is_some_and(|text| text.parse::<DocumentMut>().is_ok()),
        "claude-code" => restore
            .get("settings")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.trim().is_empty()
                    || serde_json::from_str::<Value>(text).is_ok_and(|value| value.is_object())
            }),
        "opencode" => restore
            .get("model")
            .is_some_and(|model| model.is_null() || model.is_string()),
        _ => false,
    }
}

pub(super) fn observe(
    adapter: &dyn AIClientAdapter,
    status: &AIProviderImportStatus,
    binding: Option<&AIProviderImportBinding>,
) -> ClientSourceStatus {
    let target = adapter.id();
    let mut result = ClientSourceStatus {
        capabilities: adapter.source_capabilities(),
        registered: status.state == "imported",
        managed: binding.is_some(),
        configured_source: "client_managed".into(),
        auth_status: "unknown".into(),
        effective_status: "not_verified".into(),
        scope: "client_configuration".into(),
        config_path: status.config_path.clone(),
        can_restore: restore_available(target, binding),
        warnings: environment_warnings(target),
        ..Default::default()
    };
    let observation = (|| -> Result<(), Box<dyn Error>> {
        match target {
            "codex" => {
                result.scope = "config_directory".into();
                let doc =
                    read_text(&codex_config_path().join("config.toml"))?.parse::<DocumentMut>()?;
                result.default_model = doc
                    .get("model")
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .into();
                result.configured_source = match doc
                    .get("model_provider")
                    .and_then(Item::as_str)
                    .unwrap_or("openai")
                {
                    CODEX_PROVIDER_ID => "agent",
                    "openai" => "native_provider",
                    _ => "other_provider",
                }
                .into();
                if doc.get("openai_base_url").is_some() || doc.get("chatgpt_base_url").is_some() {
                    result.warnings.push(
                        "配置含官方端点覆盖，不能仅凭 openai 供应商判断使用了官方服务。".into(),
                    );
                }
                if doc.get("forced_login_method").and_then(Item::as_str) == Some("api") {
                    result
                        .warnings
                        .push("当前配置仅允许 API 登录，官方订阅登录需要切换认证方式。".into());
                }
            }
            "opencode" => {
                result.scope = "config_file".into();
                let root = json_object(&read_text(&opencode_config_path())?)?;
                result.default_model = root
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into();
                if result.default_model.starts_with("himind/") {
                    result.configured_source = "agent".into();
                } else if !result.default_model.is_empty() {
                    result.configured_source = "other_provider".into();
                }
            }
            "claude-code" => {
                result.scope = "user_settings".into();
                let root = json_object(&read_text(&claude_code_settings_path())?)?;
                result.default_model = root
                    .pointer("/env/ANTHROPIC_MODEL")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into();
                if result.registered {
                    result.configured_source = "agent".into();
                } else if root.pointer("/env/ANTHROPIC_BASE_URL").is_some()
                    || root.get("apiKeyHelper").is_some()
                {
                    result.configured_source = "other_provider".into();
                }
                result.warnings.push(
                    "只观察用户级设置；项目设置、启动参数及组织策略由 Claude Code 决定。".into(),
                );
            }
            _ => {}
        }
        Ok(())
    })();
    if observation.is_err() {
        result.configured_source = "unknown".into();
        result
            .warnings
            .push("无法读取或解析客户端配置；未推断当前来源。".into());
    }
    result
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceAction {
    NativeAccount,
    RestorePrevious,
    SetDefault,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRequest {
    pub target: String,
    pub action: SourceAction,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub expected_revision: String,
    #[serde(default)]
    pub accept_drift: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SourcePlan {
    pub target: String,
    pub action: SourceAction,
    pub revision: String,
    pub config_path: String,
    pub scope: String,
    pub changes: Vec<String>,
    pub warnings: Vec<String>,
    pub conflict: bool,
    pub detail: String,
    pub load_hint: String,
}

struct Prepared {
    plan: SourcePlan,
    path: PathBuf,
    original: String,
    updated: String,
    bindings: AIProviderImportBindings,
}

fn native_codex(original: &str) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut doc = original.parse::<DocumentMut>()?;
    let mut changes = Vec::new();
    for key in [
        "model",
        "model_reasoning_effort",
        "model_catalog_json",
        "preferred_auth_method",
        "openai_base_url",
        "chatgpt_base_url",
    ] {
        if doc.remove(key).is_some() {
            changes.push(format!("清除 {key}，交由 Codex 官方配置决定"));
        }
    }
    doc["model_provider"] = value("openai");
    doc["forced_login_method"] = value("chatgpt");
    changes.push("设置官方供应商并使用 ChatGPT 账号认证；不修改账号令牌".into());
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        if providers.remove(CODEX_PROVIDER_ID).is_some() {
            changes.push("移除 Agent 供应商登记；保留其他供应商".into());
        }
    }
    Ok((doc.to_string(), changes))
}

fn native_claude(original: &str) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut root = json_object(original)?;
    let mut changes = Vec::new();
    if root.get("env").is_some_and(|v| !v.is_object()) {
        return Err("settings.env 必须为对象".into());
    }
    if let Some(env) = root.get_mut("env").and_then(Value::as_object_mut) {
        for key in claude_env_keys().into_iter().chain([
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            "ANTHROPIC_CUSTOM_HEADERS",
        ]) {
            if env.remove(key).is_some() {
                changes.push(format!("清除用户设置 env.{key}"));
            }
        }
    }
    for key in ["apiKeyHelper", "forceLoginMethod", "model"] {
        if root.as_object_mut().unwrap().remove(key).is_some() {
            changes.push(format!("清除用户设置 {key}，交由客户端登录决定"));
        }
    }
    changes.push("在 Claude Code 中使用原生账号登录；不读取或修改登录凭据".into());
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        changes,
    ))
}

fn opencode_default(original: &str, model: &str) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut root = json_object(original)?;
    let models = root
        .pointer("/provider/himind/models")
        .and_then(Value::as_object)
        .ok_or("请先添加 Agent 模型来源")?;
    if !models.contains_key(model) {
        return Err("该模型不在已登记的 Agent 模型目录中，请刷新后选择".into());
    }
    root["model"] = json!(format!("himind/{model}"));
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        vec![format!(
            "设置用户默认模型 himind/{model}；保留其他供应商和 MCP"
        )],
    ))
}

fn prepare(options: &Options, request: &SourceRequest) -> Result<Prepared, Box<dyn Error>> {
    let adapter = adapter_for(&request.target).ok_or("不支持的 AI 客户端")?;
    if request.target != adapter.id() {
        return Err("客户端标识必须与登记名称一致".into());
    }
    let caps = adapter.source_capabilities();
    let status = adapter.status(options);
    let path = match adapter.id() {
        "codex" => codex_config_path().join("config.toml"),
        "claude-code" => claude_code_settings_path(),
        "opencode" => opencode_config_path(),
        _ => PathBuf::from(&status.config_path),
    };
    let bindings_text = read_text(&import_bindings_path(options))?;
    // Invalid bookkeeping must not silently turn into an empty binding set.
    let bindings: AIProviderImportBindings = if bindings_text.trim().is_empty() {
        Default::default()
    } else {
        serde_json::from_str(&bindings_text)?
    };
    let binding = bindings.clients.get(adapter.id());
    let original = if matches!(adapter.id(), "codex" | "claude-code" | "opencode") {
        read_text(&path)?
    } else {
        String::new()
    };
    let source = observe(adapter, &status, binding);
    let (updated, changes, detail) = match request.action {
        SourceAction::NativeAccount => {
            if !caps.native_account {
                return Err(
                    "该客户端尚未支持自动切换原生账号；请使用客户端自身的登录和选模入口".into(),
                );
            }
            let (text, mut changes) = match adapter.id() {
                "codex" => native_codex(&original)?,
                "claude-code" => native_claude(&original)?,
                _ => return Err("客户端尚未实现原生账号配置转换".into()),
            };
            changes.push("停止此客户端的 Agent 自动同步；已保存的 AI 服务继续保留".into());
            (
                text,
                changes,
                "切换原生账号配置；账号登录及订阅权益由客户端验证。",
            )
        }
        SourceAction::SetDefault => {
            if !caps.select_default {
                return Err("该客户端不支持此默认模型操作".into());
            }
            let (text, changes) = opencode_default(&original, &request.model)?;
            (text, changes, "只修改此配置文件的默认模型，不修改登录。")
        }
        SourceAction::RestorePrevious => {
            if !source.can_restore {
                return Err(
                    "没有可用的分发前快照；可使用移除 Agent 来源，但不能承诺恢复历史配置".into(),
                );
            }
            let restore = binding.and_then(|b| b.restore.as_ref()).unwrap();
            let text = match adapter.id() {
                "codex" => {
                    strip_codex_himind(
                        &original,
                        &codex_himind_models_path(),
                        restore.get("config").and_then(Value::as_str),
                    )?
                    .0
                }
                "claude-code" => {
                    strip_claude_himind(
                        &original,
                        "Claude Code",
                        restore.get("settings").and_then(Value::as_str),
                    )?
                    .0
                }
                "opencode" => strip_opencode_himind_with_restore(&original, Some(restore))?.0,
                _ => return Err("此客户端尚未支持来源快照恢复".into()),
            };
            (
                text,
                vec![
                    "按首次分发快照恢复被托管字段并移除同步绑定；恢复结果可能是其他第三方服务"
                        .into(),
                ],
                "恢复分发前配置，与切换原生账号含义不同。",
            )
        }
    };
    if !matches!(request.action, SourceAction::SetDefault) && !request.model.is_empty() {
        return Err("只有 set_default 可以指定模型".into());
    }
    let current_fields = adapter.current_managed_fields(options).ok().flatten();
    let drift = binding
        .and_then(|b| b.applied_fields.as_ref())
        .and_then(|v| serde_json::from_value::<ManagedFields>(v.clone()).ok())
        .zip(current_fields)
        .map(|(old, current)| compare_managed_fields(&old, &current));
    let conflict = matches!(drift, Some(DriftVerdict::Drifted(_)));
    let mut warnings = source.warnings;
    if conflict {
        warnings
            .push("Agent 托管字段存在外部修改；应用将按上述目标覆盖相关字段，请核对变更。".into());
    }
    let revision = projection_digest(&json!({
        "request": {"target": request.target, "action": request.action, "model": request.model},
        "path": path, "config": original, "bindings": bindings_text,
        "snapshot": adapter.owned_snapshot(options), "warnings": warnings,
        "transformation": updated, "contract": "ai.client.source.v1",
    }));
    Ok(Prepared {
        plan: SourcePlan {
            target: adapter.id().into(),
            action: request.action.clone(),
            revision,
            config_path: path.to_string_lossy().into(),
            scope: source.scope,
            changes,
            warnings,
            conflict,
            detail: detail.into(),
            load_hint: caps.load_hint,
        },
        path,
        original,
        updated,
        bindings,
    })
}

pub(crate) fn plan(
    options: &Options,
    request: &SourceRequest,
) -> Result<SourcePlan, Box<dyn Error>> {
    Ok(prepare(options, request)?.plan)
}

pub(crate) fn apply(options: &Options, request: &SourceRequest) -> Result<Value, Box<dyn Error>> {
    let _guard = SOURCE_ACTION_LOCK
        .lock()
        .map_err(|_| "来源切换锁不可用，请重试")?;
    let prepared = prepare(options, request)?;
    if request.expected_revision.is_empty() || request.expected_revision != prepared.plan.revision {
        return Err("来源切换计划已过期或缺失，请重新预览后应用".into());
    }
    if prepared.plan.conflict && !request.accept_drift {
        return Err("存在外部修改，请核对来源切换计划并明确确认覆盖".into());
    }
    let existed = prepared.path.exists();
    let backup = backup_and_write(&prepared.path, prepared.updated.as_bytes())?;
    let mut bindings = prepared.bindings;
    if matches!(request.action, SourceAction::SetDefault) {
        if let Some(binding) = bindings.clients.get_mut(&request.target) {
            if binding.restore.is_none() {
                let original = json_object(&prepared.original)?;
                binding.restore = Some(json!({"model": original.get("model")}));
            }
        }
    } else {
        bindings.clients.remove(&request.target);
    }
    if let Err(error) = save_import_bindings(options, &bindings) {
        if existed {
            backup_and_write(&prepared.path, prepared.original.as_bytes())?;
        } else {
            fs::remove_file(&prepared.path)?;
        }
        return Err(format!("同步绑定保存失败，已回退客户端配置：{error}").into());
    }
    let backup = backup
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(
        json!({"ok": true, "target": request.target, "action": request.action,
        "config_written": true, "effective_status": "not_verified", "auth_status": "unknown",
        "backup_path": backup, "detail": prepared.plan.detail, "load_hint": prepared.plan.load_hint,
        "warnings": prepared.plan.warnings}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
        options: Options,
    }

    impl Fixture {
        fn new() -> Self {
            let root = env::temp_dir().join(format!(
                "client-source-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let mut previous = Vec::new();
            for (key, path) in [
                ("CODEX_HOME", root.join("codex")),
                ("CLAUDE_CONFIG_DIR", root.join("claude")),
                ("OPENCODE_CONFIG", root.join("opencode.json")),
                ("HIMIND_AGENT_HOME", root.join("agent")),
            ] {
                previous.push((key, env::var_os(key)));
                env::set_var(key, path);
            }
            let mut options = Options::from_env();
            options.state_path = root.join("state.json");
            Self {
                root,
                previous,
                options,
            }
        }

        fn config(&self, target: &str, text: &str) -> PathBuf {
            let path = match target {
                "codex" => codex_config_path().join("config.toml"),
                "claude-code" => claude_code_settings_path(),
                _ => opencode_config_path(),
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        fn bind(&self, target: &str, restore: Option<Value>) {
            let mut bindings = AIProviderImportBindings::default();
            bindings.clients.insert(
                target.into(),
                AIProviderImportBinding {
                    service: "custom:fixture".into(),
                    restore,
                    applied_fields: adapter_for(target)
                        .unwrap()
                        .managed_fields(&self.options)
                        .map(|v| serde_json::to_value(v).unwrap()),
                    ..Default::default()
                },
            );
            save_import_bindings(&self.options, &bindings).unwrap();
        }

        fn request(&self, target: &str, action: SourceAction) -> SourceRequest {
            SourceRequest {
                target: target.into(),
                action,
                model: String::new(),
                expected_revision: String::new(),
                accept_drift: false,
            }
        }

        fn apply(&self, request: &mut SourceRequest) -> Value {
            request.expected_revision = plan(&self.options, request).unwrap().revision;
            apply(&self.options, request).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                if let Some(value) = value {
                    env::set_var(key, value);
                } else {
                    env::remove_var(key);
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn capability_matrix_preserves_different_client_mechanisms() {
        assert_eq!(capabilities("codex").registration_effect, "changes_default");
        assert_eq!(capabilities("opencode").registration_effect, "additive");
        assert_eq!(capabilities("crush").registration_effect, "model_slots");
        assert_eq!(
            capabilities("cc-switch").registration_effect,
            "external_manager"
        );
        assert_eq!(
            capabilities("claude-desktop").registration_effect,
            "configuration_profile"
        );
        assert!(!capabilities("claude-desktop").native_account);
        for adapter in known_adapters() {
            assert!(!capabilities(adapter.id()).load_hint.is_empty());
        }
    }

    #[test]
    fn native_codex_removes_api_restrictions_and_catalog_but_preserves_tools() {
        let (text, changes) = native_codex("model='third-party-model'\nmodel_provider='himind'\nforced_login_method='api'\npreferred_auth_method='apikey'\nmodel_catalog_json='catalog.json'\nopenai_base_url='https://proxy.invalid'\n[mcp_servers.agent]\ncommand='agent'\n[model_providers.himind]\nbase_url='http://127.0.0.1'\n[model_providers.other]\nbase_url='https://other.invalid'\n").unwrap();
        let doc = text.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("openai"));
        assert_eq!(doc["forced_login_method"].as_str(), Some("chatgpt"));
        for key in [
            "model",
            "model_catalog_json",
            "preferred_auth_method",
            "openai_base_url",
        ] {
            assert!(doc.get(key).is_none());
        }
        assert_eq!(
            doc["mcp_servers"]["agent"]["command"].as_str(),
            Some("agent")
        );
        assert!(doc["model_providers"].get("himind").is_none());
        assert!(doc["model_providers"].get("other").is_some());
        assert!(!changes.is_empty());
    }

    #[test]
    fn native_claude_clears_local_api_overrides_without_touching_tools_or_permissions() {
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://proxy.invalid","ANTHROPIC_AUTH_TOKEN":"fixture-secret","ANTHROPIC_API_KEY":"key","CLAUDE_CODE_USE_VERTEX":"1","HTTP_PROXY":"keep"},"apiKeyHelper":"helper","model":"custom","permissions":{"allow":["Read"]},"mcpServers":{"agent":{}}}"#;
        let (text, changes) = native_claude(original).unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["env"], json!({"HTTP_PROXY":"keep"}));
        assert!(root.get("apiKeyHelper").is_none());
        assert!(root.get("model").is_none());
        assert_eq!(root["permissions"]["allow"], json!(["Read"]));
        assert!(root["mcpServers"].get("agent").is_some());
        assert!(!changes.join(" ").contains("fixture-secret"));
    }

    #[test]
    fn opencode_selects_exact_registered_model_with_slash_and_keeps_other_providers() {
        let original = r#"{"provider":{"himind":{"models":{"service/model":{}}},"openai":{}},"model":"openai/gpt","mcp":{"agent":{}}}"#;
        let (text, _) = opencode_default(original, "service/model").unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["model"], "himind/service/model");
        assert!(root["provider"].get("openai").is_some());
        assert!(root["mcp"].get("agent").is_some());
        assert!(opencode_default(original, "missing").is_err());
    }

    #[test]
    fn codex_registration_does_not_claim_it_is_the_active_source_or_authenticated() {
        let f = Fixture::new();
        f.config(
            "codex",
            "model_provider='openai'\n[model_providers.himind]\nname='HiMind'\n",
        );
        let adapter = adapter_for("codex").unwrap();
        let source = observe(adapter, &adapter.status(&f.options), None);
        assert!(source.registered);
        assert_eq!(source.configured_source, "native_provider");
        assert_eq!(source.auth_status, "unknown");
        assert_eq!(source.effective_status, "not_verified");
    }

    #[test]
    fn plan_is_read_only_and_apply_rejects_missing_or_stale_revision() {
        let f = Fixture::new();
        let path = f.config("codex", "model_provider='himind'\n");
        let mut request = f.request("codex", SourceAction::NativeAccount);
        let preview = plan(&f.options, &request).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "model_provider='himind'\n"
        );
        assert!(apply(&f.options, &request).is_err());
        request.expected_revision = preview.revision;
        fs::write(&path, "model_provider='other'\n").unwrap();
        assert!(apply(&f.options, &request).is_err());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "model_provider='other'\n"
        );
    }

    #[test]
    fn native_switch_unbinds_sync_keeps_auth_file_and_saved_service_identity() {
        let f = Fixture::new();
        let path = f.config("codex", "model_provider='himind'\nforced_login_method='api'\n[model_providers.himind]\nname='HiMind'\n");
        let auth = path.parent().unwrap().join("auth.json");
        fs::write(&auth, "fixture-account-do-not-read").unwrap();
        f.bind("codex", Some(json!({"config":"model_provider='other'\n"})));
        let mut request = f.request("codex", SourceAction::NativeAccount);
        let result = f.apply(&mut request);
        assert_eq!(result["effective_status"], "not_verified");
        assert_eq!(
            fs::read_to_string(auth).unwrap(),
            "fixture-account-do-not-read"
        );
        assert!(binding_service(&f.options, "codex").is_none());
        assert!(sync_binding(&f.options, "", "codex", None).is_err());
        assert_eq!(
            fs::read_to_string(path)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap()["model_provider"]
                .as_str(),
            Some("openai")
        );
    }

    #[test]
    fn restoring_snapshot_can_restore_another_provider_instead_of_native() {
        let f = Fixture::new();
        let path = f.config("codex", "model_provider='himind'\nforced_login_method='api'\n[mcp_servers.agent]\ncommand='agent'\n[model_providers.himind]\nname='HiMind'\n");
        f.bind(
            "codex",
            Some(json!({"config":"model_provider='custom'\nmodel='previous'\n"})),
        );
        let mut request = f.request("codex", SourceAction::RestorePrevious);
        f.apply(&mut request);
        let doc = fs::read_to_string(path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("custom"));
        assert_eq!(doc["model"].as_str(), Some("previous"));
        assert!(doc.get("mcp_servers").is_some());
    }

    #[test]
    fn legacy_without_snapshot_cannot_claim_restoration_but_can_choose_native() {
        let f = Fixture::new();
        f.config("codex", "forced_login_method='api'\n");
        f.bind("codex", None);
        assert!(plan(
            &f.options,
            &f.request("codex", SourceAction::RestorePrevious)
        )
        .is_err());
        f.apply(&mut f.request("codex", SourceAction::NativeAccount));
    }

    #[test]
    fn drift_requires_explicit_consent_even_with_current_plan() {
        let f = Fixture::new();
        let path = f.config(
            "codex",
            "model_provider='himind'\n[model_providers.himind]\nname='HiMind'\n",
        );
        f.bind("codex", None);
        fs::write(
            &path,
            "model_provider='other'\n[model_providers.himind]\nname='HiMind'\n",
        )
        .unwrap();
        let mut request = f.request("codex", SourceAction::NativeAccount);
        let preview = plan(&f.options, &request).unwrap();
        assert!(preview.conflict);
        request.expected_revision = preview.revision;
        assert!(apply(&f.options, &request).is_err());
        request.accept_drift = true;
        apply(&f.options, &request).unwrap();
    }

    #[test]
    fn unsupported_client_and_malformed_config_do_not_write() {
        let f = Fixture::new();
        assert!(plan(
            &f.options,
            &f.request("opencode", SourceAction::NativeAccount)
        )
        .is_err());
        let path = f.config("codex", "[invalid");
        assert!(plan(&f.options, &f.request("codex", SourceAction::NativeAccount)).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "[invalid");
        assert!(serde_json::from_value::<SourceRequest>(
            json!({"target":"codex","action":"native_account","unrecognized":true})
        )
        .is_err());
    }

    #[test]
    fn opencode_default_round_trip_restores_previous_choice_and_legacy_snapshot() {
        let f = Fixture::new();
        let path = f.config("opencode", r#"{"provider":{"himind":{"models":{"a":{}}},"openai":{}},"model":"openai/previous","mcp":{"agent":{}}}"#);
        f.bind("opencode", None);
        let mut request = f.request("opencode", SourceAction::SetDefault);
        request.model = "a".into();
        f.apply(&mut request);
        assert_eq!(
            json_object(&fs::read_to_string(&path).unwrap()).unwrap()["model"],
            "himind/a"
        );
        let mut restore = f.request("opencode", SourceAction::RestorePrevious);
        f.apply(&mut restore);
        let root = json_object(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(root["model"], "openai/previous");
        assert!(root["provider"].get("openai").is_some());
        assert!(root["provider"].get("himind").is_none());
        assert!(root["mcp"].get("agent").is_some());
    }

    #[test]
    fn empty_codex_snapshot_restores_absent_keys_instead_of_legacy_cleanup() {
        let (text, changed) = strip_codex_himind(
            "model='custom'\nmodel_provider='himind'\nforced_login_method='api'\npreferred_auth_method='apikey'\n",
            Path::new("fixture-catalog.json"), Some(""),
        ).unwrap();
        assert!(changed);
        assert!(text.trim().is_empty());
        let f = Fixture::new();
        assert_eq!(codex_owned_snapshot().unwrap()["config"], "");
        f.config("opencode", "{}");
        assert_eq!(
            adapter_for("opencode")
                .unwrap()
                .owned_snapshot(&f.options)
                .unwrap()["model"],
            Value::Null
        );
    }

    #[test]
    fn failed_binding_write_rolls_back_client_configuration() {
        let f = Fixture::new();
        let original = "model_provider='himind'\nforced_login_method='api'\n";
        let path = f.config("codex", original);
        let mut request = f.request("codex", SourceAction::NativeAccount);
        request.expected_revision = plan(&f.options, &request).unwrap().revision;
        fs::create_dir_all(crate::store::atomic_file::lock_path(&import_bindings_path(
            &f.options,
        )))
        .unwrap();
        assert!(apply(&f.options, &request)
            .unwrap_err()
            .to_string()
            .contains("已回退"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn failed_history_restore_rolls_back_config_and_keeps_binding() {
        let f = Fixture::new();
        let original = "model_provider='himind'\nforced_login_method='api'\n";
        let path = f.config("codex", original);
        f.bind(
            "codex",
            Some(json!({"config":"model_provider='previous'\n"})),
        );
        let mut request = f.request("codex", SourceAction::RestorePrevious);
        request.expected_revision = plan(&f.options, &request).unwrap().revision;
        let lock_path = crate::store::atomic_file::lock_path(&import_bindings_path(&f.options));
        fs::remove_file(&lock_path).unwrap();
        fs::create_dir_all(lock_path).unwrap();
        assert!(apply(&f.options, &request)
            .unwrap_err()
            .to_string()
            .contains("已回退"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert!(binding_service(&f.options, "codex").is_some());
    }

    #[test]
    fn invalid_restore_payload_is_not_advertised_as_a_snapshot() {
        let f = Fixture::new();
        for (target, config, invalid) in [
            (
                "codex",
                "model_provider='himind'\n",
                json!({"config":"[invalid"}),
            ),
            ("claude-code", "{}", json!({"settings":"[]"})),
            ("opencode", "{}", json!({"model":42})),
        ] {
            f.config(target, config);
            f.bind(target, Some(invalid));
            let bindings = load_import_bindings(&f.options);
            let adapter = adapter_for(target).unwrap();
            assert!(
                !observe(
                    adapter,
                    &adapter.status(&f.options),
                    bindings.clients.get(target)
                )
                .can_restore
            );
            assert!(plan(
                &f.options,
                &f.request(target, SourceAction::RestorePrevious)
            )
            .is_err());
        }
    }

    #[test]
    fn claude_history_restore_and_native_selection_remain_distinct() {
        let f = Fixture::new();
        let path = f.config("claude-code", r#"{"env":{"ANTHROPIC_BASE_URL":"https://agent.invalid","ANTHROPIC_AUTH_TOKEN":"fixture-agent","HTTP_PROXY":"keep"},"permissions":{"allow":["Read"]}}"#);
        let auth = path.parent().unwrap().join(".credentials.json");
        fs::write(&auth, "fixture-credentials-untouched").unwrap();
        f.bind("claude-code", Some(json!({"settings":r#"{"env":{"ANTHROPIC_BASE_URL":"https://previous.invalid","ANTHROPIC_AUTH_TOKEN":"fixture-previous"}}"#})));
        f.apply(&mut f.request("claude-code", SourceAction::RestorePrevious));
        let root = json_object(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            root["env"]["ANTHROPIC_BASE_URL"],
            "https://previous.invalid"
        );
        assert_eq!(root["env"]["HTTP_PROXY"], "keep");
        f.apply(&mut f.request("claude-code", SourceAction::NativeAccount));
        let root = json_object(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(root["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert_eq!(root["env"]["HTTP_PROXY"], "keep");
        assert_eq!(root["permissions"]["allow"], json!(["Read"]));
        assert!(binding_service(&f.options, "claude-code").is_none());
        assert_eq!(
            fs::read_to_string(auth).unwrap(),
            "fixture-credentials-untouched"
        );
    }

    #[test]
    fn opencode_removal_preserves_a_subsequent_other_provider_selection() {
        let original = r#"{"provider":{"himind":{"models":{"a":{}}},"other":{}},"model":"other/user-choice","mcp":{"agent":{}}}"#;
        let (text, changed) =
            strip_opencode_himind_with_restore(original, Some(&json!({"model":"previous/choice"})))
                .unwrap();
        assert!(changed);
        let root = json_object(&text).unwrap();
        assert_eq!(root["model"], "other/user-choice");
        assert!(root["provider"].get("himind").is_none());
        assert!(root["mcp"].get("agent").is_some());
    }

    #[test]
    fn ui_and_mcp_contexts_apply_the_same_gateway_plan() {
        use crate::capability::service::CapabilityGateway;
        use crate::capability::types::{InvocationContext, InvocationSource};
        use std::sync::{Arc, Mutex};

        let mut f = Fixture::new();
        let path = f.config(
            "codex",
            "model_provider='himind'\n[mcp_servers.agent]\ncommand='fixture-agent'\n",
        );
        f.options
            .set_mode(crate::app::runtime_mode::AgentMode::Independent);
        let gateway = CapabilityGateway::new(
            f.options.clone(),
            Arc::new(Mutex::new(crate::store::types::LocalWorkerStatus::default())),
        );
        let input = json!({"target":"codex","action":"native_account"});
        let ui = InvocationContext::tauri().without_agent_core_run();
        let mcp =
            InvocationContext::new(InvocationSource::Mcp, "local-user").without_agent_core_run();
        let ui_plan = gateway
            .invoke(&ui, "ai.client.source.plan", input.clone())
            .unwrap();
        let mcp_plan = gateway
            .invoke(&mcp, "ai.client.source.plan", input.clone())
            .unwrap();
        assert_eq!(ui_plan["revision"], mcp_plan["revision"]);
        assert!(gateway
            .invoke(&mcp, "ai.client.source.apply", input.clone())
            .is_err());
        let mut apply_input = input;
        apply_input["expected_revision"] = ui_plan["revision"].clone();
        let result = gateway
            .invoke(&mcp, "ai.client.source.apply", apply_input.clone())
            .unwrap();
        assert_eq!(result["effective_status"], "not_verified");
        let doc = fs::read_to_string(path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("openai"));
        assert_eq!(
            doc["mcp_servers"]["agent"]["command"].as_str(),
            Some("fixture-agent")
        );
        assert!(gateway
            .invoke(&ui, "ai.client.source.apply", apply_input)
            .is_err());
    }

    #[test]
    fn malformed_binding_bookkeeping_never_silently_discards_other_clients() {
        let f = Fixture::new();
        let path = f.config("codex", "model_provider='himind'\n");
        fs::write(import_bindings_path(&f.options), "{bad-json").unwrap();
        assert!(plan(&f.options, &f.request("codex", SourceAction::NativeAccount)).is_err());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "model_provider='himind'\n"
        );
    }
}
