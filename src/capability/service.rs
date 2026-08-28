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
use crate::app::{mcp_downstream::DownstreamMcpManager, mcp_registry, mcp_targets};
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
    downstream_mcp: DownstreamMcpManager,
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
    SkillClientRegister,
    SkillClientUnregister,
    SkillClientsUnregister,
    SkillSubmissionSubmit,
    SkillSubmissionStatus,
    PluginCandidateSave,
    PluginCandidateTest,
    PluginSubmissionSubmit,
    PluginSubmissionStatus,
    ExtensionReviewQueue,
    ExtensionReviewGet,
    ExtensionReviewDecide,
    SoftwareDistributionPublish,
    DashboardContextResolve,
    DashboardProjectContext,
    DashboardExhibitContext,
    DashboardMyWorkSummary,
    DashboardKnowledgeSearch,
    DashboardProjectList,
    DashboardProjectCreate,
    DashboardProjectUpdate,
    DashboardProjectDelete,
    DashboardExhibitList,
    DashboardExhibitCreate,
    DashboardExhibitUpdate,
    DashboardExhibitDelete,
    DashboardProjectManagersReplace,
    DashboardProjectOwnersReplace,
    DashboardExhibitCrewReplace,
    DashboardProjectExhibitAttach,
    DashboardProjectExhibitDetach,
    DashboardExhibitWorkspaceGet,
    DashboardExhibitWorkspaceBind,
    DashboardExhibitWorkspaceCheckout,
    DashboardPeopleSearch,
    DashboardRequirementList,
    DashboardRequirementGet,
    DashboardRequirementCreate,
    DashboardRequirementUpdate,
    DashboardRequirementAssignmentUpdate,
    DashboardRequirementCancel,
    DashboardRequirementReopen,
    DashboardRequirementReview,
    DashboardRequirementComment,
    MediaSubmit(String, String),
    MediaJobGet,
    MediaJobCancel,
    PluginCapability(String),
    DownstreamMcp(String),
    McpServerList,
    McpTargetList,
    McpServerInspect,
    McpServerUpsert,
    McpServerRemove,
    McpRegistrationPlan,
    McpRegistrationApply,
    McpRegistrationApplyAll,
    McpRegistrationRemove,
    McpRegistrationRemoveAll,
    McpConnectionTest,
}

#[derive(Clone)]
struct CapabilityRegistration {
    descriptor: CapabilityDescriptor,
    handler: CapabilityHandler,
}

