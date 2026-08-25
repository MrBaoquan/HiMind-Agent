use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    inspect_project_workspace, launch_project_workspace, launch_remote_connection,
    launch_workspace_build, local_agent_executable_metadata, open_folder,
    signed_agent_updates_required, trusted_agent_update_key_ids,
};
use crate::app::types::{ProjectWorkspaceRequest, RemoteConnectRequest};
use crate::capability::plugin::{
    find_plugin, invoke_plugin_capability, invoke_plugin_capability_for_plugin,
    registry_json_for_control_plane, scan_plugins,
};
use crate::capability::software_distribution::{
    attach_inspection_receipt, consume_inspection_receipt, verify_inspection_receipt,
    VerifiedSoftwareArtifact,
};
use crate::capability::types::{CapabilityAvailability, CapabilityDescriptor, InvocationContext};
use crate::store::credentials::{local_login_status_json, local_login_status_value};
use crate::store::types::LocalWorkerStatus;
use crate::svn::service::{
    checkout_workspace, create_exhibit_repository_path, create_repository_with_post_commit_hook,
    ensure_project_exhibits_access, initialize_exhibit_repository, list_connections,
    open_workspace, scan_migration_source, test_connection, update_workspace, workspace_status,
};
use crate::svn::types::{
    CreateExhibitRepositoryPathRequest, CreateRepositoryRequest,
    EnsureProjectExhibitsAccessRequest, InitializeExhibitRepositoryRequest,
    MigrationSourceScanRequest, SvnCheckoutRequest, SvnWorkspaceRequest,
};
use crate::{Options, VERSION};

pub(crate) struct CapabilityGateway {
    options: Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
}

