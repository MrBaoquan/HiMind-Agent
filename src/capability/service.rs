use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
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
use crate::approval::manager::ApprovalManager;
use crate::approval::policy;
use crate::approval::remote::ApprovalProof;
#[cfg(test)]
use crate::business_integration::BusinessCatalogSnapshot;
use crate::business_integration::{
    BusinessCapabilityContract, BusinessIntegrationProvider, DASHBOARD_BUSINESS_PROVIDER_ID,
};
use crate::capability::dashboard_catalog::DashboardCatalogProvider;
use crate::capability::plugin::{
    find_plugin, invoke_plugin_capability, invoke_plugin_capability_for_plugin,
    registry_json_for_control_plane, scan_plugins,
};
use crate::capability::software_distribution::{
    attach_inspection_receipt, consume_inspection_receipt, verify_inspection_receipt,
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

#[derive(Clone)]
pub(crate) struct CapabilityGateway {
    options: Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    approval_manager: Arc<ApprovalManager>,
    downstream_mcp: DownstreamMcpManager,
    business_provider: Arc<dyn BusinessIntegrationProvider>,
}

#[derive(Clone)]
enum CapabilityHandler {
    SystemHealth,
    AIClientList,
    AIClientStatus,
    AIClientImport,
    AIClientRemove,
    AIClientImportPlan,
    AIClientRemovePlan,
    AIServiceList,
    AIServiceCustomUpsert,
    AIServiceCustomRemove,
    AIServiceCustomListModels,
    AuthoringIdentity,
    AuthoringPreflight,
    ExtensionWorkspaceCurrent,
    ExtensionWorkspaceBind,
    ExtensionWorkspaceClear,
    ExtensionRevisionCreate,
    ExtensionLock,
    InnerAdminLoginStatus,
    SystemOpenFolder,
    FilesystemDelete,
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
    ExtensionTest,
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
    DashboardExhibitCrewAppend,
    DashboardExhibitCrewRemove,
    DashboardProjectExhibitAttach,
    DashboardProjectExhibitDetach,
    DashboardExhibitWorkspaceGet,
    DashboardExhibitWorkspaceBind,
    DashboardExhibitWorkspaceCheckout,
    OperationGet,
    OperationCancel,
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
    BusinessIntegrationDynamic(BusinessCapabilityContract),
}

#[derive(Clone)]
struct CapabilityRegistration {
    descriptor: CapabilityDescriptor,
    handler: CapabilityHandler,
}

impl CapabilityGateway {
    pub(crate) fn new(options: Options, worker_status: Arc<Mutex<LocalWorkerStatus>>) -> Self {
        Self::new_with_approval_manager(options, worker_status, ApprovalManager::global())
    }

    pub(crate) fn new_with_approval_manager(
        options: Options,
        worker_status: Arc<Mutex<LocalWorkerStatus>>,
        approval_manager: Arc<ApprovalManager>,
    ) -> Self {
        Self {
            downstream_mcp: DownstreamMcpManager::new(&options.state_path),
            business_provider: Arc::new(DashboardCatalogProvider::new(&options)),
            options,
            worker_status,
            approval_manager,
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_business_catalog_for_test(&self, snapshot: BusinessCatalogSnapshot) {
        let provider = self
            .business_provider
            .as_any()
            .downcast_ref::<DashboardCatalogProvider>()
            .expect("test Gateway must use the Dashboard business integration provider");
        provider.replace_snapshot(snapshot);
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
        let catalog_ids = self.business_provider.catalog_snapshot().map(|snapshot| {
            snapshot
                .items
                .into_iter()
                .map(|item| item.id)
                .collect::<std::collections::BTreeSet<_>>()
        });
        Ok(self
            .registry()?
            .into_values()
            .filter(|registration| {
                let visible_for_mode = self.options.mode().control_plane_enabled()
                    || registration
                        .descriptor
                        .availability
                        .available_without_control_plane();
                if !visible_for_mode {
                    return false;
                }
                // Once a fresh catalog is available, catalog-owned business
                // capabilities absent from it have been removed by Dashboard
                // and must no longer be projected through MCP. Other
                // control-plane providers (media, review, distribution) use
                // their own contracts and are intentionally unaffected.
                if let Some(ids) = catalog_ids.as_ref() {
                    if is_business_integration_handler(&registration.handler)
                        && !ids.contains(&registration.descriptor.id)
                    {
                        return false;
                    }
                }
                true
            })
            .map(|registration| registration.descriptor)
            .collect())
    }

    fn registry(&self) -> Result<BTreeMap<String, CapabilityRegistration>, Box<dyn Error>> {
        let mut registry = BTreeMap::new();
        let ai_client_targets = crate::app::ai_provider_import::known_adapter_ids();
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
                "ai.client.list",
                "AI 客户端列表",
                "检测本机支持的 AI 客户端，不读取配置中的密钥或敏感值。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::AIClientList,
            ),
            registration(
                "ai.client.status",
                "AI 客户端接入状态",
                "读取本机 AI 客户端的 HiMind 接入状态和模型摘要。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::AIClientStatus,
            ),
            registration(
                "ai.client.import",
                "接入 AI 客户端",
                "为指定 AI 客户端配置指定 AI 服务；执行前应确认目标客户端、服务源和本机配置变更。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ai_client_targets.clone() },
                        "service": {
                            "type": "string",
                            "default": "managed",
                            "pattern": "^(managed|custom:[A-Za-z0-9_-]{1,64})$"
                        }
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIClientImport,
            ),
            registration(
                "ai.client.remove",
                "移除 AI 客户端接入",
                "移除指定 AI 客户端中的 HiMind AI 配置并保留原配置备份。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ai_client_targets.clone() }
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIClientRemove,
            ),
            registration(
                "ai.client.import.plan",
                "生成 AI 客户端接入计划",
                "只读预览指定 AI 客户端接入 HiMind 将写入和备份的配置，不修改任何本机文件。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ai_client_targets.clone() },
                        "service": { "type": "string", "description": "managed 或 custom:<id>；仅用于预览切换冲突" }
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIClientImportPlan,
            ),
            registration(
                "ai.client.remove.plan",
                "生成 AI 客户端移除计划",
                "只读预览从指定 AI 客户端移除 HiMind 将写入和备份的配置，不修改任何本机文件。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ai_client_targets.clone() },
                        "service": { "type": "string", "description": "可选服务源，便于客户端统一调用契约" }
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIClientRemovePlan,
            ),
            registration(
                "ai.service.list",
                "AI 服务列表",
                "读取本机可用的 AI 服务：HiMind 分发服务摘要与用户自定义服务；不返回 API Key。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::AIServiceList,
            ),
            registration(
                "ai.service.custom.upsert",
                "保存自定义 AI 服务",
                "新增或更新本机自定义 AI 供应商服务；API Key 加密保存，不落明文。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$" },
                        "display_name": { "type": "string" },
                        "base_url": { "type": "string" },
                        "protocol": { "type": "string", "enum": ["openai-chat", "openai-responses"] },
                        "model": { "type": "string" },
                        "models": { "type": "array", "items": { "type": "string" } },
                        "api_key": { "type": "string" }
                    },
                    "required": ["id", "display_name", "base_url", "protocol", "model"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIServiceCustomUpsert,
            ),
            registration(
                "ai.service.custom.remove",
                "删除自定义 AI 服务",
                "删除本机自定义 AI 供应商服务及其加密存储的 API Key。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIServiceCustomRemove,
            ),
            registration(
                "ai.service.custom.list_models",
                "拉取自定义 AI 服务模型",
                "读取指定自定义 AI 服务的 /models 接口，返回可用模型 ID 列表；不修改任何配置。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AIServiceCustomListModels,
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
                "extension.authoring.preflight",
                "扩展创作预检",
                "检查当前工作区、三件套、Agent 能力和运行模式，返回可机器处理的受阻点。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill"] },
                        "workspace_root": { "type": "string" }
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                }),
                CapabilityHandler::AuthoringPreflight,
            ),
            registration(
                "extension.workspace.current",
                "当前扩展工作区",
                "返回当前扩展工作区、绑定来源，以及检测到的插件或 Skill 项目身份。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::ExtensionWorkspaceCurrent,
            ),
            registration(
                "extension.workspace.bind",
                "绑定扩展工作区",
                "将外部 AI 会话绑定到聚合仓库、插件或 Skill 目录；不打开文件夹，不依赖 Dashboard。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "workspace_root": { "type": "string", "minLength": 1 } },
                    "required": ["workspace_root"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionWorkspaceBind,
            ),
            registration(
                "extension.workspace.clear",
                "清除扩展工作区绑定",
                "清除 Agent 保存的外部 AI 扩展工作区绑定，恢复会话或进程目录。",
                "local_write",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::ExtensionWorkspaceClear,
            ),
            registration(
                "extension.revision.create",
                "创建扩展修订",
                "基于已有插件或 Skill 候选创建下一个补丁版本，并清除旧测试、确认和提审状态。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill"] },
                        "id": { "type": "string" },
                        "version": { "type": "string" }
                    },
                    "required": ["kind", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionRevisionCreate,
            ),
            registration(
                "extension.lock",
                "扩展锁定快照",
                "读取当前 Agent 已安装扩展的来源、版本、摘要和依赖闭包。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::ExtensionLock,
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
                "filesystem.delete",
                "删除本机文件或目录",
                "高风险删除能力。默认只生成删除预览；必须显式 permanent=true，并通过桌面审批后才会执行。系统目录、Agent 数据目录和根目录永远拒绝。",
                "R3",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "minLength": 1, "maxLength": 2000 },
                        "recursive": { "type": "boolean" },
                        "permanent": { "type": "boolean" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::FilesystemDelete,
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
                "extension.test",
                "测试扩展候选",
                "按插件或 Skill 类型执行完整的候选测试闭环并返回结构化报告。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill"] },
                        "id": { "type": "string" },
                        "version": { "type": "string" }
                    },
                    "required": ["kind", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionTest,
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
                "business.project.update", "更新项目", "更新项目资料和协作中心；项目经理/负责人必须通过专用人员能力调整。", "network_write",
                json!({"type":"object","properties":{"project_id":{"type":"string"},"project_name":{"type":"string"},"business_unit_id":{"type":"string"},"management_center_ids":{"type":"array","items":{"type":"string"}},"status":{"type":"string"},"note":{"type":"string"},"exhibit_visibility":{"type":"string"},"repository_access":{"type":"string","enum":["members","all_read","all_read_write"]}},"required":["project_id","project_name"],"additionalProperties":false}), CapabilityHandler::DashboardProjectUpdate,
            ),
            dashboard_business_registration(
                "business.project.delete", "删除项目", "删除项目及其展项、工作区和项目关系。该操作为 R3 高风险能力，必须经过审批。", "R3", json!({"type":"object","properties":{"project_id":{"type":"string"}},"required":["project_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectDelete,
            ),
            dashboard_business_registration(
                "business.exhibit.list", "展项列表", "读取当前用户可见的展项列表。", "read_only", json!({"type":"object","properties":{"q":{"type":"string"},"project":{"type":"string"},"engine":{"type":"string"},"page":{"type":"integer"},"page_size":{"type":"integer"}},"additionalProperties":false}), CapabilityHandler::DashboardExhibitList,
            ),
            dashboard_business_registration(
                "business.exhibit.create", "创建展项", "在项目下创建展项。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_name":{"type":"string"},"parent_exhibit_pid":{"type":["string","null"]},"resolution":{"type":"string"},"hall_id":{"type":"string"},"hall":{"type":"string"},"workload":{"type":"number"},"engineering_id":{"type":"string"},"developer_source":{"type":"string"},"edit_url":{"type":"string"},"status":{"type":"string"},"repository_url":{"type":"string"},"source_path":{"type":"string"},"release_path":{"type":"string"},"config_params":{"type":"array","items":{"type":"string"}},"code_uploads":{"type":"array","items":{"type":"string"}},"engine_type":{"type":"string"},"developer_user_ids":{"type":"array","items":{"type":"string"}},"onsite_debugger_user_ids":{"type":"array","items":{"type":"string"}},"note":{"type":"string"}},"required":["project_id","exhibit_name"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCreate,
            ),
            dashboard_business_registration(
                "business.exhibit.update", "更新展项", "更新展项资料和项目归属；制作人员必须通过 crew 专用能力调整。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"project_id":{"type":"string"},"exhibit_name":{"type":"string"},"parent_exhibit_pid":{"type":["string","null"]},"hall_id":{"type":"string"},"hall":{"type":"string"},"engine_type":{"type":"string"},"status":{"type":"string"},"repository_url":{"type":"string"},"note":{"type":"string"}},"required":["exhibit_id","exhibit_name"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitUpdate,
            ),
            dashboard_business_registration(
                "business.exhibit.delete", "删除展项", "删除展项及其工作区、设备和关联关系。该操作为 R3 高风险能力，必须经过审批。", "R3", json!({"type":"object","properties":{"exhibit_id":{"type":"string"}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitDelete,
            ),
            dashboard_business_registration(
                "business.project.managers.replace", "替换项目经理", "全量替换项目经理；未包含的既有人员会被移除。该操作为 R3 高风险能力，需要审批。", "R3", json!({"type":"object","properties":{"project_id":{"type":"string"},"user_ids":{"type":"array","items":{"type":"string"}},"expected_user_ids":{"type":"array","items":{"type":"string"}}},"required":["project_id","user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardProjectManagersReplace,
            ),
            dashboard_business_registration(
                "business.project.owners.replace", "替换项目负责人", "全量替换项目负责人；未包含的既有人员会被移除。该操作为 R3 高风险能力，需要审批。", "R3", json!({"type":"object","properties":{"project_id":{"type":"string"},"user_ids":{"type":"array","items":{"type":"string"}},"expected_user_ids":{"type":"array","items":{"type":"string"}}},"required":["project_id","user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardProjectOwnersReplace,
            ),
            dashboard_business_registration(
                "business.exhibit.crew.replace", "替换展项人员", "全量替换展项制作人员和现场调试人员；未包含的既有人员会被移除。该操作为 R3 高风险能力，需要审批。", "R3", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"developer_user_ids":{"type":"array","items":{"type":"string"}},"onsite_debugger_user_ids":{"type":"array","items":{"type":"string"}},"expected_developer_user_ids":{"type":"array","items":{"type":"string"}}},"required":["exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCrewReplace,
            ),
            dashboard_business_registration(
                "business.exhibit.crew.append", "追加展项制作人员", "只向展项追加制作人员，不会删除或替换已有制作人员。重复人员会被忽略。", "network_write", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"add_developer_user_ids":{"type":"array","items":{"type":"string"},"maxItems":100},"expected_developer_user_ids":{"type":"array","items":{"type":"string"}}},"required":["exhibit_id","add_developer_user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCrewAppend,
            ),
            dashboard_business_registration(
                "business.exhibit.crew.remove", "移出展项制作人员", "从展项移出制作人员，不影响现场调试人员、需求历史或用户账户；重复移出幂等。", "R3", json!({"type":"object","properties":{"exhibit_id":{"type":"string"},"remove_developer_user_ids":{"type":"array","items":{"type":"string"},"maxItems":100},"expected_developer_user_ids":{"type":"array","items":{"type":"string"}},"reason":{"type":"string","maxLength":500}},"required":["exhibit_id","remove_developer_user_ids"],"additionalProperties":false}), CapabilityHandler::DashboardExhibitCrewRemove,
            ),
            dashboard_business_registration(
                "business.project.exhibit.attach", "关联展项", "将展项关联到指定项目。", "network_write", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_id":{"type":"string"}},"required":["project_id","exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectExhibitAttach,
            ),
            dashboard_business_registration(
                "business.project.exhibit.detach", "解除展项关联", "解除展项与项目的既有关联。该操作为 R3 高风险能力，需要审批。", "R3", json!({"type":"object","properties":{"project_id":{"type":"string"},"exhibit_id":{"type":"string"}},"required":["project_id","exhibit_id"],"additionalProperties":false}), CapabilityHandler::DashboardProjectExhibitDetach,
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
                "operation.get",
                "查看异步操作",
                "读取由 Dashboard AI 能力创建的异步操作状态、进度和结果。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "operation_id": { "type": "string" } },
                    "required": ["operation_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::OperationGet,
            ),
            dashboard_business_registration(
                "operation.cancel",
                "取消异步操作",
                "请求取消仍在排队或执行中的 Dashboard AI 异步操作。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": { "operation_id": { "type": "string" } },
                    "required": ["operation_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::OperationCancel,
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
            apply_registry_metadata(&mut item.descriptor, &item.handler);
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
                    let mut registration = CapabilityRegistration {
                        descriptor: CapabilityDescriptor {
                            id: capability_id.clone(),
                            version: plugin.version.clone(),
                            name: capability.description.clone(),
                            description: capability.description.clone(),
                            risk_level: capability.risk_level.clone(),
                            source: format!("plugin:{}", plugin.id),
                            contract_source: format!("plugin:{}:manifest", plugin.id),
                            contract_generation: None,
                            availability,
                            execution_mode: "provider_defined".to_string(),
                            supports_progress: false,
                            supports_cancel: false,
                            idempotency: "provider_defined".to_string(),
                            retry_policy: "provider_defined".to_string(),
                            concurrency: "provider_defined".to_string(),
                            approval_required: false,
                            dashboard_provider: false,
                            required_scope: None,
                            dashboard_route: None,
                            input_schema: capability.input_schema.clone(),
                        },
                        handler: CapabilityHandler::PluginCapability(capability_id),
                    };
                    apply_registry_metadata(&mut registration.descriptor, &registration.handler);
                    insert_registration(&mut registry, registration)?;
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
                let mut registration = CapabilityRegistration {
                    descriptor,
                    handler: CapabilityHandler::DownstreamMcp(capability_id),
                };
                apply_registry_metadata(&mut registration.descriptor, &registration.handler);
                insert_registration(&mut registry, registration)?;
            }
        }
        // Remote business systems are optional providers. Independent mode
        // never reads their catalog; Connected mode projects ordinary
        // operations into this same Gateway. Static Agent handlers win on ID
        // collisions so special semantics remain local and stable.
        if let Some(snapshot) = self.business_provider.catalog_snapshot() {
            if snapshot.provider.id != self.business_provider.provider_id()
                || snapshot.protocol != self.business_provider.protocol_id()
                || snapshot.protocol_version != self.business_provider.protocol_version()
            {
                return Err("business integration provider protocol identity mismatch".into());
            }
            let contract_generation = snapshot.generation.clone();
            let contract_source = business_integration_contract_source(&snapshot.provider.id);
            for contract in snapshot.items {
                let id = contract.id.clone();
                if let Some(existing) = registry.get_mut(&id) {
                    // Keep the compiled handler for special semantics, while
                    // accepting Dashboard as the source of truth for its
                    // public contract and authorization metadata.
                    if existing.descriptor.dashboard_provider {
                        existing.descriptor.version = contract.version.clone();
                        existing.descriptor.name = contract.name.clone();
                        existing.descriptor.description = contract.description.clone();
                        existing.descriptor.risk_level = if policy::is_destructive_capability(&id) {
                            "R3".to_string()
                        } else {
                            contract.risk_level.clone()
                        };
                        let preserves_agent_execution = matches!(
                            existing.handler,
                            CapabilityHandler::DashboardExhibitWorkspaceCheckout
                                | CapabilityHandler::SoftwareDistributionPublish
                                | CapabilityHandler::MediaSubmit(_, _)
                                | CapabilityHandler::MediaJobGet
                                | CapabilityHandler::MediaJobCancel
                        );
                        if !preserves_agent_execution {
                            existing.descriptor.execution_mode = contract.execution_mode.clone();
                            existing.descriptor.supports_progress = contract.supports_progress;
                            existing.descriptor.supports_cancel = contract.supports_cancel;
                            existing.descriptor.idempotency = contract.idempotency.clone();
                            existing.descriptor.approval_required = contract.approval_required
                                || policy::is_destructive_capability(&id);
                        }
                        existing.descriptor.required_scope = Some(contract.scope.clone());
                        existing.descriptor.dashboard_route = Some(contract.route.clone());
                        existing.descriptor.input_schema = contract.input_schema.clone();
                        existing.descriptor.contract_source = contract_source.clone();
                        existing.descriptor.contract_generation = Some(contract_generation.clone());
                    }
                    continue;
                }
                // A generic HTTP proxy cannot provide Agent-side progress or
                // cancellation semantics. Long-running Dashboard operations
                // must first receive a dedicated static handler; only
                // ordinary synchronous routes are auto-discovered.
                if contract.execution_mode != "sync" {
                    continue;
                }
                insert_registration(
                    &mut registry,
                    CapabilityRegistration {
                        descriptor: CapabilityDescriptor {
                            id: id.clone(),
                            version: contract.version.clone(),
                            name: contract.name.clone(),
                            description: contract.description.clone(),
                            risk_level: if policy::is_destructive_capability(&id) {
                                "R3".to_string()
                            } else {
                                contract.risk_level.clone()
                            },
                            source: contract_source.clone(),
                            contract_source: contract_source.clone(),
                            contract_generation: Some(contract_generation.clone()),
                            availability: CapabilityAvailability::ControlPlane,
                            execution_mode: contract.execution_mode.clone(),
                            supports_progress: contract.supports_progress,
                            supports_cancel: contract.supports_cancel,
                            idempotency: contract.idempotency.clone(),
                            retry_policy: contract.retry_policy.clone(),
                            concurrency: contract.concurrency.clone(),
                            approval_required: contract.approval_required
                                || policy::is_destructive_capability(&id),
                            dashboard_provider: true,
                            required_scope: Some(contract.scope.clone()),
                            dashboard_route: Some(contract.route.clone()),
                            input_schema: contract.input_schema.clone(),
                        },
                        handler: CapabilityHandler::BusinessIntegrationDynamic(contract),
                    },
                )?;
            }
        }
        Ok(registry)
    }

    fn dashboard_user_id(&self, context: &InvocationContext) -> Result<String, Box<dyn Error>> {
        if let Some(user_id) = context
            .principal
            .strip_prefix("dashboard-user:")
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(user_id.trim().to_string());
        }
        // Local entry points share the Agent's persisted Dashboard OAuth
        // identity. Tauri used to be excluded here, which made the desktop
        // "register AI service" action fail even though the Agent was logged
        // in; the MCP path already relied on this same snapshot.
        if matches!(
            context.source,
            crate::capability::types::InvocationSource::Mcp
                | crate::capability::types::InvocationSource::Tauri
                | crate::capability::types::InvocationSource::Cli
        ) {
            if let Some(snapshot) =
                crate::api::oauth::authorization_snapshot(&self.options.state_path)?
            {
                if !snapshot.user_id.trim().is_empty() {
                    return Ok(snapshot.user_id.trim().to_string());
                }
            }
        }
        Err("AI 客户端配置需要已绑定的 Dashboard 用户身份".into())
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
        if let Some(snapshot) = self.business_provider.catalog_snapshot() {
            if is_business_integration_handler(&registration.handler)
                && !snapshot.items.iter().any(|item| item.id == capability_id)
            {
                return Err(format!(
                    "capability not available in Dashboard catalog: {capability_id}"
                )
                .into());
            }
        }
        if matches!(
            registration.descriptor.availability,
            CapabilityAvailability::ControlPlane
        ) && !self.options.mode().control_plane_enabled()
        {
            return Err(serde_json::json!({
                "code": "control_plane_required",
                "capability_id": capability_id,
                "message": "当前运行模式不支持此能力；如需使用，请在设置中切换到组织模式并重启 Agent"
            })
            .to_string()
            .into());
        }
        validate_capability_input_schema(&registration.descriptor.input_schema, &input)?;
        let mut approval_proof =
            self.enforce_high_risk_approval(context, &registration.descriptor, &input)?;
        if (registration.descriptor.approval_required
            || policy::risk_rank(policy::effective_risk_level(
                capability_id,
                &registration.descriptor.risk_level,
            )) >= policy::risk_rank("R3"))
            && policy::destructive_request_type(capability_id).is_none()
            && approval_proof.is_none()
            && matches!(
                context.source,
                crate::capability::types::InvocationSource::Mcp
                    | crate::capability::types::InvocationSource::LocalHttp
                    | crate::capability::types::InvocationSource::Tauri
                    | crate::capability::types::InvocationSource::Cli
            )
        {
            let risk_level =
                policy::effective_risk_level(capability_id, &registration.descriptor.risk_level);
            let target = policy::target_description(capability_id, &input);
            let approved = self
                .approval_manager
                .request_capability_approval(
                    capability_id,
                    risk_level,
                    format!("受控操作审批：{}", registration.descriptor.name),
                    format!(
                        "能力：{}\n风险等级：{}\n目标：{}\n来源：{}\n\n拒绝、超时或 Agent 中断都会阻止实际执行。",
                        capability_id,
                        risk_level,
                        target,
                        context.source.as_str()
                    ),
                )
                .map_err(|error| format!("审批请求失败：{error}"))?;
            // The Agent is the only interactive decision surface for an
            // agent_local request. Create the Dashboard fact only after the
            // local decision so Dashboard never exposes a second pending
            // approval while the Agent is waiting for the user.
            let remote_approval_id = if approved
                && self.options.mode().dashboard_enabled()
                && !policy::is_local_ai_configuration_capability(capability_id)
            {
                let agent_id =
                    crate::api::client::load_agent_state(&self.options.state_path)?.agent_id;
                let args_digest = policy::args_digest(&input)?;
                let generation = policy::approval_generation(
                    registration.descriptor.contract_generation.as_deref(),
                );
                let approval_id = crate::approval::remote::create_approval(
                    &self.options,
                    &agent_id,
                    &context.request_id,
                    capability_id,
                    &registration.descriptor.version,
                    &registration.descriptor.source,
                    risk_level,
                    &input,
                    &format!("{}：{}", registration.descriptor.name, target),
                    &args_digest,
                    generation,
                    120,
                )?;
                Some(approval_id)
            } else {
                None
            };
            if let Some(approval_id) = remote_approval_id.as_deref() {
                match crate::approval::remote::decide_approval(
                    &self.options,
                    approval_id,
                    approved,
                    &context.request_id,
                )? {
                    crate::approval::remote::DecisionSync::Synced => {}
                    crate::approval::remote::DecisionSync::Queued => {
                        self.approval_manager.add_log(
                            "warn",
                            &format!(
                                "审批结果已写入本地 outbox，等待 Dashboard 重放: {approval_id}"
                            ),
                        );
                    }
                }
            }
            if !approved {
                return Err(format!("受控能力 {capability_id} 未获批准，未执行实际副作用").into());
            }
            approval_proof = remote_approval_id.map(ApprovalProof::Approval);
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
            CapabilityHandler::AIClientList => Ok(serde_json::to_value(
                crate::app::ai_provider_import::status(&self.options),
            )?),
            CapabilityHandler::AIClientStatus => Ok(serde_json::to_value(
                crate::app::ai_provider_import::status(&self.options),
            )?),
            CapabilityHandler::AIClientImport => {
                let request: crate::app::ai_provider_import::AIProviderImportRequest =
                    serde_json::from_value(input)?;
                // managed 服务源凭据来自 Dashboard，需绑定 Dashboard 用户；
                // custom 服务源由本机自管，独立模式无 Dashboard 也可用。
                let user_id = if request.service_source() == "managed" {
                    self.dashboard_user_id(context)?
                } else {
                    self.dashboard_user_id(context).unwrap_or_default()
                };
                Ok(serde_json::to_value(
                    crate::app::ai_provider_import::import(&self.options, &user_id, &request)?,
                )?)
            }
            CapabilityHandler::AIClientRemove => {
                let request: crate::app::ai_provider_import::AIProviderImportRequest =
                    serde_json::from_value(input)?;
                Ok(serde_json::to_value(
                    crate::app::ai_provider_import::cancel(&self.options, &request.target)?,
                )?)
            }
            CapabilityHandler::AIClientImportPlan => {
                let request: crate::app::ai_provider_import::AIProviderImportRequest =
                    serde_json::from_value(input)?;
                Ok(serde_json::to_value(
                    crate::app::ai_provider_import::plan_with_service(
                        &self.options,
                        &request.target,
                        "import",
                        request.service_source(),
                    )?,
                )?)
            }
            CapabilityHandler::AIClientRemovePlan => {
                let request: crate::app::ai_provider_import::AIProviderImportRequest =
                    serde_json::from_value(input)?;
                Ok(serde_json::to_value(crate::app::ai_provider_import::plan(
                    &self.options,
                    &request.target,
                    "remove",
                )?)?)
            }
            CapabilityHandler::AIServiceList => {
                let custom = crate::store::ai_services::public_snapshot()?;
                let clients = crate::app::ai_provider_import::status(&self.options);
                let user_id = self.dashboard_user_id(context).unwrap_or_default();
                let managed = crate::api::ai::managed_ai_service_summary(&self.options, &user_id);
                let services = custom
                    .get("services")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                Ok(serde_json::json!({
                    "custom": { "services": services },
                    "managed": managed,
                    "clients": clients,
                }))
            }
            CapabilityHandler::AIServiceCustomUpsert => Ok(serde_json::to_value(
                crate::store::ai_services::upsert(serde_json::from_value(input)?)?.public_json(),
            )?),
            CapabilityHandler::AIServiceCustomRemove => {
                let id = input
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("id is required")?;
                crate::app::ai_provider_import::ensure_service_not_in_use(&self.options, id)?;
                Ok(serde_json::to_value(crate::store::ai_services::remove(
                    id,
                )?)?)
            }
            CapabilityHandler::AIServiceCustomListModels => {
                let id = input
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("id is required")?;
                let models = crate::store::ai_services::list_models(id)?;
                Ok(serde_json::json!({ "service_id": id, "models": models }))
            }
            CapabilityHandler::AuthoringIdentity => self.current_authoring_identity(),
            CapabilityHandler::AuthoringPreflight => self.authoring_preflight(input),
            CapabilityHandler::ExtensionWorkspaceCurrent => {
                crate::extension_projects::current_workspace()
            }
            CapabilityHandler::ExtensionWorkspaceBind => self.bind_extension_workspace(input),
            CapabilityHandler::ExtensionWorkspaceClear => self.clear_extension_workspace(),
            CapabilityHandler::ExtensionRevisionCreate => self.create_extension_revision(input),
            CapabilityHandler::ExtensionLock => {
                Ok(serde_json::to_value(crate::app::extension_lock::load()?)?)
            }
            CapabilityHandler::InnerAdminLoginStatus => Ok(local_login_status_json()),
            CapabilityHandler::SystemOpenFolder => self.open_folder(input),
            CapabilityHandler::FilesystemDelete => self.filesystem_delete(input),
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
            CapabilityHandler::ExtensionTest => self.test_extension_candidate(input),
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
            CapabilityHandler::ExtensionReviewDecide => {
                self.extension_review_decide(input, approval_proof.as_ref())
            }
            CapabilityHandler::SoftwareDistributionPublish => {
                self.publish_software_release(context, input, approval_proof.as_ref())
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
                crate::api::dashboard_business::project_delete(
                    &self.options,
                    input,
                    approval_proof.as_ref(),
                )
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
                crate::api::dashboard_business::exhibit_delete(
                    &self.options,
                    input,
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardProjectManagersReplace => {
                crate::api::dashboard_business::project_people_replace(
                    &self.options,
                    input,
                    "managers",
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardProjectOwnersReplace => {
                crate::api::dashboard_business::project_people_replace(
                    &self.options,
                    input,
                    "owners",
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardExhibitCrewReplace => {
                crate::api::dashboard_business::exhibit_crew_replace(
                    &self.options,
                    input,
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardExhibitCrewAppend => {
                crate::api::dashboard_business::exhibit_crew_append(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitCrewRemove => {
                crate::api::dashboard_business::exhibit_crew_remove(
                    &self.options,
                    input,
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardProjectExhibitAttach => {
                crate::api::dashboard_business::project_exhibit_association(
                    &self.options,
                    input,
                    "attach",
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardProjectExhibitDetach => {
                crate::api::dashboard_business::project_exhibit_association(
                    &self.options,
                    input,
                    "detach",
                    approval_proof.as_ref(),
                )
            }
            CapabilityHandler::DashboardExhibitWorkspaceGet => {
                crate::api::dashboard_business::exhibit_workspace_get(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitWorkspaceBind => {
                crate::api::dashboard_business::exhibit_workspace_bind(&self.options, input)
            }
            CapabilityHandler::DashboardExhibitWorkspaceCheckout => {
                crate::api::dashboard_business::exhibit_workspace_checkout(&self.options, input)
            }
            CapabilityHandler::OperationGet => {
                crate::api::dashboard_business::operation_get(&self.options, input)
            }
            CapabilityHandler::OperationCancel => {
                crate::api::dashboard_business::operation_cancel(&self.options, input)
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
            CapabilityHandler::BusinessIntegrationDynamic(contract) => {
                self.business_provider.invoke(
                    &contract,
                    input,
                    &context.request_id,
                    approval_proof.as_ref(),
                )
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
            "business_integration": json!({
                "provider_id": self.business_provider.provider_id(),
                "protocol": self.business_provider.protocol_id(),
                "protocol_version": self.business_provider.protocol_version(),
                "enabled": self.options.mode().control_plane_enabled(),
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

    fn bind_extension_workspace(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let workspace_root = input
            .get("workspace_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                crate::extension_authoring::blocked_error(
                    "workspace",
                    vec![crate::extension_authoring::blocker(
                        "extension_workspace_required",
                        "workspace",
                        "workspace_root 不能为空",
                        "传入聚合仓库、插件、Skill 或空白扩展目录的绝对路径",
                        false,
                    )],
                    Vec::new(),
                    vec!["修正 workspace_root 后重新调用 extension.workspace.bind".to_string()],
                )
            })?;
        let path =
            crate::extension_workspace::bind(Path::new(workspace_root)).map_err(|error| {
                crate::extension_authoring::blocked_error(
                    "workspace",
                    vec![crate::extension_authoring::blocker(
                        "extension_workspace_bind_failed",
                        "workspace",
                        error.to_string(),
                        "传入存在且可访问的扩展工程目录；不要使用 Agent 安装目录或数据目录",
                        true,
                    )],
                    Vec::new(),
                    vec![
                        "修正目录后重新调用 extension.workspace.bind".to_string(),
                        "绑定成功后重新调用 extension.workspace.current".to_string(),
                    ],
                )
            })?;
        let current = crate::extension_projects::current_workspace()?;
        Ok(json!({
            "state": "ready",
            "bound": true,
            "workspace_root": crate::extension_workspace::display_path(&path),
            "workspace": current,
            "next_steps": [
                "重新调用 extension.workspace.current 确认绑定",
                "调用 extension.authoring.preflight 并传入 kind"
            ]
        }))
    }

    fn clear_extension_workspace(&self) -> Result<Value, Box<dyn Error>> {
        let previous = crate::extension_workspace::bound_root()
            .map(|path| crate::extension_workspace::display_path(&path));
        crate::extension_workspace::clear_binding().map_err(|error| {
            crate::extension_authoring::blocked_error(
                "workspace",
                vec![crate::extension_authoring::blocker(
                    "extension_workspace_clear_failed",
                    "workspace",
                    error.to_string(),
                    "检查 Agent 用户目录权限后重试",
                    true,
                )],
                Vec::new(),
                vec!["重新调用 extension.workspace.clear".to_string()],
            )
        })?;
        let current = crate::extension_projects::current_workspace()?;
        Ok(json!({
            "state": "ready",
            "bound": false,
            "previous_workspace_root": previous,
            "workspace": current,
            "next_steps": ["重新调用 extension.workspace.current 确认当前会话目录"]
        }))
    }

    fn authoring_preflight(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(kind.as_str(), "plugin" | "skill") {
            return Err(crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "invalid_extension_kind",
                    "preflight",
                    "kind 必须是 plugin 或 skill",
                    "使用 kind=plugin 或 kind=skill 重新调用",
                    false,
                )],
                Vec::new(),
                vec!["修正 kind 后重新调用 extension.authoring.preflight".to_string()],
            ));
        }

        let mut blockers = Vec::new();
        let workspace_state = match crate::extension_workspace::current_root() {
            Ok((path, source, bound)) if path.is_dir() => Some((path, source, bound)),
            Ok((path, _, _)) => {
                blockers.push(crate::extension_authoring::blocker(
                    "extension_workspace_invalid",
                    "workspace",
                    format!("当前 AI 工作区不是目录: {}", path.display()),
                    "在外部 AI 工具中打开一个真实的扩展工作区后重试",
                    true,
                ));
                None
            }
            Err(error) => {
                blockers.push(crate::extension_authoring::blocker(
                    "extension_workspace_unavailable",
                    "workspace",
                    error.to_string(),
                    "确认 AI 会话工作区仍然存在并重新调用 extension.workspace.current",
                    true,
                ));
                None
            }
        };
        let current = workspace_state.as_ref().map(|(path, _, _)| path.clone());
        if let Some((current, source, _bound)) = workspace_state.as_ref() {
            if *source == "process_current_dir" {
                blockers.push(crate::extension_authoring::blocker(
                    "extension_workspace_unbound",
                    "workspace",
                    "当前 AI 工作区来自进程目录，尚未绑定扩展工程目录",
                    "调用 extension.workspace.bind，并传入扩展聚合仓库、插件或 Skill 目录",
                    true,
                ));
            }
            if crate::extension_workspace::is_agent_managed_path(current) {
                blockers.push(crate::extension_authoring::blocker(
                    "extension_workspace_unbound",
                    "workspace",
                    "当前 AI 工作区仍是 Agent 主目录，未绑定扩展工程目录",
                    "调用 extension.workspace.bind，并传入扩展聚合仓库或单个扩展项目目录",
                    true,
                ));
            }
        }
        // A session workspace supplied by HiMind AI is already explicit and
        // valid; only an implicit process directory must be rebound by MCP.
        if let Some(requested) = input
            .get("workspace_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if let Some(current) = current.as_ref() {
                match Path::new(requested).canonicalize() {
                    Ok(path) if path == *current || path.starts_with(current) => {}
                    Ok(path) => blockers.push(crate::extension_authoring::blocker(
                        "extension_workspace_mismatch",
                        "workspace",
                        format!(
                            "请求工作区 {} 与当前 AI 工作区 {} 不一致",
                            path.display(),
                            current.display()
                        ),
                        "切换 AI 会话工作区，或使用 extension.workspace.current 返回的 workspace_root",
                        false,
                    )),
                    Err(error) => blockers.push(crate::extension_authoring::blocker(
                        "extension_workspace_invalid",
                        "workspace",
                        format!("无法访问请求工作区: {error}"),
                        "传入存在且可访问的 workspace_root",
                        true,
                    )),
                }
            }
        }

        let required_tools = match kind.as_str() {
            "plugin" => vec![
                "extension.environment.preflight",
                "extension.plugin.scaffold",
                "extension.plugin.validate",
                "extension.plugin.build",
                "extension.plugin.package",
            ],
            _ => vec![
                "extension.environment.preflight",
                "extension.skill.scaffold",
                "extension.skill.validate",
                "extension.skill.package",
            ],
        };
        let visible_capabilities = self.list_capabilities(&InvocationContext::local_http())?;
        let visible_ids = visible_capabilities
            .iter()
            .map(|item| item.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let capability_facts = visible_capabilities
            .iter()
            .map(|item| crate::skill::resolver::CapabilityFact {
                id: item.id.clone(),
                version: item.version.clone(),
                source: item.source.clone(),
            })
            .collect::<Vec<_>>();
        for required in required_tools {
            if !visible_ids.contains(required) {
                blockers.push(crate::extension_authoring::blocker(
                    "extension_tool_missing",
                    "toolchain",
                    format!("三件套未提供必需能力: {required}"),
                    "启用并重新安装 AI 扩展开发工具插件，然后重启 Agent",
                    true,
                ));
            }
        }
        match crate::capability::plugin::find_plugin("com.himind.extension-development-tools") {
            Ok(Some(plugin)) if plugin.enabled && plugin.error.is_none() => {
                let minimum = "1.3.1";
                if crate::skill::resolver::compare_versions(&plugin.version, minimum)
                    == std::cmp::Ordering::Less
                {
                    blockers.push(crate::extension_authoring::blocker(
                        "extension_tools_plugin_outdated",
                        "toolchain",
                        format!(
                            "AI 扩展开发工具版本 {} 低于最低要求 {}",
                            plugin.version, minimum
                        ),
                        "安装最新 AI 扩展开发工具插件后重新执行预检",
                        true,
                    ));
                }
            }
            Ok(Some(plugin)) => blockers.push(crate::extension_authoring::blocker(
                "extension_tools_plugin_unavailable",
                "toolchain",
                format!(
                    "AI 扩展开发工具插件不可用{}",
                    plugin
                        .error
                        .as_deref()
                        .map(|value| format!(": {value}"))
                        .unwrap_or_default()
                ),
                "在本机启用 AI 扩展开发工具插件并重新执行预检",
                true,
            )),
            Ok(None) => blockers.push(crate::extension_authoring::blocker(
                "extension_tools_plugin_missing",
                "toolchain",
                "未安装 AI 扩展开发工具插件",
                "安装三件套中的 AI 扩展开发工具插件后重新执行预检",
                true,
            )),
            Err(error) => blockers.push(crate::extension_authoring::blocker(
                "extension_tools_plugin_lookup_failed",
                "toolchain",
                error.to_string(),
                "修复本机插件注册表后重新执行预检",
                true,
            )),
        }

        let required_skill = match kind.as_str() {
            "plugin" => (
                "com.himind.skill.develop-himind-plugins",
                "1.7.0",
                "extension.plugin.scaffold",
            ),
            _ => (
                "com.himind.skill.develop-himind-skills",
                "1.8.0",
                "extension.skill.scaffold",
            ),
        };
        match crate::skill::store::SkillStore::new().list_records() {
            Ok(records) => {
                let (skill_id, minimum, required_capability) = required_skill;
                match records.iter().find(|record| record.manifest.id == skill_id) {
                    Some(record)
                        if crate::skill::resolver::compare_versions(
                            &record.manifest.version,
                            minimum,
                        ) != std::cmp::Ordering::Less =>
                    {
                        let readiness = crate::skill::resolver::SkillReadiness::resolve(
                            &record.manifest,
                            &capability_facts,
                            VERSION,
                            "himind-ai",
                        );
                        if readiness.state == "blocked"
                            || !visible_ids.contains(required_capability)
                        {
                            blockers.push(crate::extension_authoring::blocker(
                                "authoring_skill_contract_mismatch",
                                "toolchain",
                                format!(
                                    "{} 的 MCP 依赖契约未满足: {}",
                                    record.manifest.name,
                                    if readiness.reasons.is_empty() {
                                        format!("缺少 {required_capability}")
                                    } else {
                                        readiness.reasons.join("、")
                                    }
                                ),
                                "更新 Agent 能力或重新安装与当前 Agent 契约匹配的三件套 Skill",
                                true,
                            ));
                        }
                    }
                    Some(record) => blockers.push(crate::extension_authoring::blocker(
                        "authoring_skill_outdated",
                        "toolchain",
                        format!(
                            "{} 版本 {} 低于最低要求 {}",
                            record.manifest.name, record.manifest.version, minimum
                        ),
                        "从聚合扩展仓库安装最新三件套 Skill 后重试",
                        true,
                    )),
                    None => blockers.push(crate::extension_authoring::blocker(
                        "authoring_skill_missing",
                        "toolchain",
                        format!("未安装 {skill_id}"),
                        "安装对应的插件开发助手或技能开发助手后重试",
                        true,
                    )),
                }
            }
            Err(error) => blockers.push(crate::extension_authoring::blocker(
                "authoring_skill_lookup_failed",
                "toolchain",
                format!("读取三件套 Skill 失败: {error}"),
                "修复 Agent Skill Store 后重新执行预检",
                true,
            )),
        }

        let mut warnings = Vec::new();
        if !self.options.mode().dashboard_enabled() {
            warnings.push(crate::extension_authoring::warning(
                "独立模式可完成本地创作、候选测试和客户端注册；提审与组织分发需在组织模式执行。",
            ));
        } else if crate::api::client::load_agent_state(&self.options.state_path)
            .ok()
            .is_none_or(|state| state.agent_id.trim().is_empty())
        {
            warnings.push(crate::extension_authoring::warning(
                "当前为组织模式，但 Agent 尚未完成工作台配对；本地创作不受影响，提审前需完成配对。",
            ));
        }
        let next_steps = vec![
            "调用 extension.authoring.identity 获取作者资料".to_string(),
            format!("调用 extension.{kind}.scaffold 创建或更新工程"),
            format!("调用 extension.{kind}.validate、构建/打包能力生成候选制品"),
            "调用 extension.test 完成依赖、注册、运行时和清理闭环".to_string(),
        ];
        if blockers.is_empty() {
            Ok(crate::extension_authoring::success(
                &kind,
                json!({
                    "workspace": current.map(|path| json!({
                        "root": crate::extension_workspace::display_path(&path),
                        "available": true,
                        "source": workspace_state.as_ref().map(|(_, source, _)| *source).unwrap_or("unknown"),
                        "bound": workspace_state.as_ref().map(|(_, _, bound)| *bound).unwrap_or(false),
                    })).unwrap_or_else(|| json!({"available": false, "bound": false})),
                    "mode": self.options.mode().as_str(),
                    "toolchain": "ready",
                    "submission": if self.options.mode().dashboard_enabled() { "available" } else { "connected_mode_required" },
                    "warnings": warnings,
                    "next_steps": next_steps,
                }),
            ))
        } else {
            Err(crate::extension_authoring::blocked_error(
                &kind, blockers, warnings, next_steps,
            ))
        }
    }

    fn create_extension_revision(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let (id, version) = authoring_identity(&input)?;
        let result = match kind.as_str() {
            "plugin" => {
                serde_json::to_value(crate::plugin_authoring::create_revision(&id, &version)?)?
            }
            "skill" => {
                serde_json::to_value(crate::skill::authoring::create_revision(&id, &version)?)?
            }
            _ => {
                return Err(crate::extension_authoring::blocked_error(
                    "unknown",
                    vec![crate::extension_authoring::blocker(
                        "invalid_extension_kind",
                        "revision",
                        "kind 必须是 plugin 或 skill",
                        "使用 kind=plugin 或 kind=skill 重新调用",
                        false,
                    )],
                    Vec::new(),
                    Vec::new(),
                ))
            }
        };
        let next_version = result
            .get("manifest")
            .and_then(|manifest| manifest.get("version"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(crate::extension_authoring::success(
            &kind,
            json!({
                "id": id,
                "previous_version": version,
                "version": next_version,
                "draft": result,
                "next_steps": [
                    "在修订工作区完成修改",
                    "重新校验、构建/打包并调用 extension.test",
                ],
            }),
        ))
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

    fn enforce_high_risk_approval(
        &self,
        context: &InvocationContext,
        descriptor: &CapabilityDescriptor,
        input: &Value,
    ) -> Result<Option<ApprovalProof>, Box<dyn Error>> {
        let destructive_type = policy::destructive_request_type(&descriptor.id);
        // AI 连接域只读写本机 AI 客户端配置与本机服务状态，由 Agent 自管，
        // 不依赖 Dashboard Grant 事实源；本机确认由 invoke 审批分支承担。
        if policy::is_local_ai_configuration_capability(&descriptor.id) {
            return Ok(None);
        }
        // Grant validation is part of the Gateway contract for every
        // capability that advertises approval_required, not just deletes.
        // Non-destructive capabilities may still fall back to an explicit
        // desktop confirmation, while background Worker execution must have a
        // server-issued Grant because no local user is present.
        if destructive_type.is_none()
            && !descriptor.approval_required
            && policy::risk_rank(policy::effective_risk_level(
                &descriptor.id,
                &descriptor.risk_level,
            )) < policy::risk_rank("R3")
        {
            return Ok(None);
        }
        // A non-permanent filesystem call is a read-only deletion preview.
        if descriptor.id == "filesystem.delete"
            && !input
                .get("permanent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Ok(None);
        }
        if self.options.mode().dashboard_enabled() {
            let args_digest = policy::args_digest(input)?;
            let generation = policy::approval_generation(descriptor.contract_generation.as_deref());
            match crate::approval::remote::active_grant(
                &self.options,
                &descriptor.id,
                &descriptor.version,
                &descriptor.source,
                policy::effective_risk_level(&descriptor.id, &descriptor.risk_level),
                generation,
                &args_digest,
                input,
            ) {
                Ok(Some(grant_id)) => {
                    self.approval_manager.add_log(
                        "info",
                        &format!("已使用 Dashboard Grant 放行高风险能力: {}", descriptor.id),
                    );
                    return Ok(Some(ApprovalProof::Grant(grant_id)));
                }
                Ok(None) => {}
                Err(error)
                    if context.source
                        == crate::capability::types::InvocationSource::DashboardWorker =>
                {
                    return Err(format!(
                        "无法验证审批授权，已阻止执行 {}：{}",
                        descriptor.id, error
                    )
                    .into())
                }
                Err(error) => {
                    self.approval_manager.add_log(
                        "warn",
                        &format!(
                            "Dashboard Grant 暂不可用，将等待本机确认: {} ({})",
                            descriptor.id, error
                        ),
                    );
                }
            }
        }
        if destructive_type.is_none() {
            if context.source == crate::capability::types::InvocationSource::DashboardWorker {
                return Err(json!({
                    "code": "approval_required",
                    "capability_id": descriptor.id,
                    "risk_level": policy::effective_risk_level(&descriptor.id, &descriptor.risk_level),
                    "message": "后台 Agent 任务执行需要已批准的 Dashboard Grant；当前调用未执行实际副作用"
                })
                .to_string()
                .into());
            }
            return Ok(None);
        }
        let request_type = destructive_type.expect("destructive type checked above");
        // Background work has no trustworthy local user interaction channel.
        // It must be resumed with a server-issued approval/grant in a later
        // phase; silently treating the worker as the approver is forbidden.
        if context.source == crate::capability::types::InvocationSource::DashboardWorker {
            return Err(json!({
                "code": "approval_required",
                "capability_id": descriptor.id,
                "risk_level": policy::effective_risk_level(&descriptor.id, &descriptor.risk_level),
                "message": "后台 Agent 任务执行删除类能力前必须携带已批准的 Dashboard 授权；当前调用未执行任何删除"
            })
            .to_string()
            .into());
        }
        let target = policy::target_description(&descriptor.id, input);
        let title = format!("高风险操作审批：{}", descriptor.name);
        let description = format!(
            "能力：{}\n风险等级：R3\n目标：{}\n来源：{}\n\n拒绝、超时或关闭审批窗口都会阻止实际执行。",
            descriptor.id,
            target,
            context.source.as_str()
        );
        let approved = self
            .approval_manager
            .request_approval(request_type, title, description.clone())
            .map_err(|error| format!("审批请求失败：{error}"))?;
        // Keep agent_local as the sole interactive approval surface. The
        // durable Dashboard request is created only after local approval and
        // is immediately resolved with the same decision.
        let remote_approval_id = if approved && self.options.mode().dashboard_enabled() {
            let agent_id = crate::api::client::load_agent_state(&self.options.state_path)?.agent_id;
            let args_digest = policy::args_digest(input)?;
            let generation = policy::approval_generation(descriptor.contract_generation.as_deref());
            Some(crate::approval::remote::create_approval(
                &self.options,
                &agent_id,
                &context.request_id,
                &descriptor.id,
                &descriptor.version,
                &descriptor.source,
                policy::effective_risk_level(&descriptor.id, &descriptor.risk_level),
                input,
                &description,
                &args_digest,
                generation,
                120,
            )?)
        } else {
            None
        };
        if let Some(approval_id) = remote_approval_id.as_deref() {
            match crate::approval::remote::decide_approval(
                &self.options,
                approval_id,
                approved,
                &context.request_id,
            )? {
                crate::approval::remote::DecisionSync::Synced => {}
                crate::approval::remote::DecisionSync::Queued => self.approval_manager.add_log(
                    "warn",
                    &format!(
                        "审批结果已写入本地 outbox，等待 Dashboard 重放: {}",
                        approval_id
                    ),
                ),
            }
        }
        if approved {
            Ok(remote_approval_id.map(ApprovalProof::Approval))
        } else {
            Err(format!("高风险能力 {} 未获批准，未执行任何删除", descriptor.id).into())
        }
    }

    fn filesystem_delete(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let raw_path = input
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if raw_path.is_empty() {
            return Err("path is required".into());
        }
        let target =
            fs::canonicalize(raw_path).map_err(|error| format!("无法解析删除目标：{error}"))?;
        validate_delete_target(&target)?;
        let metadata = fs::metadata(&target)?;
        let recursive = input
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (items, bytes) = delete_target_stats(&target, metadata.is_dir())?;
        if metadata.is_dir() && !recursive {
            return Err("目标是目录；必须显式 recursive=true 才能删除目录".into());
        }
        if items > 10_000 {
            return Err("为避免误删，单次删除最多允许 10000 个文件系统项".into());
        }
        if !input
            .get("permanent")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(json!({
                "ok": false,
                "preview": true,
                "requires_permanent_confirmation": true,
                "path": target.to_string_lossy(),
                "is_directory": metadata.is_dir(),
                "items": items,
                "bytes": bytes,
                "message": "这是删除预览；再次调用时需传 permanent=true，并重新经过审批"
            }));
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&target)?;
        } else {
            fs::remove_file(&target)?;
        }
        Ok(json!({
            "ok": true,
            "deleted": true,
            "path": target.to_string_lossy(),
            "items": items,
            "bytes": bytes
        }))
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
                "message": "当前运行模式不支持此能力；如需使用，请在设置中切换到组织模式并重启 Agent"
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
        approval_proof: Option<&ApprovalProof>,
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
            approval_proof,
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
        match crate::skill::authoring::test(&id, &version, &capability_facts) {
            Ok(result) => Ok(crate::extension_authoring::success(
                "skill",
                serde_json::to_value(result)?,
            )),
            Err(error) => Err(authoring_operation_error("skill", "test", error)),
        }
    }

    fn test_extension_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let (id, version) = authoring_identity(&input)?;
        match kind {
            "skill" => {
                let capability_facts = crate::skill::capability_facts_from_gateway(
                    &self.options,
                    Arc::clone(&self.worker_status),
                    &InvocationContext::new(
                        crate::capability::types::InvocationSource::Mcp,
                        "authoring-test",
                    ),
                )?;
                let result = crate::skill::authoring::test(&id, &version, &capability_facts)
                    .map_err(|error| authoring_operation_error("skill", "test", error))?;
                let cleanup_state = result
                    .cleanup
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                if cleanup_state != "passed" {
                    return Err(authoring_operation_error(
                        "skill",
                        "cleanup",
                        format!("Skill 候选测试清理未通过: {cleanup_state}"),
                    ));
                }
                Ok(json!({
                    "kind": "skill",
                    "id": id,
                    "version": version,
                    "state": "passed",
                    "checks": {
                        "manifest": "passed",
                        "dependencies": "passed",
                        "package": "passed",
                        "client_registration": "passed",
                        "cleanup": cleanup_state
                    },
                    "result": result
                }))
            }
            "plugin" => {
                let result = crate::plugin_authoring::test(&id, &version)
                    .map_err(|error| authoring_operation_error("plugin", "test", error))?;
                let report = result.test_report.clone().unwrap_or_else(|| {
                    json!({
                        "manifest": "passed",
                        "dependencies": "passed",
                        "package": "passed",
                        "runtime": { "state": "skipped" },
                        "lifecycle": { "state": "passed" }
                    })
                });
                Ok(json!({
                    "kind": "plugin",
                    "id": id,
                    "version": version,
                    "state": "passed",
                    "checks": {
                        "manifest": "passed",
                        "dependencies": "passed",
                        "package": "passed",
                        "development_registration": "passed",
                        "runtime": report.get("runtime").cloned().unwrap_or(Value::Null),
                        "lifecycle": report.get("lifecycle").cloned().unwrap_or(Value::Null),
                        "cleanup": report.get("cleanup").cloned().unwrap_or(Value::Null)
                    },
                    "result": result,
                    "report": report
                }))
            }
            _ => Err(crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "invalid_extension_kind",
                    "test",
                    "kind must be plugin or skill",
                    "使用 kind=plugin 或 kind=skill 重新调用 extension.test",
                    false,
                )],
                Vec::new(),
                Vec::new(),
            )),
        }
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
        match crate::plugin_authoring::test(&id, &version) {
            Ok(result) => Ok(crate::extension_authoring::success(
                "plugin",
                serde_json::to_value(result)?,
            )),
            Err(error) => Err(authoring_operation_error("plugin", "test", error)),
        }
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

    fn extension_review_decide(
        &self,
        input: Value,
        approval_proof: Option<&ApprovalProof>,
    ) -> Result<Value, Box<dyn Error>> {
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
            approval_proof,
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

fn authoring_operation_error(
    kind: &str,
    stage: &str,
    error: impl std::fmt::Display,
) -> Box<dyn Error> {
    let message = error.to_string();
    let normalized = message.to_ascii_lowercase();
    let (code, remediation) = if normalized.contains("依赖")
        || normalized.contains("missing required")
        || normalized.contains("not found")
    {
        (
            "extension_dependency_missing",
            "补齐 Manifest 声明的必需 Capability/插件依赖，或调整为可选依赖后重试",
        )
    } else if normalized.contains("清理") || normalized.contains("恢复") {
        (
            "extension_cleanup_failed",
            "检查客户端注册目录、Agent Store 和插件注册表的写权限，清理残留后重试",
        )
    } else if normalized.contains("运行时") || normalized.contains("runtime") {
        (
            "extension_runtime_contract_failed",
            "修复插件 JSON-RPC/stdio 入口或测试输入后重新构建候选包",
        )
    } else if normalized.contains("候选") || normalized.contains("draft") {
        (
            "extension_candidate_invalid",
            "重新执行校验和打包，并确认候选包路径、Manifest 与 SHA-256 一致",
        )
    } else {
        (
            "extension_operation_failed",
            "根据 blockers.message 修复对应阶段后重新调用 extension.test",
        )
    };
    crate::extension_authoring::operation_error_with_code(kind, stage, code, message, remediation)
}

fn validate_delete_target(target: &Path) -> Result<(), Box<dyn Error>> {
    if target.parent().is_none() || target == target.parent().unwrap_or(target) {
        return Err("禁止删除磁盘根目录".into());
    }
    let target_key = target.to_string_lossy().to_ascii_lowercase();
    let mut protected = vec![crate::store::paths::agent_home()];
    for variable in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramData",
        "ProgramFiles(x86)",
    ] {
        if let Ok(value) = std::env::var(variable) {
            if !value.trim().is_empty() {
                protected.push(std::path::PathBuf::from(value));
            }
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            protected.push(parent.to_path_buf());
        }
    }
    if protected.into_iter().any(|root| {
        let root_key = root
            .canonicalize()
            .unwrap_or(root)
            .to_string_lossy()
            .to_ascii_lowercase();
        target_key == root_key
            || target_key.starts_with(&(root_key + std::path::MAIN_SEPARATOR.to_string().as_str()))
    }) {
        return Err("禁止删除系统目录、Agent 数据目录或 Agent 安装目录".into());
    }
    Ok(())
}

fn delete_target_stats(target: &Path, is_directory: bool) -> Result<(usize, u64), Box<dyn Error>> {
    if !is_directory {
        return Ok((1, fs::metadata(target)?.len()));
    }
    let mut items = 0usize;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(target).follow_links(false) {
        let entry = entry?;
        items = items.saturating_add(1);
        if items > 10_000 {
            break;
        }
        if entry.file_type().is_file() {
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok((items, bytes))
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
        | "business.exhibit.crew.replace"
        | "business.exhibit.crew.append"
        | "business.exhibit.crew.remove" => Some(crate::api::oauth::BUSINESS_PEOPLE_WRITE_SCOPE),
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
        "operation.get" => Some(crate::api::oauth::OPERATION_READ_SCOPE),
        "operation.cancel" => Some(crate::api::oauth::OPERATION_CANCEL_SCOPE),
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

/// Populate the contract fields that are shared by HTTP, Tauri, Dashboard
/// Worker and MCP consumers.  Keeping this derivation next to the Gateway
/// registry prevents each adapter from inventing its own notion of a long
/// task, retry safety or Dashboard route.
fn apply_registry_metadata(descriptor: &mut CapabilityDescriptor, handler: &CapabilityHandler) {
    let id = descriptor.id.as_str();
    let trusted_local_authoring = is_trusted_local_authoring_capability(descriptor, handler);
    descriptor.required_scope = required_platform_scope(id).map(str::to_string);
    descriptor.dashboard_route = dashboard_route_for(id);
    descriptor.dashboard_provider = is_dashboard_provider_handler(handler);

    let long_running = matches!(
        handler,
        CapabilityHandler::SvnWorkspaceCheckout
            | CapabilityHandler::DashboardExhibitWorkspaceCheckout
            | CapabilityHandler::SoftwareDistributionPublish
            | CapabilityHandler::MediaSubmit(_, _)
            | CapabilityHandler::MediaJobCancel
    );
    descriptor.execution_mode = if long_running { "long_running" } else { "sync" }.to_string();
    descriptor.supports_progress = long_running;
    descriptor.supports_cancel = matches!(
        handler,
        CapabilityHandler::SvnWorkspaceCheckout
            | CapabilityHandler::MediaSubmit(_, _)
            | CapabilityHandler::MediaJobCancel
            | CapabilityHandler::DashboardExhibitWorkspaceCheckout
    );
    descriptor.approval_required =
        policy::risk_rank(policy::effective_risk_level(id, &descriptor.risk_level))
            >= policy::risk_rank("R3")
            || policy::is_destructive_capability(id)
            || matches!(handler, CapabilityHandler::DownstreamMcp(_))
            || (matches!(handler, CapabilityHandler::PluginCapability(_))
                && !matches!(
                    descriptor.risk_level.trim().to_ascii_uppercase().as_str(),
                    "READ_ONLY" | "R1"
                )
                && !trusted_local_authoring)
            || matches!(
                handler,
                CapabilityHandler::SoftwareDistributionPublish
                    | CapabilityHandler::ExtensionReviewDecide
            );
    descriptor.idempotency = if descriptor.risk_level == "read_only" {
        "safe"
    } else if matches!(handler, CapabilityHandler::DashboardExhibitCrewAppend) {
        "safe"
    } else if matches!(
        handler,
        CapabilityHandler::DashboardProjectManagersReplace
            | CapabilityHandler::DashboardProjectOwnersReplace
            | CapabilityHandler::DashboardExhibitCrewReplace
            | CapabilityHandler::DashboardExhibitCrewRemove
    ) {
        "conditional"
    } else if matches!(
        handler,
        CapabilityHandler::DashboardProjectExhibitAttach
            | CapabilityHandler::DashboardProjectExhibitDetach
            | CapabilityHandler::MediaJobCancel
    ) {
        "conditional"
    } else {
        "not_guaranteed"
    }
    .to_string();
    descriptor.retry_policy = if descriptor.idempotency == "safe" {
        "safe"
    } else if descriptor.idempotency == "conditional" {
        "idempotency_key"
    } else {
        "never"
    }
    .to_string();
    descriptor.concurrency = if descriptor.risk_level == "read_only" {
        "parallel"
    } else {
        "keyed"
    }
    .to_string();
}

const EXTENSION_DEVELOPMENT_TOOLS_PLUGIN_ID: &str = "com.himind.extension-development-tools";

/// These are deterministic local authoring operations supplied by the
/// first-party development-tools plugin. They are intentionally a small
/// allow-list: a future plugin capability must opt into the normal approval
/// path until its workspace and risk contract are reviewed.
fn is_extension_tool_capability(capability_id: &str) -> bool {
    matches!(
        capability_id,
        "extension.plugin.scaffold"
            | "extension.plugin.validate"
            | "extension.plugin.build"
            | "extension.plugin.package"
            | "extension.skill.scaffold"
            | "extension.skill.validate"
            | "extension.skill.package"
    )
}

fn is_trusted_local_authoring_capability(
    descriptor: &CapabilityDescriptor,
    handler: &CapabilityHandler,
) -> bool {
    matches!(handler, CapabilityHandler::PluginCapability(_))
        && descriptor.source == format!("plugin:{EXTENSION_DEVELOPMENT_TOOLS_PLUGIN_ID}")
        && descriptor.availability == CapabilityAvailability::Local
        && is_extension_tool_capability(&descriptor.id)
}

fn is_dashboard_provider_handler(handler: &CapabilityHandler) -> bool {
    matches!(
        handler,
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
            | CapabilityHandler::DashboardExhibitCrewAppend
            | CapabilityHandler::DashboardExhibitCrewRemove
            | CapabilityHandler::DashboardProjectExhibitAttach
            | CapabilityHandler::DashboardProjectExhibitDetach
            | CapabilityHandler::DashboardExhibitWorkspaceGet
            | CapabilityHandler::DashboardExhibitWorkspaceBind
            | CapabilityHandler::DashboardExhibitWorkspaceCheckout
            | CapabilityHandler::OperationGet
            | CapabilityHandler::OperationCancel
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
            | CapabilityHandler::BusinessIntegrationDynamic(_)
            | CapabilityHandler::MediaSubmit(_, _)
            | CapabilityHandler::MediaJobGet
            | CapabilityHandler::MediaJobCancel
    )
}

fn is_business_integration_handler(handler: &CapabilityHandler) -> bool {
    matches!(
        handler,
        CapabilityHandler::DashboardContextResolve
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
            | CapabilityHandler::DashboardExhibitCrewAppend
            | CapabilityHandler::DashboardExhibitCrewRemove
            | CapabilityHandler::DashboardProjectExhibitAttach
            | CapabilityHandler::DashboardProjectExhibitDetach
            | CapabilityHandler::DashboardExhibitWorkspaceGet
            | CapabilityHandler::DashboardExhibitWorkspaceBind
            | CapabilityHandler::DashboardExhibitWorkspaceCheckout
            | CapabilityHandler::OperationGet
            | CapabilityHandler::OperationCancel
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
            | CapabilityHandler::BusinessIntegrationDynamic(_)
    )
}

fn business_integration_contract_source(provider_id: &str) -> String {
    if provider_id == DASHBOARD_BUSINESS_PROVIDER_ID {
        // Preserve the established Dashboard source label for existing MCP
        // consumers while other providers use the protocol-neutral form.
        "dashboard:catalog".to_string()
    } else {
        format!("business-integration:{provider_id}:catalog")
    }
}

fn dashboard_route_for(capability_id: &str) -> Option<String> {
    let route = match capability_id {
        "context.resolve" => "/api/integrations/ai/business/context/resolve",
        "project.context.get" | "business.project.get" => {
            "/api/integrations/ai/business/projects/{project_id}"
        }
        "exhibit.context.get" | "business.exhibit.get" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}"
        }
        "work.my_summary" => "/api/integrations/ai/business/my-work/summary",
        "business.project.list" => "/api/integrations/ai/business/projects",
        "business.project.create" => "/api/integrations/ai/business/projects",
        "business.project.update" | "business.project.delete" => {
            "/api/integrations/ai/business/projects/{project_id}"
        }
        "business.project.managers.replace" => {
            "/api/integrations/ai/business/projects/{project_id}/managers"
        }
        "business.project.owners.replace" => {
            "/api/integrations/ai/business/projects/{project_id}/owners"
        }
        "business.exhibit.list" | "business.exhibit.create" => {
            "/api/integrations/ai/business/exhibits"
        }
        "business.exhibit.update" | "business.exhibit.delete" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}"
        }
        "business.exhibit.crew.replace" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}/crew"
        }
        "business.exhibit.crew.append" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}/crew/append"
        }
        "business.exhibit.crew.remove" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}/crew/remove"
        }
        "business.project.exhibit.attach" => {
            "/api/integrations/ai/business/projects/{project_id}/exhibits/{exhibit_id}/attach"
        }
        "business.project.exhibit.detach" => {
            "/api/integrations/ai/business/projects/{project_id}/exhibits/{exhibit_id}/detach"
        }
        "business.people.search" => "/api/integrations/ai/business/people/search",
        "business.requirement.list" => "/api/integrations/ai/business/requirements",
        "business.requirement.get" => "/api/integrations/ai/business/requirements/{requirement_id}",
        "business.requirement.create" => "/api/integrations/ai/business/requirements",
        "business.requirement.update"
        | "business.requirement.cancel"
        | "business.requirement.reopen"
        | "business.requirement.review"
        | "business.requirement.comment" => {
            "/api/integrations/ai/business/requirements/{requirement_id}"
        }
        "business.requirement.assignment.update" => {
            "/api/integrations/ai/business/requirements/{requirement_id}/assignment"
        }
        "knowledge.search.v1" => "/api/integrations/ai/business/knowledge/search",
        "business.exhibit.workspace.checkout" => {
            "/api/integrations/ai/business/exhibits/{exhibit_id}/workspace/checkout"
        }
        "operation.get" => "/api/integrations/ai/operations/{operation_id}",
        "operation.cancel" => "/api/integrations/ai/operations/{operation_id}/cancel",
        _ => return None,
    };
    Some(route.to_string())
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
        | CapabilityHandler::AuthoringPreflight
        | CapabilityHandler::ExtensionWorkspaceCurrent
        | CapabilityHandler::ExtensionWorkspaceBind
        | CapabilityHandler::ExtensionWorkspaceClear
        | CapabilityHandler::ExtensionRevisionCreate
        | CapabilityHandler::ExtensionLock
        | CapabilityHandler::ExtensionTest
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
        | CapabilityHandler::DashboardExhibitCrewAppend
        | CapabilityHandler::DashboardExhibitCrewRemove
        | CapabilityHandler::DashboardProjectExhibitAttach
        | CapabilityHandler::DashboardProjectExhibitDetach
        | CapabilityHandler::DashboardExhibitWorkspaceGet
        | CapabilityHandler::DashboardExhibitWorkspaceBind
        | CapabilityHandler::DashboardExhibitWorkspaceCheckout
        | CapabilityHandler::OperationGet
        | CapabilityHandler::OperationCancel
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
    // First-party authoring tools are local capabilities, but their file
    // boundary must be enforced for every invocation adapter (MCP, CLI,
    // local HTTP and Tauri). Otherwise a caller could bypass the workspace
    // guard simply by choosing a different adapter than MCP.
    if is_extension_tool_capability(capability_id) {
        return validate_extension_tool_workspace(input);
    }
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

fn validate_extension_tool_workspace(input: &Value) -> Result<(), Box<dyn Error>> {
    let workspace_root = input
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "extension_workspace_required",
                    "workspace",
                    "扩展开发能力必须提供 workspace_root",
                    "传入已绑定的聚合仓库、插件或 Skill 项目目录",
                    false,
                )],
                Vec::new(),
                vec!["调用 extension.workspace.current 确认工作区".to_string()],
            )
        })?;

    let (current, source, _bound) =
        crate::extension_workspace::current_root().map_err(|error| {
            crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "extension_workspace_unavailable",
                    "workspace",
                    error.to_string(),
                    "调用 extension.workspace.bind 绑定扩展工程目录",
                    true,
                )],
                Vec::new(),
                vec!["调用 extension.workspace.current 确认工作区".to_string()],
            )
        })?;
    if source == "process_current_dir"
        || crate::extension_workspace::is_agent_managed_path(&current)
    {
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_unbound",
                "workspace",
                "当前 AI 工作区尚未绑定扩展工程目录",
                "调用 extension.workspace.bind，并传入扩展聚合仓库、插件或 Skill 目录",
                true,
            )],
            Vec::new(),
            vec![
                "调用 extension.workspace.bind 绑定扩展工作区".to_string(),
                "重新调用 extension.workspace.current 确认绑定".to_string(),
            ],
        ));
    }

    let requested = Path::new(workspace_root).canonicalize().map_err(|error| {
        crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_invalid",
                "workspace",
                format!("无法访问请求工作区: {error}"),
                "传入存在且可访问的 workspace_root",
                true,
            )],
            Vec::new(),
            vec!["修正 workspace_root 后重新调用扩展开发能力".to_string()],
        )
    })?;
    if !requested.is_dir() {
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_invalid",
                "workspace",
                "workspace_root 必须是目录",
                "传入存在且可访问的扩展工作区目录",
                false,
            )],
            Vec::new(),
            vec!["修正 workspace_root 后重新调用扩展开发能力".to_string()],
        ));
    }
    if !requested.starts_with(&current) {
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_mismatch",
                "workspace",
                format!(
                    "请求工作区 {} 与当前扩展工作区 {} 不一致",
                    requested.display(),
                    current.display()
                ),
                "切换 AI 会话工作区，或使用 extension.workspace.current 返回的 workspace_root",
                false,
            )],
            Vec::new(),
            vec!["将 workspace_root 改为当前扩展工作区或其子目录".to_string()],
        ));
    }
    Ok(())
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
        .ok_or_else(|| {
            crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "extension_package_required",
                    "package",
                    "package_path is required",
                    "传入工作区内已生成并校验的 .hmpkg 或 .hmskill 文件",
                    false,
                )],
                Vec::new(),
                vec!["重新调用候选保存能力并传入 package_path".to_string()],
            )
        })?;
    let current = crate::extension_projects::current_workspace_path().map_err(|error| {
        crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_unavailable",
                "workspace",
                error.to_string(),
                "先调用 extension.workspace.bind 绑定扩展工程目录",
                true,
            )],
            Vec::new(),
            vec!["调用 extension.workspace.current 确认工作区".to_string()],
        )
    })?;
    let package = Path::new(package_path).canonicalize().map_err(|error| {
        crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_package_invalid",
                "package",
                format!("无法访问候选包: {error}"),
                "传入存在且可访问的 .hmpkg 或 .hmskill 文件",
                true,
            )],
            Vec::new(),
            vec!["重新构建并打包扩展后重试".to_string()],
        )
    })?;
    if !package.is_file() || !package.starts_with(&current) {
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_unbound",
                "workspace",
                format!(
                    "候选包必须位于当前 AI 扩展工作区内: {}",
                    crate::extension_workspace::display_path(&current)
                ),
                "调用 extension.workspace.bind 绑定候选包所在的聚合仓库或扩展项目目录",
                true,
            )],
            Vec::new(),
            vec![
                "调用 extension.workspace.bind，并传入候选包所在目录".to_string(),
                "重新调用 extension.workspace.current 确认绑定".to_string(),
                "再调用 extension.*.candidate.save 保存候选包".to_string(),
            ],
        ));
    }
    Ok(())
}

