use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    launch_workspace_build, local_agent_executable_metadata, local_agent_update_supported,
    open_folder, signed_agent_updates_required, trusted_agent_update_key_ids,
};
use crate::capability::plugin::{
    find_plugin, invoke_plugin_capability, registry_json, scan_plugins,
};
use crate::capability::types::{CapabilityDescriptor, InvocationContext};
use crate::store::credentials::{local_login_status_json, local_login_status_value};
use crate::store::types::LocalWorkerStatus;
use crate::svn::service::{
    checkout_workspace, create_exhibit_repository_path, create_repository,
    ensure_project_exhibits_access, initialize_exhibit_repository, list_connections,
    open_workspace, test_connection, update_workspace, workspace_status,
};
use crate::svn::types::{
    CreateExhibitRepositoryPathRequest, CreateRepositoryRequest,
    EnsureProjectExhibitsAccessRequest, InitializeExhibitRepositoryRequest, SvnCheckoutRequest,
    SvnWorkspaceRequest,
};
use crate::{Options, VERSION};

pub(crate) struct CapabilityGateway {
    options: Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
}

#[derive(Clone)]
enum CapabilityHandler {
    SystemHealth,
    InnerAdminLoginStatus,
    SystemOpenFolder,
    WorkspaceBuild,
    SvnConnectionList,
    SvnConnectionTest,
    SvnWorkspaceCheckout,
    SvnWorkspaceStatus,
    SvnWorkspaceUpdate,
    SvnWorkspaceOpen,
    SvnRepositoryCreate,
    SvnExhibitRepositoryPathCreate,
    SvnExhibitRepositoryInitialize,
    SvnProjectExhibitsAccessEnsure,
    PluginList,
    PluginManifest,
    PluginInvoke,
    SkillCandidateSave,
    SkillCandidateTest,
    SkillSubmissionSubmit,
    SkillSubmissionStatus,
    PluginCandidateSave,
    PluginCandidateTest,
    PluginSubmissionSubmit,
    PluginSubmissionStatus,
    PluginCapability(String),
}

#[derive(Clone)]
struct CapabilityRegistration {
    descriptor: CapabilityDescriptor,
    handler: CapabilityHandler,
}

impl CapabilityGateway {
    pub(crate) fn new(options: Options, worker_status: Arc<Mutex<LocalWorkerStatus>>) -> Self {
        Self {
            options,
            worker_status,
        }
    }

    pub(crate) fn list_capabilities(
        &self,
        context: &InvocationContext,
    ) -> Result<Vec<CapabilityDescriptor>, Box<dyn Error>> {
        let _visibility_context = (
            context.source.as_str(),
            context.principal.as_str(),
            context.request_id.as_str(),
        );
        Ok(self
            .registry()?
            .into_values()
            .map(|registration| registration.descriptor)
            .collect())
    }