impl CapabilityGateway {
    pub(crate) fn new(options: Options, worker_status: Arc<Mutex<LocalWorkerStatus>>) -> Self {
        Self {
            downstream_mcp: DownstreamMcpManager::new(&options.state_path),
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
                "mcp.server.list",
                "MCP 服务列表",
                "读取本机 MCP Registry 中配置的服务摘要，不返回环境变量和请求头明文。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::McpServerList,
            ),
            registration(
                "mcp.server.inspect",
                "查看 MCP 服务",
                "读取指定 MCP 服务的非敏感配置和来源信息。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "server_id": { "type": "string" } },
                    "required": ["server_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpServerInspect,
            ),
            registration(
                "mcp.server.upsert",
                "保存 MCP 服务",
                "新增或更新本机 MCP 服务；敏感值只写入 Agent 本地加密存储，并在返回结果中脱敏。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "server_id": { "type": "string", "maxLength": 32 },
                        "display_name": { "type": "string" },
                        "transport": { "type": "string", "enum": ["stdio", "streamable-http"] },
                        "command": { "type": "string" },
                        "args": { "type": "array", "items": { "type": "string" } },
                        "env": { "type": "object", "additionalProperties": { "type": "string" } },
                        "cwd": { "type": "string" },
                        "url": { "type": "string" },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                        "tool_call_timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000 },
                        "fail_on_startup_error": { "type": "boolean" },
                        "reconnect": { "type": "boolean" },
                        "enabled": { "type": "boolean" }
                    },
                    // `transport` is required for a new row, but optional when
                    // patching an existing row; the handler keeps the stored
                    // transport in that case.
                    "required": ["server_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpServerUpsert,
            ),
            registration(
                "mcp.server.remove",
                "删除 MCP 服务",
                "从本机 MCP Registry 删除指定个人服务，并让下一次 HiMind AI 会话重新加载配置。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "server_id": { "type": "string" } },
                    "required": ["server_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpServerRemove,
            ),
            registration(
                "mcp.target.list",
                "AI 客户端目标列表",
                "读取 Agent MCP 可注册的本机 AI 客户端及其已发现状态。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::McpTargetList,
            ),
            registration(
                "mcp.registration.plan",
                "规划 MCP 注册",
                "计算 Agent MCP 注册到本机 AI 客户端所需的变更和风险提示。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "target_id": { "type": "string" } },
                    "required": ["target_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpRegistrationPlan,
            ),
            registration(
                "mcp.registration.apply",
                "应用 MCP 注册",
                "将 Agent MCP 注册到指定本机 AI 客户端，保留原配置并在写入前备份。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string" },
                        "reset_invalid": { "type": "boolean" }
                    },
                    "required": ["target_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpRegistrationApply,
            ),
            registration(
                "mcp.registration.apply_all",
                "批量应用 MCP 注册",
                "将 Agent MCP 注册到已检测到的本机 AI 客户端；各目标独立执行并返回成功与失败明细。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "detected_only": { "type": "boolean" },
                        "reset_invalid": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::McpRegistrationApplyAll,
            ),
            registration(
                "mcp.registration.remove",
                "移除 MCP 注册",
                "从指定本机 AI 客户端移除 Agent MCP 配置并保留备份。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "target_id": { "type": "string" } },
                    "required": ["target_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpRegistrationRemove,
            ),
            registration(
                "mcp.registration.remove_all",
                "移除全部 MCP 注册",
                "移除已检测 AI 工具中的 HiMind MCP 配置，保留原配置备份。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "detected_only": { "type": "boolean" } },
                    "additionalProperties": false
                }),
                CapabilityHandler::McpRegistrationRemoveAll,
            ),
            registration(
                "mcp.connection.test",
                "测试 MCP 连接",
                "真实执行 MCP initialize 和 tools/list，返回协议、版本、工具数和错误分类。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "server_id": { "type": "string" } },
                    "required": ["server_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::McpConnectionTest,
            ),
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
                "extension.skill.client.register",
                "注册 Skill 客户端",
                "将指定 Skill 同步到一个 AI 工具；不影响其他客户端和 Agent Skill Store。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string" },
                        "client_id": { "type": "string" }
                    },
                    "required": ["skill_id", "client_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillClientRegister,
            ),
            registration(
                "extension.skill.client.unregister",
                "取消 Skill 客户端同步",
                "仅移除指定 AI 工具中的 HiMind 托管 Skill 副本，保留 Skill 在 Agent Store 中继续供 HiMind AI 使用。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string" },
                        "client_id": { "type": "string" }
                    },
                    "required": ["skill_id", "client_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillClientUnregister,
            ),
            registration(
                "extension.skill.clients.unregister",
                "取消 Skill 全部客户端同步",
                "独立移除指定 Skill 在全部外部 AI 工具中的 HiMind 托管副本，保留 Skill 在 Agent Store 中继续供 HiMind AI 使用。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string" }
                    },
                    "required": ["skill_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillClientsUnregister,
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
            registration(
                "extension.review.queue",
                "扩展审核队列",
                "读取待审核的 Skill 与插件提交，要求当前 Dashboard 用户具备管理员审核权限。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["all", "skill", "plugin"] },
                        "query": { "type": "string" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1, "maximum": 200 }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionReviewQueue,
            ),
            registration(
                "extension.review.get",
                "查看扩展审核详情",
                "读取指定 Skill 或插件提交的制品、测试报告和自动审核结果。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin"] },
                        "id": { "type": "string" }
                    },
                    "required": ["kind", "id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionReviewGet,
            ),
            registration(
                "extension.review.decide",
                "审核并上架扩展",
                "提交审核决定；approve_publish 会由 Dashboard 对不可变制品签名并发布，changes_requested/rejected 必须填写意见。",
                "admin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin"] },
                        "id": { "type": "string" },
                        "artifact_id": { "type": "string" },
                        "action": { "type": "string", "enum": ["approve_publish", "changes_requested", "rejected"] },
                        "note": { "type": "string", "maxLength": 4000 }
                    },
                    "required": ["kind", "id", "artifact_id", "action"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionReviewDecide,
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
                "business.project.get", "读取项目", "读取项目、展项、需求和健康度聚合事实。", "read_only",
                json!({"type":"object","properties":{"project_id":{"type":"string"}},"required":["project_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectContext,
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
                "business.exhibit.get", "读取展项", "读取展项成员、设备、需求和推进事件。", "read_only",
                json!({"type":"object","properties":{"exhibit_id":{"type":"string"}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitContext,
            ),
            dashboard_business_registration(
                "work.my_summary",
                "我的工作摘要",
                "读取当前用户负责或关注的项目、展项和需求摘要。",
                "read_only",
                json!({"type":"object","additionalProperties":false}),
                CapabilityHandler::DashboardMyWorkSummary,
            ),
            dashboard_business_registration(
                "business.project.list", "项目列表", "读取当前用户可见的项目列表。", "read_only",
                json!({"type":"object","properties":{"q":{"type":"string"},"status":{"type":"string"},"scope":{"type":"string"},"page":{"type":"integer"},"page_size":{"type":"integer"}},"additionalProperties":false}), CapabilityHandler::DashboardProjectList,
            ),
            dashboard_business_registration(
                "business.project.create", "创建项目", "创建项目并按 Dashboard 权限初始化项目责任人和仓库任务。", "network_write",
                json!({"type":"object","properties":{"project_name":{"type":"string"},"scope_type":{"type":"string","enum":["organization","personal"]},"business_unit_id":{"type":"string"},"management_center_ids":{"type":"array","items":{"type":"string"}},"project_manager_user_ids":{"type":"array","items":{"type":"string"}},"project_owner_user_ids":{"type":"array","items":{"type":"string"}},"status":{"type":"string"},"note":{"type":"string"},"exhibit_visibility":{"type":"string"},"repository_access":{"type":"string","enum":["members","all_read","all_read_write"]},"initial_engineering_name":{"type":"string"},"initial_engine_type":{"type":"string"},"agent_id":{"type":"string"}},"required":["project_name"],"additionalProperties":false}), CapabilityHandler::DashboardProjectCreate,
            ),
            dashboard_business_registration(
                "business.project.update", "更新项目", "更新项目资料、责任人和协作中心。", "network_write",
                json!({"type":"object","properties":{"project_id":{"type":"string"},"project_name":{"type":"string"},"business_unit_id":{"type":"string"},"management_center_ids":{"type":"array","items":{"type":"string"}},"project_manager_user_ids":{"type":"array","items":{"type":"string"}},"project_owner_user_ids":{"type":"array","items":{"type":"string"}},"status":{"type":"string"},"note":{"type":"string"},"exhibit_visibility":{"type":"string"},"repository_access":{"type":"string","enum":["members","all_read","all_read_write"]}},"required":["project_id","project_name"],"additionalProperties":false}), CapabilityHandler::DashboardProjectUpdate,
            ),
            dashboard_business_registration(
                "business.project.delete", "删除项目", "删除项目及其展项、工作区和项目关系。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"}},"required":["project_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectDelete,
            ),
            dashboard_business_registration(
                "business.exhibit.list", "展项列表", "读取当前用户可见的展项列表。", "read_only", json!({"type":"object","properties":{"q":{"type":"string"},"project":{"type":"string"},"engine":{"type":"string"},"page":{"type":"integer"},"page_size":{"type":"integer"}},"additionalProperties":false}), CapabilityHandler::DashboardExhibitList,
            ),
            dashboard_business_registration(
                "business.exhibit.create", "创建展项", "在项目下创建展项。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_name":{"type":"string"},"parent_exhibit_pid":{"type":["string","null"]},"resolution":{"type":"string"},"hall_id":{"type":"string"},"hall":{"type":"string"},"workload":{"type":"number"},"engineering_id":{"type":"string"},"developer_source":{"type":"string"},"edit_url":{"type":"string"},"status":{"type":"string"},"repository_url":{"type":"string"},"source_path":{"type":"string"},"release_path":{"type":"string"},"config_params":{"type":"array","items":{"type":"string"}},"code_uploads":{"type":"array","items":{"type":"string"}},"engine_type":{"type":"string"},"developer_user_ids":{"type":"array","items":{"type":"string"}},"onsite_debugger_user_ids":{"type":"array","items":{"type":"string"}},"note":{"type":"string"}},"required":["project_id","exhibit_name"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCreate,
            ),
            dashboard_business_registration(
                "business.exhibit.update", "更新展项", "更新展项资料和项目归属。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"project_id":{"type":"string"},"exhibit_name":{"type":"string"},"parent_exhibit_pid":{"type":["string","null"]},"hall_id":{"type":"string"},"hall":{"type":"string"},"engine_type":{"type":"string"},"status":{"type":"string"},"repository_url":{"type":"string"},"developer_user_ids":{"type":"array","items":{"type":"string"}},"onsite_debugger_user_ids":{"type":"array","items":{"type":"string"}},"note":{"type":"string"}},"required":["exhibit_id","exhibit_name"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitUpdate,
            ),
            dashboard_business_registration(
                "business.exhibit.delete", "删除展项", "删除展项及其工作区、设备和关联关系。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitDelete,
            ),
            dashboard_business_registration(
                "business.project.managers.replace", "配置项目经理", "全量替换项目经理。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"user_ids":{"type":"array","items":{"type":"string"}}},"required":["project_id","user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardProjectManagersReplace,
            ),
            dashboard_business_registration(
                "business.project.owners.replace", "配置项目负责人", "全量替换项目负责人。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"user_ids":{"type":"array","items":{"type":"string"}}},"required":["project_id","user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardProjectOwnersReplace,
            ),
            dashboard_business_registration(
                "business.exhibit.crew.replace", "配置展项人员", "全量或部分替换展项制作人员和现场调试人员。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"developer_user_ids":{"type":"array","items":{"type":"string"}},"onsite_debugger_user_ids":{"type":"array","items":{"type":"string"}}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCrewReplace,
            ),
            dashboard_business_registration(
                "business.project.exhibit.attach", "关联展项", "将展项关联到指定项目。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_id":{"type":"string"}},"required":["project_id","exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectExhibitAttach,
            ),
            dashboard_business_registration(
                "business.project.exhibit.detach", "解除展项关联", "解除展项与项目的关联。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_id":{"type":"string"}},"required":["project_id","exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectExhibitDetach,
            ),
            dashboard_business_registration(
                "business.exhibit.workspace.get", "查看展项工作区", "读取展项在指定 Agent 上的本地工作区绑定。", "read_only", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"agent_id":{"type":"string"}},"required":["exhibit_id","agent_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitWorkspaceGet,
            ),
            dashboard_business_registration(
                "business.exhibit.workspace.bind", "绑定展项工作区", "保存展项与 Agent 本地目录的绑定。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"agent_id":{"type":"string"},"local_path":{"type":"string"},"engine_version":{"type":"string"}},"required":["exhibit_id","agent_id","local_path"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitWorkspaceBind,
            ),
            dashboard_business_registration(
                "business.exhibit.workspace.checkout", "检出展项工作区", "检出展项 SVN 工作区并自动保存到 Dashboard 的工作区绑定。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_id":{"type":"string"},"repository_url":{"type":"string"},"target_path":{"type":"string"},"agent_id":{"type":"string"},"engine_version":{"type":"string"}},"required":["project_id","exhibit_id","target_path","agent_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitWorkspaceCheckout,
            ),
            dashboard_business_registration(
                "business.people.search", "查询人员", "按姓名、用户 ID 或部门查询可用于项目和展项配置的人员。", "read_only",
                json!({"type":"object","properties":{"q":{"type":"string","maxLength":100},"project_id":{"type":"string"},"exhibit_id":{"type":"string"},"page":{"type":"integer","minimum":1},"page_size":{"type":"integer","minimum":1,"maximum":100}},"required":["q"],"additionalProperties":false}), CapabilityHandler::DashboardPeopleSearch,
            ),
            dashboard_business_registration(
                "business.requirement.list", "需求列表", "读取展项需求及分配状态。", "read_only",
                json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"status":{"type":"string","enum":["active","done","all"]},"mine":{"type":"boolean"},"page":{"type":"integer","minimum":1},"page_size":{"type":"integer","minimum":1,"maximum":100}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementList,
            ),
            dashboard_business_registration(
                "business.requirement.get", "读取需求", "读取单个需求的完整内容、分配、评论和事件。", "read_only",
                json!({"type":"object","properties":{"requirement_id":{"type":"string"}},"required":["requirement_id"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementGet,
            ),
            dashboard_business_registration(
                "business.requirement.create", "创建需求", "在展项下创建需求并指派展项成员。", "network_write",
                json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"title":{"type":"string","maxLength":200},"description":{"type":"string","maxLength":20000},"acceptance_criteria":{"type":"string","maxLength":10000},"category":{"type":"string","enum":["feature","bug","change","optimization","content","technical","support"]},"priority":{"type":"string","enum":["low","normal","high","urgent"]},"due_at":{"type":"string"},"assignee_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":50},"draft_token":{"type":"string"},"attachment_ids":{"type":"array","items":{"type":"string"}}},"required":["exhibit_id","title","assignee_ids"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementCreate,
            ),
            dashboard_business_registration(
                "business.requirement.update", "更新需求", "更新需求内容和指派成员，使用 version 防止覆盖他人修改。", "network_write",
                json!({"type":"object","properties":{"requirement_id":{"type":"string"},"title":{"type":"string","maxLength":200},"description":{"type":"string","maxLength":20000},"acceptance_criteria":{"type":"string","maxLength":10000},"category":{"type":"string","enum":["feature","bug","change","optimization","content","technical","support"]},"priority":{"type":"string","enum":["low","normal","high","urgent"]},"due_at":{"type":"string"},"version":{"type":"integer","minimum":1},"assignee_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":50},"draft_token":{"type":"string"},"attachment_ids":{"type":"array","items":{"type":"string"}}},"required":["requirement_id","title","version","assignee_ids"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementUpdate,
            ),
            dashboard_business_registration(
                "business.requirement.assignment.update", "更新我的需求", "更新当前用户的需求进度、阻塞原因或交付说明。", "network_write",
                json!({"type":"object","properties":{"requirement_id":{"type":"string"},"status":{"type":"string","enum":["in_progress","blocked","submitted"]},"completion_note":{"type":"string","maxLength":10000},"blocked_reason":{"type":"string","maxLength":4000},"archive_when_done":{"type":"boolean"},"draft_token":{"type":"string"},"attachment_ids":{"type":"array","items":{"type":"string"}}},"required":["requirement_id","status"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementAssignmentUpdate,
            ),
            dashboard_business_registration(
                "business.requirement.cancel", "取消需求", "取消尚未完成的展项需求。", "network_write", json!({"type":"object","properties":{"requirement_id":{"type":"string"}},"required":["requirement_id"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementCancel,
            ),
            dashboard_business_registration(
                "business.requirement.reopen", "重新打开需求", "重新打开已完成或已取消的成员需求分配。", "network_write", json!({"type":"object","properties":{"requirement_id":{"type":"string"},"user_id":{"type":"string"}},"required":["requirement_id","user_id"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementReopen,
            ),
            dashboard_business_registration(
                "business.requirement.review", "验收需求", "验收或退回成员提交的需求交付。", "network_write", json!({"type":"object","properties":{"requirement_id":{"type":"string"},"user_id":{"type":"string"},"action":{"type":"string","enum":["accept","return"]},"note":{"type":"string","maxLength":4000}},"required":["requirement_id","user_id","action","note"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementReview,
            ),
            dashboard_business_registration(
                "business.requirement.comment", "评论需求", "向需求添加评论并通知参与者。", "network_write", json!({"type":"object","properties":{"requirement_id":{"type":"string"},"content_markdown":{"type":"string","maxLength":10000},"draft_token":{"type":"string"},"attachment_ids":{"type":"array","items":{"type":"string"}}},"required":["requirement_id","content_markdown"],"additionalProperties":false}), CapabilityHandler::DashboardRequirementComment,
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
        // User-managed MCP servers are discovered lazily and projected into
        // the same gateway as built-in and plugin capabilities. Discovery
        // failures are isolated to that downstream server.
        if let Ok(downstream) = self.downstream_mcp.list_capabilities() {
            for (descriptor, _) in downstream {
                let capability_id = descriptor.id.clone();
                if registry.contains_key(&capability_id) {
                    continue;
                }
                insert_registration(
                    &mut registry,
                    CapabilityRegistration {
                        descriptor,
                        handler: CapabilityHandler::DownstreamMcp(capability_id),
                    },
                )?;
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
        validate_capability_input_schema(&registration.descriptor.input_schema, &input)?;
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
            CapabilityHandler::SkillClientRegister => {
                let skill_id = input
                    .get("skill_id")
                    .and_then(Value::as_str)
                    .ok_or("skill_id is required")?;
                let client_id = input
                    .get("client_id")
                    .and_then(Value::as_str)
                    .ok_or("client_id is required")?;
                let capability_facts = crate::skill::capability_facts_from_gateway(
                    &self.options,
                    Arc::clone(&self.worker_status),
                    context,
                )?;
                crate::skill::sync_skill_client_json(
                    skill_id,
                    client_id,
                    VERSION,
                    &capability_facts,
                )
            }
            CapabilityHandler::SkillClientUnregister => {
                let skill_id = input
                    .get("skill_id")
                    .and_then(Value::as_str)
                    .ok_or("skill_id is required")?;
                let client_id = input
                    .get("client_id")
                    .and_then(Value::as_str)
                    .ok_or("client_id is required")?;
                crate::skill::unregister_skill_client_json(skill_id, client_id)
            }
            CapabilityHandler::SkillClientsUnregister => {
                let skill_id = input
                    .get("skill_id")
                    .and_then(Value::as_str)
                    .ok_or("skill_id is required")?;
                crate::skill::unregister_skill_clients_json(skill_id)
            }
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
            CapabilityHandler::ExtensionReviewQueue => self.extension_review_queue(input),
            CapabilityHandler::ExtensionReviewGet => self.extension_review_get(input),
            CapabilityHandler::ExtensionReviewDecide => self.extension_review_decide(input),
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
            CapabilityHandler::DashboardProjectList => {
                crate::api::dashboard_business::project_list(&self.options, input)
            }
            CapabilityHandler::DashboardProjectCreate => {
                crate::api::dashboard_business::project_create(&self.options, input)
            }
            CapabilityHandler::DashboardProjectUpdate => {
                crate::api::dashboard_business::project_update(&self.options, input)
            }
            CapabilityHandler::DashboardProjectDelete => {
                crate::api::dashboard_business::project_delete(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitList => {
                crate::api::dashboard_business::exhibit_list(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitCreate => {
                crate::api::dashboard_business::exhibit_create(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitUpdate => {
                crate::api::dashboard_business::exhibit_update(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitDelete => {
                crate::api::dashboard_business::exhibit_delete(&self.options, input)
            }
            CapabilityHandler::DashboardProjectManagersReplace => {
                crate::api::dashboard_business::project_people_replace(
                    &self.options,
                    input,
                    "managers",
                )
            }
            CapabilityHandler::DashboardProjectOwnersReplace => {
                crate::api::dashboard_business::project_people_replace(
                    &self.options,
                    input,
                    "owners",
                )
            }
            CapabilityHandler::DashboardExhibitCrewReplace => {
                crate::api::dashboard_business::exhibit_crew_replace(&self.options, input)
            }
            CapabilityHandler::DashboardProjectExhibitAttach => {
                crate::api::dashboard_business::project_exhibit_association(
                    &self.options,
                    input,
                    "attach",
                )
            }
            CapabilityHandler::DashboardProjectExhibitDetach => {
                crate::api::dashboard_business::project_exhibit_association(
                    &self.options,
                    input,
                    "detach",
                )
            }
            CapabilityHandler::DashboardExhibitWorkspaceGet => {
                crate::api::dashboard_business::exhibit_workspace_get(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitWorkspaceBind => {
                crate::api::dashboard_business::exhibit_workspace_bind(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitWorkspaceCheckout => {
                let checkout_request: SvnCheckoutRequest = serde_json::from_value(input.clone())?;
                let checkout = checkout_workspace(checkout_request)?;
                let exhibit_id = input
                    .get("exhibit_id")
                    .and_then(Value::as_str)
                    .ok_or("exhibit_id is required")?;
                let agent_id = input
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .ok_or("agent_id is required")?;
                let local_path = checkout
                    .get("target_path")
                    .cloned()
                    .or_else(|| input.get("target_path").cloned())
                    .ok_or("checkout did not return target_path")?;
                let mut bind_input = json!({
                    "exhibit_id": exhibit_id,
                    "agent_id": agent_id,
                    "local_path": local_path,
                });
                if let Some(engine_version) = input.get("engine_version") {
                    bind_input["engine_version"] = engine_version.clone();
                }
                let binding = crate::api::dashboard_business::exhibit_workspace_bind(
                    &self.options,
                    bind_input,
                )?;
                Ok(json!({"checkout": checkout, "binding": binding}))
            }
            CapabilityHandler::DashboardPeopleSearch => {
                crate::api::dashboard_business::people_search(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementList => {
                crate::api::dashboard_business::requirement_list(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementGet => {
                crate::api::dashboard_business::requirement_get(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementCreate => {
                crate::api::dashboard_business::requirement_create(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementUpdate => {
                crate::api::dashboard_business::requirement_update(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementAssignmentUpdate => {
                crate::api::dashboard_business::requirement_assignment_update(&self.options, input)
            }
            CapabilityHandler::DashboardRequirementCancel => {
                crate::api::dashboard_business::requirement_action(&self.options, input, "cancel")
            }
            CapabilityHandler::DashboardRequirementReopen => {
                crate::api::dashboard_business::requirement_action(&self.options, input, "reopen")
            }
            CapabilityHandler::DashboardRequirementReview => {
                crate::api::dashboard_business::requirement_action(&self.options, input, "review")
            }
            CapabilityHandler::DashboardRequirementComment => {
                crate::api::dashboard_business::requirement_action(&self.options, input, "comments")
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
            CapabilityHandler::DownstreamMcp(id) => self.downstream_mcp.invoke(&id, input),
            CapabilityHandler::McpServerList => Ok(json!({
                "registry": mcp_registry::public_snapshot(&self.options.state_path)?,
                "targets": mcp_targets::list(&self.options)?,
            })),
            CapabilityHandler::McpServerInspect => {
                let server_id = input
                    .get("server_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Ok(mcp_registry::inspect(&self.options.state_path, server_id)?)
            }
            CapabilityHandler::McpServerUpsert => {
                // Treat an existing row as the baseline for partial updates.  MCP
                // callers only see redacted secret metadata, so replacing an
                // otherwise unchanged row from that snapshot must not erase
                // credentials, command arguments, or transport details.
                let existing = mcp_registry::get(
                    &self.options.state_path,
                    input
                        .get("server_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )?
                .map(|server| server.into_config());
                let config = mcp_server_config_from_input(&input, existing.clone())?;
                let changed = existing
                    .as_ref()
                    .map(|value| value != &config)
                    .unwrap_or(true);
                let server = if changed {
                    mcp_registry::upsert_config(&self.options.state_path, config)?
                } else {
                    config
                };
                // A running DSH session snapshots the MCP overlay at startup.
                // Stop only the locally owned session; the next one will read
                // the updated Registry without requiring a Dashboard.
                if changed {
                    crate::app::ui::stop_builtin_ai_process();
                }
                Ok(json!({
                    "server": mcp_registry::inspect(&self.options.state_path, &server.server_name)?,
                    "changed": changed,
                    "restart_required": changed
                }))
            }
            CapabilityHandler::McpServerRemove => {
                let server_id = input
                    .get("server_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let removed = mcp_registry::remove_config(&self.options.state_path, server_id)?;
                if removed {
                    crate::app::ui::stop_builtin_ai_process();
                }
                Ok(json!({
                    "server_id": server_id,
                    "removed": removed,
                    "restart_required": removed
                }))
            }
            CapabilityHandler::McpTargetList => {
                Ok(serde_json::to_value(mcp_targets::list(&self.options)?)?)
            }
            CapabilityHandler::McpRegistrationPlan => {
                let target_id = input
                    .get("target_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Ok(serde_json::to_value(mcp_targets::plan(
                    &self.options,
                    target_id,
                )?)?)
            }
            CapabilityHandler::McpRegistrationApply => {
                let target_id = input
                    .get("target_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let reset_invalid = input
                    .get("reset_invalid")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(serde_json::to_value(mcp_targets::apply(
                    &self.options,
                    target_id,
                    reset_invalid,
                )?)?)
            }
            CapabilityHandler::McpRegistrationApplyAll => {
                let detected_only = input
                    .get("detected_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let reset_invalid = input
                    .get("reset_invalid")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(serde_json::to_value(mcp_targets::apply_all(
                    &self.options,
                    detected_only,
                    reset_invalid,
                )?)?)
            }
            CapabilityHandler::McpRegistrationRemove => {
                let target_id = input
                    .get("target_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Ok(serde_json::to_value(mcp_targets::remove(
                    &self.options,
                    target_id,
                )?)?)
            }
            CapabilityHandler::McpRegistrationRemoveAll => {
                let detected_only = input
                    .get("detected_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Ok(serde_json::to_value(mcp_targets::remove_all(
                    &self.options,
                    detected_only,
                )?)?)
            }
            CapabilityHandler::McpConnectionTest => {
                let server_id = input
                    .get("server_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let server = mcp_registry::get(&self.options.state_path, server_id)?
                    .ok_or_else(|| format!("MCP server not found: {server_id}"))?;
                Ok(serde_json::to_value(crate::app::mcp_probe::probe_report(
                    &server,
                ))?)
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

    fn extension_review_queue(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let agent_id = self.load_paired_agent()?;
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::RELEASE_MANAGE_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        crate::api::distribution::extension_review_queue(
            &client,
            &self.options.api_base,
            &agent_id,
            &access.token,
            &input,
        )
    }

    fn extension_review_get(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (kind, id) = extension_review_identity(&input)?;
        let agent_id = self.load_paired_agent()?;
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::RELEASE_MANAGE_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        crate::api::distribution::extension_review_get(
            &client,
            &self.options.api_base,
            &agent_id,
            &access.token,
            &kind,
            &id,
        )
    }

    fn extension_review_decide(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (kind, id) = extension_review_identity(&input)?;
        let artifact_id = input
            .get("artifact_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let note = input
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if artifact_id.is_empty() {
            return Err("artifact_id is required".into());
        }
        if !matches!(action, "approve_publish" | "changes_requested" | "rejected") {
            return Err("action must be approve_publish, changes_requested, or rejected".into());
        }
        if matches!(action, "changes_requested" | "rejected") && note.is_empty() {
            return Err("note is required for changes_requested or rejected".into());
        }
        if note.chars().count() > 4000 {
            return Err("note is too long".into());
        }
        let agent_id = self.load_paired_agent()?;
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::RELEASE_MANAGE_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        crate::api::distribution::extension_review_decide(
            &client,
            &self.options.api_base,
            &agent_id,
            &access.token,
            &kind,
            &id,
            artifact_id,
            action,
            note,
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

fn extension_review_identity(input: &Value) -> Result<(String, String), Box<dyn Error>> {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(kind.as_str(), "skill" | "plugin") {
        return Err("kind must be skill or plugin".into());
    }
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() || id.len() > 200 || id.contains('/') || id.contains('\\') {
        return Err("id is required and must be a review identifier".into());
    }
    Ok((kind, id))
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
        "extension.review.queue" | "extension.review.get" | "extension.review.decide" => {
            Some(crate::api::oauth::RELEASE_MANAGE_SCOPE)
        }
        "software.distribution.release.publish" => Some(crate::api::oauth::RELEASE_MANAGE_SCOPE),
        "context.resolve" | "work.my_summary" => {
            Some(crate::api::oauth::BUSINESS_CONTEXT_READ_SCOPE)
        }
        "project.context.get" | "business.project.get" => {
            Some(crate::api::oauth::BUSINESS_PROJECT_READ_SCOPE)
        }
        "exhibit.context.get" | "business.exhibit.get" => {
            Some(crate::api::oauth::BUSINESS_EXHIBIT_READ_SCOPE)
        }
        "business.project.list" => Some(crate::api::oauth::BUSINESS_PROJECT_READ_SCOPE),
        "business.project.create" | "business.project.update" | "business.project.delete" => {
            Some(crate::api::oauth::BUSINESS_PROJECT_WRITE_SCOPE)
        }
        "business.exhibit.list" => Some(crate::api::oauth::BUSINESS_EXHIBIT_READ_SCOPE),
        "business.exhibit.create" | "business.exhibit.update" | "business.exhibit.delete" => {
            Some(crate::api::oauth::BUSINESS_EXHIBIT_WRITE_SCOPE)
        }
        "business.project.managers.replace"
        | "business.project.owners.replace"
        | "business.exhibit.crew.replace" => Some(crate::api::oauth::BUSINESS_PEOPLE_WRITE_SCOPE),
        "business.people.search" => Some(crate::api::oauth::BUSINESS_PEOPLE_READ_SCOPE),
        "business.requirement.list" | "business.requirement.get" => {
            Some(crate::api::oauth::BUSINESS_REQUIREMENT_READ_SCOPE)
        }
        "business.requirement.create"
        | "business.requirement.update"
        | "business.requirement.assignment.update"
        | "business.requirement.cancel"
        | "business.requirement.reopen"
        | "business.requirement.review"
        | "business.requirement.comment" => {
            Some(crate::api::oauth::BUSINESS_REQUIREMENT_WRITE_SCOPE)
        }
        "business.project.exhibit.attach" | "business.project.exhibit.detach" => {
            Some(crate::api::oauth::BUSINESS_PROJECT_WRITE_SCOPE)
        }
        "business.exhibit.workspace.get" => Some(crate::api::oauth::BUSINESS_WORKSPACE_READ_SCOPE),
        "business.exhibit.workspace.bind" | "business.exhibit.workspace.checkout" => {
            Some(crate::api::oauth::BUSINESS_WORKSPACE_WRITE_SCOPE)
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

// Validate every Gateway invocation, including built-ins and downstream MCP
// projections. Plugin invocations perform the same check inside the plugin
// runtime, but validating here gives all MCP callers one deterministic error
// contract before any network, process or filesystem side effect occurs.
fn validate_capability_input_schema(schema: &Value, input: &Value) -> Result<(), Box<dyn Error>> {
    let Some(schema_object) = schema.as_object() else {
        return Ok(());
    };
    if !schema_object
        .get("type")
        .map(|value| capability_schema_allows_type(value, "object"))
        .unwrap_or(true)
    {
        return Ok(());
    }
    if !input.is_object() {
        return Err("capability input must be an object".into());
    }
    validate_capability_value("capability input", schema, input)
}

fn validate_capability_value(
    name: &str,
    schema: &Value,
    value: &Value,
) -> Result<(), Box<dyn Error>> {
    let expected_types: Vec<&str> = match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !expected_types.is_empty()
        && !expected_types
            .iter()
            .any(|expected| capability_value_matches_type(expected, value))
    {
        return Err(format!("capability input property has invalid type: {name}").into());
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.iter().any(|candidate| candidate == value) {
            return Err(format!("capability input property has invalid value: {name}").into());
        }
    }
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
        if value
            .as_str()
            .map(|item| item.chars().count() as u64 > max_length)
            .unwrap_or(false)
        {
            return Err(format!("capability input property is too long: {name}").into());
        }
    }
    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
        if value
            .as_str()
            .map(|item| (item.chars().count() as u64) < min_length)
            .unwrap_or(false)
        {
            return Err(format!("capability input property is too short: {name}").into());
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if let Some(text) = value.as_str() {
            if !matches_capability_pattern(pattern, text) {
                return Err(format!("capability input property has invalid format: {name}").into());
            }
        }
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value
            .as_f64()
            .map(|number| number < minimum)
            .unwrap_or(false)
        {
            return Err(format!("capability input property is below minimum: {name}").into());
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if value
            .as_f64()
            .map(|number| number > maximum)
            .unwrap_or(false)
        {
            return Err(format!("capability input property exceeds maximum: {name}").into());
        }
    }
    if let Some(items) = schema.get("items") {
        if let Some(values) = value.as_array() {
            for (index, item) in values.iter().enumerate() {
                validate_capability_value(&format!("{name}[{index}]"), items, item)?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for property in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(property) {
                    return Err(format!("{name} is missing required property: {property}").into());
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
            if let Some(unknown) = object.keys().find(|key| {
                properties
                    .map(|items| !items.contains_key(*key))
                    .unwrap_or(true)
            }) {
                return Err(format!("{name} contains unknown property: {unknown}").into());
            }
        }
        if let Some(properties) = properties {
            for (property, property_schema) in properties {
                if let Some(value) = object.get(property) {
                    validate_capability_value(
                        &format!("{name}.{property}"),
                        property_schema,
                        value,
                    )?;
                }
            }
        }
        if let Some(additional_schema) = schema
            .get("additionalProperties")
            .filter(|value| value.is_object())
        {
            for (property, value) in object {
                if properties
                    .map(|items| items.contains_key(property))
                    .unwrap_or(false)
                {
                    continue;
                }
                validate_capability_value(&format!("{name}.{property}"), additional_schema, value)?;
            }
        }
    }
    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if value
            .as_array()
            .map(|items| (items.len() as u64) < min_items)
            .unwrap_or(false)
        {
            return Err(format!("capability input array has too few items: {name}").into());
        }
    }
    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
        if value
            .as_array()
            .map(|items| items.len() as u64 > max_items)
            .unwrap_or(false)
        {
            return Err(format!("capability input array has too many items: {name}").into());
        }
    }
    Ok(())
}

fn capability_value_matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn capability_schema_allows_type(schema_type: &Value, expected: &str) -> bool {
    match schema_type {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => true,
    }
}

fn matches_capability_pattern(pattern: &str, value: &str) -> bool {
    // The Gateway deliberately supports a small, deterministic subset instead
    // of embedding a regex engine. This is the pattern used by release SHA-256
    // inputs; unknown patterns remain advisory rather than blocking clients.
    if pattern == "^[0-9a-fA-F]{64}$" {
        return value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    true
}

fn availability_for_handler(handler: &CapabilityHandler) -> CapabilityAvailability {
    match handler {
        CapabilityHandler::AuthoringIdentity
        | CapabilityHandler::ExtensionWorkspaceCurrent
        | CapabilityHandler::SkillCandidateSave
        | CapabilityHandler::SkillCandidateTest
        | CapabilityHandler::SkillClientRegister
        | CapabilityHandler::SkillClientUnregister
        | CapabilityHandler::SkillClientsUnregister
        | CapabilityHandler::PluginCandidateSave
        | CapabilityHandler::PluginCandidateTest => CapabilityAvailability::Local,
        CapabilityHandler::SkillSubmissionSubmit
        | CapabilityHandler::SkillSubmissionStatus
        | CapabilityHandler::PluginSubmissionSubmit
        | CapabilityHandler::PluginSubmissionStatus
        | CapabilityHandler::ExtensionReviewQueue
        | CapabilityHandler::ExtensionReviewGet
        | CapabilityHandler::ExtensionReviewDecide
        | CapabilityHandler::SoftwareDistributionPublish
        | CapabilityHandler::DashboardContextResolve
        | CapabilityHandler::DashboardProjectContext
        | CapabilityHandler::DashboardExhibitContext
        | CapabilityHandler::DashboardMyWorkSummary
        | CapabilityHandler::DashboardKnowledgeSearch
        | CapabilityHandler::DashboardProjectList
        | CapabilityHandler::DashboardProjectCreate
        | CapabilityHandler::DashboardProjectUpdate
        | CapabilityHandler::DashboardProjectDelete
        | CapabilityHandler::DashboardExhibitList
        | CapabilityHandler::DashboardExhibitCreate
        | CapabilityHandler::DashboardExhibitUpdate
        | CapabilityHandler::DashboardExhibitDelete
        | CapabilityHandler::DashboardProjectManagersReplace
        | CapabilityHandler::DashboardProjectOwnersReplace
        | CapabilityHandler::DashboardExhibitCrewReplace
        | CapabilityHandler::DashboardProjectExhibitAttach
        | CapabilityHandler::DashboardProjectExhibitDetach
        | CapabilityHandler::DashboardExhibitWorkspaceGet
        | CapabilityHandler::DashboardExhibitWorkspaceBind
        | CapabilityHandler::DashboardExhibitWorkspaceCheckout
        | CapabilityHandler::DashboardPeopleSearch
        | CapabilityHandler::DashboardRequirementList
        | CapabilityHandler::DashboardRequirementGet
        | CapabilityHandler::DashboardRequirementCreate
        | CapabilityHandler::DashboardRequirementUpdate
        | CapabilityHandler::DashboardRequirementAssignmentUpdate
        | CapabilityHandler::DashboardRequirementCancel
        | CapabilityHandler::DashboardRequirementReopen
        | CapabilityHandler::DashboardRequirementReview
        | CapabilityHandler::DashboardRequirementComment
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
            Err("软件分发能力必须提供待分发软件所在目录 workspace_root".into())
        } else {
            Ok(())
        };
    };
    if software_scoped {
        validate_software_workspace_root(workspace_root)
    } else {
        let current = crate::extension_projects::current_workspace_path()?;
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

fn validate_software_workspace_root(requested: &str) -> Result<(), Box<dyn Error>> {
    let requested = Path::new(requested).canonicalize()?;
    if !requested.is_dir() {
        return Err(serde_json::json!({
            "code": "software_workspace_invalid",
            "message": "workspace_root 必须是可访问的本机目录；软件分发允许使用当前 AI 工作区之外的目录"
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

fn mcp_server_config_from_input(
    input: &Value,
    existing: Option<crate::app::mcp_registry::McpServerConfig>,
) -> Result<crate::app::mcp_registry::McpServerConfig, Box<dyn Error>> {
    let object = input
        .as_object()
        .ok_or("MCP server input must be a JSON object")?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
    };
    let server_name = text("server_id").unwrap_or_default();
    if server_name.is_empty() {
        return Err("server_id is required".into());
    }
    let transport = text("transport")
        .or_else(|| existing.as_ref().map(|value| value.transport.clone()))
        .unwrap_or_default();
    if transport.is_empty() {
        return Err("transport is required".into());
    }
    let args = object
        .get("args")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let env = match object.get("env") {
        Some(value) => serde_json::from_value(value.clone())?,
        None => existing
            .as_ref()
            .map(|value| value.env.clone())
            .unwrap_or_default(),
    };
    let headers = match object.get("headers") {
        Some(value) => serde_json::from_value(value.clone())?,
        None => existing
            .as_ref()
            .map(|value| value.headers.clone())
            .unwrap_or_default(),
    };
    Ok(crate::app::mcp_registry::McpServerConfig {
        server_name,
        display_name: text("display_name")
            .or_else(|| existing.as_ref().map(|value| value.display_name.clone()))
            .unwrap_or_default(),
        transport,
        command: text("command")
            .or_else(|| existing.as_ref().map(|value| value.command.clone()))
            .unwrap_or_default(),
        args: args
            .or_else(|| existing.as_ref().map(|value| value.args.clone()))
            .unwrap_or_default(),
        env,
        cwd: text("cwd")
            .or_else(|| existing.as_ref().map(|value| value.cwd.clone()))
            .unwrap_or_default(),
        url: text("url")
            .or_else(|| existing.as_ref().map(|value| value.url.clone()))
            .unwrap_or_default(),
        headers,
        tool_call_timeout_ms: object
            .get("tool_call_timeout_ms")
            .and_then(Value::as_u64)
            .or_else(|| existing.as_ref().map(|value| value.tool_call_timeout_ms))
            .unwrap_or(30_000),
        fail_on_startup_error: object
            .get("fail_on_startup_error")
            .and_then(Value::as_bool)
            .or_else(|| existing.as_ref().map(|value| value.fail_on_startup_error))
            .unwrap_or(false),
        reconnect: object
            .get("reconnect")
            .and_then(Value::as_bool)
            .or_else(|| existing.as_ref().map(|value| value.reconnect))
            .unwrap_or(true),
        enabled: object
            .get("enabled")
            .and_then(Value::as_bool)
            .or_else(|| existing.as_ref().map(|value| value.enabled))
            .unwrap_or(true),
    })
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
    fn mcp_server_input_uses_stable_id_and_safe_defaults() {
        let config = mcp_server_config_from_input(
            &json!({
                "server_id": "local-tools",
                "transport": "stdio",
                "command": "node",
                "args": ["server.js"],
                "env": { "TOKEN": "secret" }
            }),
            None,
        )
        .unwrap();
        assert_eq!(config.server_name, "local-tools");
        assert_eq!(config.transport, "stdio");
        assert_eq!(config.tool_call_timeout_ms, 30_000);
        assert!(config.reconnect);
        assert!(config.enabled);
        assert_eq!(config.env.get("TOKEN"), Some(&"secret".to_string()));
    }

    #[test]
    fn mcp_server_input_preserves_omitted_existing_fields_and_secrets() {
        let existing = mcp_server_config_from_input(
            &json!({
                "server_id": "local-tools",
                "transport": "stdio",
                "command": "node",
                "args": ["server.js", "--port", "3210"],
                "env": { "TOKEN": "secret" },
                "cwd": "C:/tools",
                "tool_call_timeout_ms": 12_000,
                "enabled": true
            }),
            None,
        )
        .unwrap();
        let updated = mcp_server_config_from_input(
            &json!({
                "server_id": "local-tools",
                "transport": "stdio",
                "display_name": "Local Tools"
            }),
            Some(existing.clone()),
        )
        .unwrap();
        assert_eq!(updated.display_name, "Local Tools");
        assert_eq!(updated.command, existing.command);
        assert_eq!(updated.args, existing.args);
        assert_eq!(updated.env, existing.env);
        assert_eq!(updated.cwd, existing.cwd);
        assert_eq!(updated.tool_call_timeout_ms, existing.tool_call_timeout_ms);
    }

    #[test]
    fn capability_schema_validates_nested_objects_and_additional_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "object",
                    "properties": { "mode": { "type": "string", "enum": ["fast", "safe"] } },
                    "required": ["mode"],
                    "additionalProperties": false
                }
            },
            "required": ["options"],
            "additionalProperties": false
        });

        validate_capability_input_schema(
            &schema,
            &json!({
                "options": { "mode": "safe" }
            }),
        )
        .unwrap();
        let missing = validate_capability_input_schema(
            &schema,
            &json!({
                "options": {}
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(missing.contains("options is missing required property: mode"));
        let unknown = validate_capability_input_schema(
            &schema,
            &json!({
                "options": { "mode": "safe", "debug": true }
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("options contains unknown property: debug"));
    }

    #[test]
    fn capability_schema_enforces_sha256_pattern() {
        let schema = json!({
            "type": "object",
            "properties": { "sha256": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } },
            "required": ["sha256"],
            "additionalProperties": false
        });
        validate_capability_input_schema(
            &schema,
            &json!({
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }),
        )
        .unwrap();
        assert!(
            validate_capability_input_schema(&schema, &json!({ "sha256": "not-a-sha256" }))
                .is_err()
        );
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
    fn software_distribution_workspace_accepts_an_explicit_external_root() {
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
        validate_software_workspace_root(workspace.to_str().unwrap()).unwrap();
        validate_software_workspace_root(outside.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn software_distribution_mcp_validation_does_not_bind_to_session_root() {
        let root = std::env::temp_dir().join(format!(
            "himind-software-workspace-mcp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mcp = InvocationContext::new(
            crate::capability::types::InvocationSource::Mcp,
            "ai-client:test",
        );
        let input = serde_json::json!({ "workspace_root": root });
        validate_mcp_capability_workspace(
            &mcp,
            "software.distribution.artifact.inspect",
            &input,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(input["workspace_root"].as_str().unwrap());
    }

    #[test]
    fn only_mcp_software_publish_requires_native_confirmation() {
        let mcp = InvocationContext::new(
            crate::capability::types::InvocationSource::Mcp,
            "ai-client:test",
        );
        let cli =
            InvocationContext::new(crate::capability::types::InvocationSource::Cli, "local-cli");
        assert!(requires_native_software_confirmation(&mcp));
        assert!(!requires_native_software_confirmation(&cli));
    }
}