fn validate_extension_workspace_root(
    current: &Path,
    requested: &str,
) -> Result<(), Box<dyn Error>> {
    let requested = Path::new(requested).canonicalize()?;
    if !requested.starts_with(current) {
        return Err(serde_json::json!({
            "code": "extension_workspace_mismatch",
            "message": "workspace_root 必须与当前 AI 扩展工作区一致，或位于该工作区内"
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
    let mut descriptor = CapabilityDescriptor {
        id: id.to_string(),
        version: version.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        risk_level: risk_level.to_string(),
        source: "builtin".to_string(),
        contract_source: "builtin".to_string(),
        contract_generation: None,
        availability: availability_for_handler(&handler),
        execution_mode: "sync".to_string(),
        supports_progress: false,
        supports_cancel: false,
        idempotency: "unknown".to_string(),
        retry_policy: "unknown".to_string(),
        concurrency: "unknown".to_string(),
        approval_required: false,
        dashboard_provider: false,
        required_scope: None,
        dashboard_route: None,
        input_schema,
    };
    apply_registry_metadata(&mut descriptor, &handler);
    CapabilityRegistration {
        descriptor,
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
    item.descriptor.contract_source = "agent:dashboard-fallback".to_string();
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
    item.descriptor.contract_source = "agent:dashboard-fallback".to_string();
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
    mut registration: CapabilityRegistration,
) -> Result<(), Box<dyn Error>> {
    let id = registration.descriptor.id.trim().to_string();
    if id.is_empty() {
        return Err("capability id is required".into());
    }
    if registration.descriptor.version.trim().is_empty() {
        return Err(format!("capability version is required: {id}").into());
    }
    if !matches!(
        registration.descriptor.execution_mode.as_str(),
        "sync" | "long_running" | "provider_defined"
    ) {
        return Err(format!("invalid execution mode for capability: {id}").into());
    }
    if !matches!(
        registration.descriptor.idempotency.as_str(),
        "safe" | "conditional" | "not_guaranteed" | "provider_defined" | "unknown"
    ) {
        return Err(format!("invalid idempotency contract for capability: {id}").into());
    }
    if !matches!(
        registration.descriptor.retry_policy.as_str(),
        "safe" | "idempotency_key" | "never" | "provider_defined" | "unknown"
    ) {
        return Err(format!("invalid retry policy for capability: {id}").into());
    }
    if !matches!(
        registration.descriptor.concurrency.as_str(),
        "parallel" | "keyed" | "exclusive" | "provider_defined" | "unknown"
    ) {
        return Err(format!("invalid concurrency policy for capability: {id}").into());
    }
    // Every Dashboard-owned capability must declare the OAuth scope that the
    // Gateway will resolve before invoking it. This catches accidental drift
    // between the business registry and the authorization table at startup.
    if registration.descriptor.dashboard_provider
        && registration.descriptor.required_scope.is_none()
    {
        return Err(format!("Dashboard capability is missing required scope: {id}").into());
    }
    if registry.contains_key(&id) {
        return Err(format!("duplicate capability id: {id}").into());
    }
    registration.descriptor.id = id.clone();
    registry.insert(id, registration);
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
    fn third_party_write_and_unknown_mcp_capabilities_require_approval() {
        let plugin_write = registration(
            "third.party.write",
            "Third-party write",
            "Writes through an installed plugin",
            "network_write",
            json!({}),
            CapabilityHandler::PluginCapability("third.party.write".into()),
        );
        let plugin_read = registration(
            "third.party.read",
            "Third-party read",
            "Reads through an installed plugin",
            "read_only",
            json!({}),
            CapabilityHandler::PluginCapability("third.party.read".into()),
        );
        let downstream = registration(
            "mcp.database.drop_table",
            "Drop table",
            "Unknown downstream MCP operation",
            "mcp_downstream",
            json!({}),
            CapabilityHandler::DownstreamMcp("mcp.database.drop_table".into()),
        );

        assert!(plugin_write.descriptor.approval_required);
        assert!(!plugin_read.descriptor.approval_required);
        assert!(downstream.descriptor.approval_required);
        assert_eq!(
            policy::effective_risk_level(
                &downstream.descriptor.id,
                &downstream.descriptor.risk_level
            ),
            "R3"
        );
    }

    #[test]
    fn first_party_authoring_tools_are_local_without_interactive_approval() {
        let mut item = registration(
            "extension.plugin.build",
            "构建插件",
            "在扩展工作区运行固定构建流程",
            "local_action",
            json!({
                "type": "object",
                "properties": {"workspace_root": {"type": "string"}},
                "required": ["workspace_root"]
            }),
            CapabilityHandler::PluginCapability("extension.plugin.build".into()),
        );
        item.descriptor.source = "plugin:com.himind.extension-development-tools".to_string();
        item.descriptor.availability = CapabilityAvailability::Local;
        apply_registry_metadata(&mut item.descriptor, &item.handler);

        assert!(is_trusted_local_authoring_capability(
            &item.descriptor,
            &item.handler
        ));
        assert!(!item.descriptor.approval_required);
    }

    #[test]
    fn authoring_tool_workspace_guard_rejects_missing_binding() {
        let requested = std::env::temp_dir().join(format!(
            "himind-authoring-workspace-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&requested).unwrap();
        let error = validate_extension_tool_workspace(&json!({
            "workspace_root": requested.to_string_lossy()
        }))
        .unwrap_err()
        .to_string();
        let _ = std::fs::remove_dir_all(&requested);
        assert!(
            error.contains("extension_workspace_unbound")
                || error.contains("extension_workspace_mismatch")
        );
    }

    #[test]
    fn filesystem_delete_defaults_to_preview_without_side_effect() {
        let root = std::env::temp_dir().join(format!(
            "himind-delete-preview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("important.txt");
        std::fs::write(&file, b"keep").unwrap();
        let mut options = crate::Options::from_env();
        options.state_path = root.join("state.json");
        options.effective_mode = crate::app::runtime_mode::AgentMode::Independent;
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let result = gateway
            .filesystem_delete(json!({"path": file, "permanent": false}))
            .unwrap();
        assert_eq!(result["preview"], true);
        assert_eq!(result["requires_permanent_confirmation"], true);
        assert!(file.exists(), "preview must not delete the target");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn filesystem_delete_requires_recursive_for_directories() {
        let root = std::env::temp_dir().join(format!(
            "himind-delete-directory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let mut options = crate::Options::from_env();
        options.state_path = root.join("state.json");
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let error = gateway
            .filesystem_delete(json!({"path": root.join("nested"), "permanent": true}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("recursive=true"));
        let _ = std::fs::remove_dir_all(root);
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
        assert!(item.descriptor.dashboard_provider);
        assert_eq!(item.descriptor.execution_mode, "sync");
        assert_eq!(
            item.descriptor.required_scope.as_deref(),
            Some(crate::api::oauth::BUSINESS_EXHIBIT_READ_SCOPE)
        );
        assert_eq!(
            item.descriptor.dashboard_route.as_deref(),
            Some("/api/integrations/ai/business/exhibits/{exhibit_id}")
        );
        assert_eq!(item.descriptor.idempotency, "safe");
    }

    #[test]
    fn registry_rejects_dashboard_capability_without_scope() {
        let mut registry = BTreeMap::new();
        let mut item = registration(
            "business.test",
            "Test",
            "Test",
            "read_only",
            json!({}),
            CapabilityHandler::SystemHealth,
        );
        item.descriptor.source = "builtin:test-provider".to_string();
        item.descriptor.dashboard_provider = true;
        item.descriptor.required_scope = None;
        let error = insert_registration(&mut registry, item).unwrap_err();
        assert!(error
            .to_string()
            .contains("Dashboard capability is missing required scope"));
    }

    #[test]
    fn svn_checkout_reports_long_running_contract() {
        let item = registration(
            "exhibit.workspace.checkout",
            "检出展项工作区",
            "检出 SVN 工作区",
            "network_write",
            json!({}),
            CapabilityHandler::SvnWorkspaceCheckout,
        );
        assert_eq!(item.descriptor.execution_mode, "long_running");
        assert!(item.descriptor.supports_progress);
        assert!(item.descriptor.supports_cancel);
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
        for capability_id in [
            "ai.client.list",
            "ai.client.status",
            "ai.client.import",
            "ai.client.remove",
            "ai.client.import.plan",
            "ai.client.remove.plan",
        ] {
            assert!(
                visible.iter().any(|item| item.id == capability_id),
                "missing AI client capability: {capability_id}"
            );
        }
        assert!(visible
            .iter()
            .find(|item| item.id == "ai.client.import")
            .is_some_and(|item| item.risk_level == "local_write"));
        assert!(visible
            .iter()
            .find(|item| item.id == "ai.client.remove")
            .is_some_and(|item| item.risk_level == "local_write"));
        assert!(visible
            .iter()
            .find(|item| item.id == "ai.client.import.plan")
            .is_some_and(|item| item.risk_level == "read_only"));
        assert!(visible
            .iter()
            .find(|item| item.id == "ai.client.remove.plan")
            .is_some_and(|item| item.risk_level == "read_only"));
        for capability_id in [
            "ai.service.list",
            "ai.service.custom.upsert",
            "ai.service.custom.remove",
            "ai.service.custom.list_models",
        ] {
            assert!(
                visible.iter().any(|item| item.id == capability_id),
                "missing AI service capability: {capability_id}"
            );
        }
        assert!(visible
            .iter()
            .find(|item| item.id == "ai.service.custom.upsert")
            .is_some_and(|item| item.risk_level == "local_write"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ai_client_capability_schemas_follow_the_adapter_registry() {
        let mut options = crate::Options::from_env();
        options.effective_mode = crate::app::runtime_mode::AgentMode::Independent;
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let registry = gateway.registry().unwrap();
        let expected = crate::app::ai_provider_import::known_adapter_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();

        for capability_id in [
            "ai.client.import",
            "ai.client.remove",
            "ai.client.import.plan",
            "ai.client.remove.plan",
        ] {
            let actual = registry[capability_id]
                .descriptor
                .input_schema
                .pointer("/properties/target/enum")
                .and_then(Value::as_array)
                .expect("AI client target enum must be present")
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "target enum drifted for {capability_id}");
        }
    }

    #[test]
    fn connected_gateway_projects_new_dashboard_catalog_capabilities() {
        let root = std::env::temp_dir().join(format!(
            "himind-capability-catalog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut options = crate::Options::from_env();
        options.state_path = root.join("agent-state.json");
        options.effective_mode = crate::app::runtime_mode::AgentMode::Connected;
        let mut gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        gateway.business_provider = Arc::new(DashboardCatalogProvider::from_snapshot(
            &gateway.options,
            BusinessCatalogSnapshot::dashboard(
                "generation-test".into(),
                vec![BusinessCapabilityContract {
                    id: "business.catalog.example.list".into(),
                    version: "1.0.0".into(),
                    name: "目录示例".into(),
                    description: "读取目录示例。".into(),
                    risk_level: "read_only".into(),
                    http_method: "GET".into(),
                    scope: "business.example.read".into(),
                    route: "/api/integrations/ai/business/examples".into(),
                    input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
                    execution_mode: "sync".into(),
                    supports_progress: false,
                    supports_cancel: false,
                    idempotency: "safe".into(),
                    retry_policy: "safe".into(),
                    concurrency: "parallel".into(),
                    approval_required: false,
                }],
            ),
        ));
        let visible = gateway
            .list_capabilities(&InvocationContext::local_http())
            .unwrap();
        let dynamic = visible
            .iter()
            .find(|item| item.id == "business.catalog.example.list")
            .expect("catalog capability must be projected");
        assert_eq!(dynamic.source, "dashboard:catalog");
        assert_eq!(
            dynamic.required_scope.as_deref(),
            Some("business.example.read")
        );
        assert_eq!(
            dynamic.dashboard_route.as_deref(),
            Some("/api/integrations/ai/business/examples")
        );
        assert!(!visible.iter().any(|item| item.id == "context.resolve"));
        assert!(!visible.iter().any(|item| item.id == "operation.get"));
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
        validate_mcp_capability_workspace(&mcp, "software.distribution.artifact.inspect", &input)
            .unwrap();
        let _ = std::fs::remove_dir_all(input["workspace_root"].as_str().unwrap());
    }
}