    fn registry(&self) -> Result<BTreeMap<String, CapabilityRegistration>, Box<dyn Error>> {
        let mut registry = BTreeMap::new();
        let builtins = [
            registration(
                "system.health",
                "Agent 健康状态",
                "读取本机 Agent 版本、Worker 状态、本地服务能力和登录状态。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::SystemHealth,
            ),
            registration(
                "inner_admin.login_status",
                "内网登录状态",
                "读取本机保存的内网管理系统登录状态摘要，不返回明文凭据。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::InnerAdminLoginStatus,
            ),
            registration(
                "system.open_folder",
                "打开本机文件夹",
                "用系统文件管理器打开指定本机目录。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SystemOpenFolder,
            ),
            registration(
                "exhibit.workspace.build",
                "构建展项工作区",
                "仅执行展项工程 .himind 目录内固定命名的 build.ps1、build.cmd 或 build.bat。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "target_path": { "type": "string" } },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceBuild,
            ),
            registration(
                "svn.connection.list",
                "SVN 账号状态",
                "读取本机公司 SVN 账号配置摘要，不返回密码。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::SvnConnectionList,
            ),
            registration(
                "svn.connection.test",
                "测试 SVN 账号",
                "使用本机保存的个人凭据测试指定项目展项地址，不向调用方返回密码。",
                "network_read",
                json!({
                    "type": "object",
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnConnectionTest,
            ),
            registration(
                "exhibit.workspace.checkout",
                "检出展项工作区",
                "使用本机 SVN 连接将仓库路径检出到经过校验的绝对目录。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "target_path": { "type": "string" }
                    },
                    "required": ["project_id", "exhibit_id", "target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnWorkspaceCheckout,
            ),
            registration(
                "exhibit.workspace.status",
                "读取展项工作区状态",
                "读取本机 SVN 工作副本的仓库地址、revision 和本地变更数量。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "target_path": { "type": "string" } },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnWorkspaceStatus,
            ),
            registration(
                "exhibit.workspace.update",
                "更新展项工作区",
                "使用本机 SVN 连接更新指定工作副本。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": { "target_path": { "type": "string" } },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnWorkspaceUpdate,
            ),
            registration(
                "exhibit.workspace.open",
                "打开展项 SVN 日志",
                "使用 TortoiseSVN 打开指定工作副本的日志窗口。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "target_path": { "type": "string" } },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnWorkspaceOpen,
            ),
            registration(
                "exhibit.repository_path.create",
                "创建展项 SVN 目录",
                "使用本机个人 SVN 凭据在项目仓库的 trunk/exhibits 下创建固定展项 ID 目录。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "exhibit_name": { "type": "string" }
                    },
                    "required": ["project_id", "exhibit_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnExhibitRepositoryPathCreate,
            ),
            registration(
                "exhibit.repository.initialize_template",
                "初始化展项工程模板",
                "从受控 Unity 或 Unreal 模板初始化固定展项目录，并应用模板中的 SVN 忽略属性。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "engine_type": { "type": "string", "enum": ["Unity3D", "Unreal Engine"] },
                        "template_id": { "type": "string", "enum": ["unity-default", "unreal-default"] }
                    },
                    "required": ["project_id", "exhibit_id", "engine_type", "template_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnExhibitRepositoryInitialize,
            ),
            registration(
                "project.repository.create",
                "创建项目 SVN 仓库",
                "由当前内网 Agent 使用本机加密保存的 SvnAdmin 管理凭据，按项目唯一 ID 创建物理仓库。",
                "admin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "project_name": { "type": "string" }
                    },
                    "required": ["project_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnRepositoryCreate,
            ),
            registration(
                "project.repository.exhibits_access.ensure",
                "配置项目展项目录访问权限",
                "使用隐藏 SvnAdmin 凭据确保项目 trunk/exhibits 对所有已认证 SVN 用户开放读写。",
                "admin_action",
                json!({
                    "type": "object",
                    "properties": { "project_id": { "type": "string" } },
                    "required": ["project_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnProjectExhibitsAccessEnsure,
            ),
            registration(
                "plugin.list",
                "插件列表",
                "读取本机插件注册表状态；当前阶段返回内置插件骨架。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::PluginList,
            ),
            registration(
                "plugin.manifest",
                "插件 Manifest",
                "读取指定本机插件的 Manifest、能力和权限摘要。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "plugin_id": { "type": "string" } },
                    "required": ["plugin_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::PluginManifest,
            ),
            registration(
                "plugin.invoke",
                "调用插件能力",
                "通过子进程 JSON-RPC / stdio 调用已声明的本机插件能力。",
                "plugin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "capability_id": { "type": "string" },
                        "input": { "type": "object" }
                    },
                    "required": ["capability_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::PluginInvoke,
            ),
            registration(
                "extension.skill.candidate.save",
                "保存 Skill 候选",
                "根据结构化输入生成不可变 .hmskill 候选包并返回 SHA-256。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" }, "name": { "type": "string" },
                        "version": { "type": "string" }, "description": { "type": "string" },
                        "min_agent_version": { "type": "string" },
                        "supported_clients": { "type": "array", "items": { "type": "string" } },
                        "capabilities": { "type": "array" }, "plugin_dependencies": { "type": "array" },
                        "risk_summary": { "type": "string" }, "readme": { "type": "string" }
                    },
                    "required": ["id", "name", "version", "readme"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillCandidateSave,
            ),
            registration(
                "extension.skill.candidate.test",
                "测试 Skill 候选",
                "执行 Skill 依赖预检、包校验和客户端渲染测试。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::SkillCandidateTest,
            ),
            registration(
                "extension.skill.submission.submit",
                "提交 Skill 审核",
                "显示本机候选包确认后，以绑定用户身份提交 Skill 审核。",
                "network_write",
                authoring_identity_schema(),
                CapabilityHandler::SkillSubmissionSubmit,
            ),
            registration(
                "extension.skill.submission.status",
                "Skill 提审状态",
                "读取当前绑定用户的 Skill 提审状态和审核意见。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::SkillSubmissionStatus,
            ),
            registration(
                "extension.plugin.candidate.save",
                "保存插件候选",
                "校验并保存不可变 .hmpkg 候选包，返回插件身份和 SHA-256。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "package_path": { "type": "string" } },
                    "required": ["package_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::PluginCandidateSave,
            ),
            registration(
                "extension.plugin.candidate.test",
                "测试插件候选",
                "重新解包并校验插件 Manifest、入口文件和完整性清单。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::PluginCandidateTest,
            ),
            registration(
                "extension.plugin.submission.submit",
                "提交插件审核",
                "显示本机候选包确认后，以绑定用户身份提交插件审核。",
                "network_write",
                authoring_identity_schema(),
                CapabilityHandler::PluginSubmissionSubmit,
            ),
            registration(
                "extension.plugin.submission.status",
                "插件提审状态",
                "读取当前绑定用户的插件提审状态和审核意见。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::PluginSubmissionStatus,
            ),
        ];

        for item in builtins {
            insert_registration(&mut registry, item)?;
        }

        if let Ok(plugins) = scan_plugins() {
            for plugin in plugins
                .into_iter()
                .filter(|item| item.enabled && item.runtime == "process-jsonrpc-stdio")
            {
                for capability in plugin.capabilities {
                    let capability_id = capability.id;
                    insert_registration(
                        &mut registry,
                        CapabilityRegistration {
                            descriptor: CapabilityDescriptor {
                                id: capability_id.clone(),
                                version: plugin.version.clone(),
                                name: capability.description.clone(),
                                description: capability.description,
                                risk_level: capability.risk_level,
                                source: format!("plugin:{}", plugin.id),
                                input_schema: capability.input_schema,
                            },
                            handler: CapabilityHandler::PluginCapability(capability_id),
                        },
                    )?;
                }
            }
        }
        Ok(registry)
    }

    pub(crate) fn invoke(
        &self,
        context: &InvocationContext,
        capability_id: &str,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        let registration = self
            .registry()?
            .remove(capability_id)
            .ok_or_else(|| format!("capability not found: {capability_id}"))?;
        let _invocation_metadata = (
            context.source.as_str(),
            context.principal.as_str(),
            context.session_id_hash.as_str(),
            context.request_id.as_str(),
        );
        match registration.handler {
            CapabilityHandler::SystemHealth => Ok(self.health(context)),
            CapabilityHandler::InnerAdminLoginStatus => Ok(local_login_status_json()),
            CapabilityHandler::SystemOpenFolder => self.open_folder(input),
            CapabilityHandler::WorkspaceBuild => self.build_workspace(input),
            CapabilityHandler::SvnConnectionList => Ok(json!({ "items": list_connections()? })),
            CapabilityHandler::SvnConnectionTest => self.test_svn_connection(input),
            CapabilityHandler::SvnWorkspaceCheckout => {
                checkout_workspace(serde_json::from_value::<SvnCheckoutRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceStatus => {
                workspace_status(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceUpdate => {
                update_workspace(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceOpen => {
                open_workspace(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::SvnRepositoryCreate => {
                create_repository(serde_json::from_value::<CreateRepositoryRequest>(input)?)
            }
            CapabilityHandler::SvnExhibitRepositoryPathCreate => {
                create_exhibit_repository_path(serde_json::from_value::<
                    CreateExhibitRepositoryPathRequest,
                >(input)?)
            }
            CapabilityHandler::SvnExhibitRepositoryInitialize => {
                initialize_exhibit_repository(serde_json::from_value::<
                    InitializeExhibitRepositoryRequest,
                >(input)?)
            }
            CapabilityHandler::SvnProjectExhibitsAccessEnsure => {
                ensure_project_exhibits_access(serde_json::from_value::<
                    EnsureProjectExhibitsAccessRequest,
                >(input)?)
            }
            CapabilityHandler::PluginList => registry_json(),
            CapabilityHandler::PluginManifest => self.plugin_manifest(input),
            CapabilityHandler::PluginInvoke => self.plugin_invoke(input),
            CapabilityHandler::SkillCandidateSave => Ok(serde_json::to_value(
                crate::skill::authoring::save(serde_json::from_value(input)?)?,
            )?),
            CapabilityHandler::SkillCandidateTest => self.test_skill_candidate(input),
            CapabilityHandler::SkillSubmissionSubmit => self.submit_skill_candidate(input),
            CapabilityHandler::SkillSubmissionStatus => self.skill_submission_status(),
            CapabilityHandler::PluginCandidateSave => Ok(serde_json::to_value(
                crate::plugin_authoring::save(serde_json::from_value(input)?)?,
            )?),
            CapabilityHandler::PluginCandidateTest => self.test_plugin_candidate(input),
            CapabilityHandler::PluginSubmissionSubmit => self.submit_plugin_candidate(input),
            CapabilityHandler::PluginSubmissionStatus => self.plugin_submission_status(),
            CapabilityHandler::PluginCapability(id) => invoke_plugin_capability(&id, input),
        }
    }

    pub(crate) fn health(&self, context: &InvocationContext) -> Value {
        let worker = local_worker_snapshot(&self.worker_status);
        let executable = local_agent_executable_metadata();
        json!({
            "status": "online",
            "version": VERSION,
            "mode": "local-app",
            "native_folder_picker": true,
            "tree_api": true,
            "open_folder": true,
            "open_project": true,
            "remote_connect": true,
            "agent_update": local_agent_update_supported(),
            "agent_update_signature_required": signed_agent_updates_required(),
            "agent_update_trusted_key_ids": trusted_agent_update_key_ids(),
            "executable_name": executable["name"],
            "executable_path": executable["path"],
            "login_owner": "agent",
            "login_status": local_login_status_value(),
            "dashboard_worker_online": worker["dashboard_worker_online"],
            "dashboard_agent_id": worker["dashboard_agent_id"],
            "dashboard_worker_error": worker["dashboard_worker_error"],
            "local_service_online": worker["local_service_online"],
            "local_service_error": worker["local_service_error"],
            "capability_gateway": true,
            "capabilities": self.list_capabilities(context).map(|items| items.len()).unwrap_or_default(),
            "local_port": self.options.local_port,
        })
    }

    fn open_folder(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let folder_path = input
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if folder_path.is_empty() {
            return Err("path is required".into());
        }
        open_folder(&folder_path)?;
        Ok(json!({ "ok": true, "path": folder_path }))
    }

    fn build_workspace(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let target_path = input
            .get("target_path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if target_path.is_empty() {
            return Err("target_path is required".into());
        }
        launch_workspace_build(target_path)
    }

    fn plugin_manifest(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let plugin_id = input
            .get("plugin_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if plugin_id.is_empty() {
            return Err("plugin_id is required".into());
        }
        match find_plugin(plugin_id)? {
            Some(item) => Ok(json!({ "plugin": item })),
            None => Err(format!("plugin not found: {plugin_id}").into()),
        }
    }

    fn test_svn_connection(&self, _input: Value) -> Result<Value, Box<dyn Error>> {
        test_connection()
    }

    fn plugin_invoke(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let capability_id = input
            .get("capability_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if capability_id.is_empty() {
            return Err("capability_id is required".into());
        }
        let params = input.get("input").cloned().unwrap_or_else(|| json!({}));
        invoke_plugin_capability(capability_id, params)
    }

    fn test_skill_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (id, version) = authoring_identity(&input)?;
        let capability_facts = crate::skill::capability_facts_from_gateway(
            &self.options,
            Arc::clone(&self.worker_status),
            &InvocationContext::new(
                crate::capability::types::InvocationSource::Mcp,
                "authoring-test",
            ),
        )?;
        Ok(serde_json::to_value(crate::skill::authoring::test(
            &id,
            &version,
            &capability_facts,
        )?)?)
    }

    fn submit_skill_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (id, version) = authoring_identity(&input)?;
        let draft = crate::skill::authoring::read(&id, &version)?;
        if draft.tested_at.is_none() {
            return Err("Skill 候选包尚未完成测试".into());
        }
        if !confirm_submission(
            "Skill",
            &draft.manifest.name,
            &version,
            &draft.candidate_sha256,
        ) {
            return Err("用户取消了 Skill 提审".into());
        }
        if draft.confirmed_at.is_none() {
            crate::skill::authoring::confirm(&id, &version)?;
        }
        let agent_id = self.load_paired_agent()?;
        Ok(serde_json::to_value(crate::skill::authoring::submit(
            &self.options,
            &agent_id,
            &id,
            &version,
        )?)?)
    }

    fn skill_submission_status(&self) -> Result<Value, Box<dyn Error>> {
        let agent_id = self.load_paired_agent()?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(
            json!({ "items": crate::api::distribution::skill_submissions(
            &client, &self.options.api_base, &agent_id, &self.options.agent_credential()
        )? }),
        )
    }

    fn test_plugin_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (id, version) = authoring_identity(&input)?;
        Ok(serde_json::to_value(crate::plugin_authoring::test(
            &id, &version,
        )?)?)
    }

    fn submit_plugin_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (id, version) = authoring_identity(&input)?;
        let draft = crate::plugin_authoring::read(&id, &version)?;
        if draft.tested_at.is_none() {
            return Err("插件候选包尚未完成测试".into());
        }
        if !confirm_submission(
            "插件",
            &draft.manifest.name,
            &version,
            &draft.candidate_sha256,
        ) {
            return Err("用户取消了插件提审".into());
        }
        if draft.confirmed_at.is_none() {
            crate::plugin_authoring::confirm(&id, &version)?;
        }
        let agent_id = self.load_paired_agent()?;
        Ok(serde_json::to_value(crate::plugin_authoring::submit(
            &self.options,
            &agent_id,
            &id,
            &version,
        )?)?)
    }

    fn plugin_submission_status(&self) -> Result<Value, Box<dyn Error>> {
        let agent_id = self.load_paired_agent()?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(
            json!({ "items": crate::api::distribution::plugin_submissions(
            &client, &self.options.api_base, &agent_id, &self.options.agent_credential()
        )? }),
        )
    }

    fn load_paired_agent(&self) -> Result<String, Box<dyn Error>> {
        let state: crate::api::types::AgentState =
            serde_json::from_str(&std::fs::read_to_string(&self.options.state_path)?)?;
        if state.agent_id.trim().is_empty() || state.credential.trim().is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".into());
        }
        self.options.set_agent_credential(&state.credential);
        Ok(state.agent_id)
    }
}

fn authoring_identity_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "id": { "type": "string" }, "version": { "type": "string" } },
        "required": ["id", "version"],
        "additionalProperties": false
    })
}

fn authoring_identity(input: &Value) -> Result<(String, String), Box<dyn Error>> {
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let version = input
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() || version.is_empty() {
        return Err("id and version are required".into());
    }
    Ok((id, version))
}

fn confirm_submission(kind: &str, name: &str, version: &str, sha256: &str) -> bool {
    matches!(
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title(format!("确认提交{kind}审核"))
            .set_description(format!("名称：{name}\n版本：{version}\nSHA-256：{sha256}\n\n提交后候选制品不可变，是否继续？"))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show(),
        rfd::MessageDialogResult::Yes
    )
}

fn registration(
    id: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    CapabilityRegistration {
        descriptor: CapabilityDescriptor {
            id: id.to_string(),
            version: "1.0.0".to_string(),
            name: name.to_string(),
            description: description.to_string(),
            risk_level: risk_level.to_string(),
            source: "builtin".to_string(),
            input_schema,
        },
        handler,
    }
}

fn insert_registration(
    registry: &mut BTreeMap<String, CapabilityRegistration>,
    registration: CapabilityRegistration,
) -> Result<(), Box<dyn Error>> {
    let id = registration.descriptor.id.trim();
    if id.is_empty() {
        return Err("capability id is required".into());
    }
    if registry.contains_key(id) {
        return Err(format!("duplicate capability id: {id}").into());
    }
    registry.insert(id.to_string(), registration);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_capability_ids() {
        let mut registry = BTreeMap::new();
        let first = registration(
            "system.health",
            "Health",
            "Health",
            "read_only",
            json!({}),
            CapabilityHandler::SystemHealth,
        );
        let duplicate = first.clone();

        insert_registration(&mut registry, first).unwrap();
        let error = insert_registration(&mut registry, duplicate).unwrap_err();

        assert_eq!(error.to_string(), "duplicate capability id: system.health");
    }

    #[test]
    fn rejects_empty_capability_ids() {
        let mut registry = BTreeMap::new();
        let item = registration(
            " ",
            "Invalid",
            "Invalid",
            "read_only",
            json!({}),
            CapabilityHandler::SystemHealth,
        );

        let error = insert_registration(&mut registry, item).unwrap_err();

        assert_eq!(error.to_string(), "capability id is required");
    }
}