#[derive(Clone)]
enum CapabilityHandler {
    SystemHealth,
    AuthoringIdentity,
    ExtensionWorkspaceCurrent,
    InnerAdminLoginStatus,
    SystemOpenFolder,
    WorkspaceBuild,
    WorkspaceStatus,
    WorkspaceOpen,
    RemoteConnect,
    SvnConnectionList,
    SvnConnectionTest,
    SvnWorkspaceCheckout,
    SvnWorkspaceStatus,
    MigrationSourceScan,
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
    SoftwareDistributionPublish,
    DashboardContextResolve,
    DashboardProjectContext,
    DashboardExhibitContext,
    DashboardMyWorkSummary,
    DashboardKnowledgeSearch,
    MediaSubmit(String, String),
    MediaJobGet,
    MediaJobCancel,
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
            .filter(|registration| {
                self.options.mode().control_plane_enabled()
                    || registration
                        .descriptor
                        .availability
                        .available_without_control_plane()
            })
            .map(|registration| registration.descriptor)
            .collect())
    }

    fn registry(&self) -> Result<BTreeMap<String, CapabilityRegistration>, Box<dyn Error>> {
        let mut registry = BTreeMap::new();
        let builtins = [
            registration(
                "extension.authoring.identity",
                "扩展创作身份",
                "返回当前 Agent 的创作者身份，用于生成插件和 Skill Manifest。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::AuthoringIdentity,
            ),
            registration(
                "extension.workspace.current",
                "当前扩展工作区",
                "返回 AI 会话当前工作目录，以及检测到的 HiMind 插件或 Skill 项目身份。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::ExtensionWorkspaceCurrent,
            ),
            registration(
                "workspace.current",
                "当前 AI 工作区",
                "返回 AI 会话当前工作目录，以及检测到的 HiMind 项目身份。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::ExtensionWorkspaceCurrent,
            ),
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
                "exhibit.workspace.status.local",
                "读取本机工程状态",
                "检查本机工程目录、引擎编辑器和构建入口是否可用。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "engine_type": { "type": ["string", "null"] },
                        "engine_version": { "type": ["string", "null"] }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceStatus,
            ),
            registration(
                "workspace.open.local",
                "打开本机工程",
                "使用本机已配置的 Unity 或 Unreal 编辑器打开工程。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "engine_type": { "type": ["string", "null"] },
                        "engine_version": { "type": ["string", "null"] }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceOpen,
            ),
            registration(
                "remote.connect",
                "连接远程设备",
                "使用本机配置的远程客户端连接指定设备。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "vendor": { "type": "string" },
                        "code": { "type": "string" },
                        "password": { "type": ["string", "null"] },
                        "label": { "type": ["string", "null"] }
                    },
                    "required": ["vendor", "code"],
                    "additionalProperties": false
                }),
                CapabilityHandler::RemoteConnect,
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
                        "repository_url": { "type": "string" },
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
                    "properties": {
                        "target_path": { "type": "string" },
                        "ignore_policy": {
                            "type": "object",
                            "properties": {
                                "version": { "type": "integer", "minimum": 1 },
                                "root_large_file_threshold_bytes": { "type": "integer", "minimum": 1 },
                                "root_archive_patterns": { "type": "array", "items": { "type": "string" } },
                                "excluded_relative_paths": { "type": "array", "items": { "type": "string" } },
                                "included_relative_paths": { "type": "array", "items": { "type": "string" } }
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnWorkspaceStatus,
            ),
            registration(
                "exhibit.migration_source.scan",
                "扫描历史展项工程",
                "只读扫描本机历史工程，返回规模、引擎、来源仓库和指纹，不返回绝对路径。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "target_path": { "type": "string" } },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::MigrationSourceScan,
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
                "admin_action",
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
                "admin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "engine_type": { "type": "string", "enum": ["Unity3D", "Unreal Engine"] },
                        "template_id": { "type": "string", "enum": ["unity-uniart", "unreal-blank-4.27", "unreal-blank-5.3", "unreal-blank-5.4", "unreal-blank-5.5", "unreal-picoxr-5.3", "unreal-picoxr-5.5"] }
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
                "使用隐藏 SvnAdmin 凭据开放仓库祖先节点只读遍历、默认隔离展项目录，并保留具体展项用户 ACL。",
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
                "读取当前运行模式下可用的本机插件注册表状态。",
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
                "校验并原样保存不可变 .hmskill 候选包，返回 Skill 身份和 SHA-256。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "package_path": { "type": "string" },
                        "revision_of_version": { "type": "string" },
                        "parent_submission_id": { "type": "string" }
                    },
                    "required": ["package_path"],
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
                    "properties": {
                        "package_path": { "type": "string" },
                        "revision_of_version": { "type": "string" },
                        "parent_submission_id": { "type": "string" }
                    },
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
            registration_versioned(
                "software.distribution.release.publish",
                "1.1.0",
                "发布软件版本",
                "使用 Agent 内部短时委托身份创建软件产品、上传制品并发布版本；AI 和插件均无法读取凭据。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": {"type":"string"}, "artifact_path": {"type":"string"},
                        "product_id": {"type":"string"}, "product_name": {"type":"string"},
                        "product_type": {"type":"string", "enum":["desktop_agent","agent_plugin","organization_skill","desktop_app","runtime_component","knowledge_edge_node"]},
                        "version": {"type":"string"},
                        "channel": {"type":"string"}, "platform": {"type":"string"}, "architecture": {"type":"string"},
                        "package_type": {"type":"string", "enum":["directory-zip","apk","unity-addressables","content"]},
                        "release_notes": {"type":"string"}, "mandatory": {"type":"boolean"},
                        "rollout_percent": {"type":"integer", "minimum":1, "maximum":100},
                        "inspection_receipt": {"type":"string", "minLength": 32},
                        "expected_size": {"type":"integer", "minimum":1},
                        "expected_sha256": {"type":"string", "pattern":"^[0-9a-fA-F]{64}$"},
                        "confirmed": {"type":"boolean"}
                    },
                    "required": ["workspace_root","artifact_path","product_id","product_name","version","platform","architecture","package_type","inspection_receipt","expected_size","expected_sha256","confirmed"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SoftwareDistributionPublish,
            ),
            dashboard_business_registration(
                "context.resolve",
                "解析项目业务上下文",
                "按项目名、展项名或 IP 解析当前用户可见的稳定业务实体。",
                "read_only",
                json!({
                    "type":"object",
                    "properties":{
                        "query":{"type":"string"},
                        "project_id":{"type":"string"},
                        "entity_types":{"type":"array","items":{"type":"string","enum":["project","exhibit"]}}
                    },
                    "required":["query"],
                    "additionalProperties":false
                }),
                CapabilityHandler::DashboardContextResolve,
            ),
            dashboard_business_registration(
                "project.context.get",
                "项目全景",
                "读取当前用户可见的项目、展项、需求和健康度聚合事实。",
                "read_only",
                json!({
                    "type":"object",
                    "properties":{"project_id":{"type":"string"}},
                    "required":["project_id"],
                    "additionalProperties":false
                }),
                CapabilityHandler::DashboardProjectContext,
            ),
            dashboard_business_registration(
                "exhibit.context.get",
                "展项全景",
                "读取展项 IP、设备、成员、需求和最近推进事件。",
                "read_only",
                json!({
                    "type":"object",
                    "properties":{"exhibit_id":{"type":"string"}},
                    "required":["exhibit_id"],
                    "additionalProperties":false
                }),
                CapabilityHandler::DashboardExhibitContext,
            ),
            dashboard_business_registration(
                "work.my_summary",
                "我的工作摘要",
                "读取当前用户负责或关注的项目、展项和需求摘要。",
                "read_only",
                json!({"type":"object","additionalProperties":false}),
                CapabilityHandler::DashboardMyWorkSummary,
            ),
            dashboard_knowledge_registration(
                "knowledge.search.v1",
                "知识检索",
                "检索当前用户允许外部 AI 工具访问的知识空间，返回原文片段、稳定引用和 Trace ID；不调用 HiMind 模型。",
                "read_only",
                json!({
                    "type":"object",
                    "properties":{
                        "query":{"type":"string","maxLength":2000},
                        "space_ids":{"type":"array","items":{"type":"string"}},
                        "project_id":{"type":"string"},
                        "exhibit_id":{"type":"string"},
                        "top_k":{"type":"integer","minimum":1,"maximum":20}
                    },
                    "required":["query"],
                    "additionalProperties":false
                }),
                CapabilityHandler::DashboardKnowledgeSearch,
            ),
            media_registration("media.image.generate", "生成图片", "根据提示词生成图片，返回可查询的媒体任务。", "network_write", media_generate_schema(false), CapabilityHandler::MediaSubmit("image".into(), "generate".into())),
            media_registration("media.image.edit", "编辑图片", "根据提示词和参考图片编辑图像，返回可查询的媒体任务。", "network_write", media_generate_schema(true), CapabilityHandler::MediaSubmit("image".into(), "edit".into())),
            media_registration("media.video.generate", "生成视频", "根据提示词和可选参考素材生成视频，返回可查询的媒体任务。", "network_write", media_generate_schema(false), CapabilityHandler::MediaSubmit("video".into(), "generate".into())),
            media_registration("media.audio.speech", "生成语音", "将文案合成为语音，返回可查询的媒体任务。", "network_write", media_generate_schema(false), CapabilityHandler::MediaSubmit("speech".into(), "generate".into())),
            media_registration("media.audio.transcribe", "语音转写", "转写已上传的音频文件，返回可查询的媒体任务。", "network_write", media_transcribe_schema(), CapabilityHandler::MediaSubmit("transcription".into(), "transcribe".into())),
            media_registration("media.job.get", "查看媒体任务", "查看图片、视频或语音任务的状态和输出文件。", "read_only", media_job_schema(), CapabilityHandler::MediaJobGet),
            media_registration("media.job.cancel", "取消媒体任务", "取消仍在排队或执行中的媒体任务。", "network_write", media_job_schema(), CapabilityHandler::MediaJobCancel),
        ];

        for mut item in builtins {
            item.descriptor.availability = availability_for_handler(&item.handler);
            insert_registration(&mut registry, item)?;
        }

        if let Ok(plugins) = scan_plugins() {
            for plugin in plugins
                .into_iter()
                .filter(|item| item.enabled && item.runtime == "process-jsonrpc-stdio")
            {
                for capability in &plugin.capabilities {
                    let availability = plugin_capability_availability(&plugin, &capability);
                    let capability_id = capability.id.clone();
                    insert_registration(
                        &mut registry,
                        CapabilityRegistration {
                            descriptor: CapabilityDescriptor {
                                id: capability_id.clone(),
                                version: plugin.version.clone(),
                                name: capability.description.clone(),
                                description: capability.description.clone(),
                                risk_level: capability.risk_level.clone(),
                                source: format!("plugin:{}", plugin.id),
                                availability,
                                input_schema: capability.input_schema.clone(),
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
        if matches!(
            registration.descriptor.availability,
            CapabilityAvailability::ControlPlane
        ) && !self.options.mode().control_plane_enabled()
        {
            return Err(serde_json::json!({
                "code": "control_plane_required",
                "capability_id": capability_id,
                "message": "当前运行模式不支持此能力；如需使用，请在设置中切换 Connected 模式并重启 Agent"
            })
            .to_string()
            .into());
        }
        if is_svn_admin_capability(capability_id)
            && context.source != crate::capability::types::InvocationSource::DashboardWorker
        {
            return Err(
                "SVN management capabilities are restricted to Dashboard Worker tasks".into(),
            );
        }
        if let Some(scope) = required_platform_scope(capability_id) {
            crate::api::oauth::platform_access_token(&self.options, scope)?;
        }
        let _invocation_metadata = (
            context.source.as_str(),
            context.principal.as_str(),
            context.session_id_hash.as_str(),
            context.request_id.as_str(),
        );
        match registration.handler {
            CapabilityHandler::SystemHealth => Ok(self.health(context)),
            CapabilityHandler::AuthoringIdentity => self.current_authoring_identity(),
            CapabilityHandler::ExtensionWorkspaceCurrent => {
                crate::extension_projects::current_workspace()
            }
            CapabilityHandler::InnerAdminLoginStatus => Ok(local_login_status_json()),
            CapabilityHandler::SystemOpenFolder => self.open_folder(input),
            CapabilityHandler::WorkspaceBuild => self.build_workspace(input),
            CapabilityHandler::WorkspaceStatus => self.workspace_status(input),
            CapabilityHandler::WorkspaceOpen => self.workspace_open(input),
            CapabilityHandler::RemoteConnect => self.remote_connect(input),
            CapabilityHandler::SvnConnectionList => Ok(json!({ "items": list_connections()? })),
            CapabilityHandler::SvnConnectionTest => self.test_svn_connection(input),
            CapabilityHandler::SvnWorkspaceCheckout => {
                checkout_workspace(serde_json::from_value::<SvnCheckoutRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceStatus => {
                workspace_status(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::MigrationSourceScan => {
                scan_migration_source(serde_json::from_value::<MigrationSourceScanRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceUpdate => {
                update_workspace(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::SvnWorkspaceOpen => {
                open_workspace(serde_json::from_value::<SvnWorkspaceRequest>(input)?)
            }
            CapabilityHandler::SvnRepositoryCreate => {
                create_repository_with_post_commit_hook(serde_json::from_value::<
                    CreateRepositoryRequest,
                >(input)?)
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
            CapabilityHandler::PluginList => {
                registry_json_for_control_plane(self.options.mode().control_plane_enabled())
            }
            CapabilityHandler::PluginManifest => self.plugin_manifest(input),
            CapabilityHandler::PluginInvoke => self.plugin_invoke(context, input),
            CapabilityHandler::SkillCandidateSave => {
                validate_mcp_candidate_package(context, capability_id, &input)?;
                self.save_skill_candidate(input)
            }
            CapabilityHandler::SkillCandidateTest => self.test_skill_candidate(input),
            CapabilityHandler::SkillSubmissionSubmit => self.submit_skill_candidate(input),
            CapabilityHandler::SkillSubmissionStatus => self.skill_submission_status(),
            CapabilityHandler::PluginCandidateSave => {
                validate_mcp_candidate_package(context, capability_id, &input)?;
                Ok(serde_json::to_value(crate::plugin_authoring::save(
                    serde_json::from_value(input)?,
                )?)?)
            }
            CapabilityHandler::PluginCandidateTest => self.test_plugin_candidate(input),
            CapabilityHandler::PluginSubmissionSubmit => self.submit_plugin_candidate(input),
            CapabilityHandler::PluginSubmissionStatus => self.plugin_submission_status(),
            CapabilityHandler::SoftwareDistributionPublish => {
                self.publish_software_release(context, input)
            }
            CapabilityHandler::DashboardContextResolve => {
                crate::api::dashboard_business::resolve_context(&self.options, input)
            }
            CapabilityHandler::DashboardProjectContext => {
                crate::api::dashboard_business::project_context(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitContext => {
                crate::api::dashboard_business::exhibit_context(&self.options, input)
            }
            CapabilityHandler::DashboardMyWorkSummary => {
                crate::api::dashboard_business::my_work_summary(&self.options)
            }
            CapabilityHandler::DashboardKnowledgeSearch => {
                crate::api::dashboard_business::search_knowledge(&self.options, input)
            }
            CapabilityHandler::MediaSubmit(kind, operation) => {
                crate::api::media::submit(&self.options, &kind, &operation, input)
            }
            CapabilityHandler::MediaJobGet => crate::api::media::get(&self.options, input),
            CapabilityHandler::MediaJobCancel => crate::api::media::cancel(&self.options, input),
            CapabilityHandler::PluginCapability(id) => {
                validate_mcp_capability_workspace(context, &id, &input)?;
                let output =
                    invoke_plugin_capability(&id, input.clone(), self.trusted_dashboard_url())?;
                finalize_plugin_capability(context, &id, &input, output)
            }
        }
    }

    pub(crate) fn health(&self, context: &InvocationContext) -> Value {
        let worker = local_worker_snapshot(&self.worker_status);
        let executable = local_agent_executable_metadata();
        json!({
            "status": "online",
            "version": VERSION,
            "mode": self.options.mode().as_str(),
            "dashboard_enabled": self.options.mode().dashboard_enabled(),
            "control_plane": json!({
                "kind": self.options.mode().control_plane(),
                "enabled": self.options.mode().control_plane_enabled(),
                "worker_online": worker["dashboard_worker_online"],
            }),
            "native_folder_picker": true,
            "tree_api": true,
            "open_folder": true,
            "open_project": true,
            "remote_connect": true,
            "agent_update_signature_required": signed_agent_updates_required(),
            "agent_update_trusted_key_ids": trusted_agent_update_key_ids(),
            "executable_name": executable["name"],
            "executable_path": executable["path"],
            "login_owner": "agent",
            "login_status": local_login_status_value(),
            "dashboard_worker_online": worker["dashboard_worker_online"],
            "dashboard_agent_id": worker["dashboard_agent_id"],
            "dashboard_worker_error": worker["dashboard_worker_error"],
            "svn_admin_ready": crate::svn::service::svn_admin_ready(),
            "svn_admin_status": crate::svn::service::svn_admin_status(),
            "local_service_online": worker["local_service_online"],
            "local_service_error": worker["local_service_error"],
            "capability_gateway": true,
            "capabilities": self.list_capabilities(context).map(|items| items.len()).unwrap_or_default(),
            "local_port": self.options.local_port,
            "profile": crate::store::paths::profile_name(),
        })
    }

    fn current_authoring_identity(&self) -> Result<Value, Box<dyn Error>> {
        let identity = crate::app::identity::authoring_identity(&self.options);
        Ok(json!({
            "user_id": identity.user_id,
            "user_name": identity.user_name,
            "online_verified": identity.online_verified,
            "source": identity.source,
            "scopes": []
        }))
    }

    fn save_skill_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::to_value(
            crate::skill::authoring::import_package(serde_json::from_value(input)?)?,
        )?)
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

    fn workspace_status(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let request: ProjectWorkspaceRequest = serde_json::from_value(input)?;
        inspect_project_workspace(
            &request.path,
            request.engine_type.as_deref(),
            request.engine_version.as_deref(),
        )
    }

    fn workspace_open(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let request: ProjectWorkspaceRequest = serde_json::from_value(input)?;
        launch_project_workspace(
            &request.path,
            request.engine_type.as_deref(),
            request.engine_version.as_deref(),
        )
    }

    fn remote_connect(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let request: RemoteConnectRequest = serde_json::from_value(input)?;
        launch_remote_connection(&request, &self.options.state_path)
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
            Some(item)
                if self.options.mode().control_plane_enabled()
                    || item.availability != "control_plane" =>
            {
                Ok(json!({ "plugin": item }))
            }
            None => Err(format!("plugin not found: {plugin_id}").into()),
            Some(_) => Err(format!("plugin not found: {plugin_id}").into()),
        }
    }

    fn test_svn_connection(&self, _input: Value) -> Result<Value, Box<dyn Error>> {
        test_connection()
    }

    fn plugin_invoke(
        &self,
        context: &InvocationContext,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        let capability_id = input
            .get("capability_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if capability_id.is_empty() {
            return Err("capability_id is required".into());
        }
        let plugin = scan_plugins()?
            .into_iter()
            .find(|item| {
                item.enabled
                    && item.runtime == "process-jsonrpc-stdio"
                    && item
                        .capabilities
                        .iter()
                        .any(|capability| capability.id == capability_id)
            })
            .ok_or_else(|| format!("plugin capability not found: {capability_id}"))?;
        let capability = plugin
            .capabilities
            .iter()
            .find(|capability| capability.id == capability_id)
            .ok_or_else(|| format!("plugin capability not found: {capability_id}"))?;
        if matches!(
            plugin_capability_availability(&plugin, capability),
            CapabilityAvailability::ControlPlane
        ) && !self.options.mode().control_plane_enabled()
        {
            return Err(serde_json::json!({
                "code": "control_plane_required",
                "capability_id": capability_id,
                "message": "当前运行模式不支持此能力；如需使用，请在设置中切换 Connected 模式并重启 Agent"
            })
            .to_string()
            .into());
        }
        let params = input.get("input").cloned().unwrap_or_else(|| json!({}));
        validate_mcp_capability_workspace(context, capability_id, &params)?;
        let output = invoke_plugin_capability_for_plugin(
            &plugin.id,
            capability_id,
            params.clone(),
            self.trusted_dashboard_url(),
        )?;
        finalize_plugin_capability(context, capability_id, &params, output)
    }

    fn trusted_dashboard_url(&self) -> Option<&str> {
        self.options
            .mode()
            .control_plane_enabled()
            .then_some(self.options.api_base.as_str())
    }

    fn publish_software_release(
        &self,
        context: &InvocationContext,
        input: Value,
    ) -> Result<Value, Box<dyn Error>> {
        let mut request = serde_json::from_value::<
            crate::api::distribution::SoftwareReleasePublishRequest,
        >(input)?;
        if !request.confirmed {
            return Err("发布软件版本前必须获得用户明确确认".into());
        }
        request.product_id = request.product_id.trim().to_ascii_lowercase();
        request.channel = request.channel.trim().to_ascii_lowercase();
        request.platform = request.platform.trim().to_ascii_lowercase();
        request.architecture = request.architecture.trim().to_ascii_lowercase();
        request.package_type = request.package_type.trim().to_ascii_lowercase();
        request.expected_sha256 = request.expected_sha256.trim().to_ascii_lowercase();
        validate_distribution_publish_request(&request)?;
        validate_mcp_capability_workspace(
            context,
            "software.distribution.release.publish",
            &serde_json::json!({ "workspace_root": request.workspace_root.clone() }),
        )?;
        let verified = verify_inspection_receipt(context, &request)?;
        if requires_native_software_confirmation(context)
            && !confirm_software_publish(&request, &verified)
        {
            return Err("用户取消了软件版本发布".into());
        }
        consume_inspection_receipt(&verified)?;
        request.artifact_path = verified.artifact_path.to_string_lossy().to_string();
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::RELEASE_MANAGE_SCOPE,
        )?;
        let agent_id = crate::api::client::load_agent_state(&self.options.state_path)?.agent_id;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10 * 60))
            .build()?;
        crate::api::distribution::publish_software_release_with_artifact(
            &client,
            &self.options.api_base,
            &agent_id,
            &access.token,
            &request,
            verified.file,
            verified.size,
            verified.file_name,
        )
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
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(
            json!({ "items": crate::api::distribution::skill_submissions(
            &client, &self.options.api_base, &agent_id, &access.token
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
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(
            json!({ "items": crate::api::distribution::plugin_submissions(
            &client, &self.options.api_base, &agent_id, &access.token
        )? }),
        )
    }

    fn load_paired_agent(&self) -> Result<String, Box<dyn Error>> {
        let state = crate::api::client::load_agent_state(&self.options.state_path)?;
        if state.agent_id.trim().is_empty() || state.credential.trim().is_empty() {
            return Err("Agent 尚未完成 Dashboard 配对".into());
        }
        self.options.set_agent_credential(&state.credential);
        Ok(state.agent_id)
    }
}

fn is_svn_admin_capability(capability_id: &str) -> bool {
    matches!(
        capability_id,
        "project.repository.create"
            | "project.repository.exhibits_access.ensure"
            | "exhibit.repository_path.create"
            | "exhibit.repository.initialize_template"
    )
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

fn requires_native_software_confirmation(context: &InvocationContext) -> bool {
    context.source == crate::capability::types::InvocationSource::Mcp
}

fn confirm_software_publish(
    request: &crate::api::distribution::SoftwareReleasePublishRequest,
    artifact: &VerifiedSoftwareArtifact,
) -> bool {
    matches!(
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("确认发布软件版本")
            .set_description(format!(
                "产品：{} ({})\n版本：{}\n渠道：{}\n目标：{}/{}/{}\n文件：{}\n大小：{} 字节\nSHA-256：{}\n强制更新：{}\n灰度比例：{}%\n\n该操作会创建不可变制品并发布到组织分发服务，是否继续？",
                request.product_name,
                request.product_id,
                request.version,
                request.channel,
                request.platform,
                request.architecture,
                request.package_type,
                artifact.file_name,
                artifact.size,
                artifact.sha256,
                if request.mandatory { "是" } else { "否" },
                request.rollout_percent,
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show(),
        rfd::MessageDialogResult::Yes
    )
}

fn required_platform_scope(capability_id: &str) -> Option<&'static str> {
    match capability_id {
        "extension.skill.submission.submit"
        | "extension.skill.submission.status"
        | "extension.plugin.submission.submit"
        | "extension.plugin.submission.status" => Some(crate::api::oauth::CREATIVE_SUBMIT_SCOPE),
        "software.distribution.release.publish" => Some(crate::api::oauth::RELEASE_MANAGE_SCOPE),
        "context.resolve" | "project.context.get" | "exhibit.context.get" | "work.my_summary" => {
            Some(crate::api::oauth::BUSINESS_CONTEXT_READ_SCOPE)
        }
        "knowledge.search.v1" => Some(crate::api::oauth::KNOWLEDGE_SEARCH_SCOPE),
        "media.image.generate"
        | "media.image.edit"
        | "media.video.generate"
        | "media.audio.speech"
        | "media.audio.transcribe" => Some(crate::api::oauth::MEDIA_SUBMIT_SCOPE),
        "media.job.get" => Some(crate::api::oauth::MEDIA_READ_SCOPE),
        "media.job.cancel" => Some(crate::api::oauth::MEDIA_CANCEL_SCOPE),
        _ => None,
    }
}

fn availability_for_handler(handler: &CapabilityHandler) -> CapabilityAvailability {
    match handler {
        CapabilityHandler::AuthoringIdentity
        | CapabilityHandler::ExtensionWorkspaceCurrent
        | CapabilityHandler::SkillCandidateSave
        | CapabilityHandler::SkillCandidateTest
        | CapabilityHandler::PluginCandidateSave
        | CapabilityHandler::PluginCandidateTest => CapabilityAvailability::Local,
        CapabilityHandler::SkillSubmissionSubmit
        | CapabilityHandler::SkillSubmissionStatus
        | CapabilityHandler::PluginSubmissionSubmit
        | CapabilityHandler::PluginSubmissionStatus
        | CapabilityHandler::SoftwareDistributionPublish
        | CapabilityHandler::DashboardContextResolve
        | CapabilityHandler::DashboardProjectContext
        | CapabilityHandler::DashboardExhibitContext
        | CapabilityHandler::DashboardMyWorkSummary
        | CapabilityHandler::DashboardKnowledgeSearch
        | CapabilityHandler::MediaSubmit(_, _)
        | CapabilityHandler::MediaJobGet
        | CapabilityHandler::MediaJobCancel => CapabilityAvailability::ControlPlane,
        CapabilityHandler::SvnConnectionTest
        | CapabilityHandler::SvnWorkspaceCheckout
        | CapabilityHandler::SvnWorkspaceStatus
        | CapabilityHandler::MigrationSourceScan
        | CapabilityHandler::SvnWorkspaceUpdate
        | CapabilityHandler::SvnWorkspaceOpen => CapabilityAvailability::NetworkService,
        CapabilityHandler::SvnRepositoryCreate
        | CapabilityHandler::SvnExhibitRepositoryPathCreate
        | CapabilityHandler::SvnExhibitRepositoryInitialize
        | CapabilityHandler::SvnProjectExhibitsAccessEnsure => CapabilityAvailability::ControlPlane,
        CapabilityHandler::PluginCapability(_) => CapabilityAvailability::Local,
        _ => CapabilityAvailability::Local,
    }
}

fn plugin_capability_availability(
    plugin: &crate::capability::plugin::PluginRegistryItem,
    capability: &crate::capability::plugin::PluginCapabilityManifest,
) -> CapabilityAvailability {
    match capability.availability.trim().to_ascii_lowercase().as_str() {
        "control_plane" | "dashboard" => CapabilityAvailability::ControlPlane,
        "network_service" | "network" => CapabilityAvailability::NetworkService,
        "local" => CapabilityAvailability::Local,
        _ if plugin
            .permissions
            .iter()
            .any(|permission| permission == "network.dashboard.public") =>
        {
            CapabilityAvailability::ControlPlane
        }
        _ => CapabilityAvailability::Local,
    }
}

fn finalize_plugin_capability(
    context: &InvocationContext,
    capability_id: &str,
    input: &Value,
    output: Value,
) -> Result<Value, Box<dyn Error>> {
    if capability_id == "software.distribution.artifact.inspect" {
        attach_inspection_receipt(context, input, output)
    } else {
        Ok(output)
    }
}

fn validate_mcp_capability_workspace(
    context: &InvocationContext,
    capability_id: &str,
    input: &Value,
) -> Result<(), Box<dyn Error>> {
    if context.source != crate::capability::types::InvocationSource::Mcp {
        return Ok(());
    }
    let extension_scoped = capability_id.starts_with("extension.");
    let software_scoped = matches!(
        capability_id,
        "software.distribution.project.inspect"
            | "software.distribution.artifact.inspect"
            | "software.distribution.release.publish"
    );
    if !extension_scoped && !software_scoped {
        return Ok(());
    }
    let Some(workspace_root) = input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return if software_scoped {
            Err("软件分发能力必须提供当前 AI 工作区 workspace_root".into())
        } else {
            Ok(())
        };
    };
    let current = crate::extension_projects::current_workspace_path()?;
    if software_scoped {
        validate_software_workspace_root(&current, workspace_root)
    } else {
        validate_extension_workspace_root(&current, workspace_root)
    }
}

fn validate_mcp_candidate_package(
    context: &InvocationContext,
    capability_id: &str,
    input: &Value,
) -> Result<(), Box<dyn Error>> {
    if context.source != crate::capability::types::InvocationSource::Mcp
        || !matches!(
            capability_id,
            "extension.plugin.candidate.save" | "extension.skill.candidate.save"
        )
    {
        return Ok(());
    }
    let package_path = input
        .get("package_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("package_path is required")?;
    let current = crate::extension_projects::current_workspace_path()?;
    let package = Path::new(package_path).canonicalize()?;
    if !package.is_file() || !package.starts_with(&current) {
        return Err("候选包必须位于当前 AI 扩展工作区内".into());
    }
    Ok(())
}

fn validate_extension_workspace_root(
    current: &Path,
    requested: &str,
) -> Result<(), Box<dyn Error>> {
    let requested = Path::new(requested).canonicalize()?;
    if requested != current {
        return Err(serde_json::json!({
            "code": "extension_workspace_mismatch",
            "message": "workspace_root 必须与 extension.workspace.current 返回的当前 AI 工作区一致"
        })
        .to_string()
        .into());
    }
    Ok(())
}

fn validate_software_workspace_root(
    current: &Path,
    requested: &str,
) -> Result<(), Box<dyn Error>> {
    let requested = Path::new(requested).canonicalize()?;
    if requested != current {
        return Err(serde_json::json!({
            "code": "software_workspace_mismatch",
            "message": "workspace_root 必须与 workspace.current 返回的当前 AI 工作区一致"
        })
        .to_string()
        .into());
    }
    Ok(())
}

fn validate_distribution_publish_request(
    request: &crate::api::distribution::SoftwareReleasePublishRequest,
) -> Result<(), Box<dyn Error>> {
    fn stable_identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }
    if !stable_identifier(&request.product_id)
        || !stable_identifier(&request.channel)
        || !stable_identifier(&request.platform)
        || !stable_identifier(&request.architecture)
    {
        return Err("软件产品、渠道、平台或架构标识无效".into());
    }
    if request.product_name.trim().is_empty() || request.version.trim().is_empty() {
        return Err("product_name 和 version 不能为空".into());
    }
    if request.inspection_receipt.trim().is_empty()
        || request.expected_size == 0
        || request.expected_sha256.len() != 64
        || !request
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("发布必须携带有效的制品预检凭证、大小和 SHA-256".into());
    }
    if !matches!(
        request.product_type.as_str(),
        "desktop_agent"
            | "agent_plugin"
            | "organization_skill"
            | "desktop_app"
            | "runtime_component"
            | "knowledge_edge_node"
    ) {
        return Err("不支持的 product_type".into());
    }
    if !(1..=100).contains(&request.rollout_percent) {
        return Err("rollout_percent 必须在 1 到 100 之间".into());
    }
    if !matches!(
        request.package_type.as_str(),
        "directory-zip" | "apk" | "unity-addressables" | "content"
    ) {
        return Err("不支持的 package_type".into());
    }
    Ok(())
}

fn registration(
    id: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    registration_versioned(
        id,
        "1.0.0",
        name,
        description,
        risk_level,
        input_schema,
        handler,
    )
}

fn registration_versioned(
    id: &str,
    version: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    CapabilityRegistration {
        descriptor: CapabilityDescriptor {
            id: id.to_string(),
            version: version.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            risk_level: risk_level.to_string(),
            source: "builtin".to_string(),
            availability: CapabilityAvailability::Local,
            input_schema,
        },
        handler,
    }
}

fn dashboard_business_registration(
    id: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    let mut item = registration(id, name, description, risk_level, input_schema, handler);
    item.descriptor.source = "plugin:com.himind.dashboard-business".to_string();
    item.descriptor.availability = CapabilityAvailability::ControlPlane;
    item
}

fn dashboard_knowledge_registration(
    id: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    let mut item = registration(id, name, description, risk_level, input_schema, handler);
    item.descriptor.source = "plugin:com.himind.knowledge".to_string();
    item.descriptor.availability = CapabilityAvailability::ControlPlane;
    item
}

fn media_registration(
    id: &str,
    name: &str,
    description: &str,
    risk_level: &str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> CapabilityRegistration {
    let mut item = registration(id, name, description, risk_level, input_schema, handler);
    item.descriptor.source = "builtin:himind-media".to_string();
    item.descriptor.availability = CapabilityAvailability::ControlPlane;
    item
}

fn media_generate_schema(reference_required: bool) -> Value {
    let mut required = vec!["prompt"];
    if reference_required {
        required.push("reference_file_ids");
    }
    json!({
        "type": "object",
        "properties": {
            "prompt": {"type":"string", "maxLength":12000},
            "model": {"type":"string"},
            "reference_file_ids": {"type":"array", "maxItems":16, "items":{"type":"string"}},
            "parameters": {
                "type":"object",
                "properties": {
                    "aspect_ratio":{"type":"string"},
                    "resolution":{"type":"string"},
                    "duration_seconds":{"type":"number", "minimum":1},
                    "voice":{"type":"string"},
                    "format":{"type":"string"},
                    "output_count":{"type":"integer", "minimum":1, "maximum":8}
                },
                "additionalProperties":true
            },
            "project_id":{"type":"string"},
            "work_item_id":{"type":"string"},
            "agent_run_id":{"type":"string"}
        },
        "required": required,
        "additionalProperties": false
    })
}

fn media_transcribe_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "reference_file_ids":{"type":"array", "minItems":1, "maxItems":16, "items":{"type":"string"}},
            "model":{"type":"string"},
            "parameters":{"type":"object", "additionalProperties":true},
            "project_id":{"type":"string"},
            "work_item_id":{"type":"string"},
            "agent_run_id":{"type":"string"}
        },
        "required":["reference_file_ids"],
        "additionalProperties":false
    })
}

fn media_job_schema() -> Value {
    json!({
        "type":"object",
        "properties":{"job_id":{"type":"string"}},
        "required":["job_id"],
        "additionalProperties":false
    })
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
    fn svn_admin_capabilities_are_worker_only() {
        assert!(is_svn_admin_capability("project.repository.create"));
        assert!(is_svn_admin_capability(
            "exhibit.repository.initialize_template"
        ));
        assert!(!is_svn_admin_capability("exhibit.workspace.checkout"));
    }

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

    #[test]
    fn dashboard_business_capabilities_report_the_builtin_plugin_provider() {
        let item = dashboard_business_registration(
            "exhibit.context.get",
            "展项全景",
            "读取展项事实",
            "read_only",
            json!({}),
            CapabilityHandler::DashboardExhibitContext,
        );
        assert_eq!(
            item.descriptor.source,
            "plugin:com.himind.dashboard-business"
        );
    }

    #[test]
    fn dashboard_business_capabilities_use_business_scope_not_model_scope() {
        assert_eq!(
            required_platform_scope("context.resolve"),
            Some(crate::api::oauth::BUSINESS_CONTEXT_READ_SCOPE)
        );
        assert_ne!(
            required_platform_scope("context.resolve"),
            Some(crate::api::oauth::AI_CONVERSATION_SCOPE)
        );
    }

    #[test]
    fn knowledge_search_uses_knowledge_scope_not_model_scope() {
        assert_eq!(
            required_platform_scope("knowledge.search.v1"),
            Some(crate::api::oauth::KNOWLEDGE_SEARCH_SCOPE)
        );
        assert_ne!(
            required_platform_scope("knowledge.search.v1"),
            Some(crate::api::oauth::AI_CONVERSATION_SCOPE)
        );
    }

    #[test]
    fn independent_mode_rejects_dashboard_worker_capabilities() {
        let root = std::env::temp_dir().join(format!(
            "himind-capability-independent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut options = crate::Options::from_env();
        options.state_path = root.join("agent-state.json");
        crate::app::runtime_mode::save(
            &options.state_path,
            crate::app::runtime_mode::AgentMode::Independent,
        )
        .unwrap();
        options.effective_mode = crate::app::runtime_mode::AgentMode::Independent;
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let error = gateway
            .invoke(
                &InvocationContext::new(
                    crate::capability::types::InvocationSource::LocalHttp,
                    "local-user",
                ),
                "project.repository.create",
                json!({}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("control_plane_required"));
        assert!(error.contains("当前运行模式不支持"));
        let local_error = gateway
            .invoke(
                &InvocationContext::new(
                    crate::capability::types::InvocationSource::Mcp,
                    "ai-client:himind-ai",
                ),
                "extension.skill.candidate.save",
                json!({ "package_path": root.join("missing.hmskill") }),
            )
            .unwrap_err()
            .to_string();
        assert!(!local_error.contains("control_plane_required"));
        let visible = gateway
            .list_capabilities(&InvocationContext::new(
                crate::capability::types::InvocationSource::LocalHttp,
                "local-user",
            ))
            .unwrap();
        assert!(visible.iter().any(|item| item.id == "system.health"));
        assert!(visible
            .iter()
            .any(|item| item.id == "exhibit.workspace.status.local"));
        assert!(visible.iter().any(|item| item.id == "svn.connection.test"));
        assert!(!visible.iter().any(|item| item.id == "context.resolve"));
        assert!(!visible.iter().any(|item| item.id == "media.image.generate"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn connected_mode_exposes_control_plane_capabilities_without_hiding_local_ones() {
        let root = std::env::temp_dir().join(format!(
            "himind-capability-connected-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut options = crate::Options::from_env();
        options.state_path = root.join("agent-state.json");
        options.effective_mode = crate::app::runtime_mode::AgentMode::Connected;
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let visible = gateway
            .list_capabilities(&InvocationContext::new(
                crate::capability::types::InvocationSource::LocalHttp,
                "local-user",
            ))
            .unwrap();
        assert!(visible.iter().any(|item| item.id == "system.health"));
        assert!(visible.iter().any(|item| item.id == "context.resolve"));
        assert!(visible.iter().any(|item| item.id == "media.image.generate"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn extension_development_workspace_must_match_the_ai_session_root() {
        let root = std::env::temp_dir().join(format!(
            "himind-extension-workspace-boundary-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let current = workspace.canonicalize().unwrap();

        validate_extension_workspace_root(&current, workspace.to_str().unwrap()).unwrap();
        let error = validate_extension_workspace_root(&current, outside.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("extension_workspace_mismatch"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn software_distribution_workspace_must_match_the_ai_session_root() {
        let root = std::env::temp_dir().join(format!(
            "himind-software-workspace-boundary-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let current = workspace.canonicalize().unwrap();

        validate_software_workspace_root(&current, workspace.to_str().unwrap()).unwrap();
        let error = validate_software_workspace_root(&current, outside.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("software_workspace_mismatch"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_mcp_software_publish_requires_native_confirmation() {
        let mcp = InvocationContext::new(
            crate::capability::types::InvocationSource::Mcp,
            "ai-client:test",
        );
        let cli = InvocationContext::new(
            crate::capability::types::InvocationSource::Cli,
            "local-cli",
        );
        assert!(requires_native_software_confirmation(&mcp));
        assert!(!requires_native_software_confirmation(&cli));
    }
}
